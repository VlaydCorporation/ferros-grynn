use std::{
	collections::BTreeMap,
	sync::atomic::{AtomicU64, Ordering},
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

const MAX_CURSOR_SPOOLS: usize = 64;
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub(crate) struct CursorAnchor {
	pub(crate) order_keys: Vec<Value>,
	pub(crate) stable_key: Vec<Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct CursorRecord {
	pub(crate) value: Value,
	pub(crate) anchor: CursorAnchor,
}

#[derive(Debug)]
pub(crate) struct CursorPage {
	pub(crate) values: Vec<Value>,
	pub(crate) continuation: Option<QueryCursor>,
}

/// Immutable execution state retained for an index-ordered continuation.
pub(in crate::gql) struct IndexSeekPlan {
	pub(in crate::gql) plan: LogicalSelect,
	pub(in crate::gql) physical: PhysicalSelectPlan,
	pub(in crate::gql) slots: Vec<Value>,
	pub(in crate::gql) source: String,
}

struct IndexSeekState {
	transaction: Option<Transaction>,
	plan: IndexSeekPlan,
}

impl std::fmt::Debug for IndexSeekState {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter
			.debug_struct("IndexSeekState")
			.field("transaction_attached", &self.transaction.is_some())
			.finish_non_exhaustive()
	}
}

/// Temporarily checked-out exact snapshot while one keyset page is produced.
pub(in crate::gql) struct IndexSeekCheckout {
	pub(in crate::gql) transaction: Transaction,
	pub(in crate::gql) plan: LogicalSelect,
	pub(in crate::gql) physical: PhysicalSelectPlan,
	pub(in crate::gql) slots: Vec<Value>,
	pub(in crate::gql) source: String,
	pub(in crate::gql) anchor: CursorAnchor,
	spool: u64,
	cursor: QueryCursor,
	prior_next: Option<QueryCursor>,
}

#[derive(Debug)]
struct CursorSpool {
	rows: CursorRows,
	created: Instant,
	_snapshot: u64,
	fingerprint: u64,
	order_fingerprint: u64,
}

#[derive(Debug)]
enum CursorRows {
	Memory {
		rows: Vec<CursorRecord>,
		_account: BarrierAccount,
	},
	Disk(QuerySpool),
	IndexSeek(IndexSeekState),
}

#[derive(Debug, Clone, Copy)]
enum CursorOffset {
	Memory(usize),
	Disk(u64),
	IndexSeek,
}

#[derive(Debug)]
struct CursorEntry {
	spool: u64,
	offset: CursorOffset,
	anchor: CursorAnchor,
	next: Option<QueryCursor>,
}

#[derive(Debug)]
pub(crate) struct CursorRuntime {
	session: u64,
	next_id: u64,
	max_snapshot_age: Duration,
	spools: BTreeMap<u64, CursorSpool>,
	entries: BTreeMap<u64, CursorEntry>,
}

impl CursorRuntime {
	pub(crate) fn new(max_snapshot_age: Duration) -> Self {
		Self {
			session: NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
			next_id: 1,
			max_snapshot_age,
			spools: BTreeMap::new(),
			entries: BTreeMap::new(),
		}
	}

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
		self.spools.insert(spool_id, CursorSpool {
			rows: CursorRows::IndexSeek(IndexSeekState {
				transaction: None,
				plan,
			}),
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

	pub(in crate::gql) fn abort_index_seek(&mut self, checkout: &IndexSeekCheckout) {
		self.remove_spool(checkout.spool);
	}

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
			self.validate_spool(spool, fingerprint, order_fingerprint)?;
			match (&spool.rows, offset) {
				(
					CursorRows::Memory {
						rows,
						_account: _,
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
		self.validate_spool(spool, fingerprint, order_fingerprint)?;
		Ok(entry.anchor.clone())
	}

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

	fn allocate_id(&mut self) -> FerrosGrynnResult<u64> {
		let id = self.next_id;
		self.next_id = self.next_id.checked_add(1).ok_or_else(|| {
			FerrosGrynnError::invalid_query_cursor("session token space exhausted")
		})?;
		Ok(id)
	}

	fn validate_token(&self, cursor: QueryCursor) -> FerrosGrynnResult<()> {
		if cursor.session() != self.session {
			return Err(FerrosGrynnError::invalid_query_cursor(
				"token belongs to another session or database",
			));
		}
		Ok(())
	}

	fn validate_spool(
		&self,
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

	fn remove_spool(&mut self, spool: u64) {
		self.entries.retain(|_id, entry| entry.spool != spool);
		self.spools.remove(&spool);
	}
}
