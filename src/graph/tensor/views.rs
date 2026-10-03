//! Borrowed node-only and full endpoint membership views.

use super::{
	CsrMatrix,
	DenseMatrix,
	EndpointTensor,
	FerrosGrynnResult,
	GraphAlgorithmLimits,
	TensorAtom,
	TensorConnector,
	control::{Control, invalid, overflow},
};

/// Selects one logical plane or their union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncidenceSide {
	Source,
	Target,
	Both,
}

/// Borrows a NODE-only tensor without copying the underlying membership.
#[derive(Debug, Clone, Copy)]
pub struct IncidenceTensor<'a> {
	/// Immutable parent tensor borrowed by this view.
	tensor: &'a EndpointTensor,
}

impl<'a> IncidenceTensor<'a> {
	/// Initializes this snapshot-bound view or checked operation state.
	pub(super) const fn new(tensor: &'a EndpointTensor) -> Self {
		Self {
			tensor,
		}
	}

	/// Returns nodes × connectors × two logical sides.
	pub fn shape(self) -> (usize, usize, usize) {
		(self.tensor.nodes.len(), self.tensor.connectors.len(), 2)
	}

	/// Iterates canonical NODE axis identities.
	pub fn atoms(self) -> impl Iterator<Item = &'a TensorAtom> {
		self.tensor.nodes.iter().map(|row| &self.tensor.atoms[*row])
	}

	/// Finds an identity in the NODE axis dictionary.
	pub fn atom_ordinal(self, atom: &TensorAtom) -> Option<usize> {
		self.matrix(IncidenceSide::Both).atom_ordinal(atom)
	}

	/// Borrows the shared connector dictionary.
	pub fn connectors(self) -> &'a [TensorConnector] {
		self.tensor.connectors()
	}

	/// Looks up membership using NODE view ordinals.
	///
	/// # Errors
	/// Rejects out-of-range view ordinals.
	pub fn get(self, row: usize, connector: usize, side: IncidenceSide) -> FerrosGrynnResult<bool> {
		self.matrix(side).get(row, connector)
	}

	/// Borrows the selected NODE membership matrix.
	pub fn matrix(self, side: IncidenceSide) -> IncidenceMatrix<'a> {
		IncidenceMatrix::new(self.tensor, Some(&self.tensor.nodes), side)
	}
}

/// Immutable atom/node × connector membership matrix view.
#[derive(Debug, Clone, Copy)]
pub struct IncidenceMatrix<'a> {
	/// Immutable parent tensor borrowed by this view.
	tensor: &'a EndpointTensor,
	/// Sorted original row ordinals selected by the view.
	rows: Option<&'a [usize]>,
	/// Source, target or their logical union.
	side: IncidenceSide,
}

impl<'a> IncidenceMatrix<'a> {
	/// Initializes this snapshot-bound view or checked operation state.
	pub(super) const fn new(
		tensor: &'a EndpointTensor,
		rows: Option<&'a [usize]>,
		side: IncidenceSide,
	) -> Self {
		Self {
			tensor,
			rows,
			side,
		}
	}

	/// Returns row and connector extents.
	pub fn shape(self) -> (usize, usize) {
		(self.rows.map_or(self.tensor.atoms.len(), <[usize]>::len), self.tensor.connectors.len())
	}

	/// Iterates the view's canonical atom axis.
	pub fn atoms(self) -> impl Iterator<Item = &'a TensorAtom> {
		(0..self.shape().0)
			.map(move |row| &self.tensor.atoms[self.rows.map_or(row, |rows| rows[row])])
	}

	/// Borrows the common connector dictionary.
	pub fn connectors(self) -> &'a [TensorConnector] {
		&self.tensor.connectors
	}

	/// Finds a connector in the shared canonical dictionary.
	pub fn connector_ordinal(self, atom: &TensorAtom) -> Option<usize> {
		self.tensor.connector_ordinal(atom)
	}

	/// Finds an identity's ordinal in this view.
	pub fn atom_ordinal(self, atom: &TensorAtom) -> Option<usize> {
		let row = self.tensor.atom_ordinal(atom)?;
		self.rows.map_or(Some(row), |rows| rows.binary_search(&row).ok())
	}

	/// Looks up a checked logical cell.
	///
	/// # Errors
	/// Rejects invalid view or connector ordinals.
	pub fn get(self, row: usize, connector: usize) -> FerrosGrynnResult<bool> {
		if row >= self.shape().0 {
			return Err(invalid("incidence row is outside its shape"));
		}
		self.tensor.get(self.rows.map_or(row, |rows| rows[row]), connector, self.side)
	}

	/// Iterates true membership cells in canonical order, without dense scans.
	pub fn nonzeros(self) -> impl Iterator<Item = (usize, usize)> + 'a {
		(0..self.shape().0).flat_map(move |row| {
			let original = self.rows.map_or(row, |rows| rows[row]);
			let source = &self.tensor.source;
			let target = &self.tensor.target;
			let source_range = source.offsets()[original]..source.offsets()[original + 1];
			let target_range = target.offsets()[original]..target.offsets()[original + 1];
			let (first, second) = match self.side {
				IncidenceSide::Source => (&source.column_indices()[source_range], &[][..]),
				IncidenceSide::Target => (&target.column_indices()[target_range], &[][..]),
				IncidenceSide::Both => {
					(&source.column_indices()[source_range], &target.column_indices()[target_range])
				}
			};
			let mut left = first.iter().copied().peekable();
			let mut right = second.iter().copied().peekable();
			std::iter::from_fn(move || {
				let next = match (left.peek(), right.peek()) {
					(Some(a), Some(b)) if a == b => {
						right.next();
						left.next()
					}
					(Some(a), Some(b)) if a > b => right.next(),
					(Some(_), _) => left.next(),
					(None, _) => right.next(),
				};
				next.map(|column| (row, column))
			})
		})
	}

	/// Exports a checked canonical logical CSR.
	///
	/// # Errors
	/// Returns cancellation-independent memory/work/overflow errors.
	pub fn to_csr(self, limits: GraphAlgorithmLimits) -> FerrosGrynnResult<CsrMatrix<bool>> {
		let mut check = || Ok(());
		let mut control = Control::new(limits, &mut check)?;
		control.work(self.shape().0)?;
		let count = self.nonzeros().try_fold(0_usize, |count, _| {
			control.step()?;
			count.checked_add(1).ok_or_else(overflow)
		})?;
		let mut offsets = control.array(self.shape().0.checked_add(1).ok_or_else(overflow)?)?;
		let mut columns = control.array(count)?;
		let mut values = control.array(count)?;
		let mut entries = self.nonzeros().peekable();
		for row in 0..self.shape().0 {
			control.step()?;
			offsets.push(columns.len());
			while entries.peek().is_some_and(|entry| entry.0 == row) {
				control.step()?;
				if let Some((_, column)) = entries.next() {
					columns.push(column);
					values.push(true);
				}
			}
		}
		offsets.push(columns.len());
		Ok(CsrMatrix::from_canonical(self.shape(), offsets, columns, values))
	}

	/// Exports dense membership only after admitting its complete extent.
	///
	/// # Errors
	/// Rejects overflowing shape and exhausted memory/work limits.
	pub fn to_dense(self, limits: GraphAlgorithmLimits) -> FerrosGrynnResult<DenseMatrix<bool>> {
		let mut check = || Ok(());
		let mut control = Control::new(limits, &mut check)?;
		let shape = self.shape();
		control.work(shape.0)?;
		let count = shape.0.checked_mul(shape.1).ok_or_else(overflow)?;
		let mut cells = control.array(count)?;
		for _ in 0..count {
			control.step()?;
			cells.push(false);
		}
		for (row, column) in self.nonzeros() {
			control.step()?;
			cells[row * shape.1 + column] = true;
		}
		Ok(DenseMatrix::from_cells(shape, cells))
	}
}
