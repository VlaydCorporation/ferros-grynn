use super::graph_properties::{evaluate_graph_properties, execute_graph_property_update};
use crate::{
	datastore::{
		ClassField,
		ClassIndex,
		ClassIndexKind,
		ClassIndexState,
		ClassSchema,
		ClassSchemaMode,
		GraphSchema,
		IndexScope,
		Transaction,
	},
	gql::{execution::QuerySnapshot, ir::LogicalSchemaStatement},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphKind,
	syn::DefinitionMode,
	values::{AtomKind, GraphRef, Object, Type, Value},
};

/// Executes one analyzed schema statement against transaction-visible state.
pub(super) fn execute_schema(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	statement: &LogicalSchemaStatement,
	slots: &[Value],
	source: &str,
	control: &crate::gql::ExecutionControl,
) -> FerrosGrynnResult<Value> {
	match statement {
		LogicalSchemaStatement::DefineGraph {
			mode,
			name,
			kind,
			content,
		} => define_graph(transaction, *mode, name, *kind, content.as_ref(), slots, source, control),
		LogicalSchemaStatement::UpdateGraph {
			graph,
			mutation,
			returning,
		} => execute_graph_property_update(
			transaction,
			snapshot,
			graph,
			mutation,
			returning,
			slots,
			source,
			control,
		),
		LogicalSchemaStatement::DefineClass {
			mode,
			name,
			kind,
			schema_mode,
		} => define_class(transaction, *mode, name, *kind, *schema_mode),
		LogicalSchemaStatement::DefineField {
			mode,
			class,
			name,
			value_type,
		} => define_field(transaction, *mode, class, name, value_type.clone()),
		LogicalSchemaStatement::DefineIndex {
			mode,
			class,
			name,
			kind,
			fields,
			scope,
		} => define_index(transaction, *mode, class, name, *kind, fields, *scope),
		LogicalSchemaStatement::RemoveGraph {
			if_exists,
			name,
		} => remove_graph(transaction, *if_exists, name),
		LogicalSchemaStatement::RemoveClass {
			if_exists,
			name,
		} => remove_class(transaction, *if_exists, name),
		LogicalSchemaStatement::RemoveField {
			if_exists,
			class,
			name,
		} => remove_field(transaction, *if_exists, class, name),
		LogicalSchemaStatement::RemoveIndex {
			if_exists,
			class,
			name,
		} => remove_index(transaction, *if_exists, class, name),
		LogicalSchemaStatement::AlterClassMode {
			if_exists,
			name,
			mode,
		} => alter_class(transaction, *if_exists, name, *mode),
		LogicalSchemaStatement::AlterFieldType {
			if_exists,
			class,
			name,
			value_type,
		} => alter_field(transaction, *if_exists, class, name, value_type.clone()),
		LogicalSchemaStatement::PrepareIndexRemoval {
			class,
			name,
		} => transaction.prepare_index_removal(class, name).map(|()| Value::None),
		LogicalSchemaStatement::InfoDatabase => info_database(transaction),
		LogicalSchemaStatement::InfoClass {
			name,
		} => info_class(transaction, name),
		LogicalSchemaStatement::InfoIndex {
			class,
			name,
		} => info_index(transaction, class, name),
	}
}

fn define_graph(
	transaction: &mut Transaction,
	mode: DefinitionMode,
	name: &str,
	kind: GraphKind,
	content: Option<&crate::gql::ir::TypedExpr>,
	slots: &[Value],
	source: &str,
	control: &crate::gql::ExecutionControl,
) -> FerrosGrynnResult<Value> {
	if let Some(existing) = transaction.named_graph(name)? {
		return match mode {
			DefinitionMode::IfNotExists => Ok(Value::None),
			DefinitionMode::Overwrite if existing.kind() == kind => {
				if content.is_some() {
					let properties =
						evaluate_graph_properties(transaction, content, slots, source, control)?;
					transaction.replace_graph_properties(name, properties)?;
				}
				Ok(Value::None)
			}
			DefinitionMode::Overwrite => Err(FerrosGrynnError::query_feature_not_supported(
				"changing a graph family with DEFINE GRAPH OVERWRITE",
			)),
			DefinitionMode::Normal => Err(FerrosGrynnError::graph_already_exists(name)),
		};
	}
	let properties = evaluate_graph_properties(transaction, content, slots, source, control)?;
	transaction.define_graph_with_properties(name, kind, properties)?;
	Ok(Value::None)
}

fn define_class(
	transaction: &mut Transaction,
	mode: DefinitionMode,
	name: &str,
	kind: AtomKind,
	schema_mode: ClassSchemaMode,
) -> FerrosGrynnResult<Value> {
	if let Some(existing) = transaction.class(name)? {
		return match mode {
			DefinitionMode::IfNotExists => Ok(Value::None),
			DefinitionMode::Overwrite if existing.kind() == kind => {
				transaction.set_class_schema_mode(name, schema_mode)?;
				Ok(Value::None)
			}
			DefinitionMode::Overwrite => Err(FerrosGrynnError::query_feature_not_supported(
				"changing NODE to EDGE or EDGE to NODE with DEFINE CLASS OVERWRITE",
			)),
			DefinitionMode::Normal => Err(FerrosGrynnError::class_already_exists(name)),
		};
	}
	transaction.define_class(ClassSchema::new(name, kind, [])?.with_schema_mode(schema_mode))?;
	Ok(Value::None)
}

fn define_field(
	transaction: &mut Transaction,
	mode: DefinitionMode,
	class: &str,
	name: &str,
	value_type: Type,
) -> FerrosGrynnResult<Value> {
	let schema = require_class(transaction, class)?;
	let exists = schema.fields().any(|field| field.name().as_str() == name);
	if mode == DefinitionMode::IfNotExists && exists {
		return Ok(Value::None);
	}
	let field = ClassField::new(name, value_type);
	if mode == DefinitionMode::Overwrite && exists {
		transaction.overwrite_field(class, field)?;
	} else {
		transaction.define_field(class, field)?;
	}
	Ok(Value::None)
}

fn define_index(
	transaction: &mut Transaction,
	mode: DefinitionMode,
	class: &str,
	name: &str,
	kind: ClassIndexKind,
	fields: &[Box<str>],
	scope: IndexScope,
) -> FerrosGrynnResult<Value> {
	let schema = require_class(transaction, class)?;
	let exists = schema.indexes().any(|index| index.name().as_str() == name);
	if mode == DefinitionMode::IfNotExists && exists {
		return Ok(Value::None);
	}
	let index = ClassIndex::new(name, kind, scope, fields.iter().map(Box::as_ref))?;
	if mode == DefinitionMode::Overwrite && exists {
		transaction.overwrite_index(class, index)?;
	} else {
		transaction.define_index(class, index)?;
	}
	Ok(Value::None)
}

fn remove_graph(
	transaction: &mut Transaction,
	if_exists: bool,
	name: &str,
) -> FerrosGrynnResult<Value> {
	if transaction.named_graph(name)?.is_none() && if_exists {
		return Ok(Value::None);
	}
	transaction.remove_graph(name)?;
	Ok(Value::None)
}

fn remove_class(
	transaction: &mut Transaction,
	if_exists: bool,
	name: &str,
) -> FerrosGrynnResult<Value> {
	if transaction.class(name)?.is_none() && if_exists {
		return Ok(Value::None);
	}
	transaction.remove_class(name)?;
	Ok(Value::None)
}

fn remove_field(
	transaction: &mut Transaction,
	if_exists: bool,
	class: &str,
	name: &str,
) -> FerrosGrynnResult<Value> {
	let Some(schema) = transaction.class(class)? else {
		if if_exists {
			return Ok(Value::None);
		}
		return Err(FerrosGrynnError::class_does_not_exist(class));
	};
	if !schema.fields().any(|field| field.name().as_str() == name) && if_exists {
		return Ok(Value::None);
	}
	transaction.remove_field(class, name)?;
	Ok(Value::None)
}

fn remove_index(
	transaction: &mut Transaction,
	if_exists: bool,
	class: &str,
	name: &str,
) -> FerrosGrynnResult<Value> {
	let Some(schema) = transaction.class(class)? else {
		if if_exists {
			return Ok(Value::None);
		}
		return Err(FerrosGrynnError::class_does_not_exist(class));
	};
	if !schema.indexes().any(|index| index.name().as_str() == name) && if_exists {
		return Ok(Value::None);
	}
	transaction.remove_index(class, name)?;
	Ok(Value::None)
}

fn alter_class(
	transaction: &mut Transaction,
	if_exists: bool,
	name: &str,
	mode: ClassSchemaMode,
) -> FerrosGrynnResult<Value> {
	if transaction.class(name)?.is_none() && if_exists {
		return Ok(Value::None);
	}
	transaction.set_class_schema_mode(name, mode)?;
	Ok(Value::None)
}

fn alter_field(
	transaction: &mut Transaction,
	if_exists: bool,
	class: &str,
	name: &str,
	value_type: Type,
) -> FerrosGrynnResult<Value> {
	let Some(schema) = transaction.class(class)? else {
		if if_exists {
			return Ok(Value::None);
		}
		return Err(FerrosGrynnError::class_does_not_exist(class));
	};
	if !schema.fields().any(|field| field.name().as_str() == name) && if_exists {
		return Ok(Value::None);
	}
	transaction.overwrite_field(class, ClassField::new(name, value_type))?;
	Ok(Value::None)
}

fn require_class(transaction: &mut Transaction, name: &str) -> FerrosGrynnResult<ClassSchema> {
	transaction
		.class(name)?
		.map(|class| class.as_ref().clone())
		.ok_or_else(|| FerrosGrynnError::class_does_not_exist(name))
}

fn info_database(transaction: &mut Transaction) -> FerrosGrynnResult<Value> {
	let graphs = transaction
		.graphs()?
		.into_iter()
		.map(|schema| (schema.name().clone(), graph_definition(schema.reference())))
		.collect::<Object>();
	let classes = transaction
		.classes()?
		.into_iter()
		.map(|schema| (schema.name().clone(), class_definition(&schema)))
		.collect::<Object>();
	Ok(Value::from(Object::from_iter([
		("analyzers", Value::from(Object::new())),
		("classes", Value::from(classes)),
		("configs", Value::from(Object::new())),
		("functions", Value::from(Object::new())),
		("graphs", Value::from(graphs)),
		("models", Value::from(Object::new())),
		("params", Value::from(Object::new())),
	])))
}

fn info_class(transaction: &mut Transaction, name: &str) -> FerrosGrynnResult<Value> {
	let schema = require_class(transaction, name)?;
	let graphs = transaction.graphs()?;
	let fields = schema
		.fields()
		.map(|field| (field.name().clone(), field_definition(name, field)))
		.collect::<Object>();
	let mut indexes = schema
		.indexes()
		.map(|index| (index.name().clone(), index_info(name, index, &graphs)))
		.collect::<Object>();
	indexes.insert("class", system_index_info(name, "class"));
	indexes.insert("primary", system_index_info(name, "primary"));
	Ok(Value::from(Object::from_iter([
		("classes", Value::from(Object::new())),
		("definition", class_definition(&schema)),
		("events", Value::from(Object::new())),
		("fields", Value::from(fields)),
		("indexes", Value::from(indexes)),
		("lives", Value::from(Object::new())),
	])))
}

fn info_index(transaction: &mut Transaction, class: &str, name: &str) -> FerrosGrynnResult<Value> {
	let schema = require_class(transaction, class)?;
	if matches!(name, "class" | "primary") {
		return Ok(system_index_info(class, name));
	}
	let graphs = transaction.graphs()?;
	schema
		.indexes()
		.find(|index| index.name().as_str() == name)
		.map(|index| index_info(class, index, &graphs))
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument(format!("index '{class}.{name}' does not exist"))
		})
}

fn graph_definition(reference: &GraphRef) -> Value {
	let suffix = match reference.kind() {
		GraphKind::Graph => String::new(),
		kind => format!(" TYPE {}", kind.name().to_ascii_uppercase()),
	};
	Value::from(format!("DEFINE GRAPH {}{suffix}", quote_name(reference.name().as_str())))
}

fn class_definition(schema: &ClassSchema) -> Value {
	let kind = match schema.kind() {
		AtomKind::Node => "NODE",
		AtomKind::Edge => "EDGE",
	};
	let mode = match schema.schema_mode() {
		ClassSchemaMode::Schemaless => "SCHEMALESS",
		ClassSchemaMode::Schemafull => "SCHEMAFULL",
	};
	Value::from(format!("DEFINE CLASS {} TYPE {kind} {mode}", quote_name(schema.name().as_str())))
}

fn field_definition(class: &str, field: &ClassField) -> Value {
	Value::from(format!(
		"DEFINE FIELD {} ON CLASS {} TYPE {}",
		quote_name(field.name().as_str()),
		quote_name(class),
		field.value_type()
	))
}

fn index_info(class: &str, index: &ClassIndex, graphs: &[GraphSchema]) -> Value {
	let state = index_state_name(index.state());
	Value::from(Object::from_iter([
		("building", Value::from(Object::from_iter([("status", Value::from(state))]))),
		("definition", index_definition(class, index, graphs)),
		("state", Value::from(state)),
		("system", Value::Bool(false)),
	]))
}

fn system_index_info(class: &str, name: &str) -> Value {
	Value::from(Object::from_iter([
		("building", Value::from(Object::from_iter([("status", Value::from("ready"))]))),
		("definition", Value::from(format!("SYSTEM INDEX {name} ON CLASS {}", quote_name(class)))),
		("state", Value::from("ready")),
		("system", Value::Bool(true)),
	]))
}

fn index_definition(class: &str, index: &ClassIndex, graphs: &[GraphSchema]) -> Value {
	let body = match index.kind() {
		ClassIndexKind::Count => "COUNT".to_owned(),
		ClassIndexKind::Standard | ClassIndexKind::Unique => {
			let fields = index.fields().map(|field| quote_name(field.as_str())).collect::<Vec<_>>();
			let unique = if index.kind() == ClassIndexKind::Unique {
				" UNIQUE"
			} else {
				""
			};
			format!("FIELDS {}{unique}", fields.join(", "))
		}
	};
	let scope = match index.scope() {
		IndexScope::Base => "SCOPE BASE".to_owned(),
		IndexScope::Effective {
			graph,
		} => {
			let graph = graphs
				.iter()
				.find(|schema| schema.id() == graph)
				.map(|schema| quote_name(schema.name().as_str()))
				.unwrap_or_else(|| graph.to_string());
			format!("SCOPE EFFECTIVE IN GRAPH {graph}")
		}
	};
	Value::from(format!(
		"DEFINE INDEX {} ON CLASS {} {body} {scope}",
		quote_name(index.name().as_str()),
		quote_name(class)
	))
}

const fn index_state_name(state: ClassIndexState) -> &'static str {
	match state {
		ClassIndexState::Ready => "ready",
		ClassIndexState::Stale => "stale",
		ClassIndexState::PreparedForRemoval => "prepared_for_removal",
	}
}

fn quote_name(name: &str) -> String {
	format!("`{}`", name.replace('`', "``"))
}
