//! Snapshot-bound pull reading; storage and transaction adapters stay private.

use std::fmt;

use super::{
	ChangefeedCursor,
	ChangefeedReadBinding,
	ChangefeedReadItem,
	ChangefeedReadMode,
	ChangefeedReadStatistics,
	Versionstamp,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::DateTime,
};

/// Inclusive UTC time or exclusive whole-commit position for history selection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChangefeedSince {
	/// Selects commits strictly after this exact position.
	Versionstamp(
		/// Selects commits strictly after this exact position.
		Versionstamp,
	),
	/// Selects commits at or after this UTC instant, ordered by versionstamp.
	Timestamp(
		/// Selects commits at or after this UTC instant, ordered by
		/// versionstamp.
		DateTime,
	),
	/// Resumes after a whole commit with database, source and mode validation.
	Cursor(
		/// Resumes after a whole commit with database, source and mode
		/// validation.
		ChangefeedCursor,
	),
}

/// Validated selection and bounded scratch/work for one snapshot reader.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct ChangefeedReadOptions {
	/// Fixed lower selection boundary.
	pub(crate) since: ChangefeedSince,
	/// Historically captured NET or FULL view.
	pub(crate) mode: ChangefeedReadMode,
	/// Maximum returned batches and gaps; commits are never split.
	pub(crate) limit: u64,
	/// Maximum retained scratch while materializing one complete commit.
	pub(crate) memory_bytes: usize,
	/// Maximum inspected groups and pages, including FULL preflight and
	/// seeking.
	pub(crate) work: u64,
}

impl ChangefeedReadOptions {
	/// Reads NET history with 256 MiB scratch and one million work units.
	pub fn new(since: ChangefeedSince) -> Self {
		Self {
			since,
			mode: ChangefeedReadMode::Net,
			limit: u64::MAX,
			memory_bytes: 256 * 1024 * 1024,
			work: 1_000_000,
		}
	}

	/// Selects NET or FULL; a cursor must have the same immutable read mode.
	pub fn with_mode(mut self, mode: ChangefeedReadMode) -> Self {
		self.mode = mode;
		self
	}

	/// Limits whole batches and gaps. Zero returns no items.
	pub fn with_limit(mut self, limit: u64) -> Self {
		self.limit = limit;
		self
	}

	/// Sets hard scratch and work limits before constructing a reader.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error if either limit is zero.
	pub fn with_budgets(mut self, memory_bytes: usize, work: u64) -> FerrosGrynnResult<Self> {
		if memory_bytes == 0 || work == 0 {
			return Err(FerrosGrynnError::invalid_argument(
				"changefeed reader budgets must be positive",
			));
		}
		self.memory_bytes = memory_bytes;
		self.work = work;
		Ok(self)
	}
}

impl Default for ChangefeedReadOptions {
	/// Constructs the default initial state of `ChangefeedReadOptions`.
	fn default() -> Self {
		Self::new(ChangefeedSince::Versionstamp(Versionstamp::new(0)))
	}
}

/// Private adapter for RAM or durable history, sharing the public pull
/// contract.
pub trait ReadBatches {
	/// Returns a complete owned item or a typed error, without partial commits.
	fn next_batch(
		&mut self,
		memory_limit: Option<usize>,
	) -> FerrosGrynnResult<Option<ChangefeedReadItem>>;
	/// Returns a validated whole-commit continuation once a position is known.
	fn cursor(&self) -> Option<ChangefeedCursor>;
	/// Returns fixed committed metadata, or None after complete source
	/// retirement.
	fn binding(&self) -> Option<ChangefeedReadBinding>;
	/// Returns payload-free counters for this reader only.
	fn statistics(&self) -> ChangefeedReadStatistics;
}

/// Fallible pull reader borrowing its transaction for the selected snapshot.
///
/// Returned images and cursors own their data and outlive this reader. After an
/// error the reader stops; callers may retry with the last exported cursor.
pub struct ChangefeedReader<'transaction> {
	/// Opaque transaction adapter; no physical identity reaches the public API.
	inner: Box<dyn ReadBatches + 'transaction>,
}

impl<'transaction> ChangefeedReader<'transaction> {
	/// Installs the checked transaction adapter after selection preflight.
	pub(crate) fn new(inner: Box<dyn ReadBatches + 'transaction>) -> Self {
		Self {
			inner,
		}
	}

	/// Returns one whole commit or GAP and advances only after successful
	/// delivery.
	///
	/// # Errors
	///
	/// Returns snapshot expiry, resource or corruption errors. Segment loss
	/// with trusted catalog bounds is represented by a GAP instead of
	/// partial images.
	pub fn next_batch(&mut self) -> FerrosGrynnResult<Option<ChangefeedReadItem>> {
		self.inner.next_batch(None)
	}

	/// Admits the next whole batch against remaining query scratch, including
	/// retained output from earlier batches; the standalone SDK cap is
	/// preserved.
	pub(crate) fn next_batch_with_memory(
		&mut self,
		memory_limit: usize,
	) -> FerrosGrynnResult<Option<ChangefeedReadItem>> {
		self.inner.next_batch(Some(memory_limit))
	}

	/// Returns an owned resume cursor. Timestamp reads acquire a position only
	/// after reaching their inclusive time boundary; before then this is None.
	/// Exhausted reads
	/// may advance across commits without events, which do not constitute a
	/// GAP.
	pub fn cursor(&self) -> Option<ChangefeedCursor> {
		self.inner.cursor()
	}

	/// Returns the fixed committed binding; private configuration is excluded.
	pub fn binding(&self) -> Option<ChangefeedReadBinding> {
		self.inner.binding()
	}

	/// Returns cumulative counters including FULL coverage preflight and
	/// failures. These observations are independent of other readers
	/// sharing snapshot pins.
	pub fn statistics(&self) -> ChangefeedReadStatistics {
		self.inner.statistics()
	}
}

impl fmt::Debug for ChangefeedReader<'_> {
	/// Formats `ChangefeedReader<'_>` using the requested formatter.
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("ChangefeedReader").field("cursor", &self.cursor()).finish_non_exhaustive()
	}
}
