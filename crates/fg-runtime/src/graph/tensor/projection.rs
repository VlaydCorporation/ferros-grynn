//! Exact incidence projection; binary presence never depends on numeric weight.

use std::collections::{BTreeMap, BTreeSet};

use super::{
	AtomKind,
	CsrMatrix,
	DenseMatrix,
	EndpointTensor,
	FerrosGrynnResult,
	GraphAlgorithmLimits,
	Number,
	TensorAtom,
	Text,
	control,
	control::{Control, invalid, overflow},
};
use crate::{
	graph::{
		EndpointRole,
		PathDirection,
		TraversalDirection,
		TraversalSemantics,
		visit_incidence_targets,
	},
	math::add_numbers,
};

/// Selects stored strengths or one contribution per parallel connector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WeightSelection {
	/// Unit contribution independent of stored connector weights.
	#[default]
	Unweighted,
	/// Original stored connector strength or distance cost.
	Stored,
}

/// Selection applied to the induced qualified analytical domain.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProjectionOptions {
	/// NODE by default; ATOM additionally includes edge endpoints.
	pub mode: TraversalSemantics,
	/// Forward, reverse, or both incidence directions.
	pub direction: TraversalDirection,
	/// Empty means all connector labels.
	pub labels: BTreeSet<Text>,
	/// Strength reduction for the weighted view; binary connectivity is
	/// independent.
	pub weights: WeightSelection,
	/// Admission for the projection, independently of the owned tensor.
	pub limits: GraphAlgorithmLimits,
}

impl Default for ProjectionOptions {
	/// Constructs the default initial state of `ProjectionOptions`.
	fn default() -> Self {
		Self {
			mode: TraversalSemantics::Node,
			direction: TraversalDirection::Forward,
			labels: BTreeSet::new(),
			weights: WeightSelection::Stored,
			limits: GraphAlgorithmLimits::default(),
		}
	}
}

/// Exact connector transition retained independently of collapsed adjacency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TensorArc {
	/// Original source incidence, independent of weight.
	pub(crate) source: usize,
	/// Original target incidence, independent of weight.
	pub(crate) target: usize,
	/// Original tensor connector ordinal.
	pub(crate) connector: usize,
	/// Original topology or selected traversal direction.
	pub(crate) direction: PathDirection,
	/// Original incidence endpoint role.
	pub(crate) role: EndpointRole,
}

impl TensorArc {
	/// Returns the source projection ordinal.
	pub const fn source(self) -> usize {
		self.source
	}

	/// Returns the target projection ordinal.
	pub const fn target(self) -> usize {
		self.target
	}

	/// Returns the original tensor connector ordinal.
	pub const fn connector(self) -> usize {
		self.connector
	}

	/// Returns traversal direction, preserving reverse and undirected
	/// incidence.
	pub const fn direction(self) -> PathDirection {
		self.direction
	}

	/// Returns the endpoint role of this transition.
	pub const fn role(self) -> EndpointRole {
		self.role
	}
}

/// Owned exact analytical domain and sparse adjacency matrices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorProjection {
	/// Canonical qualified atom dictionary.
	pub(crate) atoms: Vec<TensorAtom>,
	/// Projection-to-original tensor row mapping.
	pub(crate) tensor_rows: Vec<usize>,
	/// Sorted node-kind row ordinals.
	pub(crate) nodes: Vec<usize>,
	/// Sorted edge-kind row ordinals.
	pub(crate) edges: Vec<usize>,
	/// Connectivity independent of numeric weights.
	pub(crate) binary: CsrMatrix<bool>,
	/// Distinct connector contributions per admissible pair.
	pub(crate) multiplicity: CsrMatrix<u64>,
	/// Summed Number contributions, retaining zero cells.
	pub(crate) weights: CsrMatrix<Number>,
	/// Original connector weights for exact distance kernels; sums are
	/// strengths.
	pub(crate) connector_weights: Vec<Number>,
	/// Exact qualified transitions with connector identity.
	pub(crate) arcs: Vec<TensorArc>,
	/// Caller-selected scope, projection and resource options.
	pub(crate) options: ProjectionOptions,
	/// Original LOCAL or ROOTED tensor domain.
	pub(crate) scope: super::TensorScope,
	/// Conservatively admitted algorithm operations.
	pub(crate) work: u64,
	/// Conservatively admitted retained and scratch bytes.
	pub(crate) bytes: usize,
}

/// Qualified incidence before optional adjacency strength reductions.
#[allow(
	clippy::redundant_pub_crate,
	reason = "Shared internal incidence remains outside the public tensor API"
)]
pub(crate) struct ProjectedRelation {
	/// Canonical qualified atom dictionary.
	pub(crate) atoms: Vec<TensorAtom>,
	/// Projection-to-original tensor row mapping.
	pub(crate) tensor_rows: Vec<usize>,
	/// Sorted node-kind ordinals.
	pub(crate) nodes: Vec<usize>,
	/// Sorted edge-kind ordinals.
	pub(crate) edges: Vec<usize>,
	/// Original connector weights.
	pub(crate) connector_weights: Vec<Number>,
	/// Sorted exact transitions with connector identity.
	pub(crate) arcs: Vec<TensorArc>,
}

impl EndpointTensor {
	/// Projects exact induced-domain adjacency from original incidence
	/// metadata.
	///
	/// # Errors
	/// Rejects malformed endpoints, arithmetic overflow or exhausted budgets.
	pub fn project(&self, options: ProjectionOptions) -> FerrosGrynnResult<TensorProjection> {
		self.project_with_check(options, &mut || Ok(()))
	}

	/// Captures shared qualified incidence without reducing connector weights.
	pub(crate) fn project_relation(
		&self,
		options: &ProjectionOptions,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<ProjectedRelation> {
		self.project_relation_selected(options, None, control)
	}

	/// Applies a qualified allowlist before generating incidence pairs.
	fn project_relation_selected(
		&self,
		options: &ProjectionOptions,
		members: Option<&BTreeSet<TensorAtom>>,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<ProjectedRelation> {
		if let Some(members) = members {
			for member in members {
				control.step()?;
				let row = self
					.atom_ordinal(member)
					.ok_or_else(|| invalid("induced member is outside the tensor axes"))?;
				if !self.domain[row] {
					return Err(invalid("induced member is outside the selected tensor scope"));
				}
			}
		}
		for label in &options.labels {
			control.step()?;
			control.admit(128_usize.checked_add(label.len()).ok_or_else(overflow)?)?;
		}
		control.work(self.atoms.len())?;
		let mut connector_weights = control.array(self.connectors.len())?;
		for connector in &self.connectors {
			control.step()?;
			connector_weights.push(connector.weight);
		}
		let count = self
			.atoms
			.iter()
			.enumerate()
			.filter(|(row, atom)| {
				self.domain[*row]
					&& members.is_none_or(|members| members.contains(*atom))
					&& (options.mode == TraversalSemantics::Atom
						|| atom.atom().kind() == AtomKind::Node)
			})
			.count();
		let mut atoms = control.array(count)?;
		let mut tensor_rows = control.array(count)?;
		let mut nodes = control.array(count)?;
		let mut edges = control.array(count)?;
		for (row, atom) in self.atoms.iter().enumerate() {
			control.step()?;
			if !self.domain[row]
				|| members.is_some_and(|members| !members.contains(atom))
				|| (options.mode == TraversalSemantics::Node
					&& atom.atom().kind() != AtomKind::Node)
			{
				continue;
			}
			control.admit(atom.heap_bytes())?;
			if atom.atom().kind() == AtomKind::Node {
				nodes.push(atoms.len());
			} else {
				edges.push(atoms.len());
			}
			atoms.push(atom.clone());
			tensor_rows.push(row);
		}
		let mut arcs = Vec::new();
		for (connector, edge) in self.connectors.iter().enumerate() {
			control.step()?;
			if !options.labels.is_empty()
				&& !edge.label.as_ref().is_some_and(|label| options.labels.contains(label))
			{
				continue;
			}
			let mut endpoints = control
				.array(edge.source.len().checked_add(edge.target.len()).ok_or_else(overflow)?)?;
			endpoints.extend(edge.source.iter().chain(&edge.target));
			control.sort(endpoints.len())?;
			endpoints.sort_unstable_by_key(|endpoint| (endpoint.row, &endpoint.owners));
			endpoints.dedup();
			for endpoint in endpoints {
				control.step()?;
				let Ok(source) = tensor_rows.binary_search(&endpoint.row) else {
					continue;
				};
				// Admit every comparison performed by the common incidence
				// kernel, including empty-result hyperedges,
				// before entering that kernel.
				control.work(
					edge.source
						.len()
						.checked_add(edge.target.len())
						.ok_or_else(overflow)?
						.checked_mul(4)
						.ok_or_else(overflow)?,
				)?;
				visit_incidence_targets(
					edge.direction,
					&edge.source,
					&edge.target,
					endpoint,
					options.direction,
					|target, direction, role| {
						control.step()?;
						let Ok(target) = tensor_rows.binary_search(&target.row) else {
							return Ok(());
						};
						control.admit(size_of::<TensorArc>() + 128)?;
						arcs.try_reserve_exact(1).map_err(|cause| {
							control::resource(
								"tensor projection arcs",
								options.limits.memory_bytes as u64,
							)
							.with_technical_source(cause)
						})?;
						arcs.push(TensorArc {
							source,
							target,
							connector,
							direction,
							role,
						});
						Ok(())
					},
				)?;
			}
		}
		control.sort(arcs.len())?;
		arcs.sort_unstable();
		arcs.dedup();
		Ok(ProjectedRelation {
			atoms,
			tensor_rows,
			nodes,
			edges,
			connector_weights,
			arcs,
		})
	}

	/// Projects adjacency with cancellation/deadline checks.
	///
	/// # Errors
	/// Propagates check errors and rejects overflow or exhausted budgets.
	pub fn project_with_check<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
		&self,
		options: ProjectionOptions,
		check: &mut CheckArg,
	) -> FerrosGrynnResult<TensorProjection> {
		self.project_selected(options, None, check)
	}

	/// Projects the induced topology of distinct qualified members.
	/// An empty set produces an empty domain; connectors retain their identity.
	///
	/// # Errors
	/// Rejects foreign or boundary-only members and exhausted resource budgets.
	#[allow(
		clippy::mutable_key_type,
		reason = "Runtime key equality and ordering use stable logical identities"
	)]
	pub fn project_induced_with_check<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
		&self,
		options: ProjectionOptions,
		members: &BTreeSet<TensorAtom>,
		check: &mut CheckArg,
	) -> FerrosGrynnResult<TensorProjection> {
		self.project_selected(options, Some(members), check)
	}

	/// Shares exact pair generation and sparse reductions across both domains.
	fn project_selected(
		&self,
		options: ProjectionOptions,
		members: Option<&BTreeSet<TensorAtom>>,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<TensorProjection> {
		let mut control = Control::new(options.limits, check)?;
		let ProjectedRelation {
			atoms,
			tensor_rows,
			nodes,
			edges,
			connector_weights,
			arcs,
		} = self.project_relation_selected(&options, members, &mut control)?;
		let mut cells = BTreeMap::<(usize, usize), (u64, Number)>::new();
		let mut previous = None;
		for arc in &arcs {
			control.step()?;
			let contribution = (arc.source, arc.target, arc.connector);
			if previous == Some(contribution) {
				continue;
			}
			previous = Some(contribution);
			let pair = (arc.source, arc.target);
			let weight = match options.weights {
				WeightSelection::Stored => connector_weights[arc.connector],
				WeightSelection::Unweighted => Number::Int(1),
			};
			if let Some((count, sum)) = cells.get_mut(&pair) {
				*count = count.checked_add(1).ok_or_else(overflow)?;
				*sum = add_numbers(*sum, weight)?;
			} else {
				control.admit(192)?;
				cells.insert(pair, (1, weight));
			}
		}
		let mut offsets = control.array(atoms.len().checked_add(1).ok_or_else(overflow)?)?;
		let mut columns = control.array(cells.len())?;
		let mut weights = control.array(cells.len())?;
		let mut multiplicity = control.array(cells.len())?;
		let mut binary = control.array(cells.len())?;
		let mut cells = cells.into_iter().peekable();
		for row in 0..atoms.len() {
			control.step()?;
			offsets.push(columns.len());
			while cells.peek().is_some_and(|(pair, _)| pair.0 == row) {
				control.step()?;
				if let Some(((_, column), (count, weight))) = cells.next() {
					columns.push(column);
					weights.push(weight);
					multiplicity.push(count);
					binary.push(true);
				}
			}
		}
		offsets.push(columns.len());
		control.admit(
			offsets
				.len()
				.checked_add(columns.len())
				.and_then(|count| count.checked_mul(2 * size_of::<usize>()))
				.ok_or_else(overflow)?,
		)?;
		let shape = (atoms.len(), atoms.len());
		Ok(TensorProjection {
			atoms,
			tensor_rows,
			nodes,
			edges,
			binary: CsrMatrix::from_canonical(shape, offsets.clone(), columns.clone(), binary),
			multiplicity: CsrMatrix::from_canonical(
				shape,
				offsets.clone(),
				columns.clone(),
				multiplicity,
			),
			weights: CsrMatrix::from_canonical(shape, offsets, columns, weights),
			connector_weights,
			arcs,
			options,
			scope: self.options.scope,
			work: control.work,
			bytes: control.bytes,
		})
	}
}

impl TensorProjection {
	/// Borrows canonical qualified analytical vertices.
	pub fn atoms(&self) -> &[TensorAtom] {
		&self.atoms
	}

	/// Finds a qualified vertex ordinal.
	pub fn atom_ordinal(&self, atom: &TensorAtom) -> Option<usize> {
		self.atoms.binary_search(atom).ok()
	}

	/// Returns the original tensor row for a checked projection ordinal.
	///
	/// # Errors
	/// Rejects out-of-range ordinals.
	pub fn tensor_row(&self, row: usize) -> FerrosGrynnResult<usize> {
		self.tensor_rows
			.get(row)
			.copied()
			.ok_or_else(|| invalid("projection ordinal is outside its shape"))
	}

	/// Borrows exact transitions, preserving connector identity.
	pub fn arcs(&self) -> &[TensorArc] {
		&self.arcs
	}

	/// Borrows binary connectivity, including zero-weight connections.
	pub const fn binary_adjacency(&self) -> BinaryAdjacencyMatrix<'_> {
		BinaryAdjacencyMatrix {
			projection: self,
		}
	}

	/// Borrows summed Number contributions, including explicit zero cells.
	pub const fn weighted_adjacency(&self) -> WeightedAdjacencyMatrix<'_> {
		WeightedAdjacencyMatrix {
			projection: self,
		}
	}

	/// Borrows distinct connector contributions per admissible pair.
	pub const fn multiplicity(&self) -> &CsrMatrix<u64> {
		&self.multiplicity
	}

	/// Returns selection parameters.
	pub const fn options(&self) -> &ProjectionOptions {
		&self.options
	}

	/// Returns admitted projection bytes and incidence work.
	pub const fn resources(&self) -> (usize, u64) {
		(self.bytes, self.work)
	}

	/// Borrows kind-restricted rows and columns without copying incidence.
	fn block<'a, T>(
		&'a self,
		matrix: &'a CsrMatrix<T>,
		source: AtomKind,
		target: AtomKind,
	) -> AdjacencyBlock<'a, T> {
		AdjacencyBlock {
			matrix,
			atoms: &self.atoms,
			rows: if source == AtomKind::Node {
				&self.nodes
			} else {
				&self.edges
			},
			columns: if target == AtomKind::Node {
				&self.nodes
			} else {
				&self.edges
			},
		}
	}
}

/// Borrowed binary adjacency with the common qualified axis dictionary.
#[derive(Debug, Clone, Copy)]
pub struct BinaryAdjacencyMatrix<'a> {
	/// Immutable parent projection borrowed by this view.
	projection: &'a TensorProjection,
}

impl<'a> BinaryAdjacencyMatrix<'a> {
	/// Returns the immutable logical CSR.
	pub const fn csr(self) -> &'a CsrMatrix<bool> {
		&self.projection.binary
	}

	/// Borrows both canonical axes.
	pub fn atoms(self) -> &'a [TensorAtom] {
		self.projection.atoms()
	}

	/// Borrows VV, VE, EV or EE without copying the projection.
	pub fn block(self, source: AtomKind, target: AtomKind) -> AdjacencyBlock<'a, bool> {
		self.projection.block(self.csr(), source, target)
	}
}

/// Borrowed weighted adjacency; zero is independent of binary presence.
#[derive(Debug, Clone, Copy)]
pub struct WeightedAdjacencyMatrix<'a> {
	/// Immutable parent projection borrowed by this view.
	projection: &'a TensorProjection,
}

impl<'a> WeightedAdjacencyMatrix<'a> {
	/// Returns the immutable logical Number CSR, including retained zero cells.
	pub const fn csr(self) -> &'a CsrMatrix<Number> {
		&self.projection.weights
	}

	/// Borrows both canonical axes.
	pub fn atoms(self) -> &'a [TensorAtom] {
		self.projection.atoms()
	}

	/// Borrows VV, VE, EV or EE without copying the projection.
	pub fn block(self, source: AtomKind, target: AtomKind) -> AdjacencyBlock<'a, Number> {
		self.projection.block(self.csr(), source, target)
	}

	/// Iterates numerically nonzero cells without discarding binary links.
	pub fn nonzeros(self) -> impl Iterator<Item = (usize, usize, &'a Number)> {
		self.csr().entries().filter(|(_, _, value)| **value != Number::Int(0))
	}
}

/// Borrowed kind-restricted matrix with view-local row/column ordinals.
#[derive(Debug, Clone, Copy)]
pub struct AdjacencyBlock<'a, T> {
	/// Immutable original sparse matrix.
	matrix: &'a CsrMatrix<T>,
	/// Canonical qualified atom dictionary.
	atoms: &'a [TensorAtom],
	/// Sorted original row ordinals selected by the view.
	rows: &'a [usize],
	/// Sorted distinct column ordinals within each row or view.
	columns: &'a [usize],
}

impl<'a, T> AdjacencyBlock<'a, T> {
	/// Returns logical view extents.
	pub const fn shape(&self) -> (usize, usize) {
		(self.rows.len(), self.columns.len())
	}

	/// Iterates canonical source axis identities.
	pub fn row_atoms(&self) -> impl Iterator<Item = &'a TensorAtom> + '_ {
		self.rows.iter().map(|row| &self.atoms[*row])
	}

	/// Iterates canonical target axis identities.
	pub fn column_atoms(&self) -> impl Iterator<Item = &'a TensorAtom> + '_ {
		self.columns.iter().map(|row| &self.atoms[*row])
	}

	/// Finds a qualified identity in the source view axis.
	pub fn row_ordinal(&self, atom: &TensorAtom) -> Option<usize> {
		self.rows.binary_search(&self.atoms.binary_search(atom).ok()?).ok()
	}

	/// Finds a qualified identity in the target view axis.
	pub fn column_ordinal(&self, atom: &TensorAtom) -> Option<usize> {
		self.columns.binary_search(&self.atoms.binary_search(atom).ok()?).ok()
	}

	/// Looks up a checked view-local cell.
	///
	/// # Errors
	/// Rejects out-of-range ordinals.
	pub fn get(&self, row: usize, column: usize) -> FerrosGrynnResult<Option<&'a T>> {
		let row = self
			.rows
			.get(row)
			.ok_or_else(|| invalid("adjacency block row is outside its shape"))?;
		let column = self
			.columns
			.get(column)
			.ok_or_else(|| invalid("adjacency block column is outside its shape"))?;
		self.matrix.get(*row, *column)
	}

	/// Iterates retained cells in canonical view order.
	pub fn entries(&self) -> impl Iterator<Item = (usize, usize, &'a T)> + '_ {
		self.rows.iter().enumerate().flat_map(move |(row, original)| {
			(self.matrix.offsets()[*original]..self.matrix.offsets()[*original + 1]).filter_map(
				move |at| {
					self.columns
						.binary_search(&self.matrix.column_indices()[at])
						.ok()
						.map(|column| (row, column, &self.matrix.values()[at]))
				},
			)
		})
	}

	/// Exports an owned canonical CSR after complete admission.
	///
	/// # Errors
	/// Rejects overflow or exceeded work/memory ceilings.
	pub fn to_csr(&self, limits: GraphAlgorithmLimits) -> FerrosGrynnResult<CsrMatrix<T>>
	where
		T: Copy,
	{
		let mut check = || Ok(());
		let mut control = Control::new(limits, &mut check)?;
		for original in self.rows {
			control.step()?;
			let cells = self.matrix.offsets()[*original + 1] - self.matrix.offsets()[*original];
			let comparisons =
				usize::try_from(self.columns.len().checked_add(1).ok_or_else(overflow)?.ilog2())
					.map_err(|_cause| overflow())?
					.checked_add(1)
					.ok_or_else(overflow)?;
			control.work(
				cells
					.checked_mul(2)
					.and_then(|cells| cells.checked_mul(comparisons))
					.ok_or_else(overflow)?,
			)?;
		}
		let mut count = 0_usize;
		for _ in self.entries() {
			control.step()?;
			count = count.checked_add(1).ok_or_else(overflow)?;
		}
		let mut offsets = control.array(self.rows.len().checked_add(1).ok_or_else(overflow)?)?;
		let mut columns = control.array(count)?;
		let mut values = control.array(count)?;
		let mut entries = self.entries().peekable();
		for row in 0..self.rows.len() {
			control.step()?;
			offsets.push(columns.len());
			while entries.peek().is_some_and(|entry| entry.0 == row) {
				control.step()?;
				if let Some((_, column, value)) = entries.next() {
					columns.push(column);
					values.push(*value);
				}
			}
		}
		offsets.push(columns.len());
		Ok(CsrMatrix::from_canonical(self.shape(), offsets, columns, values))
	}

	/// Exports row-major block cells after admitting the entire dense extent.
	///
	/// # Errors
	/// Rejects overflowing shape or exhausted memory/work ceilings.
	pub fn to_dense(&self, limits: GraphAlgorithmLimits) -> FerrosGrynnResult<DenseMatrix<T>>
	where
		T: Copy + Default,
	{
		let mut check = || Ok(());
		let mut control = Control::new(limits, &mut check)?;
		let shape = self.shape();
		let count = shape.0.checked_mul(shape.1).ok_or_else(overflow)?;
		let mut cells = control.array(count)?;
		for _ in 0..count {
			control.step()?;
			cells.push(T::default());
		}
		for (row, original) in self.rows.iter().enumerate() {
			control.step()?;
			for at in self.matrix.offsets()[*original]..self.matrix.offsets()[*original + 1] {
				control.work(
					1 + usize::try_from(
						self.columns.len().checked_add(1).ok_or_else(overflow)?.ilog2(),
					)
					.map_err(|_cause| overflow())?,
				)?;
				if let Ok(column) = self.columns.binary_search(&self.matrix.column_indices()[at]) {
					cells[row * shape.1 + column] = self.matrix.values()[at];
				}
			}
		}
		Ok(DenseMatrix::from_cells(shape, cells))
	}
}
