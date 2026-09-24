//! Fixed-memory equi-depth property histograms over external-sort output.

use std::collections::BTreeMap;

use super::{HISTOGRAM_BUCKETS, PropertyBucket};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::SortedIndexRecords,
};

/// Expected non-null values for one dense histogram ordinal.
pub(super) struct HistogramInput {
	/// Dense ordinal prepended to every external-sort key.
	pub(super) ordinal: u32,
	/// Exact number of non-null values expected for the property.
	pub(super) value_count: usize,
}

/// Bounded result retained after one sorted histogram group is consumed.
pub(super) struct HistogramOutput {
	/// Exact number of distinct canonical values.
	pub(super) distinct_count: u64,
	/// At most [`HISTOGRAM_BUCKETS`] equi-depth buckets.
	pub(super) buckets: Vec<PropertyBucket>,
}

/// Consumes composite `{ordinal:u32 BE, encoded_value}` keys while retaining
/// at most one value boundary and 100 completed buckets per histogram.
///
/// # Errors
///
/// Returns an invalid-storage status when the sorted stream, input counts, or
/// counter ranges disagree.
pub(super) fn finish_histograms(
	records: &SortedIndexRecords,
	inputs: &[HistogramInput],
) -> FerrosGrynnResult<BTreeMap<u32, HistogramOutput>> {
	let expected =
		inputs.iter().map(|input| (input.ordinal, input.value_count)).collect::<BTreeMap<_, _>>();
	let mut output = BTreeMap::new();
	let mut current = None::<HistogramBuilder>;
	records.visit_raw(|key, _sequence| {
		let (ordinal, value) = split_key(key)?;
		if current.as_ref().is_some_and(|builder| builder.ordinal != ordinal) {
			let finished = current
				.take()
				.ok_or_else(|| invalid("property histogram state disappeared"))?
				.finish()?;
			if output.insert(finished.0, finished.1).is_some() {
				return Err(invalid("property histogram ordinal is not contiguous"));
			}
		}
		if current.is_none() {
			let count = *expected
				.get(&ordinal)
				.ok_or_else(|| invalid("sorted property value has an unknown histogram ordinal"))?;
			current = Some(HistogramBuilder::new(ordinal, count));
		}
		current.as_mut().ok_or_else(|| invalid("property histogram state disappeared"))?.push(value)
	})?;
	if let Some(builder) = current {
		let (ordinal, finished) = builder.finish()?;
		if output.insert(ordinal, finished).is_some() {
			return Err(invalid("property histogram ordinal is duplicated"));
		}
	}
	for input in inputs {
		if input.value_count == 0 {
			output.insert(input.ordinal, HistogramOutput {
				distinct_count: 0,
				buckets: Vec::new(),
			});
		} else if !output.contains_key(&input.ordinal) {
			return Err(invalid("sorted property values omitted a non-empty histogram"));
		}
	}
	Ok(output)
}

/// Bounded state for one contiguous property group in sort order.
struct HistogramBuilder {
	/// Dense property ordinal currently consumed.
	ordinal: u32,
	/// Exact number of values promised by the scan pass.
	expected: usize,
	/// Values consumed so far.
	seen: usize,
	/// Exact distinct values consumed so far.
	distinct: u64,
	/// Previous canonical value for global distinct counting.
	previous: Option<Vec<u8>>,
	/// Planned item count of every equi-depth bucket.
	bucket_sizes: Vec<usize>,
	/// Bucket currently being filled.
	bucket_index: usize,
	/// Values consumed by the current bucket.
	bucket_seen: usize,
	/// Distinct values in the current bucket.
	bucket_distinct: u64,
	/// Previous canonical value for bucket-local distinct counting.
	bucket_previous: Option<Vec<u8>>,
	/// Inclusive lower canonical boundary of the current bucket.
	bucket_lower: Option<Vec<u8>>,
	/// Inclusive upper canonical boundary of the current bucket.
	bucket_upper: Option<Vec<u8>>,
	/// Completed bounded buckets.
	buckets: Vec<PropertyBucket>,
}

impl HistogramBuilder {
	/// Creates a bucket plan for one exact input cardinality.
	fn new(ordinal: u32, expected: usize) -> Self {
		let bucket_count = expected.min(HISTOGRAM_BUCKETS);
		let base = expected / bucket_count.max(1);
		let larger = expected % bucket_count.max(1);
		let bucket_sizes =
			(0..bucket_count).map(|index| base + usize::from(index < larger)).collect();
		Self {
			ordinal,
			expected,
			seen: 0,
			distinct: 0,
			previous: None,
			bucket_sizes,
			bucket_index: 0,
			bucket_seen: 0,
			bucket_distinct: 0,
			bucket_previous: None,
			bucket_lower: None,
			bucket_upper: None,
			buckets: Vec::new(),
		}
	}

	/// Consumes one canonical value in non-decreasing order.
	///
	/// # Errors
	///
	/// Returns an invalid-storage status for count overflow or a sizing
	/// mismatch.
	fn push(&mut self, value: &[u8]) -> FerrosGrynnResult<()> {
		if self.seen >= self.expected {
			return Err(invalid("property histogram received too many values"));
		}
		if self.previous.as_deref() != Some(value) {
			self.distinct = self
				.distinct
				.checked_add(1)
				.ok_or_else(|| invalid("property distinct count overflows u64"))?;
		}
		if self.bucket_previous.as_deref() != Some(value) {
			self.bucket_distinct = self
				.bucket_distinct
				.checked_add(1)
				.ok_or_else(|| invalid("property bucket distinct count overflows u64"))?;
		}
		if self.bucket_lower.is_none() {
			self.bucket_lower = Some(value.to_vec());
		}
		self.bucket_upper = Some(value.to_vec());
		self.previous = Some(value.to_vec());
		self.bucket_previous = Some(value.to_vec());
		self.seen = self
			.seen
			.checked_add(1)
			.ok_or_else(|| invalid("property histogram count overflows usize"))?;
		self.bucket_seen = self
			.bucket_seen
			.checked_add(1)
			.ok_or_else(|| invalid("property bucket count overflows usize"))?;
		let target = *self
			.bucket_sizes
			.get(self.bucket_index)
			.ok_or_else(|| invalid("property histogram exceeded its bucket plan"))?;
		if self.bucket_seen == target {
			self.flush_bucket()?;
		}
		Ok(())
	}

	/// Finalizes the current complete bucket.
	///
	/// # Errors
	///
	/// Returns an invalid-storage status if required boundaries are absent or a
	/// counter cannot be represented.
	fn flush_bucket(&mut self) -> FerrosGrynnResult<()> {
		let lower = self
			.bucket_lower
			.take()
			.ok_or_else(|| invalid("property bucket has no lower bound"))?;
		let upper = self
			.bucket_upper
			.take()
			.ok_or_else(|| invalid("property bucket has no upper bound"))?;
		self.buckets.push(PropertyBucket {
			lower,
			upper,
			frequency: u64::try_from(self.bucket_seen)
				.map_err(|_error| invalid("property bucket frequency exceeds u64"))?,
			distinct_count: self.bucket_distinct,
		});
		self.bucket_index = self
			.bucket_index
			.checked_add(1)
			.ok_or_else(|| invalid("property bucket ordinal overflows usize"))?;
		self.bucket_seen = 0;
		self.bucket_distinct = 0;
		self.bucket_previous = None;
		Ok(())
	}

	/// Verifies exact consumption and returns the bounded result.
	///
	/// # Errors
	///
	/// Returns an invalid-storage status when the scan and sizing passes
	/// disagree.
	fn finish(self) -> FerrosGrynnResult<(u32, HistogramOutput)> {
		if self.seen != self.expected
			|| self.bucket_seen != 0
			|| self.bucket_index != self.bucket_sizes.len()
		{
			return Err(invalid("property histogram value count disagrees with its sort input"));
		}
		Ok((self.ordinal, HistogramOutput {
			distinct_count: self.distinct,
			buckets: self.buckets,
		}))
	}
}

/// Splits `{ordinal:u32 BE, canonical_value}` external-sort keys.
///
/// # Errors
///
/// Returns an invalid-storage status for a missing ordinal or value payload.
fn split_key(key: &[u8]) -> FerrosGrynnResult<(u32, &[u8])> {
	let prefix =
		key.get(..4).ok_or_else(|| invalid("sorted property key has no histogram ordinal"))?;
	let value = key
		.get(4..)
		.filter(|value| !value.is_empty())
		.ok_or_else(|| invalid("sorted property key has no encoded value"))?;
	Ok((u32::from_be_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]), value))
}

/// Builds an invalid-storage-file status for sorter contract violations.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::{AtomicU64, Ordering};

	use super::*;
	use crate::storage::ExternalIndexSorter;

	static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

	#[test]
	fn spilled_values_keep_exact_distinct_counts_and_bounded_buckets() -> FerrosGrynnResult<()> {
		let root = std::env::temp_dir().join(format!(
			"fg-stats-sort-{}-{}",
			std::process::id(),
			NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
		));
		std::fs::create_dir(&root).map_err(|source| {
			FerrosGrynnError::storage_io_failure("create statistics sorter test directory")
				.with_technical_source(source)
		})?;
		let result = (|| {
			let mut sorter = ExternalIndexSorter::with_budget(&root, 64);
			for sequence in (0..250_u64).rev() {
				let mut key = 7_u32.to_be_bytes().to_vec();
				key.extend_from_slice(format!("value-{:03}", sequence / 2).as_bytes());
				sorter.push_raw(key, sequence)?;
			}
			let records = sorter.finish()?;
			let output = finish_histograms(&records, &[HistogramInput {
				ordinal: 7,
				value_count: 250,
			}])?;
			let histogram = output.get(&7).ok_or_else(|| invalid("test histogram is absent"))?;
			assert_eq!(histogram.distinct_count, 125);
			assert_eq!(histogram.buckets.len(), HISTOGRAM_BUCKETS);
			assert_eq!(histogram.buckets.iter().map(|bucket| bucket.frequency).sum::<u64>(), 250);
			assert!(histogram.buckets.iter().all(|bucket| bucket.lower <= bucket.upper));
			Ok(())
		})();
		drop(std::fs::remove_dir_all(root));
		result
	}
}
