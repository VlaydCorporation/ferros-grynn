//! GrynQL atomic-import validation, normalization, and execution.

use std::{
	collections::BTreeSet,
	time::{Duration, Instant},
};

use super::{ExecutionControl, QueryResult, QueryStatementKind};
use crate::{
	datastore::{
		Datastore,
		ImportConstraintMode,
		ImportIndexMode,
		ImportOptions,
		ImportReport,
		ImportViolationPolicy,
		IsolationLevel,
		TransactionAccess,
		TransactionOptions,
	},
	gql_status::{
		ExecutionPhase,
		FerrosGrynnError,
		FerrosGrynnResult,
		Location,
		Outcome,
		TextSpan,
	},
	syn::{
		DmlData,
		DmlReturn,
		Identifier,
		ImportGraphRef,
		ImportOption,
		ImportOptionName,
		ImportOptionValue,
		ImportStatement,
		Program,
		Statement,
		StatementKind,
	},
	values::{Array, GraphRef, Object, Value},
};

/// Validates and executes one already parsed top-level import.
///
/// # Errors
///
/// Returns semantic preflight, callback item, validation, persistence, or
/// publication failures without exposing a partial root.
pub fn execute_import(
	store: &Datastore,
	source: &str,
	statement: &ImportStatement,
	variables: &Object,
	control: &ExecutionControl,
) -> FerrosGrynnResult<QueryResult> {
	control.check()?;
	let started = Instant::now();
	let options = import_options(&statement.options, statement.full_validation, source)?;
	let graph = resolve_graph(store, statement.graph.as_ref(), variables, source)?;
	let graph_identifier = graph.as_ref().map(|graph| Identifier {
		value: graph.name().as_str().into(),
		quoted: false,
		span: import_graph_span(statement.graph.as_ref()),
	});
	let program = normalize_program(statement, graph_identifier.as_ref(), source)?;
	let timeouts = program
		.statements
		.iter()
		.map(|statement| super::resolve_timeout(statement.timeout.as_ref(), variables))
		.collect::<FerrosGrynnResult<Vec<_>>>()?;

	let mut preflight = store.begin_with(TransactionOptions::new(
		IsolationLevel::Serializable,
		TransactionAccess::ReadOnly,
	));
	let (logical, expected_commit_ts) =
		super::execution::analyze_parsed_transaction(&mut preflight, source, &program, variables)?;
	drop(preflight);
	control.check()?;

	let result = store
		.import_at(options, Some(expected_commit_ts), graph.as_ref().map(GraphRef::id), |session| {
			super::execution::execute_import_program(
				session, source, &logical, variables, control, &timeouts,
			)
		})
		.map(report_value)
		.map(Outcome::success);
	Ok(QueryResult {
		kind: QueryStatementKind::Import,
		elapsed: started.elapsed(),
		result,
		continuation: None,
	})
}

/// Converts syntactic options into the validated datastore contract.
///
/// # Errors
///
/// Returns a located semantic error for duplicates, invalid values, unsupported
/// Stage 10 options, or invalid cross-option combinations.
fn import_options(
	options: &[ImportOption],
	full_validation: bool,
	source: &str,
) -> FerrosGrynnResult<ImportOptions> {
	let mut result = ImportOptions {
		full_validation,
		..ImportOptions::default()
	};
	let mut seen = BTreeSet::new();
	for option in options {
		if !seen.insert(option.name) {
			return Err(semantic(
				source,
				option.span,
				"Import option is specified more than once.",
			));
		}
		match option.name {
			ImportOptionName::Indexes => {
				let value = option_word(option, source)?;
				result.indexes = if value.eq_ignore_ascii_case("DEFERRED") {
					ImportIndexMode::Deferred
				} else if value.eq_ignore_ascii_case("MAINTAINED") {
					ImportIndexMode::Maintained
				} else {
					return Err(invalid_option_value(source, option));
				};
			}
			ImportOptionName::Constraints => {
				let value = option_word(option, source)?;
				result.constraints = if value.eq_ignore_ascii_case("DEFERRED") {
					ImportConstraintMode::Deferred
				} else if value.eq_ignore_ascii_case("IMMEDIATE") {
					ImportConstraintMode::Immediate
				} else {
					return Err(invalid_option_value(source, option));
				};
			}
			ImportOptionName::OnViolation => {
				let value = option_word(option, source)?;
				result.on_violation = if value.eq_ignore_ascii_case("ABORT") {
					ImportViolationPolicy::Abort
				} else if value.eq_ignore_ascii_case("SKIP") {
					ImportViolationPolicy::Skip
				} else if value.eq_ignore_ascii_case("QUARANTINE") {
					return Err(unsupported(source, option.span, "IMPORT QUARANTINE"));
				} else {
					return Err(invalid_option_value(source, option));
				};
			}
			ImportOptionName::RejectLimit => {
				result.reject_limit = Some(option_usize(option, source)?);
			}
			ImportOptionName::SegmentSize => {
				result.segment_size = option_usize(option, source)?;
			}
			ImportOptionName::Events => {
				let value = option_word(option, source)?;
				if value.eq_ignore_ascii_case("ON") {
					return Err(unsupported(source, option.span, "IMPORT EVENTS=ON"));
				}
				if !value.eq_ignore_ascii_case("OFF") {
					return Err(invalid_option_value(source, option));
				}
			}
			ImportOptionName::Changefeed => {
				let value = option_word(option, source)?;
				if value.eq_ignore_ascii_case("ON") {
					return Err(unsupported(source, option.span, "IMPORT CHANGEFEED=ON"));
				}
				if !value.eq_ignore_ascii_case("OFF") {
					return Err(invalid_option_value(source, option));
				}
			}
			ImportOptionName::FillFactor => {
				return Err(unsupported(source, option.span, "IMPORT FILL_FACTOR"));
			}
			ImportOptionName::SortMem => {
				return Err(unsupported(source, option.span, "IMPORT SORT_MEM"));
			}
			ImportOptionName::Parallelism => {
				return Err(unsupported(source, option.span, "IMPORT PARALLELISM"));
			}
		}
	}
	result.validate()
}

/// Extracts one unquoted word-valued option.
///
/// # Errors
///
/// Returns a located semantic error for a non-word or quoted value.
fn option_word<'a>(option: &'a ImportOption, source: &str) -> FerrosGrynnResult<&'a str> {
	let ImportOptionValue::Word(value) = &option.value else {
		return Err(invalid_option_value(source, option));
	};
	if value.quoted {
		return Err(invalid_option_value(source, option));
	}
	Ok(value.value.as_ref())
}

/// Parses one non-negative machine-sized numeric option.
///
/// # Errors
///
/// Returns a located semantic error for a non-number or an out-of-range value.
fn option_usize(option: &ImportOption, source: &str) -> FerrosGrynnResult<usize> {
	let ImportOptionValue::Number {
		value,
		..
	} = &option.value
	else {
		return Err(invalid_option_value(source, option));
	};
	value.replace('_', "").parse::<usize>().map_err(|_error| invalid_option_value(source, option))
}

/// Resolves the optional import graph before staging is opened.
///
/// # Errors
///
/// Returns a located semantic error for a missing parameter, wrong parameter
/// type, or unknown graph.
fn resolve_graph(
	store: &Datastore,
	target: Option<&ImportGraphRef>,
	variables: &Object,
	source: &str,
) -> FerrosGrynnResult<Option<GraphRef>> {
	let Some(target) = target else {
		return Ok(None);
	};
	let (name, span) = match target {
		ImportGraphRef::Name(name) => (name.value.as_ref(), name.span),
		ImportGraphRef::Parameter(parameter) => {
			let value = variables.get(&parameter.value).ok_or_else(|| {
				semantic(
					source,
					parameter.span,
					&format!("Unknown parameter `${}`.", parameter.value),
				)
			})?;
			let graph = value.as_graph_ref().ok_or_else(|| {
				semantic(source, parameter.span, "Import graph parameter must contain GraphRef.")
			})?;
			(graph.name().as_str(), parameter.span)
		}
	};
	store
		.schema_catalog()
		.named_graph(name)
		.map(|schema| Some(schema.reference().clone()))
		.ok_or_else(|| located(source, span, FerrosGrynnError::graph_does_not_exist(name)))
}

/// Clones and normalizes every item into the existing transaction executor.
///
/// # Errors
///
/// Returns a semantic error when an item violates import scope or result rules.
fn normalize_program(
	statement: &ImportStatement,
	default_graph: Option<&Identifier>,
	source: &str,
) -> FerrosGrynnResult<Program> {
	let mut statements = statement.statements.clone();
	for statement in &mut statements {
		normalize_statement(statement, default_graph, source)?;
	}
	Ok(Program {
		statements,
	})
}

/// Injects the default graph and `RETURN NONE` into one allowed DML statement.
///
/// # Errors
///
/// Returns a semantic error for unsupported statements, scope mismatch, or a
/// result-producing DML return clause.
fn normalize_statement(
	statement: &mut Statement,
	default_graph: Option<&Identifier>,
	source: &str,
) -> FerrosGrynnResult<()> {
	let statement_span = statement.span;
	match &mut statement.kind {
		StatementKind::Create(statement) => {
			normalize_return(&mut statement.returning, source, statement_span)?;
		}
		StatementKind::Update(statement) => {
			let graph_required =
				statement.label.is_some() || matches!(statement.data, Some(DmlData::Reset(_)));
			normalize_graph(
				&mut statement.graph,
				default_graph,
				graph_required,
				source,
				statement.targets[0].class().span,
			)?;
			normalize_return(&mut statement.returning, source, statement_span)?;
		}
		StatementKind::Upsert(statement) => {
			normalize_return(&mut statement.returning, source, statement_span)?;
		}
		StatementKind::Delete(statement) => {
			normalize_return(&mut statement.returning, source, statement_span)?;
		}
		StatementKind::Attach(statement) => normalize_graph(
			&mut statement.graph,
			default_graph,
			true,
			source,
			statement.record.class().span,
		)?,
		StatementKind::Detach(statement) => normalize_graph(
			&mut statement.graph,
			default_graph,
			true,
			source,
			statement.record.class().span,
		)?,
		StatementKind::Relate(statement) => {
			normalize_graph(
				&mut statement.graph,
				default_graph,
				true,
				source,
				statement.source.class().span,
			)?;
			normalize_return(&mut statement.returning, source, statement_span)?;
		}
		StatementKind::If(statement) => {
			for (_condition, block) in &mut statement.branches {
				normalize_block(block, default_graph, source)?;
			}
			if let Some(block) = &mut statement.else_block {
				normalize_block(block, default_graph, source)?;
			}
		}
		StatementKind::For(statement) => {
			normalize_block(&mut statement.body, default_graph, source)?;
		}
		StatementKind::Try(statement) => {
			normalize_block(&mut statement.body, default_graph, source)?;
			if let Some(catch) = &mut statement.catch {
				normalize_block(&mut catch.body, default_graph, source)?;
			}
		}
		StatementKind::Throw(_) | StatementKind::Break | StatementKind::Continue => {}
		_ => return Err(unsupported(source, statement.span, "statement inside BEGIN IMPORT")),
	}
	Ok(())
}

fn normalize_block(
	block: &mut crate::syn::Block,
	default_graph: Option<&Identifier>,
	source: &str,
) -> FerrosGrynnResult<()> {
	for statement in &mut block.statements {
		normalize_statement(statement, default_graph, source)?;
	}
	Ok(())
}

/// Validates or injects one physical graph scope.
///
/// # Errors
///
/// Returns a semantic error for a conflicting or missing required scope.
fn normalize_graph(
	graph: &mut Option<Identifier>,
	default_graph: Option<&Identifier>,
	required: bool,
	source: &str,
	span: TextSpan,
) -> FerrosGrynnResult<()> {
	if let (Some(explicit), Some(default)) = (graph.as_ref(), default_graph)
		&& explicit.value != default.value
	{
		return Err(semantic(
			source,
			explicit.span,
			"Import item addresses a graph different from INTO GRAPH.",
		));
	}
	if graph.is_none() && required {
		*graph = Some(default_graph.cloned().ok_or_else(|| {
			semantic(source, span, "Graph-local import item requires an explicit graph scope.")
		})?);
	}
	Ok(())
}

/// Converts an omitted DML return into `RETURN NONE`.
///
/// # Errors
///
/// Returns a semantic error for any result-producing return form.
fn normalize_return(
	returning: &mut Option<DmlReturn>,
	source: &str,
	span: TextSpan,
) -> FerrosGrynnResult<()> {
	match returning {
		None => {
			*returning = Some(DmlReturn::None);
			Ok(())
		}
		Some(DmlReturn::None) => Ok(()),
		Some(_) => {
			Err(semantic(source, span, "BEGIN IMPORT does not allow result-producing DML RETURN."))
		}
	}
}

/// Converts the structured Rust report into the GrynQL value envelope.
fn report_value(report: ImportReport) -> Value {
	let violations = report
		.violations()
		.iter()
		.map(|(code, count)| (code.to_string(), Value::from(*count)))
		.collect::<Object>();
	let details = report
		.violation_details()
		.iter()
		.map(|violation| {
			Value::from(Object::from_iter([
				("item", Value::from(violation.item())),
				("code", Value::from(violation.code().to_string())),
				("description", Value::from(violation.description())),
			]))
		})
		.collect::<Array>();
	let built_indexes =
		report.built_indexes().iter().map(|index| Value::from(index.get())).collect::<Array>();
	let phase_timings = report
		.phase_timings()
		.iter()
		.map(|(phase, duration)| (phase.as_str(), duration_value(*duration)))
		.collect::<Object>();
	let index_timings = report
		.index_timings()
		.iter()
		.map(|(index, duration)| (index.get().to_string(), duration_value(*duration)))
		.collect::<Object>();
	Value::from(Object::from_iter([
		("processed", Value::from(report.processed_items())),
		("applied", Value::from(report.applied_items())),
		("skipped", Value::from(report.skipped_items())),
		("segments", Value::from(report.segments())),
		("violations", Value::from(violations)),
		("details", Value::from(details)),
		("built_indexes", Value::from(built_indexes)),
		("phase_timings_ns", Value::from(phase_timings)),
		("index_timings_ns", Value::from(index_timings)),
		(
			"indexes",
			Value::from(match report.index_mode() {
				ImportIndexMode::Deferred => "DEFERRED",
				ImportIndexMode::Maintained => "MAINTAINED",
			}),
		),
		(
			"constraints",
			Value::from(match report.constraint_mode() {
				ImportConstraintMode::Deferred => "DEFERRED",
				ImportConstraintMode::Immediate => "IMMEDIATE",
			}),
		),
		("full_validation", Value::from(report.full_validation())),
	]))
}

/// Converts a duration to saturated nanoseconds for the query report.
fn duration_value(duration: Duration) -> Value {
	Value::from(u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
}

/// Returns the source span of an optional import graph declaration.
fn import_graph_span(graph: Option<&ImportGraphRef>) -> TextSpan {
	match graph {
		Some(ImportGraphRef::Name(name) | ImportGraphRef::Parameter(name)) => name.span,
		None => TextSpan::new(0, 0),
	}
}

/// Builds a located invalid-option status.
fn invalid_option_value(source: &str, option: &ImportOption) -> FerrosGrynnError {
	semantic(source, option.span, "Invalid value for import option.")
}

/// Builds a semantic-analysis status at one source span.
fn semantic(source: &str, span: TextSpan, message: &str) -> FerrosGrynnError {
	located(
		source,
		span,
		FerrosGrynnError::invalid_argument(message).with_phase(ExecutionPhase::SemanticAnalysis),
	)
}

/// Builds a located unsupported-feature status.
fn unsupported(source: &str, span: TextSpan, feature: &str) -> FerrosGrynnError {
	located(
		source,
		span,
		FerrosGrynnError::query_feature_not_supported(feature)
			.with_phase(ExecutionPhase::SemanticAnalysis),
	)
}

/// Adds source location data when the supplied span can be resolved.
fn located(source: &str, span: TextSpan, error: FerrosGrynnError) -> FerrosGrynnError {
	Location::from_source(None, source, span)
		.map_or(error.clone(), |location| error.with_location(location))
}
