//! GQL consumers of the standalone changefeed API; no physical storage access.

mod images;
mod operands;
mod planning;

pub(super) use planning::{plan, project_plan};

use super::eval::eval;
use crate::{
	changefeed::{
		ChangefeedConfig,
		ChangefeedCursor,
		ChangefeedId,
		ChangefeedReadItem,
		ChangefeedReadMode,
		ChangefeedReadOptions,
		ChangefeedSelector,
		ChangefeedSince,
		ChangefeedSource,
		ChangefeedState,
	},
	datastore::Transaction,
	gql::{
		ExecutionControl,
		ir::{LogicalChangefeedTarget, TypedExpr},
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Array, Object, Value},
};

/// Configures one current owner while keeping retained name lookup read-only.
pub(super) fn configure(
	transaction: &mut Transaction,
	graph: bool,
	if_exists: bool,
	name: &str,
	configuration: Option<ChangefeedConfig>,
) -> FerrosGrynnResult<Value> {
	let source = if graph {
		transaction.named_graph(name)?.map(|graph| ChangefeedSource::Graph(graph.id()))
	} else {
		transaction.class(name)?.and_then(|class| class.id()).map(ChangefeedSource::Class)
	};
	let Some(source) = source else {
		return if if_exists {
			Ok(Value::None)
		} else if graph {
			Err(FerrosGrynnError::graph_does_not_exist(name))
		} else {
			Err(FerrosGrynnError::class_does_not_exist(name))
		};
	};
	if let Some(config) = configuration {
		transaction.configure_changefeed(source, config)?;
	} else {
		transaction.drop_changefeed(source)?;
	}
	Ok(Value::None)
}

/// Resolves exact retained UUIDs without consulting current primary records.
fn feed_id(value: Value) -> FerrosGrynnResult<ChangefeedId> {
	match value {
		Value::Uuid(uuid) => ChangefeedId::from_uuid(uuid),
		Value::String(text) => text.as_str().parse(),
		_ => Err(FerrosGrynnError::invalid_argument("CHANGEFEED requires a UUID or UUID string")),
	}
}

/// Reads logical source metadata from the same public SDK surface.
pub(super) fn info(
	transaction: &mut Transaction,
	id: &TypedExpr,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let operands = operands::Operands::new(control);
	let id = feed_id(eval(id, None, slots, source, Some(&operands), Some(control))?)?;
	let mut account = control.barrier_account();
	let info = transaction.changefeed_info_admitted(id, |metadata_bytes| {
		// Owned metadata and the projected object remain live together.
		let required = metadata_bytes.saturating_mul(2).saturating_add(32 * 1024);
		let actual = control
			.barrier_memory_limit()
			.saturating_sub(control.barrier_memory_available())
			.saturating_add(required);
		if !account.reserve(required) {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"INFO CHANGEFEED result",
				actual as u64,
				control.barrier_memory_limit() as u64,
			));
		}
		Ok(())
	})?;
	let config = info.configuration();
	let mut result = Object::new();
	result.insert("id", info.id().as_uuid());
	result.insert("database", info.database_id());
	result.insert("name", info.name().clone());
	result.insert("source_kind", match info.source() {
		ChangefeedSource::Class(_) => "CLASS",
		ChangefeedSource::Graph(_) => "GRAPH",
	});
	result.insert("source_id", match info.source() {
		ChangefeedSource::Class(class) => u64::from(class.get()),
		ChangefeedSource::Graph(graph) => graph.get(),
	});
	result.insert("state", match info.state() {
		ChangefeedState::Active => "ACTIVE",
		ChangefeedState::Stopped => "STOPPED",
		ChangefeedState::Retired => "RETIRED",
	});
	result.insert("version", u64::from(info.version().get()));
	result.insert("retention", config.retention());
	result.insert("include_original", config.include_original());
	result.insert("full", config.full());
	result.insert("defined_at", info.defined_at().get());
	result.insert("published_at", info.published_at().get());
	result.insert("last_versionstamp", info.last_versionstamp().get());
	result.insert("purged_through", info.purged_through().get());
	result.insert("snapshot_versionstamp", info.snapshot_versionstamp().get());
	Ok(Value::Object(result))
}

/// Validates source values using the runtime schema boundary, including
/// historical graph references whose current names have disappeared.
fn selector(
	target: &LogicalChangefeedTarget,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	operands: &operands::Operands<'_>,
) -> FerrosGrynnResult<ChangefeedSelector> {
	match target {
		LogicalChangefeedTarget::Named(selector) => Ok(selector.clone()),
		LogicalChangefeedTarget::Id(id) => {
			feed_id(eval(id, None, slots, source, Some(operands), Some(control))?)
				.map(ChangefeedSelector::Id)
		}
		LogicalChangefeedTarget::Graph(graph) => {
			match eval(graph, None, slots, source, Some(operands), Some(control))? {
				Value::GraphRef(graph) => Ok(ChangefeedSource::Graph(graph.id()).into()),
				Value::String(name) => Ok(ChangefeedSelector::GraphName(name)),
				_ => Err(FerrosGrynnError::invalid_argument(
					"GRAPH source requires a GraphRef or graph-name string",
				)),
			}
		}
	}
}

/// Reads whole commits and admits owned result conversion before allocation.
#[allow(
	clippy::too_many_arguments,
	reason = "one typed SHOW command uses the shared evaluator context"
)]
pub(super) fn show(
	transaction: &mut Transaction,
	target: &LogicalChangefeedTarget,
	since: &TypedExpr,
	full: bool,
	limit: Option<&TypedExpr>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	control.check()?;
	let operands = operands::Operands::new(control);
	let target = selector(target, slots, source, control, &operands)?;
	let since = match eval(since, None, slots, source, Some(&operands), Some(control))? {
		Value::Number(stamp) => ChangefeedSince::Versionstamp(u64::try_from(stamp)?.into()),
		Value::DateTime(time) => ChangefeedSince::Timestamp(time),
		Value::Bytes(cursor) => {
			ChangefeedSince::Cursor(ChangefeedCursor::decode(cursor.as_slice())?)
		}
		_ => {
			return Err(FerrosGrynnError::invalid_argument(
				"SINCE requires an integer stamp, UTC timestamp or cursor bytes",
			));
		}
	};
	let limit = limit
		.map(|limit| match eval(limit, None, slots, source, Some(&operands), Some(control))? {
			Value::Number(limit) => u64::try_from(limit),
			_ => Err(FerrosGrynnError::invalid_argument("LIMIT requires a nonnegative integer")),
		})
		.transpose()?
		.unwrap_or(u64::MAX);
	let mode = if full {
		ChangefeedReadMode::Full
	} else {
		ChangefeedReadMode::Net
	};
	let options = ChangefeedReadOptions::new(since)
		.with_mode(mode)
		.with_limit(limit)
		.with_budgets(control.barrier_memory_available(), 1_000_000)?;
	let mut reader = transaction.changes(target, options)?;
	let mut account = control.barrier_account();
	let mut output = Vec::new();
	let mut work = 0_u64;
	while let Some(item) = reader.next_batch_with_memory(control.barrier_memory_available())? {
		control.check()?;
		let required = images::item_bytes(&item, &mut work)?;
		let actual = control
			.barrier_memory_limit()
			.saturating_sub(control.barrier_memory_available())
			.saturating_add(required);
		if !account.reserve(required) {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"SHOW CHANGES result",
				actual as u64,
				control.barrier_memory_limit() as u64,
			));
		}
		output.try_reserve_exact(1).map_err(|error| {
			FerrosGrynnError::invalid_argument("SHOW CHANGES allocation failed")
				.with_technical_source(error)
		})?;
		let mut row = match item {
			ChangefeedReadItem::Commit(batch) => {
				let mut row = Object::new();
				row.insert("kind", "commit");
				row.insert("source", batch.source().as_uuid());
				row.insert("versionstamp", batch.versionstamp().get());
				row.insert("committed_at", batch.committed_at().clone());
				row.insert("changes", images::entries(&batch));
				row
			}
			ChangefeedReadItem::Gap(gap) => images::gap(&gap),
		};
		row.insert(
			"cursor",
			reader.cursor().map_or(Value::None, |cursor| Value::Bytes(cursor.encode())),
		);
		output.push(Value::Object(row));
	}
	control.record_changefeed(reader.statistics());
	Ok(Value::Array(Array::new(output)))
}
