mod incremental_xxh64;
mod util_hash;
mod wy_hash;
mod xor_shift32;

pub use incremental_xxh64::IncrementalXXH64;
pub use wy_hash::WyHash;
pub use xor_shift32::XorShift32HashFunction;

/// A hash function produces a deterministic value based on its input.
///
/// Hash functions are first initialized with a seed (which may be zero),
/// and then updated with a sequence of values that are mixed into the
/// hash state in order.
///
/// A hash function may have internal state, but it may also be stateless
/// if its complete state can be represented solely by the 64-bit
/// intermediate hash value.
///
/// See also:
/// - [`incremental_xxh64`]
/// - [`java_util_hashing`]
/// - [`xor_shift32`]
pub trait HashFunction {
    /// Initialize the hash function with the given seed.
    ///
    /// Different seeds should produce different final hash values.
    ///
    /// # Parameters
    /// - `seed`: The initialization seed for the hash function.
    ///
    /// # Returns
    /// An initialized intermediate hash state.
    fn initialize(&self, seed: u64) -> u64;

    /// Update the hash state by mixing the given value into the provided
    /// intermediate hash state.
    ///
    /// # Parameters
    /// - `intermediate_hash`: The intermediate hash state obtained either
    ///   from [`initialize`] or from a previous call to this method.
    /// - `value`: The value to mix into the hash state.
    ///
    /// # Returns
    /// A new intermediate hash state with the value mixed in.
    fn update(&self, state: u64, value: u64) -> u64;

    /// Produce a final hash value from the given intermediate hash state.
    ///
    /// # Parameters
    /// - `intermediate_hash`: The intermediate hash state from which to
    ///   produce the final hash value.
    ///
    /// # Returns
    /// The final hash value.
    fn finalize(&self, state: u64) -> u64;

    /// Reduce a 64-bit hash value to a 32-bit value.
    ///
    /// # Parameters
    /// - `hash`: The 64-bit hash value to reduce.
    ///
    /// # Returns
    /// A 32-bit representation of the given hash value.
    fn to_u32(&self, hash: u64) -> u32 {
        ((hash >> 32) ^ hash) as u32
    }

    /// Produce a 64-bit hash value from a single input value.
    ///
    /// # Parameters
    /// - `value`: The value to hash.
    ///
    /// # Returns
    /// The 64-bit hash of the given value.
    fn hash_single(&self, value: u64) -> u64 {
        self.finalize(self.update(self.initialize(0), value))
    }

    /// Produce a 32-bit hash value from a single input value.
    ///
    /// # Parameters
    /// - `value`: The value to hash.
    ///
    /// # Returns
    /// The 32-bit hash of the given value.
    fn hash_single_to_u32(&self, value: u64) -> u32 {
        self.to_u32(self.hash_single(value))
    }

    /// Update the hash state by mixing in the given array and all of its
    /// elements, in order.
    ///
    /// This operation also works if the array is `None`, in which case a
    /// special marker value is mixed into the hash state to ensure that
    /// the hashing step leaves a detectable trace.
    ///
    /// Each element is projected to a `u64` value before being mixed into
    /// the hash state, which is why a projection function is required.
    ///
    /// # Parameters
    /// - `intermediate_hash`: The intermediate hash state obtained either
    ///   from [`initialize`] or from a previous call to this method or
    ///   [`update`].
    /// - `array`: The array whose length and elements should be mixed into
    ///   the hash state.
    /// - `projection`: A function that converts each element into a `u64`
    ///   value prior to hashing.
    ///
    /// # Returns
    /// A new intermediate hash state with the array mixed in.
    fn update_with_array(
        &self,
        intermediate_hash: u64,
        array: Option<&[u32]>,
        projection: impl Fn(&u32) -> u64,
    ) -> u64 {
        match array {
            None => self.update(intermediate_hash, u64::MAX),
            Some(slice) => {
                let mut hash = self.update(intermediate_hash, slice.len() as u64);
                for item in slice {
                    hash = self.update(hash, projection(item));
                }
                hash
            }
        }
    }
}