//! Snapshot-bound LIVE metadata planning; descriptors never execute user code.

use std::hash::{Hash, Hasher};

use super::{
	changefeed::{HashBytes, Operands, hash_function},
	cursor::{AtomFieldRead, QueryRead, Snapshot},
};
use crate::{
	changefeed::ChangefeedSource,
	gql::{
		ExecutionControl,
		ir::{
			LogicalLetValue,
			LogicalLiveCapture,
			LogicalSchemaStatement,
			LogicalStatement,
			TypedExpr,
			TypedExprKind,
		},
		optimizer::{
			CanonicalHasher,
			LiveOperation,
			PLAN_ENCODING_VERSION,
			PhysicalLivePlan,
			PhysicalLiveSource,
		},
		options::BarrierAccount,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveBindingId, LiveCursor, LiveId, LiveOptions, LiveSince, LiveSource},
	storage::live_descriptor::{self, LiveDescriptorRef},
	values::{Object, Value},
};

/// Selects both direct commands and LET registration without a second executor.
pub(super) fn command(
	statement: &LogicalStatement,
) -> Option<(&LogicalSchemaStatement, LiveOperation)> {
	let schema = match statement {
		LogicalStatement::Schema(schema) => schema,
		LogicalStatement::Let {
			value: LogicalLetValue::Live(schema),
			..
		} => schema,
		_ => return None,
	};
	let operation = match schema {
		LogicalSchemaStatement::RegisterLive(_) => LiveOperation::Register,
		LogicalSchemaStatement::DefineLiveBinding(_) => LiveOperation::DefineBinding,
		LogicalSchemaStatement::ReplaceLiveBinding {
			..
		} => LiveOperation::ReplaceBinding,
		LogicalSchemaStatement::InfoLive {
			..
		} => LiveOperation::Info,
		LogicalSchemaStatement::InfoLiveBinding {
			..
		} => LiveOperation::InfoBinding,
		LogicalSchemaStatement::RemoveLiveBinding {
			..
		} => LiveOperation::RemoveBinding,
		LogicalSchemaStatement::AckLive {
			..
		} => LiveOperation::Ack,
		LogicalSchemaStatement::CloseLive {
			..
		} => LiveOperation::Close,
		_ => return None,
	};
	Some((schema, operation))
}

/// Constants and lexical scalars are inspectable; every other operand is
/// deferred.
fn scalar<'a, Read: QueryRead>(
	read: &'a Read,
	snapshot: Snapshot,
	expression: &'a TypedExpr,
	slots: &'a [Value],
) -> FerrosGrynnResult<Option<&'a Value>> {
	Ok(match &expression.kind {
		TypedExprKind::Constant(value) => Some(value),
		TypedExprKind::Slot(slot) => Some(
			slots
				.get(*slot as usize)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE operand slot missing"))?,
		),
		TypedExprKind::LiveSlot(slot) => {
			let Some(Value::Uuid(uuid)) = slots.get(*slot as usize) else {
				return Err(FerrosGrynnError::invalid_argument("LIVE LET identity requires UUID"));
			};
			Some(read.live_binding_value(snapshot, LiveBindingId::from_uuid(*uuid)?)?)
		}
		_ => None,
	})
}

/// Admits both passes before allocating canonical hash buffers.
fn hash_bounded(
	admission: &Operands<'_>,
	domain: &[u8],
	mut write: impl FnMut(&mut dyn Hasher) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<u64> {
	let mut count = HashBytes::default();
	write(&mut count)?;
	admission.work(count.bytes() as u64)?;
	let _scratch = admission.temporary(count.bytes().saturating_mul(2).saturating_add(128))?;
	let mut writer = CanonicalHasher::versioned(PLAN_ENCODING_VERSION, domain);
	write(&mut writer)?;
	Ok(writer.finish())
}

/// Encodes a borrowed logical recipe inside an already admitted scratch
/// ceiling.
fn descriptor_hash(
	descriptor: LiveDescriptorRef<'_>,
	admission: &Operands<'_>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<u64> {
	let maximum = control.barrier_memory_available() / 3;
	let _scratch = admission.temporary(maximum)?;
	let bytes =
		live_descriptor::encode_ref_controlled(descriptor, maximum, &|work| admission.work(work))?;
	hash_bounded(admission, b"live-descriptor", |mut writer| {
		bytes.hash(&mut writer);
		Ok(())
	})
}

/// Ordinary captures hash their frozen values; reactive captures hash
/// identities.
fn capture_hash(
	captures: &[LogicalLiveCapture],
	slots: &[Value],
	admission: &Operands<'_>,
) -> FerrosGrynnResult<u64> {
	for capture in captures {
		admission.work(capture.name.len() as u64 + 1)?;
		let value = slots
			.get(capture.slot as usize)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE capture slot missing"))?;
		admission.admit_value(value)?;
		if capture.reactive {
			let Value::Uuid(uuid) = value else {
				return Err(FerrosGrynnError::invalid_argument("LIVE LET capture requires UUID"));
			};
			LiveBindingId::from_uuid(*uuid)?;
		}
	}
	hash_bounded(admission, b"live-captures", |mut writer| {
		for capture in captures {
			capture.name.hash(&mut writer);
			capture.reactive.hash(&mut writer);
			slots[capture.slot as usize].hash(&mut writer);
		}
		Ok(())
	})
}

/// Copies only admitted source identities; scope expressions remain
/// unevaluated.
fn sources(
	query: &crate::live::LiveQuery,
	admission: &Operands<'_>,
) -> FerrosGrynnResult<Vec<PhysicalLiveSource>> {
	let count = query
		.joins()
		.len()
		.checked_add(1)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE source count"))?;
	admission.work(count as u64)?;
	admission.admit(
		count
			.checked_mul(std::mem::size_of::<PhysicalLiveSource>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE source bytes"))?,
	)?;
	let mut output = Vec::new();
	output.try_reserve_exact(count).map_err(|error| {
		FerrosGrynnError::invalid_argument("LIVE source allocation failed")
			.with_technical_source(error)
	})?;
	for (ordinal, source) in query.sources().enumerate() {
		let (kind, class, graph, record, dynamic_scope) = match source {
			LiveSource::Class(class) => ("CLASS", Some(*class), None, None, false),
			LiveSource::Record(record) => ("RECORD", None, None, Some(record), false),
			LiveSource::Graph(graph) => ("GRAPH", None, Some(graph.id()), None, false),
			LiveSource::GraphClass {
				graph,
				class,
			} => ("GRAPH_CLASS", Some(*class), Some(graph.id()), None, false),
			LiveSource::GraphRecord {
				graph,
				record,
			} => ("GRAPH_RECORD", None, Some(graph.id()), Some(record), false),
			LiveSource::GraphExpr(_) => ("GRAPH", None, None, None, true),
			LiveSource::GraphClassExpr {
				class,
				..
			} => ("GRAPH_CLASS", Some(*class), None, None, true),
			LiveSource::GraphRecordExpr {
				record,
				..
			} => ("GRAPH_RECORD", None, None, Some(record), true),
		};
		if let Some(record) = record {
			admission.work(record.estimated_heap_bytes() as u64 + 1)?;
			admission.admit(record.estimated_heap_bytes())?;
		}
		output.push(PhysicalLiveSource {
			ordinal: u32::try_from(ordinal)
				.map_err(|_error| FerrosGrynnError::operation_overflow("LIVE source ordinal"))?,
			kind,
			class,
			graph,
			record: record.cloned(),
			dynamic_scope,
		});
	}
	Ok(output)
}

/// Preserves validated definition budgets and an exact starting operand.
fn options(
	plan: &mut PhysicalLivePlan,
	options: &LiveOptions,
	admission: &Operands<'_>,
) -> FerrosGrynnResult<()> {
	plan.queue_entries = Some(options.queue_entries() as u64);
	plan.queue_bytes = options.queue_bytes() as u64;
	plan.scratch_bytes = options.scratch_bytes() as u64;
	plan.work = options.work();
	plan.overflow = Some(options.overflow());
	let (kind, value) = match options.since() {
		LiveSince::Now => ("NOW", None),
		LiveSince::Versionstamp(stamp) => ("VERSIONSTAMP", Some(Value::from(stamp.get()))),
		LiveSince::Timestamp(time) => ("TIMESTAMP", Some(Value::DateTime(time.clone()))),
		LiveSince::Cursor(cursor) => ("CURSOR", Some(Value::Bytes(cursor.encode()))),
	};
	if let Some(value) = &value {
		admission.admit_value(value)?;
	}
	plan.since_kind = kind;
	plan.since_value = value;
	Ok(())
}

/// Builds one bounded logical operator, without attaching a queue or invoking
/// UDFs.
pub(super) fn plan<Read: QueryRead>(
	read: &Read,
	statement: &LogicalStatement,
	slots: &[Value],
	control: &ExecutionControl,
) -> FerrosGrynnResult<Option<(PhysicalLivePlan, BarrierAccount)>> {
	let Some((schema, operation)) = command(statement) else {
		return Ok(None);
	};
	control.check()?;
	let snapshot = read.snapshot()?;
	let (database, through) = read.changefeed_context()?;
	let admission = Operands::new(control);
	admission.admit(std::mem::size_of::<PhysicalLivePlan>() + 4096)?;
	let defaults = LiveOptions::default();
	let mut plan = PhysicalLivePlan {
		operation,
		request_hash: 0,
		descriptor_hash: None,
		function_fingerprint: 0,
		database,
		snapshot: through,
		identity: None,
		definition_version: None,
		sources: Vec::new(),
		output: None,
		since_kind: "NONE",
		since_value: None,
		queue_entries: None,
		queue_bytes: defaults.queue_bytes() as u64,
		scratch_bytes: control.barrier_memory_limit() as u64,
		work: defaults.work().min(control.max_loop_iterations),
		overflow: None,
		dynamic_operands: false,
	};
	let id = match schema {
		LogicalSchemaStatement::ReplaceLiveBinding {
			id,
			..
		}
		| LogicalSchemaStatement::InfoLiveBinding {
			id,
		}
		| LogicalSchemaStatement::RemoveLiveBinding {
			id,
		}
		| LogicalSchemaStatement::InfoLive {
			id,
		}
		| LogicalSchemaStatement::CloseLive {
			id,
		}
		| LogicalSchemaStatement::AckLive {
			id,
			..
		} => Some(id),
		_ => None,
	};
	if let Some(id) = id {
		match scalar(read, snapshot, id, slots)? {
			Some(Value::Uuid(uuid)) => {
				LiveId::from_uuid(*uuid)?;
				plan.identity = Some(*uuid);
			}
			Some(_) => {
				return Err(FerrosGrynnError::invalid_argument("LIVE identity requires UUID"));
			}
			None => plan.dynamic_operands = true,
		}
	}
	let mut captures = 0;
	let mut metadata = 0;
	// Metadata accounts outlive borrowed recipe hashing within each command
	// arm.
	if let Some(uuid) = plan.identity {
		if matches!(operation, LiveOperation::Info | LiveOperation::Ack | LiveOperation::Close) {
			let maximum = control.barrier_memory_available() / 4;
			let _metadata_scratch = admission.temporary(maximum)?;
			let info = read.live_info(snapshot, LiveId::from_uuid(uuid)?, maximum)?;
			plan.definition_version = Some(info.version().get());
			plan.descriptor_hash =
				Some(descriptor_hash(LiveDescriptorRef::Query(info.query()), &admission, control)?);
			plan.sources = sources(info.query(), &admission)?;
			plan.output = Some(info.query().output());
			options(&mut plan, info.options(), &admission)?;
			if let Some(first) = plan.sources.first_mut() {
				first.class = info.source_class().or(first.class);
				if let ChangefeedSource::Graph(graph) = info.capture_source() {
					first.graph = Some(graph);
				}
			}
			for (name, _) in info.record_sources() {
				admission.work(name.len() as u64 + 1)?;
			}
			metadata = hash_bounded(&admission, b"live-definition", |mut writer| {
				info.function_dependencies().hash(&mut writer);
				info.record_sources().hash(&mut writer);
				info.capture_source().hash(&mut writer);
				info.source_class().hash(&mut writer);
				info.options().block_timeout().as_nanos().hash(&mut writer);
				Ok(())
			})?;
		} else {
			let (info, _metadata_scratch) =
				read.live_binding_info(snapshot, LiveBindingId::from_uuid(uuid)?, control)?;
			plan.definition_version = Some(info.revision().definition().get());
			plan.scratch_bytes = info.options().scratch_bytes() as u64;
			plan.work = info.options().work();
			plan.descriptor_hash = Some(descriptor_hash(
				LiveDescriptorRef::Binding {
					expression: info.expression(),
					parameters: info.parameters(),
				},
				&admission,
				control,
			)?);
			metadata = hash_bounded(&admission, b"live-binding-definition", |mut writer| {
				info.dependencies().hash(&mut writer);
				info.function_dependencies().hash(&mut writer);
				Ok(())
			})?;
		}
	}
	match schema {
		LogicalSchemaStatement::RegisterLive(recipe) => {
			plan.descriptor_hash = Some(descriptor_hash(
				LiveDescriptorRef::Query(&recipe.query),
				&admission,
				control,
			)?);
			captures = capture_hash(&recipe.captures, slots, &admission)?;
			plan.sources = sources(&recipe.query, &admission)?;
			plan.output = Some(recipe.query.output());
			options(&mut plan, &defaults.clone().with_overflow(recipe.overflow), &admission)?;
			plan.scratch_bytes = control.barrier_memory_limit() as u64;
			plan.work = defaults.work().min(control.max_loop_iterations);
			for frozen in &recipe.frozen_graphs {
				let entry = plan.sources.get_mut(frozen.source as usize).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("LIVE scope source missing")
				})?;
				match scalar(read, snapshot, &frozen.graph, slots)? {
					Some(value) => {
						admission.admit_value(value)?;
						let graph = match value {
							Value::GraphRef(graph) => graph.as_ref().clone(),
							Value::String(name) => read.catalog(snapshot).graph(name.as_str())?,
							_ => {
								return Err(FerrosGrynnError::invalid_argument(
									"LIVE scope requires graph or name",
								));
							}
						};
						entry.graph = Some(graph.id());
						entry.dynamic_scope = false;
					}
					None => {
						entry.dynamic_scope = true;
						plan.dynamic_operands = true;
					}
				}
			}
			if let Some(since) = &recipe.since {
				match scalar(read, snapshot, since, slots)? {
					Some(value) => {
						admission.admit_value(value)?;
						plan.since_kind = match value {
							Value::Number(stamp) => {
								u64::try_from(*stamp)?;
								"VERSIONSTAMP"
							}
							Value::DateTime(_) => "TIMESTAMP",
							_ => {
								return Err(FerrosGrynnError::invalid_argument(
									"LIVE SINCE requires stamp or timestamp",
								));
							}
						};
						plan.since_value = Some(value.clone());
					}
					None => {
						plan.since_kind = "DEFERRED";
						plan.dynamic_operands = true;
					}
				}
			}
			if let Some(buffer) = &recipe.buffer {
				match scalar(read, snapshot, buffer, slots)? {
					Some(Value::Number(number)) => {
						let entries = usize::try_from(*number)?;
						let _validated =
							defaults.clone().with_queue(entries, defaults.queue_bytes())?;
						plan.queue_entries = Some(entries as u64);
					}
					Some(_) => {
						return Err(FerrosGrynnError::invalid_argument(
							"LIVE BUFFER requires positive integer",
						));
					}
					None => {
						plan.queue_entries = None;
						plan.dynamic_operands = true;
					}
				}
			}
			admission.work(recipe.operand_spelling.len() as u64)?;
			for capture in &recipe.operand_slots {
				let input = slots.get(capture.slot as usize).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("LIVE option slot missing")
				})?;
				admission.admit_value(input)?;
				if capture.reactive {
					let Value::Uuid(uuid) = input else {
						return Err(FerrosGrynnError::invalid_argument(
							"LIVE option binding requires UUID",
						));
					};
					admission.admit_value(
						read.live_binding_value(snapshot, LiveBindingId::from_uuid(*uuid)?)?,
					)?;
				}
			}
			metadata = hash_bounded(&admission, b"live-registration-operands", |mut writer| {
				recipe.operand_spelling.hash(&mut writer);
				for capture in &recipe.operand_slots {
					capture.name.hash(&mut writer);
					capture.reactive.hash(&mut writer);
					let input = &slots[capture.slot as usize];
					input.hash(&mut writer);
					if capture.reactive {
						let Value::Uuid(uuid) = input else {
							return Err(FerrosGrynnError::invalid_argument(
								"LIVE option binding requires UUID",
							));
						};
						read.live_binding_value(snapshot, LiveBindingId::from_uuid(*uuid)?)?
							.hash(&mut writer);
					}
				}
				Ok(())
			})?;
		}
		LogicalSchemaStatement::DefineLiveBinding(recipe)
		| LogicalSchemaStatement::ReplaceLiveBinding {
			plan: recipe,
			..
		} => {
			let empty = Object::new();
			plan.descriptor_hash = Some(descriptor_hash(
				LiveDescriptorRef::Binding {
					expression: &recipe.expression,
					parameters: &empty,
				},
				&admission,
				control,
			)?);
			captures = capture_hash(&recipe.captures, slots, &admission)?;
			plan.scratch_bytes = control.barrier_memory_limit() as u64;
			plan.work = defaults.work().min(control.max_loop_iterations);
		}
		LogicalSchemaStatement::AckLive {
			cursor,
			spelling,
			..
		} => {
			admission.work(spelling.len() as u64)?;
			metadata = hash_bounded(&admission, b"live-ack-operands", |mut writer| {
				metadata.hash(&mut writer);
				spelling.hash(&mut writer);
				Ok(())
			})?;
			match scalar(read, snapshot, cursor, slots)? {
				Some(Value::Bytes(bytes)) => {
					let token = LiveCursor::decode(bytes.as_slice())?;
					if let (Some(uuid), Some(version)) = (plan.identity, plan.definition_version)
						&& (token.database_id() != database
							|| token.live_id().as_uuid() != uuid
							|| token.version().get() != version
							|| token.versionstamp().get() > snapshot.0)
					{
						return Err(FerrosGrynnError::invalid_argument(
							"LIVE ACK cursor origin/query/version mismatch",
						));
					}
					admission.admit_value(scalar(read, snapshot, cursor, slots)?.ok_or_else(
						|| FerrosGrynnError::invalid_argument("LIVE cursor missing"),
					)?)?;
					plan.since_kind = "ISSUED_CURSOR";
					plan.since_value = Some(Value::Bytes(bytes.clone()));
				}
				Some(_) => {
					return Err(FerrosGrynnError::invalid_argument("LIVE ACK requires Bytes"));
				}
				None => plan.dynamic_operands = true,
			}
		}
		_ => {}
	}
	let mut function_count = HashBytes::default();
	read.visit_changefeed_functions(&mut |function| {
		hash_function(function, &mut function_count, Some(&admission))
	})?;
	admission.work(function_count.bytes() as u64)?;
	let functions_scratch =
		admission.temporary(function_count.bytes().saturating_mul(2).saturating_add(128))?;
	let mut functions = CanonicalHasher::versioned(PLAN_ENCODING_VERSION, b"live-functions");
	read.visit_changefeed_functions(&mut |function| hash_function(function, &mut functions, None))?;
	plan.function_fingerprint = functions.finish();
	drop(functions);
	drop(functions_scratch);
	plan.request_hash = hash_bounded(&admission, b"live-request", |mut writer| {
		captures.hash(&mut writer);
		metadata.hash(&mut writer);
		Ok(())
	})?;
	let mut physical_bytes = HashBytes::default();
	plan.hash(&mut physical_bytes);
	admission.admit(physical_bytes.bytes().saturating_mul(4).saturating_add(4096))?;
	let copies = plan.operation.phases().len();
	admission.admit(
		plan.sources
			.len()
			.saturating_mul(copies)
			.saturating_mul(4096)
			.saturating_add(copies.saturating_mul(8192)),
	)?;
	for source in &plan.sources {
		if let Some(record) = &source.record {
			admission.admit(record.estimated_heap_bytes().saturating_mul(copies))?;
		}
	}
	Ok(Some((plan, admission.into_account())))
}

/// Projects payload-free metadata; exact captured values never enter EXPLAIN
/// rows.
pub(super) fn project(plan: &PhysicalLivePlan) -> Vec<Value> {
	plan.operation
		.phases()
		.iter()
		.enumerate()
		.map(|(index, phase)| {
			let mut row = Object::new();
			row.insert("node_id", index as u64);
			row.insert("operator", *phase);
			row.insert("live_command", plan.operation.name());
			row.insert("database", plan.database);
			row.insert("snapshot_through", plan.snapshot.get());
			row.insert("live_id", plan.identity.map_or(Value::None, Value::Uuid));
			row.insert(
				"definition_version",
				plan.definition_version.map_or(Value::None, Value::from),
			);
			row.insert("descriptor_hash", plan.descriptor_hash.map_or(Value::None, Value::from));
			row.insert("function_fingerprint", plan.function_fingerprint);
			row.insert("read_strategy", "conservative_rescan");
			row.insert("delivery", "at_least_once_whole_commit");
			row.insert("since", plan.since_kind);
			row.insert(
				"operand_binding",
				if plan.dynamic_operands {
					"deferred"
				} else {
					"resolved"
				},
			);
			row.insert("queue_entries", plan.queue_entries.map_or(Value::None, Value::from));
			row.insert("queue_bytes", plan.queue_bytes);
			row.insert("scratch_bytes", plan.scratch_bytes);
			row.insert("work_limit", plan.work);
			row.insert(
				"output",
				plan.output.map_or(Value::None, |output| {
					Value::from(match output {
						crate::live::LiveOutputMode::Full => "FULL",
						crate::live::LiveOutputMode::Diff => "DIFF",
						crate::live::LiveOutputMode::Patch => "PATCH",
					})
				}),
			);
			row.insert(
				"overflow",
				plan.overflow.map_or(Value::None, |policy| {
					Value::from(match policy {
						crate::live::LiveOverflowPolicy::Gap => "GAP",
						crate::live::LiveOverflowPolicy::Disconnect => "DISCONNECT",
						crate::live::LiveOverflowPolicy::Block => "BLOCK",
					})
				}),
			);
			row.insert(
				"sources",
				Value::from(
					plan.sources
						.iter()
						.map(|source| {
							let mut row = Object::new();
							row.insert("ordinal", u64::from(source.ordinal));
							row.insert("kind", source.kind);
							row.insert(
								"class_id",
								source.class.map_or(Value::None, |class| Value::from(class.get())),
							);
							row.insert(
								"graph_id",
								source.graph.map_or(Value::None, |graph| Value::from(graph.get())),
							);
							row.insert(
								"record",
								source
									.record
									.as_ref()
									.map_or(Value::None, |record| Value::from(record.clone())),
							);
							row.insert(
								"scope",
								if source.dynamic_scope {
									"reactive_or_deferred"
								} else {
									"fixed"
								},
							);
							Value::Object(row)
						})
						.collect::<Vec<_>>(),
				),
			);
			Value::Object(row)
		})
		.collect()
}
