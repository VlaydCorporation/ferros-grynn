//! Bounded ownership conversion before creating canonical tensor axes.
#![allow(
	clippy::exhaustive_structs,
	clippy::exhaustive_enums,
	reason = "Internal versioned engine/runtime protocol; not re-exported by the SDK"
)]
#![allow(
	clippy::redundant_pub_crate,
	reason = "Crate-private tensor adapters remain explicitly bounded at the public facade"
)]

use std::collections::{BTreeMap, BTreeSet};

use super::{
	AtomKind,
	AtomRef,
	CsrMatrix,
	EdgeDirection,
	EndpointTensor,
	FerrosGrynnResult,
	GraphRead,
	Number,
	TensorAtom,
	TensorBinding,
	TensorConnector,
	TensorContainment,
	TensorEndpoint,
	TensorOptions,
	TensorScope,
	Text,
	Uuid,
	control::{Control, invalid, overflow},
};
use crate::graph::{AtomId, EdgeEndpoint, EdgeRead, Frozen, Graph, NodeRead};

/// Private plane packing hook shared by runtime and durable sort consumers.
pub type PlaneBuilder<'a> = dyn FnMut(usize, &[TensorConnector], bool, &mut Control<'_>) -> FerrosGrynnResult<CsrMatrix<bool>>
	+ 'a;

/// Owned endpoint identity before canonical ordinals have been assigned.
pub struct InputEndpoint {
	/// Stable qualified atom identity.
	pub atom: TensorAtom,
	/// Ordered qualified containment route.
	pub owners: Vec<TensorAtom>,
}

/// Original connector data awaiting stable ordinal assignment.
pub struct InputConnector {
	/// Stable qualified atom identity.
	pub atom: TensorAtom,
	/// Original topology or selected traversal direction.
	pub direction: EdgeDirection,
	/// Original source incidence, independent of weight.
	pub source: Vec<InputEndpoint>,
	/// Original target incidence, independent of weight.
	pub target: Vec<InputEndpoint>,
	/// Captured optional connector label.
	pub label: Option<Text>,
	/// Captured connector weight, defaulting to one.
	pub weight: Number,
}

/// The runtime and transaction adapters share one admitted canonical builder.
#[allow(
	clippy::partial_pub_fields,
	reason = "Only capture counters are shared; cancellation and provenance stay owner-private"
)]
pub struct TensorBuilder<'a> {
	/// Shared cancellation, memory and work admission.
	pub control: Control<'a>,
	/// Caller-selected scope, projection and resource options.
	options: TensorOptions,
	/// Canonical qualified atom dictionary.
	atoms: BTreeMap<TensorAtom, bool>,
	/// Canonical connectors with original topology.
	connectors: Vec<InputConnector>,
	/// Captured source graph bindings.
	bindings: BTreeMap<Uuid, TensorBinding>,
	/// Primary requested graph, independent of canonical binding order.
	root: Option<Uuid>,
	/// Owner-to-child metadata, without implicit adjacency.
	containment: Vec<TensorContainment>,
	/// Distinct owner/child keys for bounded deduplication.
	containment_keys: BTreeSet<(TensorAtom, Uuid)>,
}

impl<'a> TensorBuilder<'a> {
	/// Initializes this snapshot-bound view or checked operation state.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn new(
		options: TensorOptions,
		check: &'a mut dyn FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Self> {
		Ok(Self {
			control: Control::new(options.limits, check)?,
			options,
			atoms: BTreeMap::new(),
			connectors: Vec::new(),
			bindings: BTreeMap::new(),
			root: None,
			containment: Vec::new(),
			containment_keys: BTreeSet::new(),
		})
	}

	/// Captures one consistent graph origin and snapshot.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn bind<G: GraphRead>(&mut self, graph: &G) -> FerrosGrynnResult<()> {
		self.control.step()?;
		if self.root.is_none() {
			self.root = Some(graph.identity());
		}
		if let Some(previous) = self.bindings.get(&graph.identity()) {
			if previous.snapshot != graph.snapshot_id() {
				return Err(invalid("tensor graph origin has inconsistent snapshots"));
			}
			return Ok(());
		}
		self.control.admit(512)?;
		self.bindings.insert(graph.identity(), TensorBinding {
			origin: graph.identity(),
			snapshot: graph.snapshot_id(),
			graph: graph.graph_ref().cloned(),
		});
		Ok(())
	}

	/// Admits and converts an existing reader atom to its public identity.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn atom<G: GraphRead>(
		&mut self,
		graph: &G,
		id: AtomId,
		domain: bool,
	) -> FerrosGrynnResult<TensorAtom> {
		self.control.step()?;
		let mut convert = |kind,
		                   record: std::borrow::Cow<'_, crate::values::RecordId>|
		 -> FerrosGrynnResult<TensorAtom> {
			self.control.admit(
				512_usize
					.checked_add(record.estimated_heap_bytes().checked_mul(4).ok_or_else(overflow)?)
					.ok_or_else(overflow)?,
			)?;
			Ok(TensorAtom::from_graph(graph, AtomRef::new(kind, record.into_owned())))
		};
		let missing = || invalid("tensor iterator references an absent atom");
		let identity = match id.kind() {
			Some(AtomKind::Node) => {
				let node = graph.node(id)?.ok_or_else(missing)?;
				convert(AtomKind::Node, node.record_id()?)?
			}
			Some(AtomKind::Edge) => {
				let edge = graph.edge(id)?.ok_or_else(missing)?;
				convert(AtomKind::Edge, edge.record_id()?)?
			}
			None => return Err(missing()),
		};
		self.identity(&identity, domain)?;
		Ok(identity)
	}

	/// Merges domain membership while retaining canonical qualified identity.
	fn identity(&mut self, identity: &TensorAtom, domain: bool) -> FerrosGrynnResult<()> {
		if let Some(previous) = self.atoms.get_mut(identity) {
			*previous |= domain;
			return Ok(());
		}
		self.control.admit(
			512_usize
				.checked_add(identity.heap_bytes().checked_mul(4).ok_or_else(overflow)?)
				.ok_or_else(overflow)?,
		)?;
		self.atoms.insert(identity.clone(), domain);
		Ok(())
	}

	/// Admits one complete original connector and its endpoints.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn connector(&mut self, connector: InputConnector) -> FerrosGrynnResult<()> {
		self.control.step()?;
		self.control.admit(512)?;
		for endpoint in connector.source.iter().chain(&connector.target) {
			self.identity(&endpoint.atom, false)?;
		}
		self.connectors.try_reserve_exact(1).map_err(|cause| {
			super::control::resource("tensor connectors", self.control.limits.memory_bytes as u64)
				.with_technical_source(cause)
		})?;
		self.connectors.push(connector);
		Ok(())
	}

	/// Admits storage for original endpoint descriptors.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn endpoint_buffer(&mut self, count: usize) -> FerrosGrynnResult<Vec<InputEndpoint>> {
		self.control.array(count)
	}

	/// Retains a distinct owner-to-child binding without creating adjacency.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn containment<G: GraphRead>(
		&mut self,
		owner: TensorAtom,
		child: &G,
	) -> FerrosGrynnResult<()> {
		self.bind(child)?;
		self.control.admit(
			1024_usize
				.checked_add(owner.heap_bytes().checked_mul(2).ok_or_else(overflow)?)
				.ok_or_else(overflow)?,
		)?;
		if !self.containment_keys.insert((owner.clone(), child.identity())) {
			return Ok(());
		}
		self.containment.try_reserve_exact(1).map_err(|cause| {
			super::control::resource("tensor containment", self.control.limits.memory_bytes as u64)
				.with_technical_source(cause)
		})?;
		self.containment.push(TensorContainment {
			owner,
			child: TensorBinding {
				origin: child.identity(),
				snapshot: child.snapshot_id(),
				graph: child.graph_ref().cloned(),
			},
		});
		Ok(())
	}

	/// Assigns canonical ordinals and writes checked sparse membership planes.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn finish(self) -> FerrosGrynnResult<EndpointTensor> {
		self.finish_with_planes(&mut plane)
	}

	/// Assigns the same axes while the caller selects bounded plane packing.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn finish_with_planes(
		mut self,
		plane: &mut PlaneBuilder<'_>,
	) -> FerrosGrynnResult<EndpointTensor> {
		self.control.poll()?;
		self.control.sort(self.connectors.len())?;
		self.connectors.sort_unstable_by(|left, right| left.atom.cmp(&right.atom));
		if self.connectors.windows(2).any(|pair| pair[0].atom == pair[1].atom) {
			return Err(invalid("tensor contains duplicate connector identities"));
		}
		let mut atoms = self.control.array(self.atoms.len())?;
		let mut domain = self.control.array(self.atoms.len())?;
		let mut nodes = self.control.array(self.atoms.len())?;
		for (atom, included) in self.atoms {
			self.control.step()?;
			if atom.atom().kind() == AtomKind::Node {
				nodes.push(atoms.len());
			}
			atoms.push(atom);
			domain.push(included);
		}
		let mut connectors = self.control.array::<TensorConnector>(self.connectors.len())?;
		for connector in self.connectors {
			let mut convert =
				|input: Vec<InputEndpoint>| -> FerrosGrynnResult<Vec<TensorEndpoint>> {
					let mut result = self.control.array(input.len())?;
					for endpoint in input {
						self.control.step()?;
						let row = atoms
							.binary_search(&endpoint.atom)
							.map_err(|_cause| invalid("tensor endpoint has no axis"))?;
						result.push(TensorEndpoint {
							row,
							owners: endpoint.owners,
						});
					}
					Ok(result)
				};
			let source = convert(connector.source)?;
			let target = convert(connector.target)?;
			connectors.push(TensorConnector {
				atom: connector.atom,
				direction: connector.direction,
				source,
				target,
				label: connector.label,
				weight: connector.weight,
			});
		}
		let source = plane(atoms.len(), &connectors, true, &mut self.control)?;
		let target = plane(atoms.len(), &connectors, false, &mut self.control)?;
		self.control.sort(self.containment.len())?;
		self.containment.sort_unstable_by(|left, right| {
			left.owner.cmp(&right.owner).then(left.child.origin.cmp(&right.child.origin))
		});
		self.control.work(self.bindings.len())?;
		let root = self.root.ok_or_else(|| invalid("tensor has no primary graph binding"))?;
		let root = self
			.bindings
			.keys()
			.position(|origin| *origin == root)
			.ok_or_else(|| invalid("tensor primary graph is absent"))?;
		let mut bindings = self.control.array(self.bindings.len())?;
		bindings.extend(self.bindings.into_values());
		Ok(EndpointTensor {
			atoms,
			domain,
			nodes,
			connectors,
			source,
			target,
			bindings,
			root,
			options: self.options,
			admitted_bytes: self.control.bytes,
			work: self.control.work,
			containment: self.containment,
		})
	}
}

/// Builds one canonical binary incidence plane from endpoint ordinals.
fn plane(
	rows: usize,
	connectors: &[TensorConnector],
	source: bool,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<CsrMatrix<bool>> {
	let count = connectors.iter().try_fold(0_usize, |count, connector| {
		connector.plane_endpoints(source).iter().try_fold(count, |count, endpoints| {
			count.checked_add(endpoints.len()).ok_or_else(overflow)
		})
	})?;
	let mut pairs = control.array(count)?;
	for (column, connector) in connectors.iter().enumerate() {
		for endpoint in connector.plane_endpoints(source).into_iter().flatten() {
			control.step()?;
			pairs.push((endpoint.row, column));
		}
	}
	control.sort(pairs.len())?;
	pairs.sort_unstable();
	pairs.dedup();
	let mut offsets = control.array(rows.checked_add(1).ok_or_else(overflow)?)?;
	let mut columns = control.array(pairs.len())?;
	let mut values = control.array(pairs.len())?;
	let mut pairs = pairs.into_iter().peekable();
	for row in 0..rows {
		control.step()?;
		offsets.push(columns.len());
		while pairs.peek().is_some_and(|pair| pair.0 == row) {
			if let Some((_, column)) = pairs.next() {
				columns.push(column);
				values.push(true);
			}
		}
	}
	offsets.push(columns.len());
	Ok(CsrMatrix::from_canonical((rows, connectors.len()), offsets, columns, values))
}

/// Converts reader metadata to owned canonical tensor data.
pub(super) fn from_graph<G: GraphRead>(
	graph: &G,
	options: TensorOptions,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<EndpointTensor> {
	let mut builder = TensorBuilder::new(options, check)?;
	let mut visited = BTreeSet::new();
	collect(graph, options.scope, &mut builder, &mut visited)?;
	builder.finish()
}

/// Collects one graph domain and its original connector topology.
fn collect<G: GraphRead>(
	graph: &G,
	scope: TensorScope,
	builder: &mut TensorBuilder<'_>,
	visited: &mut BTreeSet<Uuid>,
) -> FerrosGrynnResult<()> {
	if !collect_one(graph, builder, visited)? {
		return Ok(());
	}
	for id in graph.node_ids()?.chain(graph.edge_ids()?) {
		let id = id?;
		builder.control.step()?;
		with_child(
			graph,
			id,
			|child| {
				let owner = builder.atom(graph, id, true)?;
				builder.containment(owner, child)?;
				if scope == TensorScope::Rooted {
					collect_runtime(child, builder, visited)?;
				}
				Ok(())
			},
			false,
		)?;
	}
	Ok(())
}

/// Captures one distinct domain before choosing a containment traversal
/// strategy.
fn collect_one<G: GraphRead>(
	graph: &G,
	builder: &mut TensorBuilder<'_>,
	visited: &mut BTreeSet<Uuid>,
) -> FerrosGrynnResult<bool> {
	builder.bind(graph)?;
	if visited.contains(&graph.identity()) {
		return Ok(false);
	}
	builder.control.admit(512)?;
	visited.insert(graph.identity());
	for id in graph.node_ids()? {
		builder.atom(graph, id?, true)?;
	}
	for id in graph.edge_ids()? {
		builder.atom(graph, id?, true)?;
	}
	for id in graph.edge_ids()? {
		let id = id?;
		let atom = builder.atom(graph, id, true)?;
		let edge = graph.edge(id)?.ok_or_else(|| invalid("tensor connector is absent"))?;
		let topology = edge.topology()?;
		let mut source = builder.endpoint_buffer(topology.source().len())?;
		for endpoint in topology.source() {
			source.push(resolve(graph, endpoint, builder, Vec::new())?);
		}
		let mut target = builder.endpoint_buffer(topology.target().len())?;
		for endpoint in topology.target() {
			target.push(resolve(graph, endpoint, builder, Vec::new())?);
		}
		let label = edge.label_text()?;
		builder.control.admit(label.as_ref().map_or(0, |value| value.len()))?;
		let label = label.map(std::borrow::Cow::into_owned);
		builder.connector(InputConnector {
			atom,
			direction: topology.direction(),
			source,
			target,
			label,
			weight: edge.weight().unwrap_or(Number::Int(1)),
		})?;
	}
	Ok(true)
}

/// Walks descendants while the generic root bridge retains their borrowed
/// owner.
fn collect_runtime(
	graph: &Graph<Frozen>,
	builder: &mut TensorBuilder<'_>,
	visited: &mut BTreeSet<Uuid>,
) -> FerrosGrynnResult<()> {
	let mut pending = builder.control.array(1)?;
	pending.push(graph);
	while let Some(graph) = pending.pop() {
		builder.control.step()?;
		if !collect_one(graph, builder, visited)? {
			continue;
		}
		for id in graph.node_ids()?.chain(graph.edge_ids()?) {
			let id = id?;
			builder.control.step()?;
			let Some(child) = runtime_child(graph, id)? else {
				continue;
			};
			let owner = builder.atom(graph, id, true)?;
			builder.containment(owner, child)?;
			if pending.len() == pending.capacity() {
				let capacity = pending.capacity().checked_mul(2).ok_or_else(overflow)?.max(4);
				builder.control.admit(
					capacity.checked_mul(size_of::<&Graph<Frozen>>()).ok_or_else(overflow)?,
				)?;
				pending.try_reserve_exact(capacity - pending.len()).map_err(|cause| {
					super::control::resource(
						"tensor containment frontier",
						builder.control.limits.memory_bytes as u64,
					)
					.with_technical_source(cause)
				})?;
			}
			builder.control.step()?;
			pending.push(child);
		}
	}
	Ok(())
}

/// Uses inherent runtime views whose child borrow belongs to the source graph.
fn runtime_child(
	graph: &Graph<Frozen>,
	owner: AtomId,
) -> FerrosGrynnResult<Option<&Graph<Frozen>>> {
	let missing = || invalid("tensor containment owner is absent");
	match owner.kind() {
		Some(AtomKind::Node) => {
			graph.node(owner).ok_or_else(missing).map(|node| node.inner_graph())
		}
		Some(AtomKind::Edge) => {
			graph.edge(owner).ok_or_else(missing).map(|edge| edge.transition_graph())
		}
		None => Err(missing()),
	}
}

/// Resolves an original local or qualified endpoint.
fn resolve<G: GraphRead>(
	graph: &G,
	endpoint: &EdgeEndpoint,
	builder: &mut TensorBuilder<'_>,
	owners: Vec<TensorAtom>,
) -> FerrosGrynnResult<InputEndpoint> {
	match endpoint {
		EdgeEndpoint::Local(id) => {
			builder.bind(graph)?;
			Ok(InputEndpoint {
				atom: builder.atom(graph, *id, false)?,
				owners,
			})
		}
		EdgeEndpoint::Nested(qualified) => {
			resolve_path(graph, qualified.scope(), qualified.atom(), builder, owners)
		}
	}
}

/// Resolves a bounded containment route without flattening identity.
fn resolve_path<G: GraphRead>(
	graph: &G,
	scope: &[AtomId],
	id: AtomId,
	builder: &mut TensorBuilder<'_>,
	mut owners: Vec<TensorAtom>,
) -> FerrosGrynnResult<InputEndpoint> {
	let Some((owner, tail)) = scope.split_first() else {
		return resolve(graph, &EdgeEndpoint::Local(id), builder, owners);
	};
	let count = owners.len().checked_add(scope.len()).ok_or_else(overflow)?;
	builder.control.admit(count.checked_mul(size_of::<TensorAtom>()).ok_or_else(overflow)?)?;
	owners.try_reserve_exact(scope.len()).map_err(|cause| {
		super::control::resource("tensor owner chain", builder.control.limits.memory_bytes as u64)
			.with_technical_source(cause)
	})?;
	let identity = builder.atom(graph, *owner, false)?;
	owners.push(identity.clone());
	with_child(
		graph,
		*owner,
		|child| {
			builder.containment(identity, child)?;
			let mut current = child;
			for owner in tail {
				builder.control.step()?;
				let identity = builder.atom(current, *owner, false)?;
				owners.push(identity.clone());
				let child = runtime_child(current, *owner)?
					.ok_or_else(|| invalid("qualified tensor endpoint has no nested owner"))?;
				builder.containment(identity, child)?;
				current = child;
			}
			resolve(current, &EdgeEndpoint::Local(id), builder, owners)
		},
		true,
	)?
	.ok_or_else(|| invalid("qualified tensor endpoint has no nested owner"))
}

/// Keeps the child borrow within its node/edge reader lifetime.
fn with_child<G: GraphRead, T>(
	graph: &G,
	owner: AtomId,
	visit: impl FnOnce(&Graph<Frozen>) -> FerrosGrynnResult<T>,
	required: bool,
) -> FerrosGrynnResult<Option<T>> {
	let missing = || invalid("tensor containment owner is absent");
	let result = match owner.kind() {
		Some(AtomKind::Node) => {
			let node = graph.node(owner)?.ok_or_else(missing)?;
			node.inner_graph()?.as_deref().map(visit).transpose()?
		}
		Some(AtomKind::Edge) => {
			let edge = graph.edge(owner)?.ok_or_else(missing)?;
			edge.transition_graph()?.as_deref().map(visit).transpose()?
		}
		None => return Err(missing()),
	};
	if required && result.is_none() {
		return Err(missing());
	}
	Ok(result)
}
