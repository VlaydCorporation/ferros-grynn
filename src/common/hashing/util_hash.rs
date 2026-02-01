use crate::common::hashing::HashFunction;

pub struct UtilHash;

impl HashFunction for UtilHash {
    fn initialize(&self, seed: u64) -> u64 {
        seed
    }

    fn update(&self, state: u64, value: u64) -> u64 {
        self.hash_single_to_u32(state + value) as u64
    }

    fn finalize(&self, state: u64) -> u64 {
        state
    }

    fn hash_single_to_u32(&self, value: u64) -> u32 {
        let mut h = ((value >> 32) ^ value) as u32;
        h ^= (h >> 20) ^ (h >> 12);
        h ^ (h >> 7) ^ (h >> 4)
    }
}