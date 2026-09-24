use super::{
	DmlCandidate,
	LogicalDmlReturn,
	LogicalGraphDmlStatement,
	Object,
	Row,
	Schema,
	dml_return_value,
	expect_matches,
	generated_record_id,
	missing_candidate,
	scoped,
};
use crate::{
	datastore::{AtomSnapshot, QuerySnapshot, Transaction},
	gql::{execution::eval, ir::LogicalRecordTarget},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, EdgeTopology},
	values::{Array, GraphRef, Number, PropertyDictionary, RecordId, Value},
};

/// Executes one graph membership or incidence mutation.
pub(in crate::gql::execution) fn execute_graph_dml(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	statement: &LogicalGraphDmlStatement,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<Value> {
	match statement {
		LogicalGraphDmlStatement::Attach {
			record,
			graph,
			label,
		} => {
			let record = record_id(record, slots, source)?;
			transaction.attach_node(graph.id(), &record, label.clone(), None)?;
			if transaction.validates_import_constraints_immediately() {
				transaction.validate_graph_family(graph)?;
			}
			Ok(Value::None)
		}
		LogicalGraphDmlStatement::Detach {
			record,
			graph,
			cascade,
		} => {
			let record = record_id(record, slots, source)?;
			transaction.detach_occurrence(graph.id(), &record, *cascade)?;
			if transaction.validates_import_constraints_immediately() {
				transaction.validate_graph_family(graph)?;
			}
			Ok(Value::None)
		}
		LogicalGraphDmlStatement::Relate {
			atom_endpoints,
			source: source_target,
			edge,
			target,
			graph,
			label,
			direction,
			weight,
		} => {
			let source_record = record_id(source_target, slots, source)?;
			let target_record = record_id(target, slots, source)?;
			let source_local =
				ensure_endpoint(transaction, graph, &source_record, *atom_endpoints)?;
			let target_local =
				ensure_endpoint(transaction, graph, &target_record, *atom_endpoints)?;
			let edge_record = edge
				.key
				.as_ref()
				.map(|key| {
					eval(key, None, slots, source, None)
						.and_then(|key| RecordId::from_value(edge.class.as_ref(), key))
				})
				.transpose()?
				.map_or_else(|| generated_record_id(&edge.class), Ok)?;
			let dictionary = transaction.query_property_dictionary();
			let before = transaction.atom(&edge_record)?;
			let candidate = match &before {
				Some(atom) => candidate_from_atom(atom, &edge.schema, &dictionary)?,
				None => missing_candidate(0, edge_record.clone(), &edge.schema),
			};
			if !expect_matches(edge.expect.as_ref(), &candidate, slots, source)? {
				return Err(FerrosGrynnError::condition_failed(edge_record.to_string()));
			}
			if let Some(atom) = &before {
				if atom.kind() != AtomKind::Edge || atom.record_id().class() != edge.class.as_ref()
				{
					return Err(FerrosGrynnError::invalid_argument(
						"RELATE identity does not belong to the declared EDGE class.",
					));
				}
			} else {
				transaction.create_atom(edge_record.clone(), Default::default())?;
			}
			let topology = topology(*direction, source_local, target_local);
			let weight = weight
				.as_ref()
				.map(|weight| numeric_weight(eval(weight, None, slots, source, None)?))
				.transpose()?;
			let local = transaction.attach_edge(
				graph.id(),
				&edge_record,
				label.clone(),
				topology,
				weight,
				None,
			)?;
			let atom = transaction.atom(&edge_record)?.ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist("edge", edge_record.to_string())
			})?;
			let mut attached = candidate_from_atom(&atom, &edge.schema, &dictionary)?;
			attached.local_id = Some(local);
			let class = transaction_class(transaction, snapshot, &edge.class)?;
			let after = scoped::apply_graph_update(
				transaction,
				graph,
				None,
				edge.data.as_ref(),
				class.as_ref(),
				&attached,
				slots,
				source,
				&dictionary,
			)?;
			if transaction.validates_import_constraints_immediately() {
				transaction.validate_graph_family(graph)?;
			}
			graph_result(&edge.returning, &edge.schema, &candidate.before, &after, slots, source)
		}
	}
}

fn transaction_class(
	transaction: &mut Transaction,
	_snapshot: &QuerySnapshot,
	class: &str,
) -> FerrosGrynnResult<std::sync::Arc<crate::datastore::ClassSchema>> {
	let current = transaction.query_snapshot()?;
	transaction.query_class(&current, class)
}

fn record_id(
	target: &LogicalRecordTarget,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<RecordId> {
	RecordId::from_value(target.class.as_ref(), eval(&target.key, None, slots, source, None)?)
}

fn ensure_endpoint(
	transaction: &mut Transaction,
	graph: &GraphRef,
	record: &RecordId,
	atom_endpoints: bool,
) -> FerrosGrynnResult<AtomId> {
	let atom = transaction
		.atom(record)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("endpoint", record.to_string()))?;
	if !atom_endpoints && atom.kind() != AtomKind::Node {
		return Err(FerrosGrynnError::invalid_argument(
			"RELATE endpoints require NODE atoms unless `RELATE ATOM` is used.",
		));
	}
	if let Some(local) = transaction.membership(graph.id(), record)? {
		return Ok(local);
	}
	if atom.kind() != AtomKind::Node {
		return Err(FerrosGrynnError::graph_constraint_violation(
			graph.name().to_string(),
			"an EDGE endpoint must already have graph-local topology",
		));
	}
	transaction.attach_node(graph.id(), record, None, None)
}

fn topology(direction: crate::syn::MatchDirection, source: AtomId, target: AtomId) -> EdgeTopology {
	match direction {
		crate::syn::MatchDirection::Forward => EdgeTopology::directed([source], [target]),
		crate::syn::MatchDirection::Backward => EdgeTopology::directed([target], [source]),
		crate::syn::MatchDirection::Bidirectional => {
			EdgeTopology::bidirectional([source], [target])
		}
		crate::syn::MatchDirection::Undirected => EdgeTopology::undirected([source, target]),
	}
}

fn numeric_weight(value: Value) -> FerrosGrynnResult<Number> {
	match value {
		Value::Number(value) => Ok(value),
		value => Err(FerrosGrynnError::invalid_argument(format!(
			"RELATE WEIGHTED requires a number, found {}.",
			value.value_type()
		))),
	}
}

fn candidate_from_atom(
	atom: &AtomSnapshot,
	schema: &Schema,
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<DmlCandidate> {
	let mut before = Object::new();
	before.insert("id", Value::Record(Box::new(atom.record_id().clone())));
	for (key, value) in atom.properties() {
		let name = dictionary.resolve(key).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(format!(
				"property dictionary does not resolve key {key}"
			))
		})?;
		before.insert(name.clone(), value.clone());
	}
	let values = schema
		.columns
		.iter()
		.map(|column| before.get(&column.name).cloned().unwrap_or(Value::None))
		.collect();
	Ok(DmlCandidate {
		target: 0,
		record_id: atom.record_id().clone(),
		kind: Some(atom.kind()),
		local_id: None,
		base_properties: atom.properties().clone(),
		properties: atom.properties().clone(),
		before,
		row: Row::new(values),
	})
}

fn graph_result(
	returning: &LogicalDmlReturn,
	schema: &Schema,
	before: &Object,
	after: &Object,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<Value> {
	if matches!(returning, LogicalDmlReturn::None) {
		return Ok(Value::None);
	}
	let value = dml_return_value(returning, schema, before, Some(after), slots, source)?;
	Ok(Value::Array(Array::new(value.into_iter().collect())))
}
