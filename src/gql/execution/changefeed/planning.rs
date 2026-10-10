//! Read planning observes scalar bindings without executing UDFs or reading
//! images.

use std::hash::{Hash, Hasher};

use super::operands::Operands;
use crate::{
	changefeed::{
		ChangefeedCursor,
		ChangefeedId,
		ChangefeedReadMode,
		ChangefeedSelector,
		ChangefeedSince,
		ChangefeedSource,
		ChangefeedState,
	},
	gql::{
		ExecutionControl,
		execution::cursor::{AtomFieldRead, QueryRead},
		ir::{LogicalChangefeedTarget, LogicalSchemaStatement, TypedExpr, TypedExprKind},
		optimizer::{CanonicalHasher, PLAN_ENCODING_VERSION, PhysicalChangefeedPlan, PhysicalPlan},
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Object, Type, Value},
};

/// Counts a conservative canonical buffer ceiling before the allocating hash
/// pass.
#[derive(Default)]
pub(in crate::gql::execution) struct HashBytes(usize);

impl HashBytes {
	/// Returns the conservative allocation ceiling for the canonical writer.
	pub(in crate::gql::execution) const fn bytes(&self) -> usize {
		self.0
	}
}

impl Hasher for HashBytes {
	fn finish(&self) -> u64 {
		0
	}

	fn write(&mut self, bytes: &[u8]) {
		self.0 = self.0.saturating_add(bytes.len()).saturating_add(16);
	}
}

/// Borrows only direct scalar bindings; calls and complex expressions stay
/// deferred.
fn scalar<'a>(expression: &'a TypedExpr, slots: &'a [Value]) -> Option<&'a Value> {
	match &expression.kind {
		TypedExprKind::Constant(value) => Some(value),
		TypedExprKind::Slot(slot) => slots.get(*slot as usize),
		_ => None,
	}
}

/// Hashes bounded analyzer-owned spelling without caller offsets or typed IR.
fn hash_spelling(
	spelling: &str,
	writer: &mut impl Hasher,
	admission: Option<&Operands<'_>>,
) -> FerrosGrynnResult<()> {
	if let Some(admission) = admission {
		admission.work(spelling.len() as u64)?;
	}
	spelling.hash(writer);
	Ok(())
}

/// Includes complete operand spelling and runtime locals, without images or
/// Debug.
fn hash_request(
	statement: &LogicalSchemaStatement,
	slots: &[Value],
	writer: &mut impl Hasher,
	admission: Option<&Operands<'_>>,
) -> FerrosGrynnResult<()> {
	match statement {
		LogicalSchemaStatement::InfoChangefeed {
			spelling,
			..
		} => {
			0_u8.hash(writer);
			hash_spelling(spelling, writer, admission)?;
		}
		LogicalSchemaStatement::ShowChanges {
			target,
			full,
			limit,
			spelling,
			..
		} => {
			1_u8.hash(writer);
			match target {
				LogicalChangefeedTarget::Named(selector) => {
					0_u8.hash(writer);
					match selector {
						ChangefeedSelector::ClassName(name) => {
							if let Some(admission) = admission {
								admission.work(name.len() as u64)?;
							}
							0_u8.hash(writer);
							name.hash(writer);
						}
						ChangefeedSelector::GraphName(name) => {
							if let Some(admission) = admission {
								admission.work(name.len() as u64)?;
							}
							1_u8.hash(writer);
							name.hash(writer);
						}
						ChangefeedSelector::Id(id) => {
							2_u8.hash(writer);
							id.hash(writer);
						}
						ChangefeedSelector::Source(source) => {
							3_u8.hash(writer);
							source.hash(writer);
						}
					}
				}
				LogicalChangefeedTarget::Graph(_) => {
					1_u8.hash(writer);
				}
				LogicalChangefeedTarget::Id(_) => {
					2_u8.hash(writer);
				}
			}
			hash_spelling(spelling, writer, admission)?;
			full.hash(writer);
			limit.is_some().hash(writer);
		}
		_ => {}
	}
	slots.hash(writer);
	Ok(())
}

/// Admits recursively hashed type nodes and field names before hashing.
fn admit_type(value: &Type, operands: &Operands<'_>, depth: usize) -> FerrosGrynnResult<()> {
	operands.work(1)?;
	if depth > 64 {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"changefeed function type depth",
			depth as u64,
			64,
		));
	}
	match value {
		Type::Array(inner) | Type::Set(inner) | Type::Range(inner) | Type::Option(inner) => {
			admit_type(inner, operands, depth + 1)?;
		}
		Type::Union(types) => {
			for inner in types {
				admit_type(inner, operands, depth + 1)?;
			}
		}
		Type::RecordShape(fields) => {
			for (name, inner) in fields {
				operands.work(name.len() as u64)?;
				admit_type(inner, operands, depth + 1)?;
			}
		}
		Type::Closure {
			parameters,
			result,
		} => {
			for parameter in parameters {
				admit_type(parameter, operands, depth + 1)?;
			}
			admit_type(result, operands, depth + 1)?;
		}
		_ => {}
	}
	Ok(())
}

/// Includes private versions and bodies, even when rollback reuses a version.
/// Comments do not affect execution and are excluded.
pub(in crate::gql::execution) fn hash_function(
	function: &crate::datastore::SchemaFunction,
	writer: &mut impl Hasher,
	admission: Option<&Operands<'_>>,
) -> FerrosGrynnResult<()> {
	if let Some(operands) = admission {
		operands.work(1)?;
		for text in [function.namespace(), function.name(), function.body()] {
			operands.work(text.len() as u64)?;
		}
		admit_type(function.result(), operands, 0)?;
		for parameter in function.parameters() {
			operands.work(parameter.name().len() as u64 + 1)?;
			admit_type(parameter.value_type(), operands, 0)?;
		}
	}
	function.id().hash(writer);
	function.version().hash(writer);
	function.namespace().hash(writer);
	function.name().hash(writer);
	function.parameters().len().hash(writer);
	for parameter in function.parameters() {
		parameter.name().hash(writer);
		parameter.value_type().hash(writer);
	}
	function.result().hash(writer);
	std::mem::discriminant(&function.effect()).hash(writer);
	function.is_cost().hash(writer);
	function.max_depth().hash(writer);
	function.body().hash(writer);
	Ok(())
}

/// Parses the same public feed identity accepted by SHOW and INFO execution.
fn identity(value: &Value) -> FerrosGrynnResult<ChangefeedId> {
	match value {
		Value::Uuid(value) => ChangefeedId::from_uuid(*value),
		Value::String(value) => value.as_str().parse(),
		_ => Err(FerrosGrynnError::invalid_argument("CHANGEFEED requires a UUID or UUID string")),
	}
}

/// Resolves names through historical catalog lookup after bounded scalar
/// admission.
fn selector(
	target: &LogicalChangefeedTarget,
	slots: &[Value],
) -> FerrosGrynnResult<Option<ChangefeedSelector>> {
	Ok(match target {
		LogicalChangefeedTarget::Named(selector) => Some(selector.clone()),
		LogicalChangefeedTarget::Id(expression) => {
			scalar(expression, slots).map(identity).transpose()?.map(ChangefeedSelector::Id)
		}
		LogicalChangefeedTarget::Graph(expression) => match scalar(expression, slots) {
			Some(Value::GraphRef(graph)) => {
				Some(ChangefeedSelector::Source(ChangefeedSource::Graph(graph.id())))
			}
			Some(Value::String(name)) => Some(ChangefeedSelector::GraphName(name.clone())),
			Some(_) => {
				return Err(FerrosGrynnError::invalid_argument(
					"GRAPH changefeed requires a graph reference or name",
				));
			}
			None => None,
		},
	})
}

/// Builds the logical history operator after admitting all canonical hash
/// inputs.
///
/// # Errors
///
/// Returns bounded hashing, source lookup or direct scalar validation errors.
pub(in crate::gql::execution) fn plan<Read: QueryRead>(
	read: &Read,
	statement: &LogicalSchemaStatement,
	slots: &[Value],
	control: &ExecutionControl,
) -> FerrosGrynnResult<Option<PhysicalPlan>> {
	if !matches!(
		statement,
		LogicalSchemaStatement::ShowChanges { .. } | LogicalSchemaStatement::InfoChangefeed { .. }
	) {
		return Ok(None);
	}
	let operands = Operands::new(control);
	for value in slots {
		operands.work(1)?;
		operands.admit_value(value)?;
	}
	let mut count = HashBytes::default();
	hash_request(statement, slots, &mut count, Some(&operands))?;
	read.visit_changefeed_functions(&mut |function| {
		hash_function(function, &mut count, Some(&operands))
	})?;
	operands.work(count.0 as u64)?;
	operands.admit(count.0.saturating_mul(2).saturating_add(32 * 1024))?;
	let mut writer = CanonicalHasher::versioned(PLAN_ENCODING_VERSION, b"changefeed-request");
	hash_request(statement, slots, &mut writer, None)?;
	let mut functions = CanonicalHasher::versioned(PLAN_ENCODING_VERSION, b"changefeed-functions");
	read.visit_changefeed_functions(&mut |function| hash_function(function, &mut functions, None))?;
	let (database, snapshot) = read.changefeed_context()?;
	let function_fingerprint = functions.finish();
	let (selected, mode, since, limit) = match statement {
		LogicalSchemaStatement::InfoChangefeed {
			id,
			..
		} => (
			scalar(id, slots).map(identity).transpose()?.map(ChangefeedSelector::Id),
			None,
			None,
			Some(1),
		),
		LogicalSchemaStatement::ShowChanges {
			target,
			since,
			full,
			limit,
			..
		} => {
			let selected = selector(target, slots)?;
			let since = if selected.is_some() {
				scalar(since, slots)
					.map(|value| match value {
						Value::Number(stamp) => u64::try_from(*stamp)
							.map(|stamp| ChangefeedSince::Versionstamp(stamp.into())),
						Value::DateTime(time) => Ok(ChangefeedSince::Timestamp(time.clone())),
						Value::Bytes(bytes) => {
							ChangefeedCursor::decode(bytes.as_slice()).map(ChangefeedSince::Cursor)
						}
						_ => Err(FerrosGrynnError::invalid_argument(
							"SINCE requires an exact stamp, timestamp or cursor",
						)),
					})
					.transpose()?
			} else {
				None
			};
			let limit = if since.is_some() {
				match limit {
					None => Some(u64::MAX),
					Some(expression) => scalar(expression, slots)
						.map(|value| match value {
							Value::Number(limit) => u64::try_from(*limit),
							_ => Err(FerrosGrynnError::invalid_argument(
								"changefeed LIMIT requires a nonnegative integer",
							)),
						})
						.transpose()?,
				}
			} else {
				None
			};
			(
				selected,
				Some(if *full {
					ChangefeedReadMode::Full
				} else {
					ChangefeedReadMode::Net
				}),
				since,
				limit,
			)
		}
		_ => return Ok(None),
	};
	let ready = selected.is_some() && (mode.is_none() || since.is_some() && limit.is_some());
	let binding = if let Some(selector) = &selected {
		match read.changefeed_binding(selector) {
			Ok(binding) => binding,
			Err(error) if ready => return Err(error),
			Err(_deferred_lookup) => None,
		}
	} else {
		None
	};
	if let Some(ChangefeedSince::Cursor(cursor)) = &since
		&& ready
		&& (mode != Some(cursor.mode())
			|| database != cursor.database_id()
			|| cursor.versionstamp() > snapshot
			|| selected.as_ref().is_some_and(
				|selector| matches!(selector, ChangefeedSelector::Id(id) if *id != cursor.source_id()),
			)
			|| binding.is_some_and(|binding| {
				binding.database != cursor.database_id()
					|| binding.id != cursor.source_id()
					|| cursor.versionstamp() > binding.snapshot
			})) {
		return Err(FerrosGrynnError::invalid_argument(
			"changefeed cursor origin, source, mode or snapshot mismatch",
		));
	}
	Ok(Some(PhysicalPlan::Changefeed(PhysicalChangefeedPlan {
		request_hash: writer.finish(),
		database,
		snapshot,
		function_fingerprint,
		binding,
		mode,
		since,
		limit,
		dynamic_source: selected.is_none(),
	})))
}

/// Projects user-visible logical metadata without images or physical
/// identities.
pub(in crate::gql::execution) fn project_plan(node: &mut Object, plan: &PhysicalChangefeedPlan) {
	node.insert("database", plan.database);
	node.insert("snapshot_through", plan.snapshot.get());
	node.insert("function_fingerprint", plan.function_fingerprint);
	node.insert("read_mode", match plan.mode {
		Some(ChangefeedReadMode::Full) => "FULL",
		Some(ChangefeedReadMode::Net) => "NET",
		None => "INFO",
	});
	node.insert(
		"binding",
		if plan.dynamic_source {
			"runtime"
		} else if plan.binding.is_some() {
			"snapshot"
		} else {
			"retired_or_unpublished"
		},
	);
	node.insert("whole_commit_limit", plan.limit.map_or(Value::None, Value::from));
	node.insert("since", match &plan.since {
		Some(ChangefeedSince::Versionstamp(stamp)) => Value::from(stamp.get()),
		Some(ChangefeedSince::Timestamp(time)) => Value::from(time.clone()),
		Some(ChangefeedSince::Cursor(cursor)) => Value::from(cursor.versionstamp().get()),
		None => Value::None,
	});
	node.insert("resume", matches!(plan.since, Some(ChangefeedSince::Cursor(_))));
	if let Some(binding) = plan.binding {
		node.insert("source", binding.id.as_uuid());
		node.insert("database", binding.database);
		node.insert("source_kind", match binding.source {
			ChangefeedSource::Class(_) => "CLASS",
			ChangefeedSource::Graph(_) => "GRAPH",
		});
		node.insert("capture_revision", u64::from(binding.revision.get()));
		node.insert("capture_full", binding.configuration.full());
		node.insert("include_original", binding.configuration.include_original());
		node.insert("capture_state", match binding.state {
			ChangefeedState::Active => "ACTIVE",
			ChangefeedState::Stopped => "STOPPED",
			ChangefeedState::Retired => "RETIRED",
		});
		node.insert("snapshot_through", binding.snapshot.get());
		node.insert("last_versionstamp", binding.last.get());
		node.insert("purged_through", binding.purged_through.get());
	}
}
