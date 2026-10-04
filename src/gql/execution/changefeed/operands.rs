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
pub(super) struct Operands<'a> {
	/// Query-wide cancellation and shared memory ceiling.
	control: &'a ExecutionControl,
	/// Transient copies retained conservatively until the command completes.
	account: RefCell<BarrierAccount>,
	/// Checked aggregate work for scalar evaluation and deep value admission.
	work: Cell<u64>,
}

impl<'a> Operands<'a> {
	/// Opens one scalar account before evaluating any selector or resume value.
	pub(super) fn new(control: &'a ExecutionControl) -> Self {
		Self {
			control,
			account: RefCell::new(control.barrier_account()),
			work: Cell::new(0),
		}
	}
}

/// Retains the ordinary fieldless-expression error rather than reading
/// primaries.
fn unavailable() -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument("changefeed operands require scalar values")
}

impl AtomFieldRead for Operands<'_> {
	fn bounded(&self) -> bool {
		true
	}

	fn admit_value(&self, value: &Value) -> FerrosGrynnResult<()> {
		let mut work = self.work.get();
		let bytes = images::value_bytes(value, &mut work, 0)?;
		self.work.set(work);
		self.admit(bytes)
	}

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

	fn work(&self, operations: u64) -> FerrosGrynnResult<()> {
		let work = self
			.work
			.get()
			.checked_add(operations)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed operand work"))?;
		if work > 1_000_000 {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed operand work",
				work,
				1_000_000,
			));
		}
		self.work.set(work);
		Ok(())
	}

	fn field(&self, _atom: BoundAtom, _field: &str) -> FerrosGrynnResult<Value> {
		Err(unavailable())
	}

	fn occurrence_field(
		&self,
		_occurrence: &AtomOccurrenceRef,
		_field: &str,
	) -> FerrosGrynnResult<Value> {
		Err(unavailable())
	}

	fn inner_graph(&self, _occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value> {
		Err(unavailable())
	}

	fn level_path(&self, _occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value> {
		Err(unavailable())
	}

	fn record_field(&self, _record: &RecordId, _field: &str) -> FerrosGrynnResult<Value> {
		Err(unavailable())
	}
}
