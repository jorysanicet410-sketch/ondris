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
    pub max_compute_units: u32,
    pub max_mem_alloc_size: u64,
    pub global_mem_size: u64,
    pub max_work_group_size: usize,
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
        let max_compute_units = device.max_compute_units().unwrap_or(0);
        let max_mem_alloc_size = device.max_mem_alloc_size().unwrap_or(0);
        let global_mem_size = device.global_mem_size().unwrap_or(0);
        let max_work_group_size = device.max_work_group_size().unwrap_or(0);

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
            max_compute_units,
            max_mem_alloc_size,
            global_mem_size,
            max_work_group_size,
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
        accesses: u32,
    ) -> anyhow::Result<[u8; 32]> {
        anyhow::ensure!(
            header_bytes.len() + 8 <= 144,
            "header too long for the kernel's fixed input buffer"
        );

        let dataset_buf = self.buffer_ro(dataset)?;
        let header_buf = self.buffer_ro(header_bytes)?;
        let digest_buf = self.buffer_rw(32)?;

        let kernel = self.kernel("ondris_hash_debug")?;
        let event = unsafe {
            ExecuteKernel::new(&kernel)
                .set_arg(&dataset_buf)
                .set_arg(&(dataset.len() as cl_ulong))
                .set_arg(&header_buf)
                .set_arg(&(header_bytes.len() as u32))
                .set_arg(&(nonce as cl_ulong))
                .set_arg(&accesses)
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
    /// (read-only, shared-across-every-work-item) dataset exactly once.
    /// Unlike the scratchpad-mixing design this replaced, there's no
    /// per-work-item writable buffer to allocate here at all — each
    /// thread's `mix` state is 128 bytes of private memory computed
    /// entirely inside the kernel — so batch size is no longer bounded by
    /// a `batch_size * multi-megabyte-scratchpad` global allocation.
    pub fn mining_session(&self, dataset: &[u8]) -> anyhow::Result<MiningSession<'_>> {
        let dataset_buf = self.buffer_ro(dataset)?;
        let result_nonce = self.buffer_rw(8)?;
        let result_found = self.buffer_rw(4)?;
        let kernel = self.kernel("ondris_mine")?;
        Ok(MiningSession {
            gpu: self,
            dataset_buf,
            dataset_len: dataset.len() as u64,
            result_nonce,
            result_found,
            kernel,
            cached_header: None,
            cached_target: None,
        })
    }
}

/// Holds the buffers that stay constant across many kernel launches
/// mining the same block: the dataset (whole epoch), and — this is the
/// part that used to be rebuilt and re-uploaded on *every single batch*
/// even though `mine_block_gpu`'s loop only ever varies `nonce_base` —
/// the header and target too. Re-uploading two tiny buffers per batch
/// sounds cheap, but at ~5ms/batch on a fast GPU it's a host round-trip
/// competing with the actual mining work for a slice of every batch;
/// caching them turns that into one upload per block instead of one per
/// batch (tens of thousands of times cheaper in practice).
pub struct MiningSession<'a> {
    gpu: &'a Gpu,
    dataset_buf: Buffer<u8>,
    dataset_len: u64,
    result_nonce: Buffer<u8>,
    result_found: Buffer<u8>,
    kernel: Kernel,
    cached_header: Option<(Vec<u8>, Buffer<u8>)>,
    cached_target: Option<([u8; 32], Buffer<u8>)>,
}

impl MiningSession<'_> {
    /// Tries nonces `nonce_base..nonce_base+batch_size`. Returns
    /// `Some(nonce)` if any of them met `target`.
    pub fn try_batch(
        &mut self,
        header_bytes: &[u8],
        nonce_base: u64,
        accesses: u32,
        target: &[u8; 32],
        batch_size: usize,
    ) -> anyhow::Result<Option<u64>> {
        anyhow::ensure!(
            header_bytes.len() + 8 <= 144,
            "header too long for the kernel's fixed input buffer"
        );

        if self.cached_header.as_ref().map(|(h, _)| h.as_slice()) != Some(header_bytes) {
            let buf = self.gpu.buffer_ro(header_bytes)?;
            self.cached_header = Some((header_bytes.to_vec(), buf));
        }
        if self.cached_target.as_ref().map(|(t, _)| t) != Some(target) {
            let buf = self.gpu.buffer_ro(target)?;
            self.cached_target = Some((*target, buf));
        }
        let header_buf = &self.cached_header.as_ref().unwrap().1;
        let target_buf = &self.cached_target.as_ref().unwrap().1;

        // Non-blocking: this only tells the host call to return
        // immediately, not the device to reorder anything. The queue is
        // in-order (no CL_QUEUE_OUT_OF_ORDER_EXEC_MODE_ENABLE), so the
        // kernel launched right after is still guaranteed to see this
        // write completed first -- we just stop paying a host-side wait
        // for a transfer whose completion we don't need to observe yet.
        unsafe {
            self.gpu.queue.enqueue_write_buffer(
                &mut self.result_found,
                CL_NON_BLOCKING,
                0,
                &0i32.to_le_bytes(),
                &[],
            )?;
        }

        unsafe {
            ExecuteKernel::new(&self.kernel)
                .set_arg(&self.dataset_buf)
                .set_arg(&self.dataset_len)
                .set_arg(header_buf)
                .set_arg(&(header_bytes.len() as u32))
                .set_arg(&(nonce_base as cl_ulong))
                .set_arg(&accesses)
                .set_arg(target_buf)
                .set_arg(&self.result_nonce)
                .set_arg(&self.result_found)
                .set_global_work_size(batch_size)
                .enqueue_nd_range(&self.gpu.queue)?
        };
        // No explicit wait here: the blocking read below is ordered after
        // the kernel on this same in-order queue, so waiting for the read
        // to complete already waits for the kernel too. Removes one full
        // host-GPU round trip per batch compared to waiting on the kernel
        // event and *then* separately enqueuing and waiting on the read.

        let mut found_bytes = [0u8; 4];
        unsafe {
            self.gpu.queue.enqueue_read_buffer(
                &self.result_found,
                CL_BLOCKING,
                0,
                &mut found_bytes,
                &[],
            )?
        };
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
            };
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
