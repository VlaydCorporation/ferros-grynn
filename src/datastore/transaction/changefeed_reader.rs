//! Transaction selection and lifetime adapter for the shared history reader.

use std::sync::Arc;

use super::Transaction;
use crate::{
	changefeed::{
		ChangefeedCursor,
		ChangefeedReadItem,
		ChangefeedReadOptions,
		ChangefeedReadStatistics,
		ChangefeedReader,
		ChangefeedSelector,
		ChangefeedSince,
		ReadBatches,
	},
	datastore::{ConflictKey, source_history::SourceReader},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

/// Borrowing the transaction preserves its snapshot age and mutation contract.
struct SnapshotReader<'transaction> {
	/// The public borrow prevents commit or mutation until this reader is
	/// dropped.
	transaction: &'transaction Transaction,
	/// Common bounded logic also serves root-bound LIVE preparation.
	history: SourceReader<'transaction>,
}

impl Transaction {
	/// Opens a bounded fixed-snapshot history reader without executing GQL.
	///
	/// FULL coverage is checked across the selected history before any item is
	/// returned. Returned images and cursors outlive the reader and
	/// transaction. Private mutations are excluded; this borrow prevents
	/// writes while pulling.
	///
	/// # Errors
	///
	/// Returns snapshot expiry, missing/ambiguous source, wrong cursor origin,
	/// source or mode, unavailable FULL coverage, or resource errors.
	///
	/// # Examples
	///
	/// ```
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// use ferros_grynn::{
	/// 	changefeed::{
	/// 		ChangefeedConfig,
	/// 		ChangefeedReadItem,
	/// 		ChangefeedReadOptions,
	/// 		ChangefeedSince,
	/// 		ChangefeedSource,
	/// 	},
	/// 	datastore::{ClassSchema, Datastore},
	/// 	graph::AtomKind,
	/// 	values::PropertyMap,
	/// };
	/// let store = Datastore::in_memory();
	/// let mut tx = store.begin();
	/// let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	/// let retention = "P1D".parse::<ferros_grynn::values::Duration>().map_err(|error| {
	/// 	ferros_grynn::gql_status::FerrosGrynnError::invalid_argument(error.to_string())
	/// })?;
	/// let feed =
	/// 	tx.configure_changefeed(ChangefeedSource::Class(class), ChangefeedConfig::new(retention)?)?;
	/// tx.create_atom("item:a".parse()?, PropertyMap::new())?;
	/// tx.commit()?;
	/// let mut tx = store.begin();
	/// let mut reader = tx.changes(feed, ChangefeedReadOptions::default().with_limit(1))?;
	/// let binding = reader.binding().ok_or_else(|| {
	/// 	ferros_grynn::gql_status::FerrosGrynnError::invalid_argument("missing binding")
	/// })?;
	/// assert_eq!(binding.id, feed);
	/// let Some(ChangefeedReadItem::Commit(batch)) = reader.next_batch()? else {
	/// 	return Err(ferros_grynn::gql_status::FerrosGrynnError::invalid_argument(
	/// 		"missing initial batch",
	/// 	));
	/// };
	/// let cursor = reader.cursor().ok_or_else(|| {
	/// 	ferros_grynn::gql_status::FerrosGrynnError::invalid_argument("missing cursor")
	/// })?;
	/// let observed = reader.statistics();
	/// assert_eq!(observed.batches, 1);
	/// assert_eq!(observed.entries, 1);
	/// drop(reader);
	/// drop(tx);
	/// assert_eq!(batch.entries().len(), 1);
	/// assert_eq!(store.changefeed_statistics()?.published_groups, 1);
	/// let mut tx = store.begin();
	/// let mut resumed =
	/// 	tx.changes(feed, ChangefeedReadOptions::new(ChangefeedSince::Cursor(cursor)))?;
	/// assert!(resumed.next_batch()?.is_none());
	/// # Ok(())
	/// # }
	/// ```
	///
	/// A reader cannot outlive the transaction whose snapshot it borrows:
	///
	/// ```compile_fail,E0597
	/// use ferros_grynn::{
	/// 	changefeed::{ChangefeedId, ChangefeedReadOptions},
	/// 	datastore::Datastore,
	/// 	gql_status::FerrosGrynnResult,
	/// };
	/// fn invalid_reader_lifetime(store: &Datastore, feed: ChangefeedId) -> FerrosGrynnResult<()> {
	/// 	let mut reader;
	/// 	{
	/// 		let mut tx = store.begin();
	/// 		reader = tx.changes(feed, ChangefeedReadOptions::default())?;
	/// 	}
	/// 	let _batch = reader.next_batch()?;
	/// 	Ok(())
	/// }
	/// ```
	#[allow(
		clippy::tabs_in_doc_comments,
		reason = "nightly rustfmt uses the required hard tabs in Rustdoc examples"
	)]
	pub fn changes<Selector: Into<ChangefeedSelector>>(
		&mut self,
		selector: Selector,
		options: ChangefeedReadOptions,
	) -> FerrosGrynnResult<ChangefeedReader<'_>> {
		self.prepare_read()?;
		self.record_read(ConflictKey::ChangefeedCatalogRange);
		let selector = selector.into();
		let root = Arc::clone(&self.snapshot);
		if let ChangefeedSince::Cursor(cursor) = &options.since
			&& (cursor.database_id().as_inner() != &root.database_uuid
				|| cursor.mode() != options.mode)
		{
			return Err(FerrosGrynnError::invalid_argument(
				"changefeed cursor database or read mode mismatch",
			));
		}
		let selected = match (&selector, &options.since) {
			(ChangefeedSelector::Id(id), ChangefeedSince::Cursor(cursor))
				if cursor.source_id() == *id
					&& !self.working.changefeeds().definitions.contains_key(id) =>
			{
				None
			}
			_ => Some(Arc::clone(self.resolve_changefeed(&selector)?)),
		};
		let source = match (&selected, &selector) {
			(Some(definition), _) => definition.id,
			(None, ChangefeedSelector::Id(id)) => *id,
			_ => {
				return Err(FerrosGrynnError::invalid_argument(
					"changefeed source could not be resolved",
				));
			}
		};
		let physical =
			if root.changefeed_segments().get(&source).is_some_and(|segments| !segments.is_empty())
			{
				self.changefeed_snapshot
					.as_ref()
					.filter(|(stamp, _)| *stamp == root.commit_ts)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"changefeed snapshot bindings were not pinned",
						)
					})?
					.1
					.clone()?
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"changefeed snapshot bindings are unavailable",
						)
					})?
					.observed_reader()
					.into()
			} else {
				None
			};
		let history = SourceReader::new(
			&self.store.inner,
			root,
			source,
			options,
			physical,
			selected.is_none(),
		)?;
		Ok(ChangefeedReader::new(Box::new(SnapshotReader {
			transaction: self,
			history,
		})))
	}
}

impl ReadBatches for SnapshotReader<'_> {
	fn next_batch(
		&mut self,
		memory_limit: Option<usize>,
	) -> FerrosGrynnResult<Option<ChangefeedReadItem>> {
		if !self.history.finished()
			&& let Err(error) = self.transaction.ensure_live()
		{
			self.history.finish();
			return Err(error);
		}
		self.history.next_batch(memory_limit)
	}

	fn cursor(&self) -> Option<ChangefeedCursor> {
		self.history.cursor()
	}

	fn binding(&self) -> Option<crate::changefeed::ChangefeedReadBinding> {
		self.history.binding()
	}

	fn statistics(&self) -> ChangefeedReadStatistics {
		self.history.statistics()
	}
}

#[cfg(test)]
mod tests;
