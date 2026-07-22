//! OpenCL plumbing: find a device, build `kernel.cl`, and expose the two
//! kernels it defines (`ondris_hash_debug` for known-answer validation,
//! `ondris_mine` for real mining).

use opencl3::command_queue::{CommandQueue, CL_QUEUE_PROFILING_ENABLE};
use opencl3::context::Context;
use opencl3::device::{Device, CL_DEVICE_TYPE_GPU};
use opencl3::kernel::{ExecuteKernel, Kernel};
use opencl3::memory::{Buffer, CL_MEM_READ_ONLY, CL_MEM_READ_WRITE, CL_MEM_WRITE_ONLY};
use opencl3::platform::get_platforms;
use opencl3::program::Program;
use opencl3::types::{cl_int, cl_ulong, CL_BLOCKING, CL_NON_BLOCKING};
use std::ptr;

const KERNEL_SOURCE: &str = include_str!("kernel.cl");

pub struct Gpu {
    pub context: Context,
    pub queue: CommandQueue,
    pub program: Program,
    pub device_name: String,
}

impl Gpu {
    pub fn new() -> anyhow::Result<Self> {
        let platforms = get_platforms()?;
        anyhow::ensure!(
            !platforms.is_empty(),
            "no OpenCL platforms found on this system"
        );

        let mut device_id = None;
        for platform in &platforms {
            if let Ok(ids) = platform.get_devices(CL_DEVICE_TYPE_GPU) {
                if let Some(id) = ids.into_iter().next() {
                    device_id = Some(id);
                    break;
                }
            }
        }
        let device_id = device_id.ok_or_else(|| anyhow::anyhow!("no OpenCL GPU device found"))?;
        let device = Device::new(device_id);
        let device_name = device
            .name()
            .unwrap_or_else(|_| "<unknown device>".to_string());

        let context = Context::from_device(&device)?;
        // `create_default` is deprecated in favor of the properties-list
        // constructor added for OpenCL 2.0, but it's still fully
        // functional and opencl3 0.9's replacement API wasn't worth
        // chasing precisely here — this is a cosmetic nag, not a
        // correctness concern.
        #[allow(deprecated)]
        let queue = CommandQueue::create_default(&context, CL_QUEUE_PROFILING_ENABLE)?;

        let program = Program::create_and_build_from_source(&context, KERNEL_SOURCE, "")
            .map_err(|build_log| anyhow::anyhow!("OpenCL kernel build failed:\n{build_log}"))?;

        Ok(Gpu {
            context,
            queue,
            program,
            device_name,
        })
    }

    fn kernel(&self, name: &str) -> anyhow::Result<Kernel> {
        Ok(Kernel::create(&self.program, name)?)
    }

    fn buffer_ro(&self, data: &[u8]) -> anyhow::Result<Buffer<u8>> {
        let mut buf = unsafe {
            Buffer::<u8>::create(
                &self.context,
                CL_MEM_READ_ONLY,
                data.len().max(1),
                ptr::null_mut(),
            )?
        };
        unsafe {
            self.queue
                .enqueue_write_buffer(&mut buf, CL_BLOCKING, 0, data, &[])?;
        }
        Ok(buf)
    }

    fn buffer_rw(&self, len: usize) -> anyhow::Result<Buffer<u8>> {
        Ok(unsafe {
            Buffer::<u8>::create(
                &self.context,
                CL_MEM_READ_WRITE,
                len.max(1),
                ptr::null_mut(),
            )?
        })
    }

    /// Runs `ondris_hash_debug` for one (header, nonce, dataset) and
    /// returns the 32-byte digest. Used only to validate the kernel
    /// against the CPU reference — never trusted on its own.
    pub fn hash_debug(
        &self,
        dataset: &[u8],
        header_bytes: &[u8],
        nonce: u64,
        scratchpad_size: u32,
        mix_rounds: u32,
    ) -> anyhow::Result<[u8; 32]> {
        anyhow::ensure!(
            header_bytes.len() + 8 <= 256,
            "header too long for the kernel's fixed input buffer"
        );

        let dataset_buf = self.buffer_ro(dataset)?;
        let header_buf = self.buffer_ro(header_bytes)?;
        let scratchpad_buf = self.buffer_rw(scratchpad_size as usize)?;
        let digest_buf = self.buffer_rw(32)?;

        let kernel = self.kernel("ondris_hash_debug")?;
        let event = unsafe {
            ExecuteKernel::new(&kernel)
                .set_arg(&dataset_buf)
                .set_arg(&(dataset.len() as cl_ulong))
                .set_arg(&header_buf)
                .set_arg(&(header_bytes.len() as u32))
                .set_arg(&(nonce as cl_ulong))
                .set_arg(&scratchpad_size)
                .set_arg(&mix_rounds)
                .set_arg(&scratchpad_buf)
                .set_arg(&digest_buf)
                .set_global_work_size(1)
                .enqueue_nd_range(&self.queue)?
        };
        event.wait()?;

        let mut out = [0u8; 32];
        unsafe {
            self.queue
                .enqueue_read_buffer(&digest_buf, CL_BLOCKING, 0, &mut out, &[])?
        }
        .wait()?;
        Ok(out)
    }

    /// Starts a mining session for one epoch's dataset: uploads the
    /// dataset and allocates the (potentially huge — `batch_size *
    /// scratchpad_size`) scratchpad pool exactly once, since re-doing
    /// that on every single batch (as an earlier version of this file
    /// did) turned multi-gigabyte transfers/allocations into the
    /// dominant cost instead of the actual hashing.
    pub fn mining_session(
        &self,
        dataset: &[u8],
        scratchpad_size: u32,
        batch_size: usize,
    ) -> anyhow::Result<MiningSession<'_>> {
        let dataset_buf = self.buffer_ro(dataset)?;
        let scratchpad_pool = self.buffer_rw(scratchpad_size as usize * batch_size)?;
        let result_nonce = self.buffer_rw(8)?;
        let result_found = self.buffer_rw(4)?;
        let kernel = self.kernel("ondris_mine")?;
        Ok(MiningSession {
            gpu: self,
            dataset_buf,
            dataset_len: dataset.len() as u64,
            scratchpad_pool,
            scratchpad_size,
            batch_size,
            result_nonce,
            result_found,
            kernel,
        })
    }
}

/// Holds the buffers that stay constant across many kernel launches while
/// mining a single block (and, for the dataset, across every block in the
/// same epoch): the dataset and the scratchpad pool. Only the header,
/// nonce base and target change per launch, and those are a few hundred
/// bytes — cheap to re-upload every time.
pub struct MiningSession<'a> {
    gpu: &'a Gpu,
    dataset_buf: Buffer<u8>,
    dataset_len: u64,
    scratchpad_pool: Buffer<u8>,
    scratchpad_size: u32,
    batch_size: usize,
    result_nonce: Buffer<u8>,
    result_found: Buffer<u8>,
    kernel: Kernel,
}

impl MiningSession<'_> {
    /// Tries nonces `nonce_base..nonce_base+batch_size`. Returns
    /// `Some(nonce)` if any of them met `target`.
    pub fn try_batch(
        &mut self,
        header_bytes: &[u8],
        nonce_base: u64,
        mix_rounds: u32,
        target: &[u8; 32],
    ) -> anyhow::Result<Option<u64>> {
        anyhow::ensure!(
            header_bytes.len() + 8 <= 256,
            "header too long for the kernel's fixed input buffer"
        );

        let header_buf = self.gpu.buffer_ro(header_bytes)?;
        let target_buf = self.gpu.buffer_ro(target)?;

        unsafe {
            self.gpu.queue.enqueue_write_buffer(
                &mut self.result_found,
                CL_BLOCKING,
                0,
                &0i32.to_le_bytes(),
                &[],
            )?;
        }

        let event = unsafe {
            ExecuteKernel::new(&self.kernel)
                .set_arg(&self.dataset_buf)
                .set_arg(&self.dataset_len)
                .set_arg(&header_buf)
                .set_arg(&(header_bytes.len() as u32))
                .set_arg(&(nonce_base as cl_ulong))
                .set_arg(&self.scratchpad_size)
                .set_arg(&mix_rounds)
                .set_arg(&self.scratchpad_pool)
                .set_arg(&target_buf)
                .set_arg(&self.result_nonce)
                .set_arg(&self.result_found)
                .set_global_work_size(self.batch_size)
                .enqueue_nd_range(&self.gpu.queue)?
        };
        event.wait()?;

        let mut found_bytes = [0u8; 4];
        unsafe {
            self.gpu.queue.enqueue_read_buffer(
                &self.result_found,
                CL_BLOCKING,
                0,
                &mut found_bytes,
                &[],
            )?
        }
        .wait()?;
        let found = cl_int::from_le_bytes(found_bytes);

        if found != 0 {
            let mut nonce_bytes = [0u8; 8];
            unsafe {
                self.gpu.queue.enqueue_read_buffer(
                    &self.result_nonce,
                    CL_BLOCKING,
                    0,
                    &mut nonce_bytes,
                    &[],
                )?
            }
            .wait()?;
            Ok(Some(u64::from_le_bytes(nonce_bytes)))
        } else {
            Ok(None)
        }
    }
}

#[allow(dead_code)]
fn silence_unused_import_warning() {
    let _ = CL_NON_BLOCKING;
    let _ = CL_MEM_WRITE_ONLY;
}
