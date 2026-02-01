use crate::common::hashing::HashFunction;

#[derive(Default)]
pub struct XorShift32HashFunction;

impl HashFunction for XorShift32HashFunction {
    fn initialize(&self, seed: u64) -> u64 {
        0
    }

    fn update(&self, state: u64, value: u64) -> u64 {
        self.hash_single_to_u32(state + value) as u64
    }

    fn finalize(&self, state: u64) -> u64 {
        state
    }

    fn hash_single_to_u32(&self, value: u64) -> u32 {
        let mut value = value;
        value ^= value << 21;
        value ^= value >> 35;
        value ^= value << 4;
        ((value >> 32) ^ value) as u32
    }
}