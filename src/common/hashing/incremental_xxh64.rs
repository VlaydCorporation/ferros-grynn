use crate::common::hashing::HashFunction;

const PRIME_1: u64 = 11400714785074694791;
const PRIME_2: u64 = 14029467366897019727;
const PRIME_3: u64 = 1609587929392839161;
const PRIME_4: u64 = 9650029242287828579;
const PRIME_5: u64 = 2870177450012600261;

#[derive(Default)]
pub struct IncrementalXXH64;

impl HashFunction for IncrementalXXH64 {
    fn initialize(&self, seed: u64) -> u64 {
        seed + PRIME_5
    }

    fn update(&self, state: u64, value: u64) -> u64 {
        let mut hash = state;
        let mut block = value;
        hash += 8;
        block *= PRIME_2;
        block = block.rotate_left(31);
        block *= PRIME_1;
        hash ^= block;
        hash = hash.rotate_left(27) * PRIME_1 + PRIME_4;
        hash
    }

    fn finalize(&self, state: u64) -> u64 {
        let mut hash = state;
        hash ^= hash >> 33;
        hash *= PRIME_2;
        hash ^= hash >> 29;
        hash *= PRIME_3;
        hash ^= hash >> 32;
        hash
    }
}