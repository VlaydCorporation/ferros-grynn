use std::{
	collections::BTreeMap,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::{Duration, Instant},
};

use crate::{
	datastore::Transaction,
	gql::{
		ir::LogicalSelect,
		optimizer::PhysicalSelectPlan,
		options::BarrierAccount,
		spill::QuerySpool,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{QueryCursor, Value},
};

/// Configured max cursor spools value.
const MAX_CURSOR_SPOOLS: usize = 64;
/// Shared next session id value.
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// Exact graph selectors and root stay with the existing bounded cursor spool.
pub(in crate::gql) struct GraphCursorContext {
	/// Pins the exact immutable root required by this graph cursor.
	pub(in crate::gql) root: Arc<crate::datastore::DatabaseRoot>,
	/// Retains the bound logical graph selectors for exact cursor resume.
	pub(in crate::gql) plan: LogicalSelect,
	/// Owns the evaluated frame used by the original graph cursor.
	pub(in crate::gql) slots: Vec<Value>,
	/// Retains original source text for checked cursor diagnostics.
	pub(in crate::gql) source: String,
	/// Stores recipe and ordering fingerprints of the original cursor.
	pub(in crate::gql) template: (u64, u64),
	/// Retains admission for the complete cursor frame until its spool closes.
	pub(in crate::gql) _account: BarrierAccount,
}

impl std::fmt::Debug for GraphCursorContext {
	/// Formats `GraphCursorContext` using the requested formatter.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("GraphCursorContext")
			.field("snapshot", &self.root.commit_ts)
			.field("template", &self.template)
			.finish_non_exhaustive()
	}
}

#[derive(Debug, Clone)]
/// Cursor anchor state.
pub struct CursorAnchor {
	/// Order keys associated with this cursor anchor.
	pub(crate) order_keys: Vec<Value>,
	/// Stable key associated with this cursor anchor.
	pub(crate) stable_key: Vec<Value>,
}

#[derive(Debug, Clone)]
/// Cursor record state.
pub struct CursorRecord {
	/// Value associated with this cursor record.
	pub(crate) value: Value,
	/// Anchor associated with this cursor record.
	pub(crate) anchor: CursorAnchor,
}

#[derive(Debug)]
/// Cursor page state.
pub struct CursorPage {
	/// Values associated with this cursor page.
	pub(crate) values: Vec<Value>,
	/// Continuation associated with this cursor page.
	pub(crate) continuation: Option<QueryCursor>,
}

/// Immutable execution state retained for an index-ordered continuation.
pub(in crate::gql) struct IndexSeekPlan {
	/// Retains the bound logical graph selectors for exact cursor resume.
	pub(in crate::gql) plan: LogicalSelect,
	/// Physical associated with this index seek plan.
	pub(in crate::gql) physical: PhysicalSelectPlan,
	/// Owns the evaluated frame used by the original graph cursor.
	pub(in crate::gql) slots: Vec<Value>,
	/// Retains original source text for checked cursor diagnostics.
	pub(in crate::gql) source: String,
}

/// Session-owned exact transaction snapshot and resumable index seek plan.
/// Its registry lifetime is separate from SELECT materialization barriers.
struct IndexSeekState {
	/// Transaction associated with this index seek state.
	transaction: Option<Transaction>,
	/// Plan associated with this index seek state.
	plan: IndexSeekPlan,
}

impl std::fmt::Debug for IndexSeekState {
	/// Formats `IndexSeekState` using the requested formatter.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("IndexSeekState")
			.field("transaction_attached", &self.transaction.is_some())
			.finish_non_exhaustive()
	}
}

/// Temporarily checked-out exact snapshot while one keyset page is produced.
pub(in crate::gql) struct IndexSeekCheckout {
	/// Transaction associated with this index seek checkout.
	pub(in crate::gql) transaction: Transaction,
	/// Retains the bound logical graph selectors for exact cursor resume.
	pub(in crate::gql) plan: LogicalSelect,
	/// Physical associated with this index seek checkout.
	pub(in crate::gql) physical: PhysicalSelectPlan,
	/// Owns the evaluated frame used by the original graph cursor.
	pub(in crate::gql) slots: Vec<Value>,
	/// Retains original source text for checked cursor diagnostics.
	pub(in crate::gql) source: String,
	/// Anchor associated with this index seek checkout.
	pub(in crate::gql) anchor: CursorAnchor,
	/// Spool associated with this index seek checkout.
	spool: u64,
	/// Cursor associated with this index seek checkout.
	cursor: QueryCursor,
	/// Prior next associated with this index seek checkout.
	prior_next: Option<QueryCursor>,
}

#[derive(Debug)]
/// Cursor spool state.
struct CursorSpool {
	/// Pins the graph cursor frame for the lifetime of its existing spool.
	graph: Option<Arc<GraphCursorContext>>,
	/// Rows associated with this cursor spool.
	rows: CursorRows,
	/// Created associated with this cursor spool.
	created: Instant,
	/// Snapshot associated with this cursor spool.
	_snapshot: u64,
	/// Fingerprint associated with this cursor spool.
	fingerprint: u64,
	/// Order fingerprint associated with this cursor spool.
	order_fingerprint: u64,
}

#[derive(Debug)]
/// Cursor rows alternatives.
enum CursorRows {
	/// Memory case of cursor rows.
	Memory {
		/// Rows associated with this cursor rows.
		rows: Vec<CursorRecord>,
		/// Memory admission account retained for the lifetime of this value.
		_account: BarrierAccount,
	},
	/// Disk case of cursor rows.
	Disk(
		/// Disk case of cursor rows.
		QuerySpool,
	),
	/// Index seek case of cursor rows.
	IndexSeek(
		/// Index seek case of cursor rows.
		Box<IndexSeekState>,
	),
}

#[derive(Debug, Clone, Copy)]
/// Cursor offset alternatives.
enum CursorOffset {
	/// Memory case of cursor offset.
	Memory(
		/// Memory case of cursor offset.
		usize,
	),
	/// Disk case of cursor offset.
	Disk(
		/// Disk case of cursor offset.
		u64,
	),
	/// Index seek case of cursor offset.
	IndexSeek,
}

#[derive(Debug)]
/// Cursor entry state.
struct CursorEntry {
	/// Spool associated with this cursor entry.
	spool: u64,
	/// Position relative to the beginning of the associated buffer.
	offset: CursorOffset,
	/// Anchor associated with this cursor entry.
	anchor: CursorAnchor,
	/// Next associated with this cursor entry.
	next: Option<QueryCursor>,
}

#[derive(Debug)]
/// Cursor runtime state.
pub struct CursorRuntime {
	/// Session associated with this cursor runtime.
	session: u64,
	/// Next id associated with this cursor runtime.
	next_id: u64,
	/// Max snapshot age associated with this cursor runtime.
	max_snapshot_age: Duration,
	/// Spools associated with this cursor runtime.
	spools: BTreeMap<u64, CursorSpool>,
	/// Entries associated with this cursor runtime.
	entries: BTreeMap<u64, CursorEntry>,
}

impl CursorRuntime {
	/// Creates a new `CursorRuntime` value.
	pub(crate) fn new(max_snapshot_age: Duration) -> Self {
		Self {
			session: NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
			next_id: 1,
			max_snapshot_age,
			spools: BTreeMap::new(),
			entries: BTreeMap::new(),
		}
	}

	/// Registers cursor runtime.
	pub(crate) fn register(
		&mut self,
		rows: Vec<CursorRecord>,
		account: BarrierAccount,
		offset: usize,
		fingerprint: u64,
		order_fingerprint: u64,
		snapshot: u64,
		anchor: CursorAnchor,
	) -> FerrosGrynnResult<QueryCursor> {
		if self.spools.len() >= MAX_CURSOR_SPOOLS {
			return Err(FerrosGrynnError::query_cursor_limit_exceeded(MAX_CURSOR_SPOOLS as u64));
		}
		let spool_id = self.allocate_id()?;
		let cursor = QueryCursor::new(self.session, self.allocate_id()?);
		self.spools.insert(spool_id, CursorSpool {
			graph: None,
			rows: CursorRows::Memory {
				rows,
				_account: account,
			},
			created: Instant::now(),
			_snapshot: snapshot,
			fingerprint,
			order_fingerprint,
		});
		self.entries.insert(cursor.id(), CursorEntry {
			spool: spool_id,
			offset: CursorOffset::Memory(offset),
			anchor,
			next: None,
		});
		Ok(cursor)
	}

	/// Registers disk.
	pub(crate) fn register_disk(
		&mut self,
		rows: QuerySpool,
		offset: u64,
		fingerprint: u64,
		order_fingerprint: u64,
		snapshot: u64,
		anchor: CursorAnchor,
	) -> FerrosGrynnResult<QueryCursor> {
		if self.spools.len() >= MAX_CURSOR_SPOOLS {
			return Err(FerrosGrynnError::query_cursor_limit_exceeded(MAX_CURSOR_SPOOLS as u64));
		}
		let spool_id = self.allocate_id()?;
		let cursor = QueryCursor::new(self.session, self.allocate_id()?);
		self.spools.insert(spool_id, CursorSpool {
			graph: None,
			rows: CursorRows::Disk(rows),
			created: Instant::now(),
			_snapshot: snapshot,
			fingerprint,
			order_fingerprint,
		});
		self.entries.insert(cursor.id(), CursorEntry {
			spool: spool_id,
			offset: CursorOffset::Disk(offset),
			anchor,
			next: None,
		});
		Ok(cursor)
	}

	/// Registers index seek.
	pub(in crate::gql) fn register_index_seek(
		&mut self,
		plan: IndexSeekPlan,
		fingerprint: u64,
		order_fingerprint: u64,
		snapshot: u64,
		anchor: CursorAnchor,
	) -> FerrosGrynnResult<QueryCursor> {
		if self.spools.len() >= MAX_CURSOR_SPOOLS {
			return Err(FerrosGrynnError::query_cursor_limit_exceeded(MAX_CURSOR_SPOOLS as u64));
		}
		let spool_id = self.allocate_id()?;
		let cursor = QueryCursor::new(self.session, self.allocate_id()?);
		// Boxing relocates the existing session registry payload. Its count,
		// snapshot age and lifecycle remain owned by this runtime.
		let state = Box::new(IndexSeekState {
			transaction: None,
			plan,
		});
		self.spools.insert(spool_id, CursorSpool {
			graph: None,
			rows: CursorRows::IndexSeek(state),
			created: Instant::now(),
			_snapshot: snapshot,
			fingerprint,
			order_fingerprint,
		});
		self.entries.insert(cursor.id(), CursorEntry {
			spool: spool_id,
			offset: CursorOffset::IndexSeek,
			anchor,
			next: None,
		});
		Ok(cursor)
	}

	/// Attaches index seek.
	pub(in crate::gql) fn attach_index_seek(
		&mut self,
		cursor: QueryCursor,
		transaction: Transaction,
	) -> FerrosGrynnResult<()> {
		self.validate_token(cursor)?;
		let spool = self
			.entries
			.get(&cursor.id())
			.map(|entry| entry.spool)
			.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("unknown pending token"))?;
		let Some(CursorSpool {
			rows: CursorRows::IndexSeek(state),
			..
		}) = self.spools.get_mut(&spool)
		else {
			return Err(FerrosGrynnError::invalid_query_cursor(
				"pending token is not an index continuation",
			));
		};
		if state.transaction.is_some() {
			return Err(FerrosGrynnError::invalid_query_cursor(
				"index continuation already owns a snapshot",
			));
		}
		state.transaction = Some(transaction);
		Ok(())
	}

	/// Shares the immutable graph frame across memory/disk/index pages.
	pub(in crate::gql) fn attach_graph_context(
		&mut self,
		cursor: QueryCursor,
		context: Arc<GraphCursorContext>,
	) -> FerrosGrynnResult<()> {
		self.validate_token(cursor)?;
		let spool =
			self.entries.get(&cursor.id()).map(|entry| entry.spool).ok_or_else(|| {
				FerrosGrynnError::invalid_query_cursor("unknown graph continuation")
			})?;
		let state = self.spools.get_mut(&spool).ok_or_else(|| {
			FerrosGrynnError::invalid_query_cursor("graph result spool is unavailable")
		})?;
		if state.graph.is_none() {
			state.graph = Some(context);
		}
		Ok(())
	}

	/// Checks lifetime before lending original graph authority to an exact
	/// page.
	pub(in crate::gql) fn graph_context(
		&mut self,
		cursor: QueryCursor,
	) -> FerrosGrynnResult<Option<Arc<GraphCursorContext>>> {
		self.validate_token(cursor)?;
		let spool = self
			.entries
			.get(&cursor.id())
			.map(|entry| entry.spool)
			.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("unknown or closed token"))?;
		if self
			.spools
			.get(&spool)
			.is_some_and(|state| state.created.elapsed() > self.max_snapshot_age)
		{
			self.remove_spool(spool);
			return Err(FerrosGrynnError::query_cursor_expired(self.max_snapshot_age.as_secs()));
		}
		Ok(self.spools.get(&spool).and_then(|state| state.graph.as_ref()).map(Arc::clone))
	}

	/// Graph templates are checked separately; page identity belongs to the
	/// retained spool.
	pub(in crate::gql) fn graph_page_fingerprints(
		&mut self,
		cursor: QueryCursor,
	) -> FerrosGrynnResult<(u64, u64)> {
		if self.graph_context(cursor)?.is_none() {
			return Err(FerrosGrynnError::invalid_query_cursor(
				"graph continuation has no retained frame",
			));
		}
		let state = self
			.entries
			.get(&cursor.id())
			.and_then(|entry| self.spools.get(&entry.spool))
			.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("graph spool is unavailable"))?;
		Ok((state.fingerprint, state.order_fingerprint))
	}

	/// Returns the checkout index seek result.
	pub(in crate::gql) fn checkout_index_seek(
		&mut self,
		cursor: QueryCursor,
		fingerprint: u64,
		order_fingerprint: u64,
	) -> FerrosGrynnResult<Option<IndexSeekCheckout>> {
		self.validate_token(cursor)?;
		let (spool, anchor, prior_next) = self
			.entries
			.get(&cursor.id())
			.map(|entry| (entry.spool, entry.anchor.clone(), entry.next))
			.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("unknown or closed token"))?;
		let expired = self
			.spools
			.get(&spool)
			.is_some_and(|state| state.created.elapsed() > self.max_snapshot_age);
		if expired {
			self.remove_spool(spool);
			return Err(FerrosGrynnError::query_cursor_expired(self.max_snapshot_age.as_secs()));
		}
		let state = self
			.spools
			.get_mut(&spool)
			.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("result spool is unavailable"))?;
		if !matches!(state.rows, CursorRows::IndexSeek(_)) {
			return Ok(None);
		}
		if state.fingerprint != fingerprint || state.order_fingerprint != order_fingerprint {
			return Err(FerrosGrynnError::invalid_query_cursor(
				"query or ORDER fingerprint does not match",
			));
		}
		let CursorRows::IndexSeek(seek) = &mut state.rows else {
			unreachable!();
		};
		let transaction = seek.transaction.take().ok_or_else(|| {
			FerrosGrynnError::invalid_query_cursor("index continuation is already in use")
		})?;
		Ok(Some(IndexSeekCheckout {
			transaction,
			plan: seek.plan.plan.clone(),
			physical: seek.plan.physical.clone(),
			slots: seek.plan.slots.clone(),
			source: seek.plan.source.clone(),
			anchor,
			spool,
			cursor,
			prior_next,
		}))
	}

	/// Finishes index seek.
	pub(in crate::gql) fn finish_index_seek(
		&mut self,
		checkout: IndexSeekCheckout,
		next_anchor: Option<CursorAnchor>,
	) -> FerrosGrynnResult<Option<QueryCursor>> {
		let IndexSeekCheckout {
			transaction,
			spool,
			cursor,
			prior_next,
			..
		} = checkout;
		let Some(CursorSpool {
			rows: CursorRows::IndexSeek(state),
			..
		}) = self.spools.get_mut(&spool)
		else {
			return Err(FerrosGrynnError::invalid_query_cursor(
				"index continuation disappeared while producing a page",
			));
		};
		state.transaction = Some(transaction);
		let Some(anchor) = next_anchor else {
			self.remove_spool(spool);
			return Ok(None);
		};
		if let Some(next) = prior_next {
			return Ok(Some(next));
		}
		let next = QueryCursor::new(self.session, self.allocate_id()?);
		self.entries.insert(next.id(), CursorEntry {
			spool,
			offset: CursorOffset::IndexSeek,
			anchor,
			next: None,
		});
		self.entries
			.get_mut(&cursor.id())
			.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("cursor disappeared"))?
			.next = Some(next);
		Ok(Some(next))
	}

	/// Performs the abort index seek operation.
	pub(in crate::gql) fn abort_index_seek(&mut self, checkout: &IndexSeekCheckout) {
		self.remove_spool(checkout.spool);
	}

	/// Returns the page result.
	pub(crate) fn page(
		&mut self,
		cursor: QueryCursor,
		fingerprint: u64,
		order_fingerprint: u64,
		page_size: usize,
	) -> FerrosGrynnResult<CursorPage> {
		self.validate_token(cursor)?;
		let (spool_id, offset, prior_next) = self
			.entries
			.get(&cursor.id())
			.map(|entry| (entry.spool, entry.offset, entry.next))
			.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("unknown or closed token"))?;
		let expired = self
			.spools
			.get(&spool_id)
			.is_some_and(|spool| spool.created.elapsed() > self.max_snapshot_age);
		if expired {
			self.remove_spool(spool_id);
			return Err(FerrosGrynnError::query_cursor_expired(self.max_snapshot_age.as_secs()));
		}
		let page = (|| {
			let spool = self.spools.get(&spool_id).ok_or_else(|| {
				FerrosGrynnError::invalid_query_cursor("result spool is unavailable")
			})?;
			Self::validate_spool(spool, fingerprint, order_fingerprint)?;
			match (&spool.rows, offset) {
				(
					CursorRows::Memory {
						rows,
						..
					},
					CursorOffset::Memory(offset),
				) => {
					let end = offset.saturating_add(page_size).min(rows.len());
					Ok((rows[offset..end].to_vec(), CursorOffset::Memory(end), end == rows.len()))
				}
				(CursorRows::Disk(rows), CursorOffset::Disk(offset)) => {
					let page = rows.page(offset, page_size)?;
					Ok((page.records, CursorOffset::Disk(page.next_offset), page.eof))
				}
				(CursorRows::IndexSeek(_), CursorOffset::IndexSeek) => {
					Err(FerrosGrynnError::invalid_query_cursor(
						"index continuation must use keyset execution",
					))
				}
				_ => Err(FerrosGrynnError::invalid_query_cursor(
					"cursor offset does not match its result spool",
				)),
			}
		})();
		let (records, next_offset, eof) = match page {
			Ok(page) => page,
			Err(error) => {
				self.remove_spool(spool_id);
				return Err(error);
			}
		};
		let values = records.iter().map(|row| row.value.clone()).collect();
		if eof {
			self.remove_spool(spool_id);
			return Ok(CursorPage {
				values,
				continuation: None,
			});
		}
		let continuation = if let Some(next) = prior_next {
			next
		} else {
			let anchor = records
				.last()
				.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("cursor page is empty"))?
				.anchor
				.clone();
			let next = QueryCursor::new(self.session, self.allocate_id()?);
			self.entries.insert(next.id(), CursorEntry {
				spool: spool_id,
				offset: next_offset,
				anchor,
				next: None,
			});
			self.entries
				.get_mut(&cursor.id())
				.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("cursor disappeared"))?
				.next = Some(next);
			next
		};
		Ok(CursorPage {
			values,
			continuation: Some(continuation),
		})
	}

	/// Returns the stable key needed to re-execute an explicitly
	/// `READ COMMITTED` continuation. Unlike exact paging, this does not read
	/// the captured snapshot and therefore remains valid after its age limit.
	pub(crate) fn read_committed_anchor(
		&self,
		cursor: QueryCursor,
		fingerprint: u64,
		order_fingerprint: u64,
	) -> FerrosGrynnResult<CursorAnchor> {
		self.validate_token(cursor)?;
		let spool_id = self
			.entries
			.get(&cursor.id())
			.map(|entry| entry.spool)
			.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("unknown or closed token"))?;
		let entry = self
			.entries
			.get(&cursor.id())
			.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("unknown or closed token"))?;
		let spool = self
			.spools
			.get(&spool_id)
			.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("result spool is unavailable"))?;
		Self::validate_spool(spool, fingerprint, order_fingerprint)?;
		Ok(entry.anchor.clone())
	}

	/// Closes cursor runtime.
	pub(crate) fn close(&mut self, cursor: QueryCursor) -> FerrosGrynnResult<()> {
		self.validate_token(cursor)?;
		let spool = self
			.entries
			.get(&cursor.id())
			.map(|entry| entry.spool)
			.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("unknown or closed token"))?;
		self.remove_spool(spool);
		Ok(())
	}

	/// Returns the allocate id result.
	fn allocate_id(&mut self) -> FerrosGrynnResult<u64> {
		let id = self.next_id;
		self.next_id = self.next_id.checked_add(1).ok_or_else(|| {
			FerrosGrynnError::invalid_query_cursor("session token space exhausted")
		})?;
		Ok(id)
	}

	/// Validates token.
	fn validate_token(&self, cursor: QueryCursor) -> FerrosGrynnResult<()> {
		if cursor.session() != self.session {
			return Err(FerrosGrynnError::invalid_query_cursor(
				"token belongs to another session or database",
			));
		}
		Ok(())
	}

	/// Validates spool.
	fn validate_spool(
		spool: &CursorSpool,
		fingerprint: u64,
		order_fingerprint: u64,
	) -> FerrosGrynnResult<()> {
		if spool.fingerprint != fingerprint {
			return Err(FerrosGrynnError::invalid_query_cursor(
				"normalized query fingerprint does not match",
			));
		}
		if spool.order_fingerprint != order_fingerprint {
			return Err(FerrosGrynnError::invalid_query_cursor("ORDER fingerprint does not match"));
		}
		Ok(())
	}

	/// Removes spool.
	fn remove_spool(&mut self, spool: u64) {
		self.entries.retain(|_id, entry| entry.spool != spool);
		self.spools.remove(&spool);
	}
}
