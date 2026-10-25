//! Cross-entry validation for one decoded `FGDC v13` payload.

use std::collections::{BTreeMap, BTreeSet};

use super::{AnalyzerFilterCatalogEntry, DatabaseCatalog, PhysicalIndexBinding, codec::invalid};
use crate::{
	datastore::{
		ClassIndexKind,
		ClassIndexOptions,
		ClassIndexState,
		FunctionEffect,
		IndexMaintenanceMode,
		IndexScope,
		SchemaId,
		VectorDistance,
	},
	gql_status::FerrosGrynnResult,
	graph::{GraphKind, GraphStorageMode},
	storage::{page::PageNo, types::DataFileKind},
	values::{
		Type,
		storage::{
			StorageValue,
			codec::{CanonicalCodec, DecodeLimits},
		},
	},
};

/// Validates the current state.
pub(super) fn validate(catalog: &DatabaseCatalog) -> FerrosGrynnResult<()> {
	if let Some(binding) = catalog.bridges {
		let roots = [binding.heap, binding.primary, binding.direct, binding.reverse];
		if binding.generation == 0
			|| binding.directory.is_nil()
			|| binding.count == 0
			|| roots.iter().any(|root| {
				root.file_id.kind() != Some(DataFileKind::BridgeMappings)
					|| root.root == PageNo::NULL
			})
			|| roots.iter().map(|root| root.file_id).collect::<BTreeSet<_>>().len() != 4
		{
			return Err(invalid("mapping catalog descriptor is invalid"));
		}
	}
	if catalog.database_uuid.is_nil() {
		return Err(invalid("database origin UUID is nil"));
	}
	jiff::Timestamp::from_nanosecond(catalog.committed_at_ns).map_err(|_conversion_error| {
		invalid("database commit UTC time is outside its supported domain")
	})?;
	super::live::validate_entries(catalog)?;
	let mut feed_ids = BTreeSet::new();
	let mut feed_sources = BTreeSet::new();
	let mut feed_files = BTreeSet::new();
	let mut feed_segments = BTreeSet::new();
	for entry in &catalog.changefeeds {
		let partial = validate_changefeed_entry(catalog, entry, &mut feed_ids, &mut feed_sources)?;
		validate_changefeed_segments(catalog, entry, partial, &mut feed_files, &mut feed_segments)?;
	}
	super::live_history::validate_entries(catalog, &mut feed_files, &mut feed_segments)?;
	if catalog.next_graph_id == 0 {
		return Err(invalid("next graph identity is zero"));
	}
	if catalog.next_schema_id == 0 || catalog.next_schema_id > u64::from(u32::MAX) {
		return Err(invalid("next schema identity is outside its u32 domain"));
	}
	validate_binding(catalog.atom_store.primary)?;
	validate_binding(catalog.atom_store.reverse_membership)?;
	if catalog.atom_store.primary.file_id == catalog.atom_store.reverse_membership.file_id {
		return Err(invalid("global primary and reverse membership share one file identity"));
	}

	let mut time_zones = BTreeSet::new();
	for name in catalog.time_zones.iter() {
		if name.is_empty() || name.as_str().contains('\0') || !time_zones.insert(name.as_str()) {
			return Err(invalid("time-zone table contains an invalid or duplicate name"));
		}
	}

	let graph_by_id =
		catalog.graphs.iter().map(|graph| (graph.id, graph)).collect::<BTreeMap<_, _>>();
	let graph_ids = graph_by_id.keys().copied().collect::<BTreeSet<_>>();
	let analyzer_by_id = catalog
		.analyzers
		.iter()
		.map(|analyzer| (analyzer.id, analyzer))
		.collect::<BTreeMap<_, _>>();
	let function_by_id = catalog
		.functions
		.iter()
		.map(|function| (function.id, function))
		.collect::<BTreeMap<_, _>>();
	let mut schema_ids = BTreeSet::new();
	let mut class_ids = BTreeSet::new();
	validate_class_catalog(
		catalog,
		&mut schema_ids,
		&mut class_ids,
		&graph_ids,
		&analyzer_by_id,
		&function_by_id,
	)?;

	let mut previous_graph = None;
	let mut graph_names = BTreeSet::new();
	for graph in &catalog.graphs {
		if previous_graph.is_some_and(|previous| previous >= graph.id)
			|| graph.id.get() >= catalog.next_graph_id
			|| graph.published_at == 0
			|| graph.name.is_empty()
			|| graph.name.as_str().contains('\0')
			|| !graph_names.insert(graph.name.as_str())
			|| graph.storage_mode != GraphStorageMode::DiskBacked
			|| graph.kind == GraphKind::Subgraph
			|| graph.tensor_revision > graph.generation
		{
			return Err(invalid("graph catalog ordering, identity, or storage mode is invalid"));
		}
		match graph.owner_root {
			None if graph.name.as_str().starts_with("$fg$") => {
				return Err(invalid("public graph uses the reserved internal-name prefix"));
			}
			Some(owner) => {
				let owner_entry = graph_by_id
					.get(&owner)
					.ok_or_else(|| invalid("internal graph owner root is absent"))?;
				if owner == graph.id
					|| owner_entry.owner_root.is_some()
					|| graph.kind != GraphKind::Graph
					|| graph.name.as_str() != internal_graph_name(owner, graph.id)
				{
					return Err(invalid("internal graph ownership or name is not canonical"));
				}
			}
			None => {}
		}
		if graph.root.as_ref().is_some_and(Vec::is_empty)
			|| (graph.root.is_some() && graph.kind != GraphKind::RootedTree)
		{
			return Err(invalid("graph root value is empty or belongs to a non-rooted family"));
		}
		insert_schema(&mut schema_ids, graph.label_index.0.into(), catalog.next_schema_id)?;
		insert_schema(&mut schema_ids, graph.adjacency_index.0.into(), catalog.next_schema_id)?;
		if let Some((id, _version)) = graph.edge_endpoint_index {
			insert_schema(&mut schema_ids, id.into(), catalog.next_schema_id)?;
		}
		if graph.edge_endpoint_index.is_none() && graph.edge_endpoint.is_some() {
			return Err(invalid("endpoint physical binding has no engine capability"));
		}
		if let Some(binding) = graph.edge_endpoint {
			validate_binding(binding)?;
		}
		let mut previous_index = None;
		let mut index_names = BTreeSet::new();
		if graph.indexes.len() > 4096 {
			return Err(invalid("graph user index count exceeds its bound"));
		}
		for index in &graph.indexes {
			let id = index.id().ok_or_else(|| invalid("graph index has no identity"))?;
			if let Some(binding) = graph.index_bindings.get(&id) {
				let kind = if matches!(
					index.definition(),
					crate::datastore::GraphIndexDefinition::Tensor { .. }
				) {
					DataFileKind::TensorCache
				} else {
					DataFileKind::Index
				};
				validate_binding_kind(*binding, kind)?;
				if index.state() != ClassIndexState::Ready
					|| index.is_on_demand()
					|| binding.root != PageNo::new(0)
				{
					return Err(invalid(
						"graph artifact binding has incompatible availability or root",
					));
				}
			} else if index.state() == ClassIndexState::Ready
				&& !index.is_on_demand()
				&& !matches!(
					index.definition(),
					crate::datastore::GraphIndexDefinition::Reachability {
						algorithm: crate::datastore::ReachabilityAlgorithm::Auto
					} | crate::datastore::GraphIndexDefinition::Neighbourhood {
						algorithm: crate::datastore::NeighbourhoodAlgorithm::Auto,
						..
					}
				) {
				return Err(invalid("Ready materialized graph index has no artifact binding"));
			}
			insert_schema(&mut schema_ids, id.into(), catalog.next_schema_id)?;
			index
				.validate()
				.map_err(|cause| invalid("graph index definition is invalid").with_cause(cause))?;
			index
				.validate_family(graph.kind)
				.map_err(|cause| invalid("graph index family is invalid").with_cause(cause))?;
			if index.graph() != Some(graph.id)
				|| previous_index.is_some_and(|previous| previous >= id)
				|| !index_names.insert(index.name().as_str())
			{
				return Err(invalid("graph index ownership, ordering, or name is invalid"));
			}
			for &(function, version) in index.dependencies() {
				if index.state() == ClassIndexState::Ready
					&& function_by_id.get(&function).is_none_or(|entry| {
						entry.version != version
							|| !index.is_on_demand() && entry.effect != FunctionEffect::Pure
					}) {
					return Err(invalid(
						"Ready graph index has an incompatible function dependency",
					));
				}
			}
			previous_index = Some(id);
		}
		if graph
			.index_bindings
			.keys()
			.any(|id| graph.indexes.iter().all(|index| index.id() != Some(*id)))
		{
			return Err(invalid("graph artifact binding references an absent definition"));
		}
		validate_binding(graph.membership)?;
		let mut previous_property = None;
		for property in &graph.properties {
			if previous_property.is_some_and(|previous| previous >= property.key)
				|| property.value.is_empty()
			{
				return Err(invalid("graph properties are not canonical"));
			}
			previous_property = Some(property.key);
		}
		if graph.arena.graph_by_slot().keys().ne(graph.arena.scope_by_slot().keys())
			|| graph.arena.graph_by_slot().get(&0) != Some(&graph.id)
			|| graph.arena.scope_by_slot().get(&0).is_none_or(|scope| !scope.is_empty())
		{
			return Err(invalid("graph arena root or slot maps are inconsistent"));
		}
		let mut scope_slots = BTreeMap::new();
		let mut scoped_children = BTreeSet::new();
		for (expected, (slot, child)) in graph.arena.graph_by_slot().iter().enumerate() {
			if usize::try_from(*slot).ok() != Some(expected) || !graph_ids.contains(child) {
				return Err(invalid(
					"graph arena slots are not dense or reference an absent graph",
				));
			}
			let scope = graph
				.arena
				.scope_by_slot()
				.get(slot)
				.ok_or_else(|| invalid("graph arena slot has no scope"))?;
			if (*slot != 0 && scope.is_empty())
				|| scope_slots.insert(scope.as_ref(), *slot).is_some()
				|| !scoped_children.insert((scope.as_ref(), *child))
			{
				return Err(invalid("graph arena contains a duplicate or empty scope"));
			}
			if let Some(owner) = graph_by_id.get(child).and_then(|entry| entry.owner_root) {
				let expected_owner = graph.owner_root.unwrap_or(graph.id);
				if owner != expected_owner {
					return Err(invalid("internal graph is reachable from a foreign root arena"));
				}
			}
		}
		for (slot, scope) in graph.arena.scope_by_slot() {
			if *slot == 0 {
				continue;
			}
			let parent_scope = &scope[..scope.len() - 1];
			if !scope_slots.contains_key(parent_scope) {
				return Err(invalid("graph arena scope has no parent scope"));
			}
			let child = graph.arena.graph_by_slot()[slot];
			for depth in 0..scope.len() {
				let ancestor_slot = scope_slots
					.get(&scope[..depth])
					.ok_or_else(|| invalid("graph arena ancestry is incomplete"))?;
				if graph.arena.graph_by_slot()[ancestor_slot] == child {
					return Err(invalid("graph arena contains a containment cycle"));
				}
			}
		}
		previous_graph = Some(graph.id);
	}
	for graph in &catalog.graphs {
		let Some(owner) = graph.owner_root else {
			continue;
		};
		let owner_entry = graph_by_id
			.get(&owner)
			.ok_or_else(|| invalid("internal graph owner root is absent"))?;
		if !owner_entry.arena.graph_by_slot().values().any(|child| *child == graph.id) {
			return Err(invalid("internal graph is not reachable from its owner root arena"));
		}
	}

	let mut previous_parameter = None;
	for parameter in &catalog.parameters {
		insert_schema(&mut schema_ids, parameter.id, catalog.next_schema_id)?;
		if previous_parameter.is_some_and(|previous: &str| previous >= parameter.name.as_str())
			|| parameter.published_at == 0
			|| parameter.name.is_empty()
			|| parameter.name.as_str().contains('\0')
			|| parameter.value.is_empty()
			|| parameter.comment.as_ref().is_some_and(|comment| comment.as_str().contains('\0'))
		{
			return Err(invalid("parameter catalog ordering or value is invalid"));
		}
		previous_parameter = Some(parameter.name.as_str());
	}

	let mut previous_function = None;
	let mut function_signatures = BTreeSet::new();
	let mut functions_by_id = BTreeMap::new();
	for function in &catalog.functions {
		insert_schema(&mut schema_ids, function.id.into(), catalog.next_schema_id)?;
		if previous_function.is_some_and(|previous| previous >= function.id)
			|| function.published_at == 0
			|| function.namespace.as_str() != "fn"
			|| function.name.is_empty()
			|| function.name.as_str().contains('\0')
			|| function.body.is_empty()
			|| function.body.as_str().contains('\0')
			|| function.max_depth.is_some_and(|depth| !(1..=64).contains(&depth))
			|| function.cost && function.effect != crate::datastore::FunctionEffect::Pure
			|| function.comment.as_ref().is_some_and(|comment| comment.as_str().contains('\0'))
		{
			return Err(invalid("function catalog identity or metadata is invalid"));
		}
		let mut parameter_names = BTreeSet::new();
		if function.parameters.iter().any(|parameter| {
			parameter.name.is_empty()
				|| parameter.name.as_str().contains('\0')
				|| !parameter_names.insert(parameter.name.as_str())
		}) {
			return Err(invalid("function parameters are invalid or duplicated"));
		}
		let signature = (
			function.namespace.as_str(),
			function.name.as_str(),
			function.parameters.iter().map(|parameter| &parameter.value_type).collect::<Vec<_>>(),
		);
		if !function_signatures.insert(signature) {
			return Err(invalid("function overload signature is duplicated"));
		}
		functions_by_id.insert(function.id, function);
		previous_function = Some(function.id);
	}

	let mut previous_analyzer = None;
	let mut analyzer_names = BTreeSet::new();
	for analyzer in &catalog.analyzers {
		insert_schema(&mut schema_ids, analyzer.id.into(), catalog.next_schema_id)?;
		if previous_analyzer.is_some_and(|previous| previous >= analyzer.id)
			|| analyzer.published_at == 0
			|| analyzer.name.is_empty()
			|| analyzer.name.as_str().contains('\0')
			|| !analyzer_names.insert(analyzer.name.as_str())
			|| analyzer.comment.as_ref().is_some_and(|comment| comment.as_str().contains('\0'))
			|| (analyzer.function.is_some() && !analyzer.tokenizers.is_empty())
		{
			return Err(invalid("analyzer catalog identity or metadata is invalid"));
		}
		if let Some((id, version)) = analyzer.function {
			let function = functions_by_id
				.get(&id)
				.ok_or_else(|| invalid("analyzer references an absent function"))?;
			if function.version != version
				|| function.effect != FunctionEffect::Pure
				|| function.parameters.len() != 1
				|| function.parameters[0].value_type != Type::String
				|| function.result != Type::Array(Box::new(Type::Object))
			{
				return Err(invalid("analyzer function signature or version is invalid"));
			}
		}
		for filter in &analyzer.filters {
			match filter {
				AnalyzerFilterCatalogEntry::EdgeNgram {
					min,
					max,
				}
				| AnalyzerFilterCatalogEntry::Ngram {
					min,
					max,
				} if *min == 0 || min > max => {
					return Err(invalid("analyzer n-gram bounds are invalid"));
				}
				AnalyzerFilterCatalogEntry::Mapper(bytes) => {
					let value = CanonicalCodec::decode(bytes, DecodeLimits::default())?;
					let StorageValue::Object(entries) = &*value.as_ref() else {
						return Err(invalid("analyzer mapper is not a canonical object"));
					};
					if entries.iter().any(|(_key, value)| {
						!matches!(value, StorageValue::String(term) if !term.is_empty() && !term.as_str().contains('\0'))
							&& !matches!(value, StorageValue::Null)
					}) {
						return Err(invalid("analyzer mapper contains a non-string replacement"));
					}
				}
				_ => {}
			}
		}
		previous_analyzer = Some(analyzer.id);
	}
	let mut previous_event = None;
	let mut event_names = BTreeSet::new();
	for event in &catalog.events {
		let id = event.id().ok_or_else(|| invalid("published EVENT has no identity"))?;
		insert_schema(&mut schema_ids, id.into(), catalog.next_schema_id)?;
		let owner_exists = match event.target() {
			crate::datastore::EventTarget::Class(class) => class_ids.contains(&class),
			crate::datastore::EventTarget::Graph(graph) => graph_ids.contains(&graph),
		};
		if !owner_exists
			|| event.published_at == 0
			|| previous_event.is_some_and(|previous| previous >= id)
			|| !event_names.insert((event.target(), event.name().as_str()))
		{
			return Err(invalid("EVENT source, identity, name or ordering is invalid"));
		}
		previous_event = Some(id);
	}
	let mut previous_outbox = None;
	for segment in &catalog.event_outbox_segments {
		let header = segment.header;
		header.validate()?;
		if segment.file.kind() != Some(DataFileKind::EventOutbox)
			|| segment.file.instance_id() == 0
			|| header.database != catalog.database_uuid
			|| header.owner != crate::storage::HistoryOwner::EventOutbox(catalog.database_uuid)
			|| previous_outbox
				.is_some_and(|(stamp, file)| stamp >= header.first_stamp || file >= segment.file)
		{
			return Err(invalid("EVENT outbox segment identity, owner or order is invalid"));
		}
		previous_outbox = Some((header.last_stamp, segment.file));
	}
	Ok(())
}

/// Validates class identities, fields and index bindings against the shared
/// catalog namespaces.
///
/// # Errors
/// Returns the first existing class, field, index or dependency inconsistency
/// in the original order.
fn validate_class_catalog(
	catalog: &DatabaseCatalog,
	schema_ids: &mut BTreeSet<SchemaId>,
	class_ids: &mut BTreeSet<crate::datastore::ClassId>,
	graph_ids: &BTreeSet<crate::graph::GraphId>,
	analyzer_by_id: &BTreeMap<crate::datastore::AnalyzerId, &super::AnalyzerCatalogEntry>,
	function_by_id: &BTreeMap<crate::datastore::FunctionId, &super::FunctionCatalogEntry>,
) -> FerrosGrynnResult<()> {
	let mut class_names = BTreeSet::new();
	let mut previous_class = None;
	for class in &catalog.classes {
		if previous_class.is_some_and(|previous| previous >= class.id)
			|| u64::from(class.id.get()) >= catalog.next_schema_id
			|| class.published_at == 0
			|| class.name.is_empty()
			|| class.name.as_str().contains('\0')
			|| !class_ids.insert(class.id)
			|| !class_names.insert(class.name.as_str())
		{
			return Err(invalid("class catalog ordering or identity is invalid"));
		}
		insert_schema(schema_ids, class.id.into(), catalog.next_schema_id)?;
		let mut field_ids = BTreeSet::new();
		let mut field_names = BTreeSet::new();
		let mut field_keys = BTreeSet::new();
		for field in &class.fields {
			insert_schema(schema_ids, field.id, catalog.next_schema_id)?;
			if !field_ids.insert(field.id)
				|| !field_names.insert(field.name.as_str())
				|| !field_keys.insert(field.property_key)
				|| field.name.is_empty()
				|| field.name.as_str().contains('\0')
			{
				return Err(invalid("class fields contain duplicate or invalid metadata"));
			}
		}
		let mut index_ids = BTreeSet::new();
		let mut index_names = BTreeSet::new();
		for index in &class.indexes {
			insert_schema(schema_ids, index.id.into(), catalog.next_schema_id)?;
			if !index_ids.insert(index.id)
				|| !index_names.insert(index.name.as_str())
				|| index.name.is_empty()
				|| index.name.as_str().contains('\0')
			{
				return Err(invalid("class indexes contain duplicate or invalid metadata"));
			}
			match index.kind {
				ClassIndexKind::Count if !index.fields.is_empty() => {
					return Err(invalid("Count index contains fields"));
				}
				ClassIndexKind::Standard | ClassIndexKind::Unique if index.fields.is_empty() => {
					return Err(invalid("scalar index contains no fields"));
				}
				ClassIndexKind::Fulltext
				| ClassIndexKind::Hnsw
				| ClassIndexKind::DiskAnn
				| ClassIndexKind::Geometry
					if index.fields.len() != 1 =>
				{
					return Err(invalid("specialized index must contain exactly one field"));
				}
				_ => {}
			}
			match (index.kind, index.options) {
				(ClassIndexKind::Geometry, ClassIndexOptions::Geometry(_)) => {}
				(
					ClassIndexKind::Standard | ClassIndexKind::Unique | ClassIndexKind::Count,
					ClassIndexOptions::Scalar,
				) => {}
				(ClassIndexKind::Fulltext, ClassIndexOptions::Fulltext(options)) => {
					let analyzer = analyzer_by_id
						.get(&options.analyzer())
						.ok_or_else(|| invalid("Fulltext index references an absent analyzer"))?;
					if analyzer.version != options.analyzer_version() {
						return Err(invalid("Fulltext analyzer version is stale"));
					}
				}
				(ClassIndexKind::Hnsw, ClassIndexOptions::Hnsw(options)) => {
					validate_custom_distance(options.distance(), function_by_id)?;
				}
				(ClassIndexKind::DiskAnn, ClassIndexOptions::DiskAnn(options)) => {
					if matches!(options.distance(), VectorDistance::Custom { .. }) {
						return Err(invalid("DiskANN cannot use a custom distance"));
					}
				}
				_ => return Err(invalid("index kind-specific options do not match its kind")),
			}
			if index.maintenance == IndexMaintenanceMode::Deferred
				&& index.kind == ClassIndexKind::Unique
			{
				return Err(invalid("Unique index cannot use deferred maintenance"));
			}
			let mut keys = BTreeSet::new();
			if index.fields.iter().any(|key| !field_keys.contains(key) || !keys.insert(*key)) {
				return Err(invalid("class index references an absent or duplicate field"));
			}
			if let IndexScope::Effective {
				graph,
			} = index.scope
				&& !graph_ids.contains(&graph)
			{
				return Err(invalid("effective index references an absent graph"));
			}
			if index.state == ClassIndexState::Ready && index.physical.is_none() {
				return Err(invalid("ready class index has no physical binding"));
			}
			if index.state == ClassIndexState::Ready && index.build_revision == 0 {
				return Err(invalid("ready class index has no published build revision"));
			}
			if let Some(binding) = index.physical {
				validate_binding(binding)?;
			}
			if let Some(binding) = index.delta {
				validate_binding(binding)?;
			}
		}
		previous_class = Some(class.id);
	}

	Ok(())
}

/// Validates one retained feed's lifecycle and source before examining
/// segments.
///
/// # Errors
/// Returns the first existing catalog inconsistency in the original check
/// order.
fn validate_changefeed_entry(
	catalog: &DatabaseCatalog,
	entry: &super::ChangefeedCatalogEntry,
	feed_ids: &mut BTreeSet<crate::changefeed::ChangefeedId>,
	feed_sources: &mut BTreeSet<crate::changefeed::ChangefeedSource>,
) -> FerrosGrynnResult<bool> {
	let first =
		entry.epochs.first().ok_or_else(|| invalid("changefeed capture epochs are empty"))?;
	let last = entry.epochs.last().ok_or_else(|| invalid("changefeed capture epochs are empty"))?;
	if first.started_at().get() < entry.defined_at
		|| last.started_at().get() != entry.published_at
		|| last.revision() != entry.version
		|| entry.epochs.iter().any(|epoch| epoch.revision() > entry.version)
		|| entry.epochs.windows(2).any(|pair| pair[0].started_at() >= pair[1].started_at())
		|| entry.epochs.windows(2).any(|pair| pair[0].revision() > pair[1].revision())
	{
		return Err(invalid("changefeed capture epoch bounds are invalid"));
	}
	let final_configuration =
		(entry.state == crate::changefeed::ChangefeedState::Active).then_some(entry.config);
	if last.configuration() != final_configuration {
		return Err(invalid("changefeed final capture epoch disagrees with lifecycle"));
	}
	let below_clock = match entry.source {
		crate::changefeed::ChangefeedSource::Class(id) => {
			u64::from(id.get()) < catalog.next_schema_id
		}
		crate::changefeed::ChangefeedSource::Graph(id) => id.get() < catalog.next_graph_id,
	};
	if !below_clock {
		return Err(invalid("retained changefeed source exceeds its owner identity clock"));
	}
	if entry.name.as_str().is_empty() || entry.name.as_str().contains('\0') {
		return Err(invalid("retained changefeed source name is invalid"));
	}
	if !feed_ids.insert(entry.id) || !feed_sources.insert(entry.source) {
		return Err(invalid("changefeed catalog has duplicate identities or sources"));
	}
	if entry.defined_at > entry.published_at || entry.purged_through > entry.last_stamp {
		return Err(invalid("changefeed catalog bounds are reversed"));
	}
	if entry.purged_through == 0 && entry.purged_time_ns != 0
		|| entry.purged_through != 0 && entry.purged_time_ns > catalog.committed_at_ns
		|| jiff::Timestamp::from_nanosecond(entry.purged_time_ns).is_err()
	{
		return Err(invalid("changefeed purge time contradicts its trusted bounds"));
	}
	if (entry.state == crate::changefeed::ChangefeedState::Active) != entry.closed_at.is_none()
		|| entry.closed_at.is_some() != entry.closed_time_ns.is_some()
	{
		return Err(invalid("changefeed lifecycle and close stamp disagree"));
	}
	if entry.closed_time_ns.is_some_and(|time| {
		time > catalog.committed_at_ns || jiff::Timestamp::from_nanosecond(time).is_err()
	}) {
		return Err(invalid("changefeed close time contradicts the database clock"));
	}
	let partial = entry.retained_page != 0 || entry.retained_groups != 0;
	if partial
		&& !entry.segments.first().is_some_and(|segment| {
			entry.retained_page > 1
				&& entry.retained_page < segment.header.pages
				&& entry.retained_groups > 0
				&& entry.retained_groups < segment.header.commits
				&& entry.purged_through >= segment.header.first_stamp
				&& entry.purged_through < segment.header.last_stamp
				&& entry.purged_time_ns >= segment.header.first_time
				&& entry.purged_time_ns <= segment.header.last_time
		}) {
		return Err(invalid("changefeed retained prefix position is invalid"));
	}
	if entry
		.closed_at
		.is_some_and(|closed| closed < entry.defined_at || closed > entry.published_at)
	{
		return Err(invalid("changefeed close stamp is outside descriptor bounds"));
	}
	if entry.state == crate::changefeed::ChangefeedState::Active {
		let present = match entry.source {
			crate::changefeed::ChangefeedSource::Class(id) => {
				catalog.classes.iter().any(|class| class.id == id)
			}
			crate::changefeed::ChangefeedSource::Graph(id) => {
				catalog.graphs.iter().any(|graph| graph.id == id)
			}
		};
		if !present {
			return Err(invalid("active changefeed owner is absent"));
		}
	}
	Ok(partial)
}

/// Validates one feed's ordered history while sharing file identities across
/// owners.
///
/// # Errors
/// Returns the first existing header, identity, boundary or segment-order
/// error.
fn validate_changefeed_segments(
	catalog: &DatabaseCatalog,
	entry: &super::ChangefeedCatalogEntry,
	partial: bool,
	feed_files: &mut BTreeSet<crate::storage::types::DataFileId>,
	feed_segments: &mut BTreeSet<uuid::Uuid>,
) -> FerrosGrynnResult<()> {
	let mut previous_segment = None;
	for (ordinal, segment) in entry.segments.iter().enumerate() {
		let header = segment.header;
		header.validate()?;
		if segment.file.kind() != Some(DataFileKind::Changefeed)
			|| !feed_files.insert(segment.file)
			|| !feed_segments.insert(header.segment)
			|| header.database != catalog.database_uuid
			|| header.owner != crate::storage::HistoryOwner::Changefeed(entry.id)
			|| (!partial || ordinal != 0) && header.first_stamp <= entry.purged_through
			|| header.first_stamp < entry.defined_at
			|| header.last_stamp > entry.last_stamp
			|| header.last_time > catalog.committed_at_ns
			|| entry.purged_through != 0
				&& (!partial || ordinal != 0)
				&& header.first_time < entry.purged_time_ns
		{
			return Err(invalid("changefeed segment identities or catalog bounds disagree"));
		}
		if previous_segment
			.is_some_and(|(stamp, time)| stamp >= header.first_stamp || time > header.first_time)
		{
			return Err(invalid("changefeed segments overlap or are out of order"));
		}
		previous_segment = Some((header.last_stamp, header.last_time));
	}
	Ok(())
}

/// Validates custom distance.
fn validate_custom_distance(
	distance: VectorDistance,
	functions: &BTreeMap<crate::datastore::FunctionId, &super::FunctionCatalogEntry>,
) -> FerrosGrynnResult<()> {
	let VectorDistance::Custom {
		function,
		version,
	} = distance
	else {
		return Ok(());
	};
	let definition = functions
		.get(&function)
		.ok_or_else(|| invalid("custom vector distance references an absent function"))?;
	if definition.version != version
		|| definition.effect != FunctionEffect::Pure
		|| !definition.cost
	{
		return Err(invalid(
			"custom vector distance requires its exact PURE COST function version",
		));
	}
	Ok(())
}

/// Returns the internal graph name result.
fn internal_graph_name(owner: crate::graph::GraphId, child: crate::graph::GraphId) -> String {
	format!("$fg$build_{:016x}_{:016x}", owner.get(), child.get())
}

/// Validates binding.
fn validate_binding(binding: PhysicalIndexBinding) -> FerrosGrynnResult<()> {
	validate_binding_kind(binding, DataFileKind::Index)
}

/// Validates a derived binding against exactly its declared physical family.
fn validate_binding_kind(
	binding: PhysicalIndexBinding,
	kind: DataFileKind,
) -> FerrosGrynnResult<()> {
	if binding.file_id.kind() != Some(kind) || binding.root == PageNo::NULL {
		return Err(invalid("physical index binding is invalid"));
	}
	Ok(())
}

/// Inserts schema.
fn insert_schema(
	seen: &mut BTreeSet<SchemaId>,
	id: SchemaId,
	next_schema_id: u64,
) -> FerrosGrynnResult<()> {
	if u64::from(id.get()) >= next_schema_id || !seen.insert(id) {
		return Err(invalid("schema identity is duplicated or beyond its clock"));
	}
	Ok(())
}
