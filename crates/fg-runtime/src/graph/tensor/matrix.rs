//! Checked logical sparse and dense matrices; no physical page addresses.

use super::{
	GraphAlgorithmLimits,
	control::{Control, invalid, overflow},
};
use crate::gql_status::FerrosGrynnResult;

/// Immutable canonical compressed sparse rows with explicit logical shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsrMatrix<T> {
	/// Checked logical row and column extents.
	shape: (usize, usize),
	/// Exclusive offsets into canonical sparse rows.
	offsets: Vec<usize>,
	/// Sorted distinct column ordinals within each row or view.
	columns: Vec<usize>,
	/// Retained logical cell values.
	values: Vec<T>,
}

impl<T> CsrMatrix<T> {
	/// Validates already owned CSR without allocating from unchecked lengths.
	///
	/// # Errors
	/// Rejects invalid offsets, duplicate/unsorted columns, shape or admission.
	/// Heap-owning cells are excluded so size admission covers every owned
	/// cell.
	///
	/// ```compile_fail
	/// use ferros_grynn::graph::tensor::{CsrMatrix, GraphAlgorithmLimits};
	/// let _ = CsrMatrix::try_new((1, 1), vec![0, 1], vec![0],
	///     vec![String::from("heap value")], GraphAlgorithmLimits::default());
	/// ```
	pub fn try_new(
		shape: (usize, usize),
		offsets: Vec<usize>,
		columns: Vec<usize>,
		values: Vec<T>,
		limits: GraphAlgorithmLimits,
	) -> FerrosGrynnResult<Self>
	where
		T: Copy,
	{
		let mut check = || Ok(());
		let mut control = Control::new(limits, &mut check)?;
		// Retained spare capacity consumes the budget even when the logical CSR
		// lengths are small.
		let offset_bytes =
			offsets.capacity().checked_mul(size_of::<usize>()).ok_or_else(overflow)?;
		let column_bytes =
			columns.capacity().checked_mul(size_of::<usize>()).ok_or_else(overflow)?;
		let value_bytes = values.capacity().checked_mul(size_of::<T>()).ok_or_else(overflow)?;
		control.admit(
			offset_bytes
				.checked_add(column_bytes)
				.and_then(|bytes| bytes.checked_add(value_bytes))
				.ok_or_else(overflow)?,
		)?;
		if offsets.len() != shape.0.checked_add(1).ok_or_else(overflow)?
			|| offsets.first() != Some(&0)
			|| offsets.last() != Some(&columns.len())
			|| columns.len() != values.len()
		{
			return Err(invalid("invalid CSR offsets or values"));
		}
		for row in offsets.windows(2) {
			control.step()?;
			if row[0] > row[1] || row[1] > columns.len() {
				return Err(invalid("invalid CSR offsets"));
			}
			let targets = &columns[row[0]..row[1]];
			let mut previous = None;
			for target in targets {
				control.step()?;
				if *target >= shape.1 || previous.is_some_and(|value| value >= *target) {
					return Err(invalid("CSR columns must be in range and sorted distinct"));
				}
				previous = Some(*target);
			}
		}
		Ok(Self {
			shape,
			offsets,
			columns,
			values,
		})
	}

	/// Returns row and column extents.
	pub const fn shape(&self) -> (usize, usize) {
		self.shape
	}

	/// Returns canonical exclusive row offsets.
	pub fn offsets(&self) -> &[usize] {
		&self.offsets
	}

	/// Returns logical column ordinals.
	pub fn column_indices(&self) -> &[usize] {
		&self.columns
	}

	/// Returns retained cells, including explicit numeric zeros.
	pub fn values(&self) -> &[T] {
		&self.values
	}

	/// Returns the number of retained cells.
	pub fn stored_cells(&self) -> usize {
		self.columns.len()
	}

	/// Looks up a cell; an absent sparse cell is the caller's additive zero.
	///
	/// # Errors
	/// Rejects an out-of-range row or column.
	pub fn get(&self, row: usize, column: usize) -> FerrosGrynnResult<Option<&T>> {
		if row >= self.shape.0 || column >= self.shape.1 {
			return Err(invalid("matrix cell is outside its shape"));
		}
		let start = self.offsets[row];
		Ok(self.columns[start..self.offsets[row + 1]]
			.binary_search(&column)
			.ok()
			.map(|at| &self.values[start + at]))
	}

	/// Iterates one logical row without allocating.
	///
	/// # Errors
	/// Rejects an out-of-range row.
	pub fn row(&self, row: usize) -> FerrosGrynnResult<impl Iterator<Item = (usize, &T)> + '_> {
		if row >= self.shape.0 {
			return Err(invalid("matrix row is outside its shape"));
		}
		let range = self.offsets[row]..self.offsets[row + 1];
		Ok(self.columns[range.clone()].iter().copied().zip(self.values[range].iter()))
	}

	/// Iterates stored cells in canonical row/column order.
	pub fn entries(&self) -> impl Iterator<Item = (usize, usize, &T)> + '_ {
		self.offsets.windows(2).enumerate().flat_map(move |(row, bounds)| {
			(bounds[0]..bounds[1]).map(move |at| (row, self.columns[at], &self.values[at]))
		})
	}

	/// Exports row-major cells after admitting the complete dense extent.
	///
	/// # Errors
	/// Rejects overflow or aggregate memory/work exhaustion before allocation.
	pub fn to_dense(&self, limits: GraphAlgorithmLimits) -> FerrosGrynnResult<DenseMatrix<T>>
	where
		T: Copy + Default,
	{
		let mut check = || Ok(());
		let mut control = Control::new(limits, &mut check)?;
		control.work(self.shape.0)?;
		let count = self.shape.0.checked_mul(self.shape.1).ok_or_else(overflow)?;
		let mut cells = control.array::<T>(count)?;
		for _ in 0..count {
			control.step()?;
			cells.push(T::default());
		}
		for (row, column, value) in self.entries() {
			control.step()?;
			cells[row * self.shape.1 + column] = *value;
		}
		Ok(DenseMatrix {
			shape: self.shape,
			cells,
		})
	}

	/// Constructs CSR from already checked sorted distinct rows.
	pub(crate) fn from_canonical(
		shape: (usize, usize),
		offsets: Vec<usize>,
		columns: Vec<usize>,
		values: Vec<T>,
	) -> Self {
		Self {
			shape,
			offsets,
			columns,
			values,
		}
	}
}

/// Owned row-major logical matrix returned by explicit bounded export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenseMatrix<T> {
	/// Checked logical row and column extents.
	shape: (usize, usize),
	/// Explicit row-major dense values.
	cells: Vec<T>,
}

impl<T> DenseMatrix<T> {
	/// Constructs dense output from an already admitted complete extent.
	pub(crate) fn from_cells(shape: (usize, usize), cells: Vec<T>) -> Self {
		Self {
			shape,
			cells,
		}
	}

	/// Returns row and column extents.
	pub const fn shape(&self) -> (usize, usize) {
		self.shape
	}

	/// Borrows row-major cells.
	pub fn cells(&self) -> &[T] {
		&self.cells
	}

	/// Looks up a checked logical cell.
	///
	/// # Errors
	/// Rejects out-of-range indices.
	pub fn get(&self, row: usize, column: usize) -> FerrosGrynnResult<&T> {
		if row >= self.shape.0 || column >= self.shape.1 {
			return Err(invalid("dense cell is outside its shape"));
		}
		Ok(&self.cells[row * self.shape.1 + column])
	}
}

/// Operations shared with the engine and value codecs.
pub trait CsrMatrixTInternal<T>: Sized {
	/// Constructs CSR from already checked sorted distinct rows.
	fn from_canonical(
		shape: (usize, usize),
		offsets: Vec<usize>,
		columns: Vec<usize>,
		values: Vec<T>,
	) -> Self;
}
impl<T> CsrMatrixTInternal<T> for CsrMatrix<T> {
	fn from_canonical(
		shape: (usize, usize),
		offsets: Vec<usize>,
		columns: Vec<usize>,
		values: Vec<T>,
	) -> Self {
		<CsrMatrix<T>>::from_canonical(shape, offsets, columns, values)
	}
}
