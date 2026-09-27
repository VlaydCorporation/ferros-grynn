#![allow(
	clippy::wildcard_imports,
	reason = "graph-property execution shares the concrete query runtime vocabulary"
)]

use super::*;
use crate::{
	datastore::GraphSchema,
	graph::GraphStorageMode,
	values::{PropertyMap, RecordIdPart},
};

pub(super) fn evaluate_graph_properties(
	transaction: &mut Transaction,
	expression: Option<&TypedExpr>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<PropertyMap> {
	let Some(expression) = expression else {
		return Ok(PropertyMap::new());
	};
	let value = eval(expression, None, slots, source, None, Some(control))?;
	let Value::Object(object) = value else {
		return Err(FerrosGrynnError::invalid_argument(
			"DEFINE GRAPH CONTENT requires an object value.",
		));
	};
	object_to_property_map(transaction, &object)
}

pub(super) fn execute_graph_property_update(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	graph: &GraphRef,
	mutation: &LogicalGraphPropertyMutation,
	returning: &LogicalDmlReturn,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let schema = transaction
		.graphs()?
		.into_iter()
		.find(|candidate| candidate.id() == graph.id())
		.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.name().to_string()))?;
	let dictionary = transaction.query_property_dictionary();
	let before_properties = property_map_to_object(schema.properties(), &dictionary)?;
	let mut after_properties = before_properties.clone();
	match mutation {
		LogicalGraphPropertyMutation::Set(assignments) => {
			let assignments = assignments
				.iter()
				.map(|(path, value)| {
					eval(value, None, slots, source, None, Some(control))
						.map(|value| (path.as_slice(), value))
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			for (path, value) in assignments {
				control.check()?;
				if value.is_none() {
					return Err(FerrosGrynnError::invalid_argument(
						"SET cannot store none; use UNSET to remove a graph property.",
					));
				}
				set_object_path(&mut after_properties, path, value)?;
			}
		}
		LogicalGraphPropertyMutation::Unset(paths) => {
			for path in paths {
				control.check()?;
				unset_object_path(&mut after_properties, path)?;
			}
		}
	}
	let after_map = object_to_property_map(transaction, &after_properties)?;
	let view = transaction.query_graph(snapshot, graph)?;
	let before = graph_row(&schema, &view, before_properties)?;
	let after = graph_row(&schema, &view, after_properties)?;
	transaction.replace_graph_properties(graph.name().as_str(), after_map)?;
	match returning {
		LogicalDmlReturn::None => Ok(Value::None),
		LogicalDmlReturn::Before => Ok(Value::Object(before)),
		LogicalDmlReturn::After => Ok(Value::Object(after)),
		LogicalDmlReturn::Diff => Ok(Value::Object(super::dml::dml_diff(&before, Some(&after)))),
		LogicalDmlReturn::Fields(_) | LogicalDmlReturn::Value(_) => {
			Err(FerrosGrynnError::invalid_argument(
				"UPDATE GRAPH supports only RETURN NONE, BEFORE, AFTER, or DIFF.",
			))
		}
	}
}

pub(super) fn property_map_to_object(
	properties: &PropertyMap,
	dictionary: &crate::values::PropertyDictionary,
) -> FerrosGrynnResult<Object> {
	properties
		.iter()
		.map(|(key, value)| {
			dictionary.resolve(key).cloned().map(|name| (name, value.clone())).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"graph property key is absent from the property dictionary",
				)
			})
		})
		.collect()
}

fn object_to_property_map(
	transaction: &mut Transaction,
	object: &Object,
) -> FerrosGrynnResult<PropertyMap> {
	intern_object_keys(transaction, object)?;
	object
		.iter()
		.map(|(name, value)| {
			if value.is_none() {
				return Err(FerrosGrynnError::invalid_argument(
					"Graph properties cannot store none.",
				));
			}
			transaction.intern_property(name.as_str()).map(|key| (key, value.clone()))
		})
		.collect()
}

fn intern_object_keys(transaction: &mut Transaction, object: &Object) -> FerrosGrynnResult<()> {
	for (name, value) in object {
		transaction.intern_property(name.as_str())?;
		intern_value_keys(transaction, value)?;
	}
	Ok(())
}

fn intern_value_keys(transaction: &mut Transaction, value: &Value) -> FerrosGrynnResult<()> {
	match value {
		Value::Object(object) => intern_object_keys(transaction, object),
		Value::Array(values) => {
			for value in values {
				intern_value_keys(transaction, value)?;
			}
			Ok(())
		}
		Value::Set(values) => {
			for value in values {
				intern_value_keys(transaction, value)?;
			}
			Ok(())
		}
		Value::Range(range) => {
			for bound in [range.start(), range.end()] {
				if let std::ops::Bound::Included(value) | std::ops::Bound::Excluded(value) = bound {
					intern_value_keys(transaction, value)?;
				}
			}
			Ok(())
		}
		Value::Record(record) => intern_record_id_keys(transaction, record.id()),
		Value::Atom(atom) => intern_record_id_keys(transaction, atom.id().id()),
		Value::AtomOccurrence(occurrence) => {
			intern_record_id_keys(transaction, occurrence.atom().id().id())
		}
		_ => Ok(()),
	}
}

fn intern_record_id_keys(
	transaction: &mut Transaction,
	id: &RecordIdPart,
) -> FerrosGrynnResult<()> {
	match id {
		RecordIdPart::Array(values) => {
			for value in values {
				intern_value_keys(transaction, value)?;
			}
			Ok(())
		}
		RecordIdPart::Object(object) => intern_object_keys(transaction, object),
		_ => Ok(()),
	}
}

fn set_object_path(object: &mut Object, path: &[Box<str>], value: Value) -> FerrosGrynnResult<()> {
	let Some((head, tail)) = path.split_first() else {
		return Err(FerrosGrynnError::invalid_argument("Graph property path is empty."));
	};
	if tail.is_empty() {
		object.insert(head.as_ref(), value);
		return Ok(());
	}
	if !object.contains_key(head) {
		object.insert(head.as_ref(), Object::new());
	}
	let Some(Value::Object(nested)) = object.get_mut(head) else {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"Graph property '{}' is not an object.",
			head
		)));
	};
	set_object_path(nested, tail, value)
}

fn unset_object_path(object: &mut Object, path: &[Box<str>]) -> FerrosGrynnResult<()> {
	let Some((head, tail)) = path.split_first() else {
		return Err(FerrosGrynnError::invalid_argument("Graph property path is empty."));
	};
	if tail.is_empty() {
		object.remove(head);
		return Ok(());
	}
	let Some(value) = object.get_mut(head) else {
		return Ok(());
	};
	let Value::Object(nested) = value else {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"Graph property '{}' is not an object.",
			head
		)));
	};
	unset_object_path(nested, tail)
}

fn graph_row<Graph: GraphRead>(
	schema: &GraphSchema,
	graph: &Graph,
	properties: Object,
) -> FerrosGrynnResult<Object> {
	Ok(Object::from_iter([
		("id", Value::from(schema.reference().clone())),
		("name", Value::from(schema.name().clone())),
		("kind", Value::from(schema.reference().kind().name())),
		("storage_mode", Value::from(storage_mode_name(schema.reference().storage_mode()))),
		("revision", Value::from(graph.revision())),
		("node_count", Value::from(graph.node_count()?)),
		("edge_count", Value::from(graph.edge_count()?)),
		("properties", Value::Object(properties)),
	]))
}

pub(super) const fn storage_mode_name(mode: GraphStorageMode) -> &'static str {
	match mode {
		GraphStorageMode::InMemory => "in_memory",
		GraphStorageMode::DiskBacked => "disk_backed",
		GraphStorageMode::Hybrid => "hybrid",
	}
}
