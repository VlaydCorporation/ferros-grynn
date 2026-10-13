use super::{
	DmlCandidate,
	LogicalDmlReturn,
	LogicalGraphDmlStatement,
	Object,
	Row,
	Schema,
	dml_return_value,
	eval_dml_read_value,
	expect_matches,
	generated_record_id,
	missing_candidate,
	scoped,
};
use crate::{
	datastore::{AtomSnapshot, QuerySnapshot, Transaction},
	gql::{ir::LogicalRecordTarget, options::ExecutionControl},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, EdgeEndpoint, EdgeTopology},
	values::{Array, GraphRef, Number, PropertyDictionary, RecordId, Value},
};

/// Executes one graph membership or incidence mutation.
pub(in crate::gql::execution) fn execute_graph_dml(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	statement: &LogicalGraphDmlStatement,
	slots: &mut [Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	match statement {
		LogicalGraphDmlStatement::Attach {
			record,
			graph,
			label,
			keep_levelpath,
		} => {
			let record =
				record_id(transaction, snapshot, Some(graph), record, slots, source, control)?;
			transaction.attach_node(graph.id(), &record, label.clone(), None)?;
			if *keep_levelpath {
				transaction.store_level_path(graph.id(), &record)?;
			}
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
			let record =
				record_id(transaction, snapshot, Some(graph), record, slots, source, control)?;
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
			keep_levelpath,
		} => {
			let source_local = relate_endpoint(
				transaction,
				snapshot,
				graph,
				source_target,
				*atom_endpoints,
				slots,
				source,
				control,
			)?;
			let target_local = relate_endpoint(
				transaction,
				snapshot,
				graph,
				target,
				*atom_endpoints,
				slots,
				source,
				control,
			)?;
			let edge_record = edge
				.key
				.as_ref()
				.map(|key| {
					eval_dml_read_value(
						transaction,
						snapshot,
						Some(graph),
						key,
						&Row::new(Vec::new()),
						slots,
						source,
						control,
					)
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
			if !expect_matches(
				transaction,
				snapshot,
				Some(graph),
				edge.expect.as_ref(),
				&candidate,
				slots,
				source,
				control,
			)? {
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
				.map(|weight| {
					numeric_weight(eval_dml_read_value(
						transaction,
						snapshot,
						Some(graph),
						weight,
						&Row::new(Vec::new()),
						slots,
						source,
						control,
					)?)
				})
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
			let mut after = scoped::apply_graph_update(
				transaction,
				graph,
				None,
				edge.data.as_ref(),
				class.as_ref(),
				&attached,
				slots,
				source,
				&dictionary,
				control,
			)?;
			if *keep_levelpath {
				after.insert(
					crate::datastore::LEVEL_PATH_PROPERTY,
					transaction.store_level_path(graph.id(), &edge_record)?,
				);
			}
			if transaction.validates_import_constraints_immediately() {
				transaction.validate_graph_family(graph)?;
			}
			graph_result(
				transaction,
				Some(graph),
				&edge.returning,
				&edge.schema,
				&candidate.before,
				&after,
				slots,
				source,
				control,
			)
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
	transaction: &Transaction,
	snapshot: &QuerySnapshot,
	graph: Option<&GraphRef>,
	target: &LogicalRecordTarget,
	slots: &mut [Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RecordId> {
	RecordId::from_value(
		target.class.as_ref(),
		eval_dml_read_value(
			transaction,
			snapshot,
			graph,
			&target.key,
			&Row::new(Vec::new()),
			slots,
			source,
			control,
		)?,
	)
}

/// References use the ordinary reader-aware kernel and the common incidence
/// resolver.
fn relate_endpoint(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	graph: &GraphRef,
	endpoint: &crate::gql::ir::LogicalRelateEndpoint,
	atom: bool,
	slots: &mut [Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<EdgeEndpoint> {
	match endpoint {
		crate::gql::ir::LogicalRelateEndpoint::Record(record) => {
			let record =
				record_id(transaction, snapshot, Some(graph), record, slots, source, control)?;
			ensure_endpoint(transaction, graph, &record, atom).map(EdgeEndpoint::Local)
		}
		crate::gql::ir::LogicalRelateEndpoint::Reference {
			value,
			kind,
		} => {
			let value = eval_dml_read_value(
				transaction,
				snapshot,
				Some(graph),
				value,
				&Row::new(Vec::new()),
				slots,
				source,
				control,
			)?;
			let memory = std::cell::RefCell::new(control.barrier_account());
			transaction.resolve_relate_reference(graph, &value, atom, *kind, &|bytes| {
				control.check()?;
				if !memory.borrow_mut().reserve(bytes) {
					return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
						"RELATE endpoint scratch",
						control.barrier_memory_limit() as u64,
					));
				}
				Ok(())
			})
		}
	}
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

/// Builds the logical qualified topology used for exact mutation images.
fn topology(
	direction: crate::syn::MatchDirection,
	source: EdgeEndpoint,
	target: EdgeEndpoint,
) -> EdgeTopology {
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
	transaction: &mut Transaction,
	graph: Option<&GraphRef>,
	returning: &LogicalDmlReturn,
	schema: &Schema,
	before: &Object,
	after: &Object,
	slots: &mut [Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	if matches!(returning, LogicalDmlReturn::None) {
		return Ok(Value::None);
	}
	let value = dml_return_value(
		transaction,
		graph,
		returning,
		schema,
		before,
		Some(after),
		slots,
		source,
		control,
	)?;
	Ok(Value::Array(Array::new(value.into_iter().collect())))
}
