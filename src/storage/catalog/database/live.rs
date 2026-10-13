//! Checked subscription sections in the database's existing catalog payload.

use std::{sync::Arc, time::Duration};

use super::{
	LiveCatalogEntry,
	codec::{Decoder, invalid},
};
use crate::{
	gql_status::FerrosGrynnResult,
	live::{
		LiveCursor,
		LiveExpr,
		LiveId,
		LiveOptions,
		LiveOverflowPolicy,
		LiveSince,
		LiveState,
		LiveVersion,
	},
	storage::{
		history_codec::CodecBudget,
		live_descriptor::{self, LiveDescriptor},
	},
	values::{Object, Text, Value},
};

/// Fixed entry header plus the smallest canonical query descriptor.
pub(super) const MIN_BYTES: usize = 227;
/// Encoded catalog cap shared with all preexisting catalog sections.
const MAX_BYTES: usize = 16 * 1024 * 1024;

/// Appends a complete admitted entry without allocating a partial overflow.
pub(super) fn encode(output: &mut Vec<u8>, entry: &LiveCatalogEntry) -> FerrosGrynnResult<()> {
	let (since_tag, since) = match entry.options.since() {
		LiveSince::Now => (0, Vec::new()),
		LiveSince::Versionstamp(stamp) => (1, stamp.get().to_le_bytes().to_vec()),
		LiveSince::Timestamp(time) => (
			2,
			live_descriptor::encode(
				&LiveDescriptor::Binding {
					expression: LiveExpr::Literal(Value::DateTime(time.clone())),
					parameters: Object::new(),
				},
				MAX_BYTES,
			)?,
		),
		LiveSince::Cursor(cursor) => (3, cursor.encode().as_slice().to_vec()),
	};
	let (code, reason) = entry
		.diagnostic
		.as_ref()
		.map_or(("", ""), |(code, reason)| (code.as_str(), reason.as_str()));
	let captures = super::live_capture::size(&entry.functions, &entry.record_sources)?;
	let scope_name = entry.capture_scope_name.as_ref().map_or("", Text::as_str);
	let rhs_scopes = super::live_scope::size(&entry.rhs_scope_anchors)?;
	let length = 160_usize
		.checked_add(if entry.graph_checkpoint.is_some() {
			super::history_segment::BYTES
		} else {
			0
		})
		.and_then(|length| length.checked_add(since.len()))
		.and_then(|length| length.checked_add(entry.descriptor.len()))
		.and_then(|length| length.checked_add(scope_name.len()))
		.and_then(|length| length.checked_add(rhs_scopes))
		.and_then(|length| length.checked_add(captures))
		.and_then(|length| length.checked_add(code.len()))
		.and_then(|length| length.checked_add(reason.len()))
		.ok_or_else(|| invalid("LIVE catalog entry length overflows"))?;
	if output.len().checked_add(length).is_none_or(|length| length > MAX_BYTES) {
		return Err(invalid("LIVE catalog entry exceeds the aggregate catalog byte cap"));
	}
	output.try_reserve_exact(length).map_err(|_error| invalid("LIVE catalog allocation failed"))?;
	output.extend_from_slice(entry.id.as_uuid().as_inner().as_bytes());
	for value in [
		entry.version.get(),
		entry.defined_at,
		entry.starting_after,
		entry.published_at,
		entry.acknowledged,
	] {
		output.extend_from_slice(&value.to_le_bytes());
	}
	output.push(match entry.state {
		LiveState::Active => 0,
		LiveState::Paused => 1,
		LiveState::Closed => 2,
		LiveState::Pending => return Err(invalid("private LIVE state cannot be persisted")),
	});
	output.push(match entry.options.overflow() {
		LiveOverflowPolicy::Gap => 0,
		LiveOverflowPolicy::Disconnect => 1,
		LiveOverflowPolicy::Block => 2,
	});
	output.push(since_tag);
	output.push(
		u8::from(!entry.rhs_scope_anchors.is_empty())
			| (u8::from(entry.graph_checkpoint.is_some()) << 1),
	);
	output.extend_from_slice(
		&entry.source_class.map_or(0, crate::datastore::ClassId::get).to_le_bytes(),
	);
	for value in [
		entry.options.queue_entries() as u64,
		entry.options.queue_bytes() as u64,
		entry.options.scratch_bytes() as u64,
		entry.options.work(),
	] {
		output.extend_from_slice(&value.to_le_bytes());
	}
	output.extend_from_slice(&entry.options.block_timeout().as_secs().to_le_bytes());
	output.extend_from_slice(&entry.options.block_timeout().subsec_nanos().to_le_bytes());
	output.extend_from_slice(&[0; 4]);
	output.extend_from_slice(&(since.len() as u64).to_le_bytes());
	output.extend_from_slice(&(entry.descriptor.len() as u64).to_le_bytes());
	for count in [entry.functions.len(), entry.record_sources.len(), code.len(), reason.len()] {
		output.extend_from_slice(
			&u32::try_from(count)
				.map_err(|_error| invalid("LIVE capture count overflows"))?
				.to_le_bytes(),
		);
	}
	let (source_kind, source) = match entry.capture_source {
		crate::changefeed::ChangefeedSource::Class(id) => (0, u64::from(id.get())),
		crate::changefeed::ChangefeedSource::Graph(id) => (1, id.get()),
	};
	output.extend_from_slice(&[source_kind, 0, 0, 0]);
	output.extend_from_slice(&source.to_le_bytes());
	output.extend_from_slice(
		&u32::try_from(scope_name.len())
			.map_err(|_error| invalid("LIVE capture scope name overflows"))?
			.to_le_bytes(),
	);
	if let Some(segment) = entry.graph_checkpoint {
		super::history_segment::encode(output, segment);
	}
	super::live_capture::encode(output, &entry.functions, &entry.record_sources)?;
	output.extend_from_slice(&since);
	output.extend_from_slice(&entry.descriptor);
	output.extend_from_slice(scope_name.as_bytes());
	super::live_scope::encode(output, &entry.rhs_scope_anchors)?;
	output.extend_from_slice(code.as_bytes());
	output.extend_from_slice(reason.as_bytes());
	Ok(())
}

/// Restores identities, checked options and a canonical owned query recipe.
pub(super) fn decode(
	decoder: &mut Decoder<'_>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<LiveCatalogEntry> {
	budget.admit(std::mem::size_of::<LiveCatalogEntry>())?;
	let id = LiveId::from_uuid(
		uuid::Uuid::from_slice(decoder.read_exact(16)?)
			.map_err(|_error| invalid("LIVE catalog UUID is malformed"))?
			.into(),
	)?;
	let version = LiveVersion::new(decoder.read_u64()?)?;
	let defined_at = decoder.read_u64()?;
	let starting_after = decoder.read_u64()?;
	let published_at = decoder.read_u64()?;
	let acknowledged = decoder.read_u64()?;
	let state = match decoder.read_u8()? {
		0 => LiveState::Active,
		1 => LiveState::Paused,
		2 => LiveState::Closed,
		_ => return Err(invalid("LIVE catalog state is invalid")),
	};
	let overflow = match decoder.read_u8()? {
		0 => LiveOverflowPolicy::Gap,
		1 => LiveOverflowPolicy::Disconnect,
		2 => LiveOverflowPolicy::Block,
		_ => return Err(invalid("LIVE overflow policy is invalid")),
	};
	let since_tag = decoder.read_u8()?;
	let flags = decoder.read_u8()?;
	if flags & !3 != 0 {
		return Err(invalid("LIVE catalog options flags are invalid"));
	}
	let class_raw = decoder.read_u32()?;
	let source_class = if class_raw == 0 {
		None
	} else {
		Some(
			crate::datastore::ClassId::new(class_raw)
				.ok_or_else(|| invalid("LIVE source class uses a reserved identity"))?,
		)
	};
	let entries = addressable(decoder.read_u64()?)?;
	let bytes = addressable(decoder.read_u64()?)?;
	let scratch = addressable(decoder.read_u64()?)?;
	let work = decoder.read_u64()?;
	let timeout_seconds = decoder.read_u64()?;
	let timeout_nanos = decoder.read_u32()?;
	if timeout_nanos >= 1_000_000_000 {
		return Err(invalid("LIVE BLOCK timeout nanoseconds are not normalized"));
	}
	decoder.expect_zero(4, "LIVE timeout reserved bytes")?;
	let since_len = addressable(decoder.read_u64()?)?;
	let descriptor_len = addressable(decoder.read_u64()?)?;
	let function_count = decoder.read_count("LIVE query functions")?;
	let source_count = decoder.read_count("LIVE query record sources")?;
	let code_len = decoder.read_count("LIVE query diagnostic code")?;
	let reason_len = decoder.read_count("LIVE query diagnostic reason")?;
	let source_kind = decoder.read_u8()?;
	decoder.expect_zero(3, "LIVE capture source reserved bytes")?;
	let source_id = decoder.read_u64()?;
	let capture_source = match source_kind {
		0 => crate::changefeed::ChangefeedSource::Class(crate::datastore::ClassId::from_storage(
			u32::try_from(source_id).map_err(|_error| invalid("LIVE capture class overflows"))?,
		)?),
		1 => crate::changefeed::ChangefeedSource::Graph(
			crate::graph::GraphId::new(source_id)
				.ok_or_else(|| invalid("LIVE capture graph identity is zero"))?,
		),
		_ => return Err(invalid("LIVE capture source kind is invalid")),
	};
	let scope_name_len = decoder.read_count("LIVE capture scope name")?;
	if scope_name_len > 128 {
		return Err(invalid("LIVE capture scope name exceeds graph name cap"));
	}
	if code_len > 8 || reason_len > 4096 {
		return Err(invalid("LIVE query diagnostic cap is exceeded"));
	}
	let graph_checkpoint = if flags & 2 != 0 {
		Some(super::history_segment::decode(decoder)?)
	} else {
		None
	};
	let (functions, record_sources) =
		super::live_capture::decode(decoder, budget, function_count, source_count)?;
	let since_bytes = decoder.read_exact(since_len)?;
	let since = match since_tag {
		0 if since_bytes.is_empty() => LiveSince::Now,
		1 if since_bytes.len() == 8 => LiveSince::Versionstamp(
			u64::from_le_bytes(
				since_bytes.try_into().map_err(|_error| invalid("LIVE stamp is truncated"))?,
			)
			.into(),
		),
		2 => match live_descriptor::decode_bounded(since_bytes, budget)? {
			LiveDescriptor::Binding {
				expression: LiveExpr::Literal(Value::DateTime(time)),
				parameters,
			} if parameters.is_empty() => LiveSince::Timestamp(time),
			_ => return Err(invalid("LIVE timestamp descriptor is invalid")),
		},
		3 => LiveSince::Cursor(LiveCursor::decode(since_bytes)?),
		_ => return Err(invalid("LIVE start boundary tag or length is invalid")),
	};
	let options = LiveOptions::default()
		.with_since(since)
		.with_overflow(overflow)
		.with_queue(entries, bytes)?
		.with_budgets(scratch, work)?
		.with_block_timeout(Duration::new(timeout_seconds, timeout_nanos))?;
	let descriptor = decoder.read_exact(descriptor_len)?;
	let LiveDescriptor::Query(query) = live_descriptor::decode_bounded(descriptor, budget)? else {
		return Err(invalid("LIVE catalog entry does not contain a query recipe"));
	};
	budget.admit(descriptor.len())?;
	let scope_name_bytes = decoder.read_exact(scope_name_len)?;
	budget.admit(scope_name_len)?;
	let capture_scope_name = if scope_name_bytes.is_empty() {
		None
	} else {
		Some(Text::from(
			std::str::from_utf8(scope_name_bytes)
				.map_err(|_error| invalid("LIVE capture scope name is not UTF-8"))?,
		))
	};
	let rhs_scope_anchors = if flags & 1 != 0 {
		super::live_scope::decode(decoder, budget, &query)?
	} else {
		Vec::new()
	};
	let diagnostic = super::live_capture::diagnostic(decoder, budget, code_len, reason_len)?;
	Ok(LiveCatalogEntry {
		graph_checkpoint,
		id,
		version,
		state,
		defined_at,
		starting_after,
		published_at,
		acknowledged,
		options,
		query: Arc::new(query),
		descriptor: Arc::from(descriptor),
		source_class,
		capture_source,
		capture_scope_name,
		rhs_scope_anchors,
		functions,
		record_sources,
		diagnostic,
	})
}

/// Rejects byte/count values before converting them into allocation lengths.
fn addressable(value: u64) -> FerrosGrynnResult<usize> {
	let value =
		usize::try_from(value).map_err(|_error| invalid("LIVE catalog length is unaddressable"))?;
	if value > isize::MAX as usize {
		return Err(invalid("LIVE catalog length is unaddressable"));
	}
	Ok(value)
}

/// Checks retained incarnations without allocating duplicate identity sets.
pub(super) fn validate_entries(catalog: &super::DatabaseCatalog) -> FerrosGrynnResult<()> {
	if catalog.live_bindings.windows(2).any(|pair| pair[0].id >= pair[1].id) {
		return Err(invalid("LIVE binding identities are not sorted distinct"));
	}
	for entry in &catalog.live_bindings {
		if entry.state == LiveState::Pending
			|| entry.defined_at == 0
			|| entry.defined_at > entry.value_at
			|| entry.value_at > entry.published_at
			|| (entry.state == LiveState::Paused) != entry.diagnostic.is_some()
			|| entry.dependencies.windows(2).any(|pair| pair[0] >= pair[1])
			|| entry.functions.windows(2).any(|pair| pair[0].0 >= pair[1].0)
			|| entry.functions.iter().any(|(id, _)| u64::from(id.get()) >= catalog.next_schema_id)
		{
			return Err(invalid("LIVE binding state, bounds or dependency identities are invalid"));
		}
		let expected = crate::datastore::expression_dependencies(
			&entry.expression,
			&mut crate::datastore::BindingAdmission::new(
				&crate::live::LiveBindingOptions::default(),
			),
		)
		.map_err(|cause| {
			invalid("LIVE binding expression dependencies exceed catalog limits").with_cause(cause)
		})?;
		if expected != entry.dependencies {
			return Err(invalid(
				"LIVE binding dependency vector disagrees with its logical descriptor",
			));
		}
		if let Some((code, reason)) = &entry.diagnostic
			&& (code.as_str().parse::<crate::gql_status::StatusCode>().is_err()
				|| reason.is_empty()
				|| reason.len() > 4096)
		{
			return Err(invalid("LIVE binding fault diagnostic is malformed"));
		}
		if entry.state != LiveState::Closed {
			for id in &entry.dependencies {
				if !catalog
					.live_bindings
					.binary_search_by_key(id, |entry| entry.id)
					.ok()
					.and_then(|index| catalog.live_bindings.get(index))
					.is_some_and(|entry| entry.state != LiveState::Closed)
				{
					return Err(invalid("LIVE binding has an absent or removed dependency"));
				}
			}
		}
		if entry.state == LiveState::Active {
			for (name, id) in &entry.record_sources {
				if !catalog
					.classes
					.binary_search_by_key(id, |class| class.id)
					.ok()
					.and_then(|index| catalog.classes.get(index))
					.is_some_and(|class| &class.name == name)
				{
					return Err(invalid("active LIVE record source has an incompatible owner"));
				}
			}
			for (id, version) in &entry.functions {
				if !catalog
					.functions
					.binary_search_by_key(id, |function| function.id)
					.ok()
					.and_then(|index| catalog.functions.get(index))
					.is_some_and(|function| {
						function.version == *version
							&& function.effect == crate::datastore::FunctionEffect::Pure
					}) {
					return Err(invalid(
						"active LIVE binding has an incompatible captured PURE function",
					));
				}
			}
		}
		let names = crate::live::dependencies::expression_record_names(
			&entry.expression,
			&mut crate::live::dependencies::BindingAdmission::new(
				&crate::live::LiveBindingOptions::default(),
			),
		)
		.map_err(|cause| {
			invalid("LIVE record source traversal exceeds catalog limits").with_cause(cause)
		})?;
		if names.len() != entry.record_sources.len()
			|| names.iter().zip(&entry.record_sources).any(|(name, (stored, id))| {
				name != stored || u64::from(id.get()) >= catalog.next_schema_id
			}) {
			return Err(invalid(
				"LIVE record source owners disagree with their logical descriptor",
			));
		}
	}
	crate::live::dependencies::binding_order(
		catalog
			.live_bindings
			.iter()
			.filter(|entry| entry.state != LiveState::Closed)
			.map(|entry| (entry.id, entry.dependencies.as_slice())),
		&mut crate::live::dependencies::BindingAdmission::new(
			&crate::live::LiveBindingOptions::default(),
		),
	)
	.map_err(|cause| {
		invalid("LIVE binding DAG is malformed or exceeds catalog limits").with_cause(cause)
	})?;
	if catalog.live_queries.windows(2).any(|pair| pair[0].id >= pair[1].id) {
		return Err(invalid("LIVE catalog identities are not canonical sorted distinct"));
	}
	for entry in &catalog.live_queries {
		if let Some(segment) = entry.graph_checkpoint {
			if entry.query.graph_result().is_none()
				|| segment.owner() != crate::storage::HistoryOwner::Live(entry.id)
				|| segment.header.database != catalog.database_uuid
				|| segment.header.commits != 1
				|| segment.header.first_stamp != segment.header.last_stamp
				|| segment.header.first_time != segment.header.last_time
				|| segment.header.first_stamp < entry.defined_at
			{
				return Err(invalid(
					"LIVE graph checkpoint reference has inconsistent ownership/bounds",
				));
			}
		}
		let names = crate::live::dependencies::query_record_names(
			&entry.query,
			&mut crate::live::dependencies::BindingAdmission::new(
				&crate::live::LiveBindingOptions::default(),
			),
		)
		.map_err(|cause| {
			invalid("LIVE query record source traversal exceeds limits").with_cause(cause)
		})?;
		if names.len() != entry.record_sources.len()
			|| names.iter().zip(&entry.record_sources).any(|(name, (stored, _))| name != stored)
		{
			return Err(invalid("LIVE query source captures disagree with its logical descriptor"));
		}
		if let Some((code, reason)) = &entry.diagnostic
			&& (code.as_str().parse::<crate::gql_status::StatusCode>().is_err()
				|| reason.is_empty()
				|| reason.len() > 4096)
		{
			return Err(invalid("LIVE query fault metadata is malformed"));
		}
		if entry.functions.windows(2).any(|pair| pair[0].0 >= pair[1].0)
			|| entry.record_sources.windows(2).any(|pair| pair[0].0 >= pair[1].0)
			|| entry.functions.iter().any(|(id, _)| u64::from(id.get()) >= catalog.next_schema_id)
			|| entry
				.record_sources
				.iter()
				.any(|(_, id)| u64::from(id.get()) >= catalog.next_schema_id)
			|| (entry.state == LiveState::Paused) != entry.diagnostic.is_some()
		{
			return Err(invalid("LIVE query captures or fault state are invalid"));
		}
		if entry.state == LiveState::Active {
			for (id, version) in &entry.functions {
				if !catalog
					.functions
					.binary_search_by_key(id, |function| function.id)
					.ok()
					.and_then(|index| catalog.functions.get(index))
					.is_some_and(|function| {
						function.version == *version
							&& function.effect == crate::datastore::FunctionEffect::Pure
					}) {
					return Err(invalid("LIVE query captured PURE function is incompatible"));
				}
			}
			for (name, id) in &entry.record_sources {
				if !catalog
					.classes
					.binary_search_by_key(id, |class| class.id)
					.ok()
					.and_then(|index| catalog.classes.get(index))
					.is_some_and(|class| &class.name == name)
				{
					return Err(invalid("LIVE query captured record owner is incompatible"));
				}
			}
		}
		let dependencies = crate::live::dependencies::query_dependencies(
			&entry.query,
			&mut crate::live::dependencies::BindingAdmission::new(
				&crate::live::LiveBindingOptions::default(),
			),
		)
		.map_err(|cause| {
			invalid("LIVE query dependencies exceed catalog limits").with_cause(cause)
		})?;
		for id in &dependencies {
			let binding = catalog
				.live_bindings
				.binary_search_by_key(id, |binding| binding.id)
				.ok()
				.and_then(|index| catalog.live_bindings.get(index));
			if binding.is_none_or(|binding| {
				entry.state != LiveState::Closed && binding.state == LiveState::Closed
			}) {
				return Err(invalid("LIVE query has an absent or removed binding dependency"));
			}
		}
		if let crate::live::LiveSource::Graph(graph)
		| crate::live::LiveSource::GraphClass {
			graph,
			..
		}
		| crate::live::LiveSource::GraphRecord {
			graph,
			..
		} = entry.query.source()
			&& graph.id().get() >= catalog.next_graph_id
		{
			return Err(invalid("LIVE graph source exceeds its owner allocation clock"));
		}
		let valid_source_class = match entry.query.source() {
			crate::live::LiveSource::Class(class)
			| crate::live::LiveSource::GraphClass {
				class,
				..
			}
			| crate::live::LiveSource::GraphClassExpr {
				class,
				..
			} => entry.source_class == Some(*class),
			crate::live::LiveSource::Record(_)
			| crate::live::LiveSource::GraphRecord {
				..
			}
			| crate::live::LiveSource::GraphRecordExpr {
				..
			} => entry.source_class.is_some(),
			crate::live::LiveSource::Graph(_) | crate::live::LiveSource::GraphExpr(_) => {
				entry.source_class.is_none()
			}
		};
		if !valid_source_class
			|| entry
				.source_class
				.is_some_and(|class| u64::from(class.get()) >= catalog.next_schema_id)
		{
			return Err(invalid("LIVE source owner binding is invalid"));
		}
		super::live_scope::validate(&entry.rhs_scope_anchors, &entry.query, catalog.next_graph_id)?;
		if entry.query.source().graph_expression().is_some() {
			if entry.capture_scope_name.as_ref().is_some_and(|name| {
				name.is_empty() || name.len() > 128 || name.as_str().contains('\0')
			}) {
				return Err(invalid("LIVE expression capture scope name is invalid"));
			}
			if !matches!(entry.capture_source, crate::changefeed::ChangefeedSource::Graph(graph) if graph.get() < catalog.next_graph_id)
			{
				return Err(invalid(
					"LIVE expression capture graph exceeds its owner allocation clock",
				));
			}
		} else if entry.capture_scope_name.is_some()
			|| crate::datastore::capture_source(&entry.query, entry.source_class)?
				!= entry.capture_source
		{
			return Err(invalid("LIVE capture owner disagrees with its source descriptor"));
		}
		if entry.state == crate::live::LiveState::Pending
			|| entry.defined_at == 0
			|| entry.defined_at > entry.published_at
			|| !entry.options.since().matches_start(entry.starting_after, entry.defined_at)
			|| entry.acknowledged < entry.starting_after
		{
			return Err(invalid("LIVE catalog identity, state or commit bounds are invalid"));
		}
	}
	Ok(())
}
