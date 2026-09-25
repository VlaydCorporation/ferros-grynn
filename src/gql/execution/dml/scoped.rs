use std::collections::BTreeSet;

use super::{DmlCandidate, LogicalDmlData, Object, apply_dml_data, validate_dml_object};
use crate::{
	datastore::{ClassSchema, Transaction},
	gql::options::ExecutionControl,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphRef,
	values::{PropertyDictionary, Text, Value},
};

/// Applies one update to the effective graph view while storing only the
/// minimal difference from the database-global atom.
#[allow(
	clippy::too_many_arguments,
	reason = "the graph update boundary keeps the analyzed statement context explicit"
)]
pub(super) fn apply_graph_update(
	transaction: &mut Transaction,
	graph: &GraphRef,
	label: Option<&Option<Text>>,
	data: Option<&LogicalDmlData>,
	schema: &ClassSchema,
	candidate: &DmlCandidate,
	slots: &[Value],
	source: &str,
	dictionary: &PropertyDictionary,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Object> {
	candidate.local_id.ok_or_else(|| {
		FerrosGrynnError::atom_does_not_exist("occurrence", candidate.record_id.to_string())
	})?;
	let mut after = candidate.before.clone();
	match data {
		Some(LogicalDmlData::Reset(fields)) => {
			reset_effective_fields(fields, &candidate.base_properties, dictionary, &mut after);
		}
		data => apply_dml_data(data, &candidate.row, slots, source, &mut after, control)?,
	}
	after.insert("id", Value::Record(Box::new(candidate.record_id.clone())));
	validate_dml_object(schema, &after)?;
	apply_minimal_overlay(transaction, graph, candidate, data, dictionary, &after)?;
	if let Some(label) = label {
		transaction.set_occurrence_label(graph.id(), &candidate.record_id, label.clone())?;
	}
	Ok(after)
}

fn reset_effective_fields(
	fields: &[Box<str>],
	base: &crate::values::PropertyMap,
	dictionary: &PropertyDictionary,
	after: &mut Object,
) {
	for field in fields {
		match dictionary.get(field).and_then(|key| base.get(key)) {
			Some(value) => {
				after.insert(field.as_ref(), value.clone());
			}
			None => {
				after.remove(field.as_ref());
			}
		}
	}
}

fn apply_minimal_overlay(
	transaction: &mut Transaction,
	graph: &GraphRef,
	candidate: &DmlCandidate,
	data: Option<&LogicalDmlData>,
	dictionary: &PropertyDictionary,
	after: &Object,
) -> FerrosGrynnResult<()> {
	let reset = match data {
		Some(LogicalDmlData::Reset(fields)) => fields.iter().map(Box::as_ref).collect(),
		_ => BTreeSet::new(),
	};
	let names = candidate
		.before
		.keys()
		.chain(after.keys())
		.map(Text::as_str)
		.filter(|name| *name != "id")
		.chain(reset.iter().copied())
		.collect::<BTreeSet<_>>();
	for name in names {
		let before = candidate.before.get(name);
		let value = after.get(name);
		if before == value && !reset.contains(name) {
			continue;
		}
		let key = match (dictionary.get(name), value) {
			(Some(key), _) => key,
			(None, Some(_)) => transaction.intern_property(name)?,
			(None, None) => continue,
		};
		match (candidate.base_properties.get(key), value) {
			(Some(base), Some(value)) if base == value => {
				transaction.reset_property(graph.id(), &candidate.record_id, key)?;
			}
			(_, Some(value)) => {
				transaction.set_overlay(graph.id(), &candidate.record_id, key, value.clone())?;
			}
			(Some(_), None) => {
				transaction.hide_property(graph.id(), &candidate.record_id, key)?;
			}
			(None, None) => {
				transaction.reset_property(graph.id(), &candidate.record_id, key)?;
			}
		}
	}
	Ok(())
}
