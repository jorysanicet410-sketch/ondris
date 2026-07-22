//! From-scratch reimplementation of the xoshiro256** PRNG, validated
//! below against the real `rand_xoshiro` crate that `ondris-pow` uses.
//! Same rationale as `blake3_ref`: nail the exact seeding convention and
//! output sequence here, with fast Rust-side iteration, before
//! transcribing to OpenCL C.

pub struct Xoshiro256StarStar {
    s: [u64; 4],
}

impl Xoshiro256StarStar {
    /// Matches `rand_xoshiro::Xoshiro256StarStar::from_seed`: the 32-byte
    /// seed is interpreted directly as four little-endian u64 words,
    /// with no splitmix64 expansion step (that's only used by
    /// `seed_from_u64`, which we don't use — `ondris-pow` seeds straight
    /// from a 32-byte BLAKE3 hash via `from_seed`).
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let mut s = [0u64; 4];
        for (i, word) in s.iter_mut().enumerate() {
            *word = u64::from_le_bytes(seed[i * 8..i * 8 + 8].try_into().unwrap());
        }
        Xoshiro256StarStar { s }
    }

    pub fn next_u64(&mut self) -> u64 {
        let result = (self.s[1].wrapping_mul(5)).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;

        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{RngCore, SeedableRng};
    use rand_xoshiro::Xoshiro256StarStar as RealXoshiro;

    fn check(seed: [u8; 32]) {
        let mut ours = Xoshiro256StarStar::from_seed(seed);
        let mut real = RealXoshiro::from_seed(seed);
        for i in 0..1000 {
            let a = ours.next_u64();
            let b = real.next_u64();
            assert_eq!(a, b, "sequence diverged at step {i} for seed {seed:?}");
        }
    }

    #[test]
    fn matches_real_rand_xoshiro_for_various_seeds() {
        // Deliberately NOT testing the all-zero seed here: `rand_xoshiro`
        // special-cases it (guarding against the degenerate all-zero
        // xoshiro256 state), so it diverges from a direct byte mapping.
        // Irrelevant for us in practice — our seed is always a BLAKE3
        // hash output, never the all-zero value.
        check([0xffu8; 32]);
        let mut seed = [0u8; 32];
        for (i, b) in seed.iter_mut().enumerate() {
            *b = i as u8;
        }
        check(seed);

        // A handful of real BLAKE3 outputs, since that's what actually
        // seeds this PRNG in ondris_hash.
        for input in [
            &b""[..],
            b"ondris",
            b"a longer piece of test input for seeding",
        ] {
            check(*blake3::hash(input).as_bytes());
        }
    }

    #[test]
    fn matches_real_rand_xoshiro_for_random_seeds() {
        let mut rng = rand::rngs::OsRng;
        for _ in 0..20 {
            let mut seed = [0u8; 32];
            rng.fill_bytes(&mut seed);
            check(seed);
        }
    }
}
