//! Logical RELATE references become checked incidence in one private root.

use fg_runtime::__internal::EdgeEndpointInternal as _;

use super::Transaction;
use crate::{
	datastore::ConflictKey,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomId,
		AtomKind,
		AtomOccurrenceRef,
		EdgeEndpoint,
		GraphId,
		GraphRef,
		QualifiedAtomId,
	},
	values::{RecordId, Value},
};

impl Transaction {
	/// Qualified occurrences require explicit ordered owners outside the
	/// connector graph. Global node references retain ordinary RELATE
	/// auto-attachment.
	///
	/// # Errors
	///
	/// Returns malformed references, wrong kind/scope, absent occurrences or
	/// limits.
	pub(crate) fn resolve_relate_reference(
		&mut self,
		graph: &GraphRef,
		value: &Value,
		atom_endpoints: bool,
		expected: Option<AtomKind>,
		admit: &dyn Fn(usize) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<EdgeEndpoint> {
		self.prepare_read()?;
		let endpoint = match value {
			Value::AtomOccurrence(occurrence) => {
				self.relate_occurrence(graph.id(), occurrence, &[], admit)?
			}
			Value::Object(descriptor) => {
				let occurrence = descriptor
					.get("occurrence")
					.and_then(|value| match value {
						Value::AtomOccurrence(occurrence) => Some(occurrence.as_ref()),
						_ => None,
					})
					.ok_or_else(|| {
						FerrosGrynnError::invalid_argument(
							"qualified RELATE descriptor requires an occurrence",
						)
					})?;
				let owners =
					descriptor.get("owners").and_then(Value::as_array).ok_or_else(|| {
						FerrosGrynnError::invalid_argument(
							"qualified RELATE descriptor requires an owners array",
						)
					})?;
				if descriptor.len() != 2 {
					return Err(FerrosGrynnError::invalid_argument(
						"qualified RELATE descriptor has unknown fields",
					));
				}
				self.relate_occurrence(graph.id(), occurrence, owners.as_slice(), admit)?
			}
			Value::Record(record) => self.relate_global(graph.id(), record, None)?,
			Value::Atom(atom) => self.relate_global(graph.id(), atom.id(), Some(atom.kind()))?,
			Value::String(record) => {
				admit(record.len().saturating_add(std::mem::size_of::<RecordId>()))?;
				self.relate_global(graph.id(), &record.as_str().parse()?, None)?
			}
			_ => {
				return Err(FerrosGrynnError::invalid_argument(
					"RELATE reference must identify a record, atom or qualified occurrence",
				));
			}
		};
		let kind = endpoint
			.kind()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("RELATE endpoint kind is absent"))?;
		if !atom_endpoints && kind != AtomKind::Node
			|| expected.is_some_and(|expected| expected != kind)
		{
			return Err(FerrosGrynnError::invalid_argument(
				"RELATE endpoint does not match its node/edge wrapper",
			));
		}
		Ok(endpoint)
	}

	/// Global identities are resolved without cloning their property payloads.
	fn relate_global(
		&mut self,
		graph: GraphId,
		record: &RecordId,
		declared: Option<AtomKind>,
	) -> FerrosGrynnResult<EdgeEndpoint> {
		let global = self.require_atom(record)?;
		let (_class, kind) = self.atom_domain(global)?;
		if declared.is_some_and(|declared| declared != kind) {
			return Err(FerrosGrynnError::invalid_argument(
				"RELATE atom reference has the wrong kind",
			));
		}
		self.record_read(ConflictKey::GraphMembership(graph, global));
		if let Some(local) = self.working.membership(graph, global) {
			return Ok(EdgeEndpoint::Local(local));
		}
		if kind != AtomKind::Node {
			return Err(FerrosGrynnError::graph_constraint_violation(
				graph.to_string(),
				"an EDGE endpoint must already have graph-local topology",
			));
		}
		self.attach_node(graph, record, None, None).map(EdgeEndpoint::Local)
	}

	/// Each owner and terminal keeps its supplied logical graph and atom
	/// identity.
	fn relate_occurrence(
		&self,
		origin: GraphId,
		terminal: &AtomOccurrenceRef,
		owners: &[Value],
		admit: &dyn Fn(usize) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<EdgeEndpoint> {
		if owners.len() > 64 {
			return Err(FerrosGrynnError::invalid_argument(
				"edge endpoint scope exceeds 64 levels",
			));
		}
		let bytes = owners.len().saturating_mul(std::mem::size_of::<AtomId>());
		admit(bytes)?;
		self.ensure_capacity(bytes)?;
		let mut scope = Vec::new();
		scope
			.try_reserve_exact(owners.len())
			.map_err(|error| self.limit_error(bytes).with_technical_source(error))?;
		let mut graph = origin;
		for owner in owners {
			let Value::AtomOccurrence(owner) = owner else {
				return Err(FerrosGrynnError::invalid_argument(
					"RELATE owners must be qualified occurrences",
				));
			};
			let local = self.relate_scoped_identity(graph, owner)?;
			scope.push(local);
			graph = self
				.working
				.occurrence(graph, local)
				.and_then(|owner| owner.child_graph())
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("RELATE owner has no child graph")
				})?;
		}
		let local = self.relate_scoped_identity(graph, terminal)?;
		if scope.is_empty() {
			Ok(EdgeEndpoint::Local(local))
		} else {
			QualifiedAtomId::new(scope, local).map(EdgeEndpoint::Nested)
		}
	}

	/// Foreign graph scopes are never silently attached to the connector graph.
	fn relate_scoped_identity(
		&self,
		graph: GraphId,
		occurrence: &AtomOccurrenceRef,
	) -> FerrosGrynnResult<AtomId> {
		if occurrence.graph().id() != graph {
			return Err(FerrosGrynnError::invalid_argument(
				"RELATE occurrence scope requires an explicit complete owners route",
			));
		}
		self.record_read(ConflictKey::GraphDefinition(graph));
		let global = self.require_atom(occurrence.atom().id())?;
		let (_class, kind) = self.atom_domain(global)?;
		if kind != occurrence.atom().kind() {
			return Err(FerrosGrynnError::invalid_argument(
				"RELATE occurrence has the wrong atom kind",
			));
		}
		self.record_read(ConflictKey::GraphMembership(graph, global));
		let local = self.require_membership(graph, global)?;
		self.record_read(ConflictKey::GraphIncidence(graph, local));
		Ok(local)
	}
}
