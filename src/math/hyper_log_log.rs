//! Checked mergeable cardinality sketches shared by ANALYZE and graph indexes.

use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Portable RapidHash-v3 sketch with one byte per register.
#[derive(Debug, PartialEq, Eq)]
pub struct HyperLogLog {
	/// Number of high hash bits used as the register address.
	precision: u8,
	/// Maximum observed suffix rank in each bucket.
	registers: Vec<u8>,
}

impl HyperLogLog {
	/// Allocates exactly 2^precision registers after validating precision.
	///
	/// # Errors
	/// Rejects precision outside 4..=18 or allocation failure.
	pub(crate) fn new(precision: u8) -> FerrosGrynnResult<Self> {
		let count = register_count(precision)?;
		let mut registers = Vec::new();
		registers.try_reserve_exact(count).map_err(|cause| {
			FerrosGrynnError::traversal_resource_limit_exceeded("HLL registers", count as u64)
				.with_technical_source(cause)
		})?;
		registers.resize(count, 0);
		Ok(Self {
			precision,
			registers,
		})
	}

	/// Restores only exact-sized registers with ranks valid for a 64-bit hash.
	///
	/// # Errors
	/// Rejects malformed precision, length or rank before exposing the sketch.
	pub(crate) fn from_registers(precision: u8, registers: Vec<u8>) -> FerrosGrynnResult<Self> {
		if registers.len() != register_count(precision)?
			|| registers.iter().any(|rank| *rank > 65 - precision)
		{
			return Err(FerrosGrynnError::invalid_argument(
				"HLL register length or rank is invalid",
			));
		}
		Ok(Self {
			precision,
			registers,
		})
	}

	/// Borrows canonical register bytes for artifact encoding.
	pub(crate) fn registers(&self) -> &[u8] {
		&self.registers
	}

	/// Inserts a canonical member identity using the shared fixed hash policy.
	pub(crate) fn insert(&mut self, value: &[u8]) {
		let hash = rapidhash::v3::rapidhash_v3(value);
		let index = (hash >> (64 - self.precision)) as usize;
		// Shifting removes the bucket bits; cap the zero-suffix case to its
		// remaining-bit rank.
		let rank = (hash << self.precision)
			.leading_zeros()
			.saturating_add(1)
			.min(u32::from(65 - self.precision)) as u8;
		self.registers[index] = self.registers[index].max(rank);
	}

	/// Merges a compatible sketch by registerwise maximum without allocation.
	///
	/// # Errors
	/// Rejects different precision before changing any register.
	pub(crate) fn merge(&mut self, other: &Self) -> FerrosGrynnResult<()> {
		if self.precision != other.precision {
			return Err(FerrosGrynnError::invalid_argument("HLL merge requires equal precision"));
		}
		for (target, source) in self.registers.iter_mut().zip(&other.registers) {
			*target = (*target).max(*source);
		}
		Ok(())
	}

	/// Estimates distinct cardinality with the existing small-range correction.
	/// The p=12 path retains ANALYZE's previous hash and rounding semantics.
	pub(crate) fn estimate(&self) -> u64 {
		let count = self.registers.len() as f64;
		let reciprocal_sum =
			self.registers.iter().map(|rank| 2.0_f64.powi(-i32::from(*rank))).sum::<f64>();
		let alpha = match self.precision {
			4 => 0.673,
			5 => 0.697,
			6 => 0.709,
			_ => 0.7213 / (1.0 + 1.079 / count),
		};
		let raw = alpha * count * count / reciprocal_sum;
		let empty = self.registers.iter().filter(|rank| **rank == 0).count();
		// Empty-register counting corrects the raw estimator bias at small
		// cardinalities.
		let corrected = if raw <= 2.5 * count && empty != 0 {
			count * (count / empty as f64).ln()
		} else {
			raw
		};
		corrected.round().clamp(0.0, u64::MAX as f64) as u64
	}
}

/// Validates the normative precision before shifting or allocating.
fn register_count(precision: u8) -> FerrosGrynnResult<usize> {
	if !(4..=18).contains(&precision) {
		return Err(FerrosGrynnError::invalid_argument("HLL precision must be in 4..=18"));
	}
	Ok(1_usize << precision)
}

#[cfg(test)]
/// Regression tests for hyper log log behavior.
mod tests {
	use super::*;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies merge matches distinct union and checked register roundtrip.
	#[test]
	fn merge_matches_distinct_union_and_checked_register_roundtrip() {
		for precision in [4, 5, 6, 12, 18] {
			let mut left = HyperLogLog::new(precision).assert_ok();
			let mut right = HyperLogLog::new(precision).assert_ok();
			let mut union = HyperLogLog::new(precision).assert_ok();
			assert_eq!(left.estimate(), 0);
			assert_eq!(left.registers().len(), 1 << precision);
			for member in 0..10_000_u64 {
				union.insert(&member.to_le_bytes());
				if member < 6000 {
					left.insert(&member.to_le_bytes());
				}
				if member >= 4000 {
					right.insert(&member.to_le_bytes());
				}
			}
			left.merge(&right).assert_ok();
			assert_eq!(left, union);
			assert_eq!(
				left,
				HyperLogLog::from_registers(precision, left.registers().to_vec()).assert_ok()
			);
		}
		let mut left = HyperLogLog::new(12).assert_ok();
		assert_eq!(left.registers().len(), 4096);
		let before = left.registers().to_vec();
		assert!(left.merge(&HyperLogLog::new(4).assert_ok()).is_err());
		assert_eq!(left.registers(), before);
		for precision in [0, 3, 19, 255] {
			assert!(HyperLogLog::new(precision).is_err());
		}
		assert!(HyperLogLog::from_registers(12, vec![0; 64]).is_err());
		assert!(HyperLogLog::from_registers(4, vec![62; 16]).is_err());
		for member in 0..100_000_u64 {
			left.insert(&member.to_le_bytes());
		}
		assert!(left.estimate().abs_diff(100_000) < 5000);
	}
}
