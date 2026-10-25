//! Bounded scalar operands for history reads without primary-record lookups.

use std::cell::{Cell, RefCell};

use super::images;
use crate::{
	gql::{
		ExecutionControl,
		execution::cursor::{AtomFieldRead, BoundAtom},
		options::BarrierAccount,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomOccurrenceRef,
	values::{RecordId, Value},
};

/// Shares the evaluator's existing admission protocol across all operands.
pub(in crate::gql::execution) struct Operands<'a> {
	/// Query-wide cancellation and shared memory ceiling.
	control: &'a ExecutionControl,
	/// Transient copies retained conservatively until the command completes.
	account: RefCell<BarrierAccount>,
	/// Checked aggregate work for scalar evaluation and deep value admission.
	work: Cell<u64>,
	/// Snapshot-only binding lookup; primary-record navigation stays disabled.
	bindings: Option<&'a dyn Fn(crate::live::LiveBindingId) -> FerrosGrynnResult<&'a Value>>,
}

impl<'a> Operands<'a> {
	/// Opens one scalar account before evaluating any selector or resume value.
	pub(in crate::gql::execution) fn new(control: &'a ExecutionControl) -> Self {
		Self {
			control,
			account: RefCell::new(control.barrier_account()),
			work: Cell::new(0),
			bindings: None,
		}
	}

	/// Adds a borrowed, SSI-aware binding resolver for execution operands.
	#[must_use]
	pub(super) fn with_bindings(
		mut self,
		read: &'a dyn Fn(crate::live::LiveBindingId) -> FerrosGrynnResult<&'a Value>,
	) -> Self {
		self.bindings = Some(read);
		self
	}

	/// Keeps admitted operand copies alive after the transaction borrow ends.
	pub(in crate::gql::execution) fn into_account(self) -> BarrierAccount {
		self.account.into_inner()
	}

	/// Admits transient encoding scratch; drop releases the shared ceiling.
	///
	/// # Errors
	///
	/// Returns cancellation or shared-memory errors before allocation.
	pub(in crate::gql::execution) fn temporary(
		&self,
		bytes: usize,
	) -> FerrosGrynnResult<BarrierAccount> {
		self.control.check()?;
		let mut account = self.control.barrier_account();
		let maximum = self.control.barrier_memory_limit();
		let actual =
			maximum.saturating_sub(self.control.barrier_memory_available()).saturating_add(bytes);
		if !account.reserve(bytes) {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed operand scratch",
				actual as u64,
				maximum as u64,
			));
		}
		Ok(account)
	}
}

/// Retains the ordinary fieldless-expression error rather than reading
/// primaries.
fn unavailable() -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument("changefeed operands require scalar values")
}

impl AtomFieldRead for Operands<'_> {
	/// Returns live binding for `Operands<'_>`.
	fn live_binding(&self, id: crate::live::LiveBindingId) -> FerrosGrynnResult<&Value> {
		self.control.check()?;
		self.bindings.ok_or_else(unavailable)?(id)
	}

	/// Returns live binding value for `Operands<'_>`.
	fn live_binding_value(
		&self,
		id: crate::live::LiveBindingId,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Value> {
		control.check()?;
		self.copy_value(self.live_binding(id)?)
	}

	/// Reports whether the bounded condition holds for `Operands<'_>`.
	fn bounded(&self) -> bool {
		true
	}

	/// Admits value for `Operands<'_>`.
	fn admit_value(&self, value: &Value) -> FerrosGrynnResult<()> {
		self.admit(images::operand_bytes(value, &|work| self.work(work))?)
	}

	/// Returns admit for `Operands<'_>`.
	fn admit(&self, bytes: usize) -> FerrosGrynnResult<()> {
		self.control.check()?;
		let maximum = self.control.barrier_memory_limit();
		let actual =
			maximum.saturating_sub(self.control.barrier_memory_available()).saturating_add(bytes);
		if !self.account.borrow_mut().reserve(bytes) {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed operand scratch",
				actual as u64,
				maximum as u64,
			));
		}
		Ok(())
	}

	/// Returns work for `Operands<'_>`.
	fn work(&self, operations: u64) -> FerrosGrynnResult<()> {
		self.control.check()?;
		let work = self
			.work
			.get()
			.checked_add(operations)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed operand work"))?;
		let maximum = self.control.max_loop_iterations.min(1_000_000);
		if work > maximum {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed operand work",
				work,
				maximum,
			));
		}
		self.work.set(work);
		Ok(())
	}

	/// Returns field for `Operands<'_>`.
	fn field(&self, _atom: BoundAtom, _field: &str) -> FerrosGrynnResult<Value> {
		Err(unavailable())
	}

	/// Returns occurrence field for `Operands<'_>`.
	fn occurrence_field(
		&self,
		_occurrence: &AtomOccurrenceRef,
		_field: &str,
	) -> FerrosGrynnResult<Value> {
		Err(unavailable())
	}

	/// Returns inner graph for `Operands<'_>`.
	fn inner_graph(&self, _occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value> {
		Err(unavailable())
	}

	/// Returns level path for `Operands<'_>`.
	fn level_path(&self, _occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value> {
		Err(unavailable())
	}

	/// Records field for `Operands<'_>`.
	fn record_field(
		&self,
		_record: &RecordId,
		_field: &str,
		_control: Option<&ExecutionControl>,
	) -> FerrosGrynnResult<Value> {
		Err(unavailable())
	}
}
