use super::{
	eval::eval,
	graph_properties::{evaluate_graph_properties, execute_graph_property_update},
};
/// Event lifecycle commands use the common SDK source validator.
mod event;
use crate::{
	datastore::{
		AnalyzerFilter,
		AnalyzerLanguage,
		AnalyzerTokenizer,
		ClassField,
		ClassIndex,
		ClassIndexKind,
		ClassIndexOptions,
		ClassIndexState,
		ClassSchema,
		ClassSchemaMode,
		FunctionEffect,
		GraphSchema,
		IndexMaintenanceMode,
		IndexScope,
		MaintenanceJobInfo,
		MaintenanceTarget,
		SchemaAnalyzer,
		SchemaFunction,
		SchemaFunctionParameter,
		SchemaParameter,
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
		LogicalSchemaStatement::Event(statement) => event::execute(transaction, statement, control),
		LogicalSchemaStatement::DefineLiveBinding(plan) => {
			live::binding(transaction, plan, None, slots, source, control)
		}
		LogicalSchemaStatement::ReplaceLiveBinding {
			id,
			plan,
		} => live::binding(transaction, plan, Some(id), slots, source, control),
		LogicalSchemaStatement::RemoveLiveBinding {
			id,
		} => {
			let id = crate::live::LiveBindingId::from_uuid(
				live::identity(id, slots, source, control)?.as_uuid(),
			)?;
			let options = crate::live::LiveBindingOptions::default()
				.with_budgets(control.barrier_memory_available(), control.max_loop_iterations)?;
			transaction.remove_live_binding_controlled(id, options, Some(control))?;
			Ok(Value::None)
		}
		LogicalSchemaStatement::InfoLiveBinding {
			id,
		} => live::binding_info(transaction, snapshot, id, slots, control),
		LogicalSchemaStatement::AckLive {
			id,
			cursor,
			..
		} => live::ack(transaction, snapshot, id, cursor, slots, source, control),
		LogicalSchemaStatement::RegisterLive(plan) => {
			live::register(transaction, snapshot, plan, slots, control)
		}
		LogicalSchemaStatement::InfoLive {
			id,
		} => live::info(transaction, snapshot, id, slots, control),
		LogicalSchemaStatement::CloseLive {
			id,
		} => {
			transaction.close_live(live::identity(id, slots, source, control)?)?;
			Ok(Value::None)
		}
		LogicalSchemaStatement::DefineGraph {
			mode,
			name,
			kind,
			constraints,
			root,
			content,
			changefeed,
		} => {
			let skipped =
				*mode == DefinitionMode::IfNotExists && transaction.named_graph(name)?.is_some();
			let value = define_graph(
				transaction,
				*mode,
				name,
				*kind,
				*constraints,
				root.as_ref(),
				content.as_ref(),
				slots,
				source,
				control,
			)?;
			if !skipped && let Some(config) = changefeed {
				super::changefeed::configure(transaction, true, false, name, Some(*config))?;
			}
			Ok(value)
		}
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
			changefeed,
		} => {
			let skipped =
				*mode == DefinitionMode::IfNotExists && transaction.class(name)?.is_some();
			let value = define_class(transaction, *mode, name, *kind, *schema_mode)?;
			if !skipped && let Some(config) = changefeed {
				super::changefeed::configure(transaction, false, false, name, Some(*config))?;
			}
			Ok(value)
		}
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
			options,
			scope,
			maintenance,
			concurrently,
		} => define_index(
			transaction,
			*mode,
			class,
			name,
			*kind,
			fields,
			*options,
			*scope,
			*maintenance,
			*concurrently,
		),
		LogicalSchemaStatement::DefineGraphIndex {
			mode,
			graph,
			index,
			..
		} => {
			let schemas = transaction.graphs()?;
			let schema = schemas
				.iter()
				.find(|schema| schema.name().as_str() == graph.as_ref())
				.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.as_ref()))?;
			let exists = schema.indexes().any(|old| old.name() == index.name());
			if *mode == DefinitionMode::IfNotExists && exists {
				return Ok(Value::None);
			}
			if *mode == DefinitionMode::Overwrite && exists {
				transaction.overwrite_graph_index(graph, index.clone())?;
			} else {
				transaction.define_graph_index(graph, index.clone())?;
			}
			Ok(Value::None)
		}
		LogicalSchemaStatement::DefineAnalyzer {
			mode,
			analyzer,
		} => define_analyzer(transaction, *mode, analyzer),
		LogicalSchemaStatement::DefineParameter {
			mode,
			name,
			value,
			comment,
		} => define_parameter(
			transaction,
			*mode,
			name,
			value,
			comment.as_ref(),
			slots,
			source,
			control,
		),
		LogicalSchemaStatement::DefineFunction {
			mode,
			namespace,
			name,
			parameters,
			result,
			effect,
			cost,
			max_depth,
			body,
			comment,
		} => define_function(
			transaction,
			*mode,
			namespace,
			name,
			parameters,
			result.clone(),
			*effect,
			*cost,
			*max_depth,
			body,
			comment.as_ref(),
		),
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
		LogicalSchemaStatement::RemoveGraphIndex {
			if_exists,
			graph,
			name,
		} => {
			let exists = transaction.graphs()?.iter().any(|schema| {
				schema.name().as_str() == graph.as_ref()
					&& schema.indexes().any(|index| index.name().as_str() == name.as_ref())
			});
			if !exists && *if_exists {
				return Ok(Value::None);
			}
			transaction.remove_graph_index(graph, name)?;
			Ok(Value::None)
		}
		LogicalSchemaStatement::RemoveParameter {
			if_exists,
			name,
		} => remove_parameter(transaction, *if_exists, name),
		LogicalSchemaStatement::RemoveFunction {
			if_exists,
			namespace,
			name,
			signature,
		} => remove_function(transaction, *if_exists, namespace, name, signature.as_deref()),
		LogicalSchemaStatement::RemoveAnalyzer {
			if_exists,
			name,
		} => remove_analyzer(transaction, *if_exists, name),
		LogicalSchemaStatement::AlterClassMode {
			if_exists,
			name,
			mode,
		} => alter_class(transaction, *if_exists, name, *mode),
		LogicalSchemaStatement::AlterChangefeed {
			graph,
			if_exists,
			name,
			configuration,
		} => super::changefeed::configure(transaction, *graph, *if_exists, name, *configuration),
		LogicalSchemaStatement::InfoChangefeed {
			id,
			..
		} => super::changefeed::info(transaction, snapshot, id, slots, source, control),
		LogicalSchemaStatement::ShowChanges {
			target,
			since,
			full,
			limit,
			..
		} => super::changefeed::show(
			transaction,
			snapshot,
			target,
			since,
			*full,
			limit.as_ref(),
			slots,
			source,
			control,
		),
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
		LogicalSchemaStatement::AlterParameter {
			if_exists,
			name,
			value,
			comment,
		} => alter_parameter(
			transaction,
			*if_exists,
			name,
			value.as_ref(),
			comment.clone(),
			slots,
			source,
			control,
		),
		LogicalSchemaStatement::AlterFunction {
			if_exists,
			namespace,
			name,
			signature,
			effect,
			cost,
			max_depth,
			body,
			comment,
		} => alter_function(
			transaction,
			*if_exists,
			namespace,
			name,
			signature.as_deref(),
			*effect,
			*cost,
			*max_depth,
			body.as_ref(),
			comment.clone(),
		),
		LogicalSchemaStatement::AlterAnalyzer {
			if_exists,
			name,
			function,
			tokenizers,
			filters,
			comment,
		} => alter_analyzer(
			transaction,
			*if_exists,
			name,
			*function,
			tokenizers.as_deref(),
			filters.as_deref(),
			comment.clone(),
		),
		LogicalSchemaStatement::InfoDatabase => info_database(transaction),
		LogicalSchemaStatement::InfoAnalyzer {
			name,
		} => info_analyzer(transaction, name),
		LogicalSchemaStatement::InfoClass {
			name,
		} => info_class(transaction, name, control),
		LogicalSchemaStatement::InfoGraph {
			name,
		} => {
			let graph = transaction
				.named_graph(name)?
				.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name.as_ref()))?;
			let events = event::info(
				transaction,
				crate::datastore::EventTarget::Graph(graph.id()),
				control,
			)?;
			Ok(Value::from(Object::from_iter([
				("definition", Value::from(graph)),
				("events", Value::from(events)),
			])))
		}
		LogicalSchemaStatement::InfoIndex {
			class,
			name,
		} => info_index(transaction, class, name),
		LogicalSchemaStatement::InfoGraphIndex {
			graph,
			name,
		} => {
			let schemas = transaction.graphs()?;
			let schema = schemas
				.iter()
				.find(|schema| schema.name().as_str() == graph.as_ref())
				.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.as_ref()))?;
			if name.as_ref() == "edge_endpoint"
				&& let Some(index) = schema.edge_endpoint_index()
			{
				let mut info = Object::default();
				info.insert("id", Value::from(u64::from(index.id().get())));
				info.insert("version", Value::from(u64::from(index.version().get())));
				info.insert("graph", Value::from(schema.id().get()));
				info.insert("kind", Value::from("EDGE_ENDPOINT"));
				info.insert("system", Value::Bool(true));
				info.insert("state", Value::from("READY"));
				info.insert("maintenance", Value::from("IMMEDIATE"));
				return Ok(Value::Object(info));
			}
			let index = schema
				.indexes()
				.find(|index| index.name().as_str() == name.as_ref())
				.ok_or_else(|| FerrosGrynnError::invalid_argument("graph index is absent"))?;
			graph_index_info(index)
		}
		LogicalSchemaStatement::InfoParameter {
			name,
		} => info_parameter(transaction, name),
		LogicalSchemaStatement::InfoFunction {
			namespace,
			name,
			signature,
		} => info_function(transaction, namespace, name, signature.as_deref()),
	}
}

/// LIVE command execution only adapts owned recipes to the existing SDK.
mod live;

fn define_analyzer(
	transaction: &mut Transaction,
	mode: DefinitionMode,
	analyzer: &SchemaAnalyzer,
) -> FerrosGrynnResult<Value> {
	if transaction.analyzer(analyzer.name().as_str())?.is_some() {
		return match mode {
			DefinitionMode::IfNotExists => Ok(Value::None),
			DefinitionMode::Normal => Err(FerrosGrynnError::invalid_argument(format!(
				"analyzer '{}' already exists",
				analyzer.name()
			))),
			DefinitionMode::Overwrite => {
				transaction.replace_analyzer(analyzer.name().as_str(), analyzer.clone())?;
				Ok(Value::None)
			}
		};
	}
	transaction.define_analyzer(analyzer.clone())?;
	Ok(Value::None)
}

fn remove_analyzer(
	transaction: &mut Transaction,
	if_exists: bool,
	name: &str,
) -> FerrosGrynnResult<Value> {
	if transaction.analyzer(name)?.is_none() && if_exists {
		return Ok(Value::None);
	}
	transaction.remove_analyzer(name)?;
	Ok(Value::None)
}

fn alter_analyzer(
	transaction: &mut Transaction,
	if_exists: bool,
	name: &str,
	function: Option<Option<(crate::datastore::FunctionId, crate::datastore::SchemaVersion)>>,
	tokenizers: Option<&[AnalyzerTokenizer]>,
	filters: Option<&[AnalyzerFilter]>,
	comment: Option<Option<crate::values::Text>>,
) -> FerrosGrynnResult<Value> {
	let Some(existing) = transaction.analyzer(name)? else {
		if if_exists {
			return Ok(Value::None);
		}
		return Err(FerrosGrynnError::invalid_argument(format!(
			"analyzer '{name}' does not exist"
		)));
	};
	let selected_tokenizers = tokenizers
		.map(<[AnalyzerTokenizer]>::to_vec)
		.unwrap_or_else(|| existing.tokenizers().collect());
	let selected_filters = filters
		.map(<[AnalyzerFilter]>::to_vec)
		.unwrap_or_else(|| existing.filters().cloned().collect());
	let selected_function = function.unwrap_or(existing.function());
	let mut replacement = SchemaAnalyzer::new(name, selected_tokenizers, selected_filters)?;
	if let Some((id, version)) = selected_function {
		replacement = replacement.with_function(id, version)?;
	}
	if let Some(comment) = comment.unwrap_or_else(|| existing.comment().cloned()) {
		replacement = replacement.with_comment(comment);
	}
	transaction.replace_analyzer(name, replacement)?;
	Ok(Value::None)
}

#[allow(
	clippy::too_many_arguments,
	reason = "function DDL forwards the fixed catalog definition fields"
)]
fn define_function(
	transaction: &mut Transaction,
	mode: DefinitionMode,
	namespace: &str,
	name: &str,
	parameters: &[(Box<str>, Type)],
	result: Type,
	effect: FunctionEffect,
	cost: bool,
	max_depth: Option<u8>,
	body: &crate::values::Text,
	comment: Option<&crate::values::Text>,
) -> FerrosGrynnResult<Value> {
	let signature = parameters.iter().map(|(_, value_type)| value_type.clone()).collect::<Vec<_>>();
	let existing = resolve_function(transaction, namespace, name, Some(&signature))?;
	if let Some(existing) = existing {
		return match mode {
			DefinitionMode::IfNotExists => Ok(Value::None),
			DefinitionMode::Normal => Err(FerrosGrynnError::invalid_argument(format!(
				"function overload '{namespace}::{name}' already exists"
			))),
			DefinitionMode::Overwrite => {
				let mut replacement = new_function(
					namespace, name, parameters, result, effect, cost, max_depth, body, comment,
				)?;
				replacement.id = existing.id();
				replacement.version = existing.version();
				transaction.replace_function(replacement)?;
				Ok(Value::None)
			}
		};
	}
	let function =
		new_function(namespace, name, parameters, result, effect, cost, max_depth, body, comment)?;
	transaction.define_function(function)?;
	Ok(Value::None)
}

#[allow(
	clippy::too_many_arguments,
	reason = "function replacement forwards the fixed catalog definition fields"
)]
fn new_function(
	namespace: &str,
	name: &str,
	parameters: &[(Box<str>, Type)],
	result: Type,
	effect: FunctionEffect,
	cost: bool,
	max_depth: Option<u8>,
	body: &crate::values::Text,
	comment: Option<&crate::values::Text>,
) -> FerrosGrynnResult<SchemaFunction> {
	let parameters = parameters
		.iter()
		.map(|(name, value_type)| SchemaFunctionParameter::new(name.as_ref(), value_type.clone()))
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let mut function = SchemaFunction::new(
		namespace,
		name,
		parameters,
		result,
		effect,
		cost,
		max_depth,
		body.clone(),
	)?;
	if let Some(comment) = comment {
		function = function.with_comment(comment.clone());
	}
	Ok(function)
}

fn define_parameter(
	transaction: &mut Transaction,
	mode: DefinitionMode,
	name: &str,
	value: &crate::gql::ir::TypedExpr,
	comment: Option<&crate::values::Text>,
	slots: &[Value],
	source: &str,
	control: &crate::gql::ExecutionControl,
) -> FerrosGrynnResult<Value> {
	if transaction.parameter(name)?.is_some() {
		return match mode {
			DefinitionMode::IfNotExists => Ok(Value::None),
			DefinitionMode::Normal => Err(FerrosGrynnError::invalid_argument(format!(
				"parameter '${name}' already exists"
			))),
			DefinitionMode::Overwrite => {
				let value = eval(value, None, slots, source, None, Some(control))?;
				value.ensure_storable()?;
				transaction.replace_parameter(name, Some(value), Some(comment.cloned()))?;
				Ok(Value::None)
			}
		};
	}
	let value = eval(value, None, slots, source, None, Some(control))?;
	value.ensure_storable()?;
	let mut parameter = SchemaParameter::new(name, value)?;
	if let Some(comment) = comment {
		parameter = parameter.with_comment(comment.clone());
	}
	transaction.define_parameter(parameter)?;
	Ok(Value::None)
}

fn alter_parameter(
	transaction: &mut Transaction,
	if_exists: bool,
	name: &str,
	value: Option<&crate::gql::ir::TypedExpr>,
	comment: Option<Option<crate::values::Text>>,
	slots: &[Value],
	source: &str,
	control: &crate::gql::ExecutionControl,
) -> FerrosGrynnResult<Value> {
	if transaction.parameter(name)?.is_none() {
		if if_exists {
			return Ok(Value::None);
		}
		return Err(FerrosGrynnError::invalid_argument(format!(
			"parameter '${name}' does not exist"
		)));
	}
	let value =
		value.map(|value| eval(value, None, slots, source, None, Some(control))).transpose()?;
	if let Some(value) = &value {
		value.ensure_storable()?;
	}
	transaction.replace_parameter(name, value, comment)?;
	Ok(Value::None)
}

fn define_graph(
	transaction: &mut Transaction,
	mode: DefinitionMode,
	name: &str,
	kind: GraphKind,
	constraints: crate::graph::GraphTopologyConstraints,
	root: Option<&crate::gql::ir::TypedExpr>,
	content: Option<&crate::gql::ir::TypedExpr>,
	slots: &[Value],
	source: &str,
	control: &crate::gql::ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let root = root
		.map(|root| {
			let value = eval(root, None, slots, source, None, Some(control))?;
			let Value::Record(root) = value else {
				return Err(FerrosGrynnError::invalid_argument(
					"DEFINE GRAPH ROOT requires one RecordId.",
				));
			};
			Ok(*root)
		})
		.transpose()?;
	if let Some(existing) = transaction.named_graph(name)? {
		return match mode {
			DefinitionMode::IfNotExists => Ok(Value::None),
			DefinitionMode::Overwrite if existing.kind() == kind => {
				let properties = content
					.map(|content| {
						evaluate_graph_properties(
							transaction,
							Some(content),
							slots,
							source,
							control,
						)
					})
					.transpose()?;
				transaction.replace_graph_definition(name, properties, constraints, root)?;
				Ok(Value::None)
			}
			DefinitionMode::Overwrite => Err(FerrosGrynnError::query_feature_not_supported(
				"changing a graph family with DEFINE GRAPH OVERWRITE",
			)),
			DefinitionMode::Normal => Err(FerrosGrynnError::graph_already_exists(name)),
		};
	}
	let properties = evaluate_graph_properties(transaction, content, slots, source, control)?;
	transaction.define_graph_with_contract(name, kind, properties, constraints, root, None)?;
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
	options: ClassIndexOptions,
	scope: IndexScope,
	maintenance: IndexMaintenanceMode,
	concurrently: bool,
) -> FerrosGrynnResult<Value> {
	let schema = require_class(transaction, class)?;
	let exists = schema.indexes().any(|index| index.name().as_str() == name);
	if mode == DefinitionMode::IfNotExists && exists {
		return Ok(Value::None);
	}
	let mut index = match options {
		ClassIndexOptions::Scalar => {
			ClassIndex::new(name, kind, scope, fields.iter().map(Box::as_ref))?
		}
		ClassIndexOptions::Geometry(options) => ClassIndex::new_geometry(
			name,
			scope,
			fields
				.first()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("Geometry index field is absent")
				})?
				.as_ref(),
			options,
		)?,
		ClassIndexOptions::Fulltext(options) => {
			ClassIndex::new_fulltext(name, scope, only_index_field(fields)?, options)?
		}
		ClassIndexOptions::Hnsw(options) => {
			ClassIndex::new_hnsw(name, scope, only_index_field(fields)?, options)?
		}
		ClassIndexOptions::DiskAnn(options) => {
			ClassIndex::new_disk_ann(name, scope, only_index_field(fields)?, options)?
		}
	}
	.with_maintenance_mode(maintenance)?;
	if concurrently {
		index = index.with_state(ClassIndexState::Stale);
	}
	if mode == DefinitionMode::Overwrite && exists {
		transaction.overwrite_index(class, index)?;
	} else {
		transaction.define_index(class, index)?;
	}
	Ok(Value::None)
}

fn only_index_field(fields: &[Box<str>]) -> FerrosGrynnResult<&str> {
	match fields {
		[field] => Ok(field.as_ref()),
		_ => Err(FerrosGrynnError::invalid_argument(
			"a specialized index requires exactly one field",
		)),
	}
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

fn remove_parameter(
	transaction: &mut Transaction,
	if_exists: bool,
	name: &str,
) -> FerrosGrynnResult<Value> {
	if transaction.parameter(name)?.is_none() && if_exists {
		return Ok(Value::None);
	}
	transaction.remove_parameter(name)?;
	Ok(Value::None)
}

fn remove_function(
	transaction: &mut Transaction,
	if_exists: bool,
	namespace: &str,
	name: &str,
	signature: Option<&[Type]>,
) -> FerrosGrynnResult<Value> {
	let Some(function) = resolve_function(transaction, namespace, name, signature)? else {
		if if_exists {
			return Ok(Value::None);
		}
		return Err(function_missing(namespace, name));
	};
	let id = function.id().ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("published function is missing FunctionId")
	})?;
	transaction.remove_function(id)?;
	Ok(Value::None)
}

#[allow(
	clippy::too_many_arguments,
	reason = "named-function execution shares one explicit statement context"
)]
fn alter_function(
	transaction: &mut Transaction,
	if_exists: bool,
	namespace: &str,
	name: &str,
	signature: Option<&[Type]>,
	effect: Option<FunctionEffect>,
	cost: Option<bool>,
	max_depth: Option<Option<u8>>,
	body: Option<&crate::values::Text>,
	comment: Option<Option<crate::values::Text>>,
) -> FerrosGrynnResult<Value> {
	let Some(existing) = resolve_function(transaction, namespace, name, signature)? else {
		if if_exists {
			return Ok(Value::None);
		}
		return Err(function_missing(namespace, name));
	};
	let parameters = existing
		.parameters()
		.map(|parameter| {
			SchemaFunctionParameter::new(parameter.name().clone(), parameter.value_type().clone())
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let mut replacement = SchemaFunction::new(
		existing.namespace().clone(),
		existing.name().clone(),
		parameters,
		existing.result().clone(),
		effect.unwrap_or(existing.effect()),
		cost.unwrap_or(existing.is_cost()),
		max_depth.unwrap_or(existing.max_depth()),
		body.cloned().unwrap_or_else(|| existing.body().clone()),
	)?;
	replacement.id = existing.id();
	replacement.version = existing.version();
	replacement.comment = comment.unwrap_or_else(|| existing.comment().cloned());
	transaction.replace_function(replacement)?;
	Ok(Value::None)
}

fn resolve_function(
	transaction: &mut Transaction,
	namespace: &str,
	name: &str,
	signature: Option<&[Type]>,
) -> FerrosGrynnResult<Option<std::sync::Arc<SchemaFunction>>> {
	let mut candidates = transaction.functions_named(namespace, name)?;
	if let Some(signature) = signature {
		candidates.retain(|function| {
			function.parameters().map(SchemaFunctionParameter::value_type).eq(signature.iter())
		});
	}
	match candidates.len() {
		0 => Ok(None),
		1 => Ok(candidates.pop()),
		_ => Err(FerrosGrynnError::invalid_argument(format!(
			"function '{namespace}::{name}' is ambiguous; specify its parameter types"
		))),
	}
}

fn function_missing(namespace: &str, name: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("function '{namespace}::{name}' does not exist"))
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
	let analyzers = transaction
		.analyzers()?
		.into_iter()
		.map(|analyzer| (analyzer.name().clone(), analyzer_info(&analyzer)))
		.collect::<Object>();
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
	let parameters = transaction
		.parameters()?
		.into_iter()
		.map(|parameter| (parameter.name().clone(), parameter_info(&parameter)))
		.collect::<Object>();
	let functions = transaction
		.functions()?
		.into_iter()
		.map(|function| (function_info_key(&function), function_info(&function)))
		.collect::<Object>();
	Ok(Value::from(Object::from_iter([
		("analyzers", Value::from(analyzers)),
		("classes", Value::from(classes)),
		("configs", Value::from(Object::new())),
		("functions", Value::from(functions)),
		("graphs", Value::from(graphs)),
		("models", Value::from(Object::new())),
		("params", Value::from(parameters)),
	])))
}

fn info_analyzer(transaction: &mut Transaction, name: &str) -> FerrosGrynnResult<Value> {
	transaction.analyzer(name)?.map(|analyzer| analyzer_info(&analyzer)).ok_or_else(|| {
		FerrosGrynnError::invalid_argument(format!("analyzer '{name}' does not exist"))
	})
}

fn analyzer_info(analyzer: &SchemaAnalyzer) -> Value {
	let tokenizers = analyzer
		.tokenizers()
		.map(|tokenizer| Value::from(analyzer_tokenizer_name(tokenizer)))
		.collect::<Vec<_>>();
	let filters = analyzer.filters().map(analyzer_filter_info).collect::<Vec<_>>();
	let function = analyzer.function().map_or(Value::None, |(id, version)| {
		Value::from(Object::from_iter([
			("id", Value::from(u64::from(id.get()))),
			("version", Value::from(u64::from(version.get()))),
		]))
	});
	Value::from(Object::from_iter([
		("comment", analyzer.comment().cloned().map_or(Value::None, Value::from)),
		("filters", Value::from(filters)),
		("function", function),
		("id", analyzer.id().map_or(Value::None, |id| Value::from(u64::from(id.get())))),
		("name", Value::from(analyzer.name().clone())),
		("tokenizers", Value::from(tokenizers)),
		("version", Value::from(u64::from(analyzer.version().get()))),
	]))
}

const fn analyzer_tokenizer_name(tokenizer: AnalyzerTokenizer) -> &'static str {
	match tokenizer {
		AnalyzerTokenizer::Blank => "blank",
		AnalyzerTokenizer::Camel => "camel",
		AnalyzerTokenizer::Class => "class",
		AnalyzerTokenizer::Punct => "punct",
	}
}

fn analyzer_filter_info(filter: &AnalyzerFilter) -> Value {
	match filter {
		AnalyzerFilter::Nfc => Value::from("nfc"),
		AnalyzerFilter::Nfkc => Value::from("nfkc"),
		AnalyzerFilter::Ascii => Value::from("ascii"),
		AnalyzerFilter::Lowercase => Value::from("lowercase"),
		AnalyzerFilter::Uppercase => Value::from("uppercase"),
		AnalyzerFilter::EdgeNgram {
			min,
			max,
		} => Value::from(Object::from_iter([
			("filter", Value::from("edgengram")),
			("max", Value::from(u64::from(*max))),
			("min", Value::from(u64::from(*min))),
		])),
		AnalyzerFilter::Ngram {
			min,
			max,
		} => Value::from(Object::from_iter([
			("filter", Value::from("ngram")),
			("max", Value::from(u64::from(*max))),
			("min", Value::from(u64::from(*min))),
		])),
		AnalyzerFilter::Snowball(language) => Value::from(Object::from_iter([
			("filter", Value::from("snowball")),
			("language", Value::from(analyzer_language_name(*language))),
		])),
		AnalyzerFilter::Mapper(entries) => {
			let mapper = entries
				.iter()
				.map(|(source, target)| {
					(source.clone(), target.clone().map_or(Value::Null, Value::from))
				})
				.collect::<Object>();
			Value::from(Object::from_iter([
				("filter", Value::from("mapper")),
				("value", Value::from(mapper)),
			]))
		}
	}
}

const fn analyzer_language_name(language: AnalyzerLanguage) -> &'static str {
	match language {
		AnalyzerLanguage::Arabic => "arabic",
		AnalyzerLanguage::Danish => "danish",
		AnalyzerLanguage::Dutch => "dutch",
		AnalyzerLanguage::English => "english",
		AnalyzerLanguage::Finnish => "finnish",
		AnalyzerLanguage::French => "french",
		AnalyzerLanguage::German => "german",
		AnalyzerLanguage::Greek => "greek",
		AnalyzerLanguage::Hungarian => "hungarian",
		AnalyzerLanguage::Italian => "italian",
		AnalyzerLanguage::Norwegian => "norwegian",
		AnalyzerLanguage::Portuguese => "portuguese",
		AnalyzerLanguage::Romanian => "romanian",
		AnalyzerLanguage::Russian => "russian",
		AnalyzerLanguage::Spanish => "spanish",
		AnalyzerLanguage::Swedish => "swedish",
		AnalyzerLanguage::Tamil => "tamil",
		AnalyzerLanguage::Turkish => "turkish",
	}
}

fn info_function(
	transaction: &mut Transaction,
	namespace: &str,
	name: &str,
	signature: Option<&[Type]>,
) -> FerrosGrynnResult<Value> {
	resolve_function(transaction, namespace, name, signature)?
		.map(|function| function_info(&function))
		.ok_or_else(|| function_missing(namespace, name))
}

fn function_info(function: &SchemaFunction) -> Value {
	let effect = match function.effect() {
		FunctionEffect::Pure => "pure",
		FunctionEffect::Stable => "stable",
		FunctionEffect::Impure => "impure",
	};
	let parameters = function
		.parameters()
		.map(|parameter| {
			Value::from(Object::from_iter([
				("name", Value::from(parameter.name().clone())),
				("type", Value::from(parameter.value_type().to_string())),
			]))
		})
		.collect::<Vec<_>>();
	Value::from(Object::from_iter([
		("body", Value::from(function.body().clone())),
		("comment", function.comment().cloned().map_or(Value::None, Value::from)),
		("cost", Value::Bool(function.is_cost())),
		("effect", Value::from(effect)),
		("id", function.id().map_or(Value::None, |id| Value::from(u64::from(id.get())))),
		(
			"max_depth",
			function.max_depth().map_or(Value::None, |depth| Value::from(u64::from(depth))),
		),
		("name", Value::from(function.name().clone())),
		("namespace", Value::from(function.namespace().clone())),
		("parameters", Value::from(parameters)),
		("result", Value::from(function.result().to_string())),
		("version", Value::from(u64::from(function.version().get()))),
	]))
}

fn function_info_key(function: &SchemaFunction) -> crate::values::Text {
	let parameters = function
		.parameters()
		.map(|parameter| parameter.value_type().to_string())
		.collect::<Vec<_>>()
		.join(",");
	crate::values::Text::from(format!(
		"{}::{}({parameters})",
		function.namespace(),
		function.name()
	))
}

fn info_parameter(transaction: &mut Transaction, name: &str) -> FerrosGrynnResult<Value> {
	transaction.parameter(name)?.map(|parameter| parameter_info(&parameter)).ok_or_else(|| {
		FerrosGrynnError::invalid_argument(format!("parameter '${name}' does not exist"))
	})
}

fn parameter_info(parameter: &SchemaParameter) -> Value {
	Value::from(Object::from_iter([
		("comment", parameter.comment().cloned().map_or(Value::None, Value::from)),
		("id", parameter.id().map_or(Value::None, |id| Value::from(u64::from(id.get())))),
		("name", Value::from(parameter.name().clone())),
		("value", parameter.value().clone()),
		("version", Value::from(u64::from(parameter.version().get()))),
	]))
}

/// Projects the current class definition and its source-owned Events.
fn info_class(
	transaction: &mut Transaction,
	name: &str,
	control: &crate::gql::ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let schema = require_class(transaction, name)?;
	let graphs = transaction.graphs()?;
	let fields = schema
		.fields()
		.map(|field| (field.name().clone(), field_definition(name, field)))
		.collect::<Object>();
	let mut indexes = schema
		.indexes()
		.map(|index| (index.name().clone(), index_info(name, index, &graphs, None)))
		.collect::<Object>();
	indexes.insert("class", system_index_info(name, "class"));
	indexes.insert("primary", system_index_info(name, "primary"));
	let class = schema
		.id()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("published class has no identity"))?;
	let events = event::info(transaction, crate::datastore::EventTarget::Class(class), control)?;
	Ok(Value::from(Object::from_iter([
		("classes", Value::from(Object::new())),
		("definition", class_definition(&schema)),
		("events", Value::from(events)),
		("fields", Value::from(fields)),
		("indexes", Value::from(indexes)),
		("lives", Value::from(Object::new())),
	])))
}

/// Projects logical graph definitions without exposing physical binding
/// addresses.
fn graph_index_info(index: &crate::datastore::GraphIndex) -> FerrosGrynnResult<Value> {
	use crate::datastore::{
		GraphIndexCost,
		GraphIndexDefinition,
		NeighbourhoodAlgorithm,
		PathIndexAlgorithm,
		ReachabilityAlgorithm,
		TensorCacheFormat,
	};
	let (kind, algorithm, parameter, radius, cost, patterns) = match index.definition() {
		GraphIndexDefinition::Tensor {
			scope,
			format,
		} => (
			"TENSOR",
			match format {
				TensorCacheFormat::Auto => "AUTO",
				TensorCacheFormat::Csr => "CSR",
				TensorCacheFormat::Dense => "DENSE",
			},
			u64::from(*scope == crate::graph::tensor::TensorScope::Rooted),
			0,
			GraphIndexCost::Hop,
			&[][..],
		),
		GraphIndexDefinition::Reachability {
			algorithm,
		} => {
			let (name, parameter) = match algorithm {
				ReachabilityAlgorithm::Auto => ("AUTO", 0),
				ReachabilityAlgorithm::TreeCover => ("TREE_COVER", 0),
				ReachabilityAlgorithm::Grail(value) => ("GRAIL", u64::from(*value)),
				ReachabilityAlgorithm::Ferrari(value) => ("FERRARI", u64::from(*value)),
				ReachabilityAlgorithm::Bfl(value) => ("BFL", u64::from(*value)),
				ReachabilityAlgorithm::ChainCover => ("CHAIN_COVER", 0),
				ReachabilityAlgorithm::OnDemand => ("ON_DEMAND", 0),
			};
			("REACHABILITY", name, parameter, 0, GraphIndexCost::Hop, &[][..])
		}
		GraphIndexDefinition::Neighbourhood {
			radius,
			algorithm,
		} => {
			let (name, parameter) = match algorithm {
				NeighbourhoodAlgorithm::Auto => ("AUTO", 0),
				NeighbourhoodAlgorithm::Exact => ("EXACT", 0),
				NeighbourhoodAlgorithm::Sketch(value) => ("SKETCH", u64::from(*value)),
				NeighbourhoodAlgorithm::Landmark(value) => ("LANDMARK", u64::from(*value)),
				NeighbourhoodAlgorithm::OnDemand => ("ON_DEMAND", 0),
			};
			("NEIGHBOURHOOD", name, parameter, u64::from(*radius), GraphIndexCost::Hop, &[][..])
		}
		GraphIndexDefinition::Path {
			algorithm,
			cost,
			patterns,
		} => {
			let (name, parameter) = match algorithm {
				PathIndexAlgorithm::LandmarkSssp(value) => ("LANDMARK_SSSP", u64::from(*value)),
				PathIndexAlgorithm::PatternCache => ("PATTERN_CACHE", 0),
				PathIndexAlgorithm::OnDemand => ("ON_DEMAND", 0),
			};
			("PATH", name, parameter, 0, *cost, patterns.as_slice())
		}
	};
	let cost = match cost {
		GraphIndexCost::Hop => Value::from("HOP"),
		GraphIndexCost::Weighted => Value::from("WEIGHTED"),
		GraphIndexCost::Function {
			function,
			version,
		} => Value::from(Object::from_iter([
			("function_id", Value::from(u64::from(function.get()))),
			("version", Value::from(u64::from(version.get()))),
		])),
	};
	Ok(Value::from(Object::from_iter([
		("name", Value::from(index.name().clone())),
		("id", index.id().map_or(Value::None, |id| Value::from(u64::from(id.get())))),
		("version", Value::from(u64::from(index.version().get()))),
		("graph_id", index.graph().map_or(Value::None, |id| Value::from(id.get()))),
		("kind", Value::from(kind)),
		("algorithm", Value::from(algorithm)),
		("parameter", Value::from(parameter)),
		("radius", Value::from(radius)),
		(
			"mode",
			Value::from(match index.mode() {
				crate::graph::TraversalSemantics::Node => "NODE",
				crate::graph::TraversalSemantics::Atom => "ATOM",
			}),
		),
		(
			"direction",
			Value::from(match index.direction() {
				crate::graph::TraversalDirection::Forward => "OUT",
				crate::graph::TraversalDirection::Backward => "IN",
				crate::graph::TraversalDirection::Both => "BOTH",
			}),
		),
		("edges", Value::from(index.edge_labels().cloned().map(Value::from).collect::<Vec<_>>())),
		("cost", cost),
		("patterns", Value::from(patterns.iter().cloned().map(Value::from).collect::<Vec<_>>())),
		(
			"dependencies",
			Value::from(
				index
					.dependencies()
					.map(|(function, version)| {
						Value::from(Object::from_iter([
							("function_id", Value::from(u64::from(function.get()))),
							("version", Value::from(u64::from(version.get()))),
						]))
					})
					.collect::<Vec<_>>(),
			),
		),
		("comment", index.comment().cloned().map_or(Value::None, Value::from)),
		("max_bytes", index.max_bytes().map_or(Value::None, Value::from)),
		("fingerprint", Value::from(index.fingerprint()?)),
		("state", Value::from(index_state_name(index.state()))),
		("system", Value::Bool(false)),
		("query_visible", Value::Bool(index.state() == ClassIndexState::Ready)),
		("build_revision", Value::from(index.build_revision())),
		(
			"maintenance",
			Value::from(match index.maintenance_mode() {
				IndexMaintenanceMode::Immediate => "immediate",
				IndexMaintenanceMode::Deferred => "deferred",
			}),
		),
	])))
}

fn info_index(transaction: &mut Transaction, class: &str, name: &str) -> FerrosGrynnResult<Value> {
	let schema = require_class(transaction, class)?;
	if matches!(name, "class" | "primary") {
		return Ok(system_index_info(class, name));
	}
	let graphs = transaction.graphs()?;
	let index = schema.indexes().find(|index| index.name().as_str() == name).ok_or_else(|| {
		FerrosGrynnError::invalid_argument(format!("index '{class}.{name}' does not exist"))
	})?;
	let active_job = match (schema.id(), index.id()) {
		(Some(class), Some(id)) => transaction.active_maintenance_job(MaintenanceTarget::Index {
			class,
			index: id,
			version: index.version(),
		}),
		_ => None,
	};
	Ok(index_info(class, index, &graphs, active_job.as_ref()))
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

fn index_info(
	class: &str,
	index: &ClassIndex,
	graphs: &[GraphSchema],
	active_job: Option<&MaintenanceJobInfo>,
) -> Value {
	let state = index_state_name(index.state());
	let maintenance = match index.maintenance_mode() {
		IndexMaintenanceMode::Immediate => "immediate",
		IndexMaintenanceMode::Deferred => "deferred",
	};
	let fallback = match index.state() {
		ClassIndexState::Ready => Value::None,
		ClassIndexState::Stale => Value::from("scan:index_stale"),
		ClassIndexState::PreparedForRemoval => Value::from("scan:index_prepared_for_removal"),
	};
	let building = active_job.map_or_else(
		|| Value::from(Object::from_iter([("status", Value::from(state))])),
		|job| {
			Value::from(Object::from_iter([
				("status", Value::from(state)),
				("job", job.value()),
				("source_revision", Value::from(job.source_revision())),
			]))
		},
	);
	Value::from(Object::from_iter([
		("active_job", active_job.map_or(Value::None, MaintenanceJobInfo::value)),
		("build_revision", Value::from(index.build_revision())),
		("building", building),
		("definition", index_definition(class, index, graphs)),
		("fallback_reason", fallback),
		("maintenance", Value::from(maintenance)),
		("query_visible", Value::Bool(index.state() == ClassIndexState::Ready)),
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
		ClassIndexKind::Geometry => {
			let field = index
				.fields()
				.next()
				.map_or("<missing>".to_owned(), |field| quote_name(field.as_str()));
			let kind = index
				.geometry_options()
				.and_then(|options| options.geometry_type())
				.map_or(String::new(), |kind| format!(" TYPE {}", kind.as_str()));
			format!("FIELDS {field} GEOMETRY{kind}")
		}
		ClassIndexKind::Standard | ClassIndexKind::Unique => {
			let fields = index.fields().map(|field| quote_name(field.as_str())).collect::<Vec<_>>();
			let unique = if index.kind() == ClassIndexKind::Unique {
				" UNIQUE"
			} else {
				""
			};
			format!("FIELDS {}{unique}", fields.join(", "))
		}
		ClassIndexKind::Fulltext => {
			let field = index
				.fields()
				.next()
				.map_or("<missing>".to_owned(), |field| quote_name(field.as_str()));
			let options = index.fulltext_options();
			let analyzer = options.map_or(0, |options| options.analyzer().get());
			let highlights = options.is_some_and(|options| options.highlights());
			format!(
				"FIELDS {field} FULLTEXT ANALYZER #{analyzer}{}",
				if highlights {
					" HIGHLIGHTS"
				} else {
					""
				}
			)
		}
		ClassIndexKind::Hnsw => "FIELDS <vector> HNSW".to_owned(),
		ClassIndexKind::DiskAnn => "FIELDS <vector> DISKANN".to_owned(),
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
