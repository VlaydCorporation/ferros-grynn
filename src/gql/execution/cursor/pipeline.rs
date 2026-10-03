//! Row sources, field resolvers, and relational cursor adapters.

use super::*;

pub(in crate::gql::execution) struct ScanCursor<'a> {
	pub(in crate::gql::execution) inner: Box<dyn RowCursor + 'a>,
}

impl RowCursor for ScanCursor<'_> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		control.check()?;
		self.inner.next(control)
	}
}

pub(in crate::gql::execution) struct OneRowCursor {
	pub(in crate::gql::execution) row: Option<Row>,
}

impl RowCursor for OneRowCursor {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		control.check()?;
		Ok(self.row.take())
	}
}

/// Resolves public fields for an atom bound in a MATCH row.
pub(in crate::gql) trait AtomFieldRead {
	/// Records a snapshot dependency used by a context-dependent PURE helper.
	fn observe(&self, _key: crate::datastore::ConflictKey) {}

	/// Whether this resolver enforces an embedding's hard evaluation budgets.
	fn bounded(&self) -> bool {
		false
	}

	/// Admits the complete value representation before cloning or scanning it.
	fn admit_value(&self, _value: &Value) -> FerrosGrynnResult<()> {
		Ok(())
	}
	/// Admits owned evaluation allocations before copying or growing values.
	fn admit(&self, _bytes: usize) -> FerrosGrynnResult<()> {
		Ok(())
	}

	/// Charges embedding-specific evaluator work before executing it.
	fn work(&self, _operations: u64) -> FerrosGrynnResult<()> {
		Ok(())
	}

	/// Copies a value only after admitting its complete owned payload.
	fn copy_value(&self, value: &Value) -> FerrosGrynnResult<Value> {
		self.admit_value(value)?;
		Ok(value.clone())
	}

	/// Reads `id`, `label`, or a dictionary-backed property.
	fn field(&self, atom: BoundAtom, field: &str) -> FerrosGrynnResult<Value>;

	/// Reads one public field through a graph-scoped occurrence identity.
	fn occurrence_field(
		&self,
		occurrence: &AtomOccurrenceRef,
		field: &str,
	) -> FerrosGrynnResult<Value>;

	/// Returns the inner/transition graph owned by one occurrence.
	fn inner_graph(&self, occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value>;

	/// Computes the canonical read-only LevelPath for one occurrence.
	fn level_path(&self, occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value>;

	/// Reads a field through a stable database record reference.
	fn record_field(&self, record: &RecordId, field: &str) -> FerrosGrynnResult<Value>;

	/// Validates a runtime path before reading snapshot-bound contributions.
	fn validate_path(
		&self,
		_path: &crate::graph::Path,
		_control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		Err(FerrosGrynnError::invalid_argument("custom path cost requires a query snapshot"))
	}
}

/// Immutable builder resolver; values and typed execution retain public
/// identities.
pub(in crate::gql) struct RootFields<'a> {
	/// Exact immutable database read root.
	root: &'a crate::datastore::DatabaseRoot,
	/// Shared runtime memory and cancellation policy.
	control: &'a ExecutionControl,
	/// Conservatively retained owned/transient value admission.
	account: RefCell<BarrierAccount>,
	/// Snapshot-aware SSI observer supplied by the transaction adapter.
	read: &'a dyn Fn(crate::datastore::ConflictKey),
	/// Operation-wide work admission supplied by the caller.
	work: &'a dyn Fn(u64) -> FerrosGrynnResult<()>,
}

impl RootFields<'_> {
	/// Shares one invocation account and snapshot read observer with nested
	/// calls.
	pub(in crate::gql) fn new<'a>(
		root: &'a crate::datastore::DatabaseRoot,
		control: &'a ExecutionControl,
		read: &'a dyn Fn(crate::datastore::ConflictKey),
		work: &'a dyn Fn(u64) -> FerrosGrynnResult<()>,
	) -> RootFields<'a> {
		RootFields {
			root,
			control,
			account: RefCell::new(control.barrier_account()),
			read,
			work,
		}
	}

	/// Resolves a qualified durable occurrence against this exact root.
	fn bound(&self, occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<BoundAtom> {
		let global = self
			.root
			.resolve_atom(occurrence.atom().id())
			.filter(|global| global.kind() == Some(occurrence.atom().kind()))
			.ok_or_else(|| {
				if let Some(class) = self
					.root
					.schema
					.class(occurrence.atom().id().class())
					.and_then(|class| class.id())
				{
					(self.read)(crate::datastore::ConflictKey::BaseClassRange(class));
				}
				FerrosGrynnError::atom_does_not_exist("atom", occurrence.atom().id().to_string())
			})?;
		(self.read)(crate::datastore::ConflictKey::BaseAtom(global));
		(self.read)(crate::datastore::ConflictKey::GraphMembership(
			occurrence.graph().id(),
			global,
		));
		let local = self.root.membership(occurrence.graph().id(), global).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("occurrence", occurrence.atom().id().to_string())
		})?;
		Ok(BoundAtom::new(occurrence.graph().id(), local))
	}
}

impl AtomFieldRead for RootFields<'_> {
	fn observe(&self, key: crate::datastore::ConflictKey) {
		(self.read)(key);
	}

	fn bounded(&self) -> bool {
		true
	}

	fn admit_value(&self, value: &Value) -> FerrosGrynnResult<()> {
		self.admit(2 * size_of::<&Value>())?;
		let mut pending = vec![value];
		while let Some(value) = pending.pop() {
			self.work(1)?;
			let count = match value {
				Value::Array(values) => values.len(),
				Value::Set(values) => values.len(),
				Value::Object(values) => values.len(),
				Value::Range(_) => 2,
				Value::Path(path) => {
					self.work(path.steps().len() as u64)?;
					0
				}
				_ => 0,
			};
			self.work(count as u64)?;
			self.admit(
				count
					.checked_mul(2 * size_of::<Value>() + 128 + 2 * size_of::<&Value>())
					.ok_or_else(|| {
						FerrosGrynnError::operation_overflow("PURE collection scratch")
					})?,
			)?;
			pending.try_reserve(count).map_err(|cause| {
				FerrosGrynnError::traversal_resource_limit_exceeded(
					"PURE value traversal",
					self.control.barrier_memory_limit() as u64,
				)
				.with_technical_source(cause)
			})?;
			match value {
				Value::Array(values) => pending.extend(values.iter()),
				Value::Set(values) => pending.extend(values.iter()),
				Value::Object(values) => {
					for (name, value) in values.iter() {
						self.admit(name.len())?;
						pending.push(value);
					}
				}
				Value::Range(range) => {
					for bound in [range.start(), range.end()] {
						if let std::ops::Bound::Included(value) | std::ops::Bound::Excluded(value) =
							bound
						{
							pending.push(value);
						}
					}
				}
				_ => self.admit(value.estimated_heap_bytes())?,
			}
		}
		Ok(())
	}

	fn admit(&self, bytes: usize) -> FerrosGrynnResult<()> {
		// ponytail: retain conservative transient charges until the outer call
		// returns; use value-lifetime accounts if long scalar loops need more
		// room.
		if !self.account.borrow_mut().reserve(bytes) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"PURE value scratch",
				self.control.barrier_memory_limit() as u64,
			));
		}
		Ok(())
	}

	fn work(&self, operations: u64) -> FerrosGrynnResult<()> {
		(self.work)(operations)
	}

	fn field(&self, atom: BoundAtom, field: &str) -> FerrosGrynnResult<Value> {
		let bound = atom;
		(self.read)(crate::datastore::ConflictKey::GraphIncidence(bound.graph, bound.atom));
		let occurrence = self.root.occurrence(bound.graph, bound.atom).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("occurrence", bound.atom.to_string())
		})?;
		let base = self
			.root
			.atom(occurrence.global_id())
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("PURE occurrence has no base"))?;
		(self.read)(crate::datastore::ConflictKey::BaseAtom(occurrence.global_id()));
		(self.read)(crate::datastore::ConflictKey::GraphMembership(
			bound.graph,
			occurrence.global_id(),
		));
		(self.read)(crate::datastore::ConflictKey::GraphOverlay(
			bound.graph,
			occurrence.global_id(),
		));
		(self.read)(crate::datastore::ConflictKey::PropertyDictionary);
		match field {
			"id" => {
				self.admit(size_of::<RecordId>() + base.record_id().estimated_heap_bytes())?;
				Ok(Value::Record(Box::new(base.record_id().clone())))
			}
			"class" => {
				self.admit(base.record_id().class().len())?;
				Ok(Value::from(base.record_id().class()))
			}
			"kind" => Ok(Value::from(if bound.atom.is_edge() {
				"edge"
			} else {
				"node"
			})),
			"label" => {
				self.admit(occurrence.label().map_or(0, |label| label.len()))?;
				Ok(occurrence.label().cloned().map_or(Value::None, Value::String))
			}
			"graph" => self
				.root
				.graph(bound.graph)
				.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(bound.graph.to_string()))
				.and_then(|graph| {
					(self.read)(crate::datastore::ConflictKey::GraphDefinition(bound.graph));
					self.admit(size_of::<GraphRef>() + graph.reference().name().len())?;
					Ok(Value::GraphRef(Box::new(graph.reference().clone())))
				}),
			"properties" => {
				let mut object = Object::new();
				for (key, value) in base.properties().iter() {
					self.work(1)?;
					let name = self.root.dictionary.resolve(key).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("PURE property name is absent")
					})?;
					self.admit(name.len() + 128)?;
					object.insert(name.clone(), self.copy_value(value)?);
				}
				if let Some(overlay) = occurrence.overlay() {
					for (key, value) in overlay.iter() {
						self.work(1)?;
						let name = self.root.dictionary.resolve(key).ok_or_else(|| {
							FerrosGrynnError::invalid_storage_file(
								"PURE overlay property name is absent",
							)
						})?;
						match value {
							crate::datastore::OverlayValue::Value(value) => {
								self.admit(name.len() + 128)?;
								object.insert(name.clone(), self.copy_value(value)?);
							}
							crate::datastore::OverlayValue::Hidden => {
								object.remove(name);
							}
						}
					}
				}
				Ok(Value::Object(object))
			}
			_ => self
				.root
				.dictionary
				.get(field)
				.and_then(|key| occurrence.effective_value(base.properties().get(key), key))
				.map_or(Ok(Value::None), |value| self.copy_value(value)),
		}
	}

	fn occurrence_field(
		&self,
		occurrence: &AtomOccurrenceRef,
		field: &str,
	) -> FerrosGrynnResult<Value> {
		self.field(self.bound(occurrence)?, field)
	}

	fn inner_graph(&self, occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value> {
		let bound = self.bound(occurrence)?;
		(self.read)(crate::datastore::ConflictKey::GraphIncidence(bound.graph, bound.atom));
		let current = self.root.occurrence(bound.graph, bound.atom).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("PURE owner occurrence is absent")
		})?;
		let nested = match current.state() {
			crate::datastore::OccurrenceState::Node {
				nested_graph,
			} => *nested_graph,
			crate::datastore::OccurrenceState::Edge {
				transition_graph,
				..
			} => *transition_graph,
		};
		match nested {
			None => Ok(Value::None),
			Some(id) => {
				(self.read)(crate::datastore::ConflictKey::GraphDefinition(id));
				let graph = self
					.root
					.graph(id)
					.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(id.to_string()))?;
				self.admit(size_of::<GraphRef>() + graph.reference().name().len())?;
				Ok(Value::GraphRef(Box::new(graph.reference().clone())))
			}
		}
	}

	fn level_path(&self, occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value> {
		let bound = self.bound(occurrence)?;
		(self.read)(crate::datastore::ConflictKey::GraphCatalogRange);
		for graph in self.root.schema.graphs() {
			self.work(1)?;
			(self.read)(crate::datastore::ConflictKey::GraphKindRange(graph.id(), AtomKind::Node));
			(self.read)(crate::datastore::ConflictKey::GraphKindRange(graph.id(), AtomKind::Edge));
		}
		let current = self.root.occurrence(bound.graph, bound.atom).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("PURE LevelPath occurrence is absent")
		})?;
		self.root.level_path_value_bounded(bound.graph, current.global_id(), &|bytes, work| {
			self.admit(bytes)?;
			self.work(work)
		})
	}

	fn record_field(&self, record: &RecordId, field: &str) -> FerrosGrynnResult<Value> {
		let global = self.root.resolve_atom(record).ok_or_else(|| {
			if let Some(class) = self.root.schema.class(record.class()).and_then(|class| class.id())
			{
				(self.read)(crate::datastore::ConflictKey::BaseClassRange(class));
			}
			FerrosGrynnError::atom_does_not_exist("record", record.to_string())
		})?;
		(self.read)(crate::datastore::ConflictKey::BaseAtom(global));
		(self.read)(crate::datastore::ConflictKey::PropertyDictionary);
		let base = self
			.root
			.atom(global)
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("record", record.to_string()))?;
		match field {
			"id" => {
				self.admit(size_of::<RecordId>() + record.estimated_heap_bytes())?;
				Ok(Value::Record(Box::new(record.clone())))
			}
			"label" => Ok(Value::None),
			_ => self
				.root
				.dictionary
				.get(field)
				.and_then(|key| base.properties().get(key))
				.map_or(Ok(Value::None), |value| self.copy_value(value)),
		}
	}
}

/// Checks the origin graph, snapshot and every qualified path occurrence.
fn validate_query_path<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	path: &crate::graph::Path,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	let Some(first) = path.steps().first() else {
		return Ok(());
	};
	let graph = read.graph(snapshot, first.atom().graph())?;
	if graph.identity() != path.origin() {
		return Err(FerrosGrynnError::graph_identity_mismatch(
			path.origin().to_string(),
			graph.identity().to_string(),
		));
	}
	if graph.snapshot_id() != path.snapshot() {
		return Err(FerrosGrynnError::snapshot_unavailable(
			path.origin().to_string(),
			path.snapshot().to_string(),
		));
	}
	drop(graph);
	for step in path.steps() {
		control.check()?;
		if step.via_edge().is_some() {
			control.record_path_expansion()?;
		}
		if step.via_edge().is_some_and(|connector| connector.atom().kind() != AtomKind::Edge) {
			return Err(FerrosGrynnError::invalid_argument(
				"path connector must be an edge occurrence",
			));
		}
		for occurrence in step.via_edge().into_iter().chain([step.atom()]) {
			let graph = read.graph(snapshot, occurrence.graph())?;
			if graph.atom_id(occurrence.atom())?.is_none() {
				return Err(FerrosGrynnError::atom_does_not_exist(
					"occurrence",
					occurrence.atom().id().to_string(),
				));
			}
		}
	}
	Ok(())
}

/// Record-reference resolver bound to one statement snapshot and graph scope.
pub(in crate::gql::execution) struct RecordFields<'a, Read> {
	pub(in crate::gql::execution) read: &'a Read,
	pub(in crate::gql::execution) snapshot: Snapshot,
	pub(in crate::gql::execution) graph: Option<&'a GraphRef>,
}

impl<Read: QueryRead> AtomFieldRead for RecordFields<'_, Read> {
	fn field(&self, atom: BoundAtom, _field: &str) -> FerrosGrynnResult<Value> {
		Err(FerrosGrynnError::invalid_argument(format!(
			"SELECT row has no graph-local atom binding for {}.",
			atom.atom
		)))
	}

	fn occurrence_field(
		&self,
		occurrence: &AtomOccurrenceRef,
		field: &str,
	) -> FerrosGrynnResult<Value> {
		let fields = MatchGraphFields::new(
			self.read,
			self.snapshot,
			self.read.property_dictionary(self.snapshot),
			Arc::new(occurrence.graph().clone()),
		);
		fields.occurrence_field(occurrence, field)
	}

	fn inner_graph(&self, _occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value> {
		Err(FerrosGrynnError::invalid_argument("graph::inner requires a MATCH atom occurrence"))
	}

	fn level_path(&self, _occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value> {
		Err(FerrosGrynnError::invalid_argument(
			"graph::level_path requires a MATCH atom occurrence",
		))
	}

	fn record_field(&self, record: &RecordId, field: &str) -> FerrosGrynnResult<Value> {
		self.read.record_field(self.snapshot, record, self.graph, field)
	}

	fn validate_path(
		&self,
		path: &crate::graph::Path,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		validate_query_path(self.read, self.snapshot, path, control)
	}
}

/// Atom field resolver with a lazy cache of snapshot-visible graph references.
pub(in crate::gql::execution) struct MatchGraphFields<'a, Read> {
	pub(in crate::gql::execution) read: &'a Read,
	pub(in crate::gql::execution) snapshot: Snapshot,
	/// Snapshot property dictionary.
	pub(in crate::gql::execution) dictionary: Arc<PropertyDictionary>,
	references: RefCell<std::collections::BTreeMap<crate::graph::GraphId, Arc<GraphRef>>>,
}

impl<'a, Read: QueryRead> MatchGraphFields<'a, Read> {
	pub(in crate::gql::execution) fn new(
		read: &'a Read,
		snapshot: Snapshot,
		dictionary: Arc<PropertyDictionary>,
		root: Arc<GraphRef>,
	) -> Self {
		let references = std::collections::BTreeMap::from([(root.id(), root)]);
		Self {
			read,
			snapshot,
			dictionary,
			references: RefCell::new(references),
		}
	}

	pub(in crate::gql::execution) fn register(&self, reference: GraphRef) -> Arc<GraphRef> {
		let graph = reference.id();
		if let Some(reference) = self.references.borrow().get(&graph) {
			return reference.clone();
		}
		let reference = Arc::new(reference);
		self.references.borrow_mut().insert(graph, reference.clone());
		reference
	}

	pub(in crate::gql::execution) fn reference(
		&self,
		graph: crate::graph::GraphId,
	) -> FerrosGrynnResult<Arc<GraphRef>> {
		if let Some(reference) = self.references.borrow().get(&graph) {
			return Ok(reference.clone());
		}
		self.read.graph_reference(self.snapshot, graph).map(|reference| self.register(reference))
	}
}

impl<Read: QueryRead> AtomFieldRead for MatchGraphFields<'_, Read> {
	fn field(&self, atom: BoundAtom, field: &str) -> FerrosGrynnResult<Value> {
		let reference = self.reference(atom.graph)?;
		if field == "graph" {
			return Ok(Value::GraphRef(Box::new(reference.as_ref().clone())));
		}
		let graph = self.read.graph(self.snapshot, &reference)?;
		match atom.atom.kind() {
			Some(AtomKind::Node) => {
				let node = graph.node(atom.atom)?.ok_or_else(|| {
					FerrosGrynnError::atom_does_not_exist("node", atom.atom.to_string())
				})?;
				let record = node.record_id()?;
				match field {
					"id" => Ok(Value::Record(Box::new(record.clone().into_owned()))),
					"class" => Ok(Value::from(record.class())),
					"kind" => Ok(Value::from("node")),
					"label" => Ok(node
						.label_text()?
						.map(|label| Value::String(label.into_owned()))
						.unwrap_or(Value::None)),
					"properties" => Ok(Value::Object(properties_object(
						node.properties()?.as_ref(),
						&self.dictionary,
					)?)),
					_ => {
						let Some(key) = self.dictionary.get(field) else {
							return Ok(Value::None);
						};
						Ok(node.properties()?.get(key).cloned().unwrap_or(Value::None))
					}
				}
			}
			Some(AtomKind::Edge) => {
				let edge = graph.edge(atom.atom)?.ok_or_else(|| {
					FerrosGrynnError::atom_does_not_exist("edge", atom.atom.to_string())
				})?;
				let record = edge.record_id()?;
				match field {
					"id" => Ok(Value::Record(Box::new(record.clone().into_owned()))),
					"class" => Ok(Value::from(record.class())),
					"kind" => Ok(Value::from("edge")),
					"label" => Ok(edge
						.label_text()?
						.map(|label| Value::String(label.into_owned()))
						.unwrap_or(Value::None)),
					"properties" => Ok(Value::Object(properties_object(
						edge.properties()?.as_ref(),
						&self.dictionary,
					)?)),
					_ => {
						let Some(key) = self.dictionary.get(field) else {
							return Ok(Value::None);
						};
						Ok(edge.properties()?.get(key).cloned().unwrap_or(Value::None))
					}
				}
			}
			None => Err(FerrosGrynnError::atom_does_not_exist("atom", atom.atom.to_string())),
		}
	}

	fn occurrence_field(
		&self,
		occurrence: &AtomOccurrenceRef,
		field: &str,
	) -> FerrosGrynnResult<Value> {
		let graph = self.read.graph(self.snapshot, occurrence.graph())?;
		let atom = graph.atom_id(occurrence.atom())?.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist(
				occurrence.atom().kind().to_string(),
				occurrence.atom().id().to_string(),
			)
		})?;
		drop(graph);
		self.field(BoundAtom::new(occurrence.graph().id(), atom), field)
	}

	fn inner_graph(&self, occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value> {
		let owner = self.register(occurrence.graph().clone());
		let graph = self.read.graph(self.snapshot, &owner)?;
		let atom = graph.atom_id(occurrence.atom())?.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist(
				occurrence.atom().kind().to_string(),
				occurrence.atom().id().to_string(),
			)
		})?;
		let nested = match atom.kind() {
			Some(AtomKind::Node) => graph
				.node(atom)?
				.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("node", atom.to_string()))?
				.inner_graph_ref()?
				.map(|value| value.into_owned()),
			Some(AtomKind::Edge) => graph
				.edge(atom)?
				.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", atom.to_string()))?
				.transition_graph_ref()?
				.map(|value| value.into_owned()),
			None => None,
		};
		Ok(nested.map_or(Value::None, |reference| {
			let reference = self.register(reference);
			Value::GraphRef(Box::new(reference.as_ref().clone()))
		}))
	}

	fn level_path(&self, occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value> {
		self.read.level_path(self.snapshot, occurrence)
	}

	fn record_field(&self, record: &RecordId, field: &str) -> FerrosGrynnResult<Value> {
		self.read.record_field(self.snapshot, record, None, field)
	}

	fn validate_path(
		&self,
		path: &crate::graph::Path,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		validate_query_path(self.read, self.snapshot, path, control)
	}
}

pub(in crate::gql::execution) fn properties_object(
	properties: &crate::values::PropertyMap,
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<Object> {
	properties
		.iter()
		.map(|(key, value)| {
			dictionary.resolve(key).cloned().map(|name| (name, value.clone())).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"MATCH property key is absent from the property dictionary",
				)
			})
		})
		.collect()
}

pub(in crate::gql::execution) struct FilterCursor<'a> {
	pub(in crate::gql::execution) input: Box<dyn RowCursor + 'a>,
	pub(in crate::gql::execution) predicate: &'a TypedExpr,
	pub(in crate::gql::execution) slots: &'a [Value],
	pub(in crate::gql::execution) source: &'a str,
	/// Optional MATCH atom-field resolver.
	pub(in crate::gql::execution) fields: Option<&'a dyn AtomFieldRead>,
}

impl RowCursor for FilterCursor<'_> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		while let Some(row) = self.input.next(control)? {
			if eval(
				self.predicate,
				Some(&row),
				self.slots,
				self.source,
				self.fields,
				Some(control),
			)?
			.is_truthy()
			{
				return Ok(Some(row));
			}
		}
		Ok(None)
	}
}

pub(in crate::gql::execution) struct ProjectCursor<'a> {
	pub(in crate::gql::execution) input: Box<dyn RowCursor + 'a>,
	pub(in crate::gql::execution) expressions: &'a [TypedExpr],
	pub(in crate::gql::execution) slots: &'a [Value],
	pub(in crate::gql::execution) source: &'a str,
	/// Optional MATCH atom-field resolver.
	pub(in crate::gql::execution) fields: Option<&'a dyn AtomFieldRead>,
}

impl RowCursor for ProjectCursor<'_> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		let Some(row) = self.input.next(control)? else {
			return Ok(None);
		};
		let values = self
			.expressions
			.iter()
			.map(|expression| {
				eval(expression, Some(&row), self.slots, self.source, self.fields, Some(control))
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		Ok(Some(Row::new(values)))
	}
}

pub(in crate::gql::execution) struct LimitCursor<'a> {
	pub(in crate::gql::execution) input: Box<dyn RowCursor + 'a>,
	pub(in crate::gql::execution) remaining: Option<usize>,
	pub(in crate::gql::execution) skip: usize,
}

impl RowCursor for LimitCursor<'_> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		while self.skip > 0 {
			if self.input.next(control)?.is_none() {
				return Ok(None);
			}
			self.skip -= 1;
		}
		if self.remaining == Some(0) {
			return Ok(None);
		}
		let row = self.input.next(control)?;
		if row.is_some()
			&& let Some(remaining) = &mut self.remaining
		{
			*remaining -= 1;
		}
		Ok(row)
	}
}
