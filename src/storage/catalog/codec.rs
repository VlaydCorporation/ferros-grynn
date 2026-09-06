#[allow(clippy::wildcard_imports, reason = "catalog layers use their facade format contract")]
use super::*;

/// Builds a sealed checksummed page chain for one logical payload.
pub(super) fn encode_page_chain(
	kind: CatalogKind,
	root: PageNo,
	continuation_start: PageNo,
	payload: &[u8],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<Vec<(PageNo, AlignedPage)>> {
	validate_catalog_size(payload.len())?;
	let page_count = payload.len().div_ceil(CATALOG_PAYLOAD_BYTES).max(1);
	let total_len = u32::try_from(payload.len())
		.map_err(|_error| invalid_catalog("catalog payload exceeds u32"))?;
	let mut pages = Vec::with_capacity(page_count);
	for index in 0..page_count {
		let index_u64 = u64::try_from(index)
			.map_err(|_error| invalid_catalog("catalog page index exceeds u64"))?;
		let page_no = if index == 0 {
			root
		} else {
			continuation_start
				.get()
				.checked_add(index_u64 - 1)
				.map(PageNo::new)
				.ok_or_else(|| invalid_catalog("catalog page number overflows u64"))?
		};
		let next_page = if index + 1 == page_count {
			PageNo::NULL
		} else {
			continuation_start
				.get()
				.checked_add(index_u64)
				.map(PageNo::new)
				.ok_or_else(|| invalid_catalog("catalog next-page number overflows u64"))?
		};
		let start = index
			.checked_mul(CATALOG_PAYLOAD_BYTES)
			.ok_or_else(|| invalid_catalog("catalog chunk offset overflows usize"))?;
		let end = start.saturating_add(CATALOG_PAYLOAD_BYTES).min(payload.len());
		let chunk = &payload[start..end];
		let chunk_len = u32::try_from(chunk.len())
			.map_err(|_error| invalid_catalog("catalog chunk exceeds u32"))?;
		let mut page =
			AlignedPage::new(PageType::SchemaRoot, PageFlags::empty(), page_lsn, page_no);
		let bytes = page.as_bytes_mut();
		bytes[PAGE_HEADER_SIZE..CATALOG_KIND_OFFSET].copy_from_slice(&CATALOG_PAGE_MAGIC);
		bytes[CATALOG_KIND_OFFSET] = kind.get();
		bytes[CATALOG_VERSION_OFFSET] = CATALOG_PAGE_VERSION;
		bytes[CATALOG_NEXT_PAGE_OFFSET..CATALOG_CHUNK_LEN_OFFSET]
			.copy_from_slice(&next_page.get().to_le_bytes());
		bytes[CATALOG_CHUNK_LEN_OFFSET..CATALOG_TOTAL_LEN_OFFSET]
			.copy_from_slice(&chunk_len.to_le_bytes());
		bytes[CATALOG_TOTAL_LEN_OFFSET..CATALOG_RESERVED_B_OFFSET]
			.copy_from_slice(&total_len.to_le_bytes());
		bytes[CATALOG_PAYLOAD_OFFSET..CATALOG_PAYLOAD_OFFSET + chunk.len()].copy_from_slice(chunk);
		page.seal()?;
		pages.push((page_no, page));
	}
	Ok(pages)
}

/// Returns the first page after `count` consecutive continuation pages.
pub(super) fn next_tail_page(first: PageNo, count: usize) -> FerrosGrynnResult<PageNo> {
	first
		.get()
		.checked_add(
			u64::try_from(count)
				.map_err(|_error| invalid_catalog("catalog page count exceeds u64"))?,
		)
		.map(PageNo::new)
		.ok_or_else(|| invalid_catalog("catalog continuation range overflows u64"))
}

/// Loads a bounded page chain without trusting persisted lengths or links.
pub(super) fn decode_page_chain(
	read_page: &mut impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	expected_kind: CatalogKind,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut page_no = root;
	let mut visited = BTreeSet::new();
	let mut output = Vec::new();
	let mut expected_total = None;
	loop {
		if page_no == PageNo::NULL || !visited.insert(page_no) {
			return Err(invalid_catalog("catalog page chain is null or cyclic"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		let header = page.validate(page_no)?;
		if header.page_type() != PageType::SchemaRoot || !header.flags().is_empty() {
			return Err(invalid_catalog("catalog chain contains an invalid page type or flags"));
		}
		let bytes = page.as_bytes();
		if bytes[PAGE_HEADER_SIZE..CATALOG_KIND_OFFSET] != CATALOG_PAGE_MAGIC
			|| CatalogKind::from_raw(bytes[CATALOG_KIND_OFFSET]) != Some(expected_kind)
			|| bytes[CATALOG_VERSION_OFFSET] != CATALOG_PAGE_VERSION
		{
			return Err(invalid_catalog("catalog page marker, kind, or version is invalid"));
		}
		validate_zero(
			&bytes[CATALOG_RESERVED_A_OFFSET..CATALOG_NEXT_PAGE_OFFSET],
			"catalog page reserved bytes",
		)?;
		validate_zero(
			&bytes[CATALOG_RESERVED_B_OFFSET..CATALOG_PAYLOAD_OFFSET],
			"catalog page reserved bytes",
		)?;
		let next_page = PageNo::new(read_u64(bytes, CATALOG_NEXT_PAGE_OFFSET));
		let chunk_len = usize::try_from(read_u32(bytes, CATALOG_CHUNK_LEN_OFFSET))
			.map_err(|_error| invalid_catalog("catalog chunk length does not fit usize"))?;
		let total_len = usize::try_from(read_u32(bytes, CATALOG_TOTAL_LEN_OFFSET))
			.map_err(|_error| invalid_catalog("catalog total length does not fit usize"))?;
		validate_catalog_size(total_len)?;
		if chunk_len > CATALOG_PAYLOAD_BYTES
			|| expected_total.is_some_and(|value| value != total_len)
		{
			return Err(invalid_catalog("catalog page contains inconsistent lengths"));
		}
		if expected_total.is_none() {
			output.reserve(total_len);
			expected_total = Some(total_len);
		}
		let remaining = total_len.saturating_sub(output.len());
		if chunk_len > remaining {
			return Err(invalid_catalog("catalog chunks exceed the declared total length"));
		}
		output
			.extend_from_slice(&bytes[CATALOG_PAYLOAD_OFFSET..CATALOG_PAYLOAD_OFFSET + chunk_len]);
		validate_zero(&bytes[CATALOG_PAYLOAD_OFFSET + chunk_len..], "catalog page trailing bytes")?;
		if next_page == PageNo::NULL {
			break;
		}
		if output.len() == total_len {
			return Err(invalid_catalog("catalog page chain continues after its declared payload"));
		}
		page_no = next_page;
	}
	if output.len() != expected_total.unwrap_or(0) {
		return Err(invalid_catalog("catalog page chain ended before its declared payload"));
	}
	Ok(output)
}

/// Appends one compact property location used by graph-catalog metadata.
pub(super) fn encode_property_location(
	output: &mut Vec<u8>,
	location: Option<PropertyLocation>,
) -> FerrosGrynnResult<()> {
	let (page, slot, length) = match location {
		None => (PageNo::NULL, 0, 0),
		Some(PropertyLocation::Inline {
			page_no,
			slot,
			length,
		}) => (page_no, u32::from(slot), length),
		Some(PropertyLocation::Large {
			first_page,
			length,
		}) => (first_page, LARGE_PROPERTY_SLOT, length),
	};
	if (location.is_some() && (page == PageNo::NULL || length == 0))
		|| (location.is_none() && (page != PageNo::NULL || slot != 0 || length != 0))
	{
		return Err(invalid_catalog("graph property location is invalid"));
	}
	output.extend_from_slice(&page.get().to_le_bytes());
	output.extend_from_slice(&slot.to_le_bytes());
	output.extend_from_slice(&length.to_le_bytes());
	Ok(())
}

/// Reads one compact graph property location.
pub(super) fn decode_property_location(
	decoder: &mut CatalogDecoder<'_>,
) -> FerrosGrynnResult<Option<PropertyLocation>> {
	let page = PageNo::new(decoder.read_u64()?);
	let slot = decoder.read_u32()?;
	let length = decoder.read_u32()?;
	if page == PageNo::NULL {
		return if slot == 0 && length == 0 {
			Ok(None)
		} else {
			Err(invalid_catalog("null graph property page has non-zero fields"))
		};
	}
	if length == 0 {
		return Err(invalid_catalog("graph property record has zero length"));
	}
	if slot == LARGE_PROPERTY_SLOT {
		Ok(Some(PropertyLocation::Large {
			first_page: page,
			length,
		}))
	} else {
		Ok(Some(PropertyLocation::Inline {
			page_no: page,
			slot: u16::try_from(slot)
				.map_err(|_error| invalid_catalog("graph property slot exceeds u16"))?,
			length,
		}))
	}
}

/// Appends one bounded UTF-8 schema string.
pub(super) fn write_catalog_string(
	output: &mut Vec<u8>,
	value: &str,
	field: &str,
) -> FerrosGrynnResult<()> {
	if value.is_empty() || value.contains('\0') || value.len() > MAX_SCHEMA_NAME_BYTES {
		return Err(invalid_catalog(format!("{field} violates its format bound")));
	}
	let length = u16::try_from(value.len())
		.map_err(|_error| invalid_catalog(format!("{field} exceeds u16")))?;
	output.extend_from_slice(&length.to_le_bytes());
	output.extend_from_slice(value.as_bytes());
	Ok(())
}

/// Reads one bounded UTF-8 schema string.
pub(super) fn read_catalog_string(
	decoder: &mut CatalogDecoder<'_>,
	field: &str,
) -> FerrosGrynnResult<String> {
	let length = usize::from(decoder.read_u16()?);
	if length == 0 || length > MAX_SCHEMA_NAME_BYTES {
		return Err(invalid_catalog(format!("{field} violates its format bound")));
	}
	let value = decoder.read_utf8(length, field)?;
	if value.contains('\0') {
		return Err(invalid_catalog(format!("{field} contains NUL")));
	}
	Ok(value.to_owned())
}

/// Encodes one recursive runtime type without a serialization dependency.
pub(super) fn encode_type(
	output: &mut Vec<u8>,
	value: &Type,
	depth: usize,
) -> FerrosGrynnResult<()> {
	if depth > MAX_TYPE_DEPTH {
		return Err(invalid_catalog("table type nesting exceeds its format limit"));
	}
	let tag = match value {
		Type::Any => SchemaTypeTag::Any,
		Type::None => SchemaTypeTag::None,
		Type::Null => SchemaTypeTag::Null,
		Type::Bool => SchemaTypeTag::Bool,
		Type::Number => SchemaTypeTag::Number,
		Type::Int => SchemaTypeTag::Int,
		Type::Float => SchemaTypeTag::Float,
		Type::Decimal => SchemaTypeTag::Decimal,
		Type::String => SchemaTypeTag::String,
		Type::Duration => SchemaTypeTag::Duration,
		Type::DateTime => SchemaTypeTag::DateTime,
		Type::Ulid => SchemaTypeTag::Ulid,
		Type::Uuid => SchemaTypeTag::Uuid,
		Type::Array(_) => SchemaTypeTag::Array,
		Type::Set(_) => SchemaTypeTag::Set,
		Type::Object => SchemaTypeTag::Object,
		Type::Geometry => SchemaTypeTag::Geometry,
		Type::Vector(_) => SchemaTypeTag::Vector,
		Type::Bytes => SchemaTypeTag::Bytes,
		Type::Record => SchemaTypeTag::Record,
		Type::Regex => SchemaTypeTag::Regex,
		Type::Range(_) => SchemaTypeTag::Range,
		Type::Atom(_) => SchemaTypeTag::Atom,
		Type::Path => SchemaTypeTag::Path,
		Type::Graph(_) => SchemaTypeTag::Graph,
		Type::Option(_) => SchemaTypeTag::Option,
		Type::Union(_) => SchemaTypeTag::Union,
		Type::RecordShape(_) => SchemaTypeTag::RecordShape,
		Type::Closure {
			..
		} => SchemaTypeTag::Closure,
	};
	output.push(tag.get());
	match value {
		Type::Array(inner) | Type::Set(inner) | Type::Range(inner) | Type::Option(inner) => {
			encode_type(output, inner, depth + 1)?;
		}
		Type::Vector(kind) => output.push(vector_kind_byte(*kind)),
		Type::Atom(kind) => output.push(atom_kind_byte(*kind)),
		Type::Graph(kind) => output.push(graph_kind_byte(*kind)),
		Type::Union(values) => {
			write_type_member_count(output, values.len())?;
			for value in values {
				encode_type(output, value, depth + 1)?;
			}
		}
		Type::RecordShape(fields) => {
			write_type_member_count(output, fields.len())?;
			for (name, value) in fields {
				write_catalog_string(output, name.as_str(), "record-shape field")?;
				encode_type(output, value, depth + 1)?;
			}
		}
		Type::Closure {
			parameters,
			result,
		} => {
			write_type_member_count(output, parameters.len())?;
			for parameter in parameters {
				encode_type(output, parameter, depth + 1)?;
			}
			encode_type(output, result, depth + 1)?;
		}
		_ => {}
	}
	Ok(())
}

/// Decodes one recursive runtime type with bounds checked before allocation.
pub(super) fn decode_type(
	decoder: &mut CatalogDecoder<'_>,
	depth: usize,
) -> FerrosGrynnResult<Type> {
	if depth > MAX_TYPE_DEPTH {
		return Err(invalid_catalog("table type nesting exceeds its format limit"));
	}
	let tag = SchemaTypeTag::from_raw(decoder.read_u8()?)
		.ok_or_else(|| invalid_catalog("table schema contains an unknown type tag"))?;
	Ok(match tag {
		SchemaTypeTag::Any => Type::Any,
		SchemaTypeTag::None => Type::None,
		SchemaTypeTag::Null => Type::Null,
		SchemaTypeTag::Bool => Type::Bool,
		SchemaTypeTag::Number => Type::Number,
		SchemaTypeTag::Int => Type::Int,
		SchemaTypeTag::Float => Type::Float,
		SchemaTypeTag::Decimal => Type::Decimal,
		SchemaTypeTag::String => Type::String,
		SchemaTypeTag::Duration => Type::Duration,
		SchemaTypeTag::DateTime => Type::DateTime,
		SchemaTypeTag::Ulid => Type::Ulid,
		SchemaTypeTag::Uuid => Type::Uuid,
		SchemaTypeTag::Array => Type::Array(Box::new(decode_type(decoder, depth + 1)?)),
		SchemaTypeTag::Set => Type::Set(Box::new(decode_type(decoder, depth + 1)?)),
		SchemaTypeTag::Object => Type::Object,
		SchemaTypeTag::Geometry => Type::Geometry,
		SchemaTypeTag::Vector => Type::Vector(decode_vector_kind(decoder.read_u8()?)?),
		SchemaTypeTag::Bytes => Type::Bytes,
		SchemaTypeTag::Record => Type::Record,
		SchemaTypeTag::Regex => Type::Regex,
		SchemaTypeTag::Range => Type::Range(Box::new(decode_type(decoder, depth + 1)?)),
		SchemaTypeTag::Atom => Type::Atom(decode_atom_kind(decoder.read_u8()?)?),
		SchemaTypeTag::Path => Type::Path,
		SchemaTypeTag::Graph => Type::Graph(decode_graph_kind(decoder.read_u8()?)?),
		SchemaTypeTag::Option => Type::Option(Box::new(decode_type(decoder, depth + 1)?)),
		SchemaTypeTag::Union => {
			let count = read_type_member_count(decoder)?;
			let mut values = Vec::with_capacity(count);
			for _ in 0..count {
				values.push(decode_type(decoder, depth + 1)?);
			}
			Type::Union(values)
		}
		SchemaTypeTag::RecordShape => {
			let count = read_type_member_count(decoder)?;
			let mut fields = Vec::with_capacity(count);
			for _ in 0..count {
				fields.push((
					read_catalog_string(decoder, "record-shape field")?.into(),
					decode_type(decoder, depth + 1)?,
				));
			}
			Type::RecordShape(fields)
		}
		SchemaTypeTag::Closure => {
			let count = read_type_member_count(decoder)?;
			let mut parameters = Vec::with_capacity(count);
			for _ in 0..count {
				parameters.push(decode_type(decoder, depth + 1)?);
			}
			Type::Closure {
				parameters,
				result: Box::new(decode_type(decoder, depth + 1)?),
			}
		}
	})
}

pub(super) fn write_type_member_count(output: &mut Vec<u8>, count: usize) -> FerrosGrynnResult<()> {
	if count > MAX_TYPE_MEMBERS {
		return Err(invalid_catalog("table type member count exceeds its format limit"));
	}
	output.extend_from_slice(
		&u16::try_from(count)
			.map_err(|_error| invalid_catalog("table type member count exceeds u16"))?
			.to_le_bytes(),
	);
	Ok(())
}

pub(super) fn read_type_member_count(decoder: &mut CatalogDecoder<'_>) -> FerrosGrynnResult<usize> {
	let count = usize::from(decoder.read_u16()?);
	if count > MAX_TYPE_MEMBERS {
		return Err(invalid_catalog("table type member count exceeds its format limit"));
	}
	Ok(count)
}

/// Validates ordering and graph-id clock invariants.
pub(super) fn validate_graph_catalog(catalog: &DatabaseCatalog) -> FerrosGrynnResult<()> {
	if catalog.next_graph_id == 0 {
		return Err(invalid_catalog("graph identity zero is reserved for system metadata"));
	}
	if catalog.next_schema_id == 0 || catalog.next_schema_id > u64::from(u32::MAX) {
		return Err(invalid_catalog("schema identity clock is outside its u32 domain"));
	}
	let mut previous = GraphId::SYSTEM;
	let mut graph_ids = BTreeSet::new();
	let mut root_graph_ids = BTreeSet::new();
	let mut schema_ids = BTreeSet::new();
	let mut graph_roots = BTreeMap::new();
	let mut graph_records = BTreeSet::new();
	let mut graph_names = BTreeSet::new();
	let mut nested_slots = BTreeSet::new();
	for graph in &catalog.graphs {
		let root_schema = graph.parent_graph_id.is_none();
		if root_schema {
			validate_schema_entry(
				graph.schema_id,
				graph.schema_version,
				catalog.next_schema_id,
				&mut schema_ids,
				"graph",
			)?;
			validate_schema_entry(
				graph.label_index.schema_id,
				graph.label_index.schema_version,
				catalog.next_schema_id,
				&mut schema_ids,
				"label system index",
			)?;
			validate_schema_entry(
				graph.adjacency_index.schema_id,
				graph.adjacency_index.schema_version,
				catalog.next_schema_id,
				&mut schema_ids,
				"adjacency system index",
			)?;
		} else if graph.schema_id != 0
			|| graph.schema_version != 0
			|| graph.label_index.schema_id != 0
			|| graph.label_index.schema_version != 0
			|| graph.adjacency_index.schema_id != 0
			|| graph.adjacency_index.schema_version != 0
		{
			return Err(invalid_catalog(
				"nested graph address space contains root schema metadata",
			));
		}
		let root_graph_id = match (graph.parent_graph_id, graph.subgraph_slot, graph.parent_atom) {
			(None, None, None) => {
				root_graph_ids.insert(graph.graph_id);
				Some(graph.graph_id)
			}
			(Some(parent), Some(slot), Some(_atom)) if slot != 0 && parent < graph.graph_id => {
				graph_roots.get(&parent).copied().filter(|root| nested_slots.insert((*root, slot)))
			}
			_ => None,
		}
		.ok_or_else(|| invalid_catalog("graph catalog entry violates a format-v1 bound"))?;
		if graph.graph_id == GraphId::SYSTEM || graph.graph_id <= previous {
			return Err(invalid_catalog("graph catalog entries are not in strict id order"));
		}
		if graph.graph_id.get() >= catalog.next_graph_id
			|| graph.record_id.is_empty()
			|| graph.record_id.len() > MAX_GRAPH_RECORD_ID_BYTES
			|| graph.name.is_empty()
			|| graph.name.len() > MAX_GRAPH_NAME_BYTES
			|| graph.name.contains('\0')
			|| !graph_ids.insert(graph.graph_id)
			|| !graph_records.insert(graph.record_id.as_str())
			|| (graph.parent_graph_id.is_none() && !graph_names.insert(graph.name.as_str()))
			|| graph.primary_index_file_id.kind() != Some(DataFileKind::Index)
			|| graph.primary_index_root == PageNo::NULL
		{
			return Err(invalid_catalog("graph catalog entry violates a format-v1 bound"));
		}
		graph_roots.insert(graph.graph_id, root_graph_id);
		previous = graph.graph_id;
	}
	let mut property_names = BTreeSet::new();
	for property in &catalog.property_keys {
		if property.is_empty()
			|| property.contains('\0')
			|| property.len() > MAX_SCHEMA_NAME_BYTES
			|| !property_names.insert(property.as_str())
		{
			return Err(invalid_catalog("property dictionary violates a format-v1 bound"));
		}
	}
	let mut previous_table = None;
	for table in &catalog.tables {
		validate_schema_entry(
			table.schema_id,
			table.schema_version,
			catalog.next_schema_id,
			&mut schema_ids,
			"table",
		)?;
		validate_schema_entry(
			table.primary_index.schema_id,
			table.primary_index.schema_version,
			catalog.next_schema_id,
			&mut schema_ids,
			"primary system index",
		)?;
		if table.name.is_empty()
			|| table.name.contains('\0')
			|| table.name.len() > MAX_SCHEMA_NAME_BYTES
			|| previous_table.is_some_and(|previous: &str| previous >= table.name.as_str())
			|| !root_graph_ids.contains(&table.graph_id)
			|| table.fields.len() > MAX_TYPE_MEMBERS
			|| table.indexes.len() > MAX_TYPE_MEMBERS
		{
			return Err(invalid_catalog("table catalog entry violates a format-v1 bound"));
		}
		let mut field_names = BTreeSet::new();
		for field in &table.fields {
			validate_schema_entry(
				field.schema_id,
				field.schema_version,
				catalog.next_schema_id,
				&mut schema_ids,
				"table field",
			)?;
			let property_index = usize::try_from(field.property_key)
				.map_err(|_error| invalid_catalog("property key does not fit usize"))?;
			if field.name.is_empty()
				|| field.name.contains('\0')
				|| field.name.len() > MAX_SCHEMA_NAME_BYTES
				|| !field_names.insert(field.name.as_str())
				|| catalog.property_keys.get(property_index).map(String::as_str)
					!= Some(field.name.as_str())
			{
				return Err(invalid_catalog("table field violates its property-key contract"));
			}
		}
		let mut previous_index = None;
		for index in &table.indexes {
			validate_schema_entry(
				index.schema_id,
				index.schema_version,
				catalog.next_schema_id,
				&mut schema_ids,
				"table index",
			)?;
			if index.name.is_empty()
				|| index.name.contains('\0')
				|| index.name.len() > MAX_SCHEMA_NAME_BYTES
				|| previous_index.is_some_and(|previous: &str| previous >= index.name.as_str())
				|| index.fields.is_empty()
				|| index.fields.len() > MAX_TYPE_MEMBERS
				|| index.file_id.kind() != Some(DataFileKind::Index)
				|| index.root == PageNo::NULL
			{
				return Err(invalid_catalog("table index violates a format-v1 bound"));
			}
			let mut index_fields = BTreeSet::new();
			for property_key in &index.fields {
				if !index_fields.insert(*property_key)
					|| !table.fields.iter().any(|field| field.property_key == *property_key)
				{
					return Err(invalid_catalog(
						"table index references an unknown or duplicate field",
					));
				}
			}
			previous_index = Some(index.name.as_str());
		}
		previous_table = Some(table.name.as_str());
	}
	let mut previous_parameter = None;
	for parameter in &catalog.parameters {
		validate_schema_entry(
			parameter.schema_id,
			parameter.schema_version,
			catalog.next_schema_id,
			&mut schema_ids,
			"parameter",
		)?;
		if parameter.name.is_empty()
			|| parameter.name.contains('\0')
			|| parameter.name.len() > MAX_SCHEMA_NAME_BYTES
			|| previous_parameter.is_some_and(|previous: &str| previous >= parameter.name.as_str())
			|| parameter.value.is_empty()
			|| parameter.table_names.len() > MAX_TYPE_MEMBERS
			|| parameter.time_zones.len() > MAX_TYPE_MEMBERS
			|| !valid_string_dictionary(&parameter.table_names)
			|| !valid_string_dictionary(&parameter.time_zones)
		{
			return Err(invalid_catalog("parameter catalog entry violates a format-v1 bound"));
		}
		previous_parameter = Some(parameter.name.as_str());
	}
	Ok(())
}

fn validate_schema_entry(
	id: u32,
	version: u32,
	next_id: u64,
	seen: &mut BTreeSet<u32>,
	kind: &str,
) -> FerrosGrynnResult<()> {
	if id == 0 || id == u32::MAX || version == 0 || u64::from(id) >= next_id || !seen.insert(id) {
		return Err(invalid_catalog(format!(
			"{kind} schema identity or version violates the catalog clock"
		)));
	}
	Ok(())
}

fn valid_string_dictionary(values: &[String]) -> bool {
	let mut unique = BTreeSet::new();
	values.iter().all(|value| {
		!value.is_empty()
			&& !value.contains('\0')
			&& value.len() <= MAX_SCHEMA_NAME_BYTES
			&& unique.insert(value.as_str())
	})
}

/// Validates ordering, roles, and the mandatory system-file registration.
pub(super) fn validate_file_catalog(catalog: &FileCatalog) -> FerrosGrynnResult<()> {
	if catalog.next_file_instance_id == 0
		|| catalog.next_file_instance_id > DataFileId::EXHAUSTED_INSTANCE_ID
	{
		return Err(invalid_catalog("file instance clock is outside its 56-bit domain"));
	}
	let mut previous = None;
	let mut system_catalogs = 0_u8;
	for file in &catalog.files {
		let key = (file.graph_id, file.file_id);
		if previous.is_some_and(|previous| previous >= key) {
			return Err(invalid_catalog("file catalog entries are not in strict identity order"));
		}
		let graph_metadata = file.file_id.kind() == Some(DataFileKind::GraphSuperblock);
		if graph_metadata != (file.role == DataFileRole::GraphSuperblock) {
			return Err(invalid_catalog("file catalog role does not match DataFileKind"));
		}
		if file.file_id == SYSTEM_CATALOG_FILE_ID {
			if file.graph_id != GraphId::SYSTEM
				|| file.role != DataFileRole::WalBackfilled
				|| file.path.as_str() != SYSTEM_CATALOG_PATH
			{
				return Err(invalid_catalog("system catalog registration is not canonical"));
			}
			system_catalogs = system_catalogs.saturating_add(1);
		}
		previous = Some(key);
		if file.file_id.instance_id() >= catalog.next_file_instance_id {
			return Err(invalid_catalog("file catalog would reuse a physical-file identity"));
		}
	}
	if system_catalogs != 1 {
		return Err(invalid_catalog("file catalog must contain exactly one system catalog"));
	}
	Ok(())
}

/// Maps runtime graph families to stable catalog bytes.
pub(super) const fn graph_kind_byte(kind: GraphKind) -> u8 {
	match kind {
		GraphKind::Graph => 0,
		GraphKind::Tree => 1,
		GraphKind::RootedTree => 2,
		GraphKind::Forest => 3,
		GraphKind::Dag => 4,
		GraphKind::Hypergraph => 5,
		GraphKind::Metagraph => 6,
		GraphKind::Subgraph => 7,
	}
}

/// Rejects catalog graph-family bytes not assigned by format v1.
pub(super) fn decode_graph_kind(value: u8) -> FerrosGrynnResult<GraphKind> {
	match value {
		0 => Ok(GraphKind::Graph),
		1 => Ok(GraphKind::Tree),
		2 => Ok(GraphKind::RootedTree),
		3 => Ok(GraphKind::Forest),
		4 => Ok(GraphKind::Dag),
		5 => Ok(GraphKind::Hypergraph),
		6 => Ok(GraphKind::Metagraph),
		7 => Ok(GraphKind::Subgraph),
		_ => Err(invalid_catalog("graph catalog contains an unknown GraphKind")),
	}
}

/// Maps table applicability to stable catalog bytes.
pub(super) const fn table_kind_byte(kind: TableKind) -> u8 {
	match kind {
		TableKind::Any => 0,
		TableKind::Normal => 1,
		TableKind::Relation => 2,
	}
}

/// Rejects unassigned table applicability bytes.
pub(super) fn decode_table_kind(value: u8) -> FerrosGrynnResult<TableKind> {
	match value {
		0 => Ok(TableKind::Any),
		1 => Ok(TableKind::Normal),
		2 => Ok(TableKind::Relation),
		_ => Err(invalid_catalog("table catalog contains an unknown TableKind")),
	}
}

/// Maps scalar-index uniqueness to stable catalog bytes.
pub(super) const fn table_index_kind_byte(kind: TableIndexKind) -> u8 {
	match kind {
		TableIndexKind::Standard => 0,
		TableIndexKind::Unique => 1,
	}
}

/// Rejects unassigned scalar-index kind bytes.
pub(super) fn decode_table_index_kind(value: u8) -> FerrosGrynnResult<TableIndexKind> {
	match value {
		0 => Ok(TableIndexKind::Standard),
		1 => Ok(TableIndexKind::Unique),
		_ => Err(invalid_catalog("table catalog contains an unknown TableIndexKind")),
	}
}

/// Maps atom kinds to stable schema bytes.
pub(super) const fn atom_kind_byte(kind: AtomKind) -> u8 {
	match kind {
		AtomKind::Node => 0,
		AtomKind::Edge => 1,
	}
}

/// Rejects unassigned atom-kind bytes.
pub(super) fn decode_atom_kind(value: u8) -> FerrosGrynnResult<AtomKind> {
	match value {
		0 => Ok(AtomKind::Node),
		1 => Ok(AtomKind::Edge),
		_ => Err(invalid_catalog("table schema contains an unknown AtomKind")),
	}
}

/// Maps vector element kinds to stable schema bytes.
pub(super) const fn vector_kind_byte(kind: VectorKind) -> u8 {
	match kind {
		VectorKind::F64 => 0,
		VectorKind::F32 => 1,
		VectorKind::I64 => 2,
		VectorKind::I32 => 3,
		VectorKind::I16 => 4,
	}
}

/// Rejects unassigned vector-kind bytes.
pub(super) fn decode_vector_kind(value: u8) -> FerrosGrynnResult<VectorKind> {
	match value {
		0 => Ok(VectorKind::F64),
		1 => Ok(VectorKind::F32),
		2 => Ok(VectorKind::I64),
		3 => Ok(VectorKind::I32),
		4 => Ok(VectorKind::I16),
		_ => Err(invalid_catalog("table schema contains an unknown VectorKind")),
	}
}

/// Stable file-role byte.
pub(super) const fn role_byte(role: DataFileRole) -> u8 {
	match role {
		DataFileRole::WalBackfilled => 0x01,
		DataFileRole::GraphSuperblock => 0x02,
	}
}

/// Rejects file-role bytes not assigned by format v1.
pub(super) fn decode_role(value: u8) -> FerrosGrynnResult<DataFileRole> {
	match value {
		0x01 => Ok(DataFileRole::WalBackfilled),
		0x02 => Ok(DataFileRole::GraphSuperblock),
		_ => Err(invalid_catalog("file catalog contains an unknown role")),
	}
}

/// Returns whether one slash-separated component is ambiguous on Windows.
pub(super) fn invalid_path_component(component: &str) -> bool {
	if component.is_empty()
		|| component == "."
		|| component == ".."
		|| component.ends_with(['.', ' '])
	{
		return true;
	}
	let stem = component.split('.').next().unwrap_or(component);
	let upper = stem.to_ascii_uppercase();
	matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
		|| (upper.len() == 4
			&& (upper.starts_with("COM") || upper.starts_with("LPT"))
			&& upper.as_bytes()[3].is_ascii_digit()
			&& upper.as_bytes()[3] != b'0')
}

/// Validates a catalog allocation bound before reserving memory.
pub(super) fn validate_catalog_size(length: usize) -> FerrosGrynnResult<()> {
	if length > MAX_CATALOG_BYTES {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"catalog bytes",
			MAX_CATALOG_BYTES as u64,
			length as u64,
		));
	}
	Ok(())
}

/// Rejects non-zero bytes in format-reserved regions.
pub(super) fn validate_zero(bytes: &[u8], field: &str) -> FerrosGrynnResult<()> {
	if bytes.iter().any(|byte| *byte != 0) {
		return Err(invalid_catalog(format!("{field} are non-zero")));
	}
	Ok(())
}

/// Reads one little-endian `u32` from a checked fixed offset.
pub(super) fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0_u8; size_of::<u32>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u32>()]);
	u32::from_le_bytes(value)
}

/// Reads one little-endian `u64` from a checked fixed offset.
pub(super) fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0_u8; size_of::<u64>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u64>()]);
	u64::from_le_bytes(value)
}

/// Bounded cursor used by both catalog payload decoders.
pub(super) struct CatalogDecoder<'a> {
	/// Untrusted logical payload.
	bytes: &'a [u8],
	/// Next unread byte.
	pub(super) position: usize,
}

impl<'a> CatalogDecoder<'a> {
	/// Starts at the first logical payload byte.
	pub(super) const fn new(bytes: &'a [u8]) -> Self {
		Self {
			bytes,
			position: 0,
		}
	}

	/// Validates one exact payload marker.
	pub(super) fn expect_magic(&mut self, expected: [u8; 4], kind: &str) -> FerrosGrynnResult<()> {
		if self.read_exact(expected.len())? != expected {
			return Err(invalid_catalog(format!("invalid {kind} marker")));
		}
		Ok(())
	}

	/// Validates one payload version and its reserved bytes.
	pub(super) fn expect_version(&mut self, expected: u16) -> FerrosGrynnResult<()> {
		if self.read_u16()? != expected {
			return Err(invalid_catalog("unsupported catalog payload version"));
		}
		self.expect_zero(size_of::<u16>(), "catalog payload reserved bytes")
	}

	/// Reads one byte.
	pub(super) fn read_u8(&mut self) -> FerrosGrynnResult<u8> {
		Ok(self.read_exact(1)?[0])
	}

	/// Reads one little-endian `u16`.
	pub(super) fn read_u16(&mut self) -> FerrosGrynnResult<u16> {
		let bytes: [u8; 2] = self
			.read_exact(size_of::<u16>())?
			.try_into()
			.map_err(|source| invalid_catalog(format!("catalog u16 width mismatch: {source}")))?;
		Ok(u16::from_le_bytes(bytes))
	}

	/// Reads one little-endian `u32`.
	pub(super) fn read_u32(&mut self) -> FerrosGrynnResult<u32> {
		let bytes: [u8; 4] = self
			.read_exact(size_of::<u32>())?
			.try_into()
			.map_err(|source| invalid_catalog(format!("catalog u32 width mismatch: {source}")))?;
		Ok(u32::from_le_bytes(bytes))
	}

	/// Reads one little-endian `u64`.
	pub(super) fn read_u64(&mut self) -> FerrosGrynnResult<u64> {
		let bytes: [u8; 8] = self
			.read_exact(size_of::<u64>())?
			.try_into()
			.map_err(|source| invalid_catalog(format!("catalog u64 width mismatch: {source}")))?;
		Ok(u64::from_le_bytes(bytes))
	}

	/// Reads one bounded UTF-8 field.
	pub(super) fn read_utf8(&mut self, length: usize, field: &str) -> FerrosGrynnResult<&'a str> {
		std::str::from_utf8(self.read_exact(length)?)
			.map_err(|source| invalid_catalog(format!("{field} is not UTF-8: {source}")))
	}

	/// Requires a fixed region to be canonical zero padding.
	pub(super) fn expect_zero(&mut self, length: usize, field: &str) -> FerrosGrynnResult<()> {
		validate_zero(self.read_exact(length)?, field)
	}

	/// Returns one checked byte range without allocating.
	pub(super) fn read_exact(&mut self, length: usize) -> FerrosGrynnResult<&'a [u8]> {
		let end = self
			.position
			.checked_add(length)
			.ok_or_else(|| invalid_catalog("catalog decoder offset overflows usize"))?;
		let bytes = self
			.bytes
			.get(self.position..end)
			.ok_or_else(|| invalid_catalog("catalog payload ends inside a field"))?;
		self.position = end;
		Ok(bytes)
	}

	/// Rejects trailing bytes after the declared entries.
	pub(super) fn finish(self) -> FerrosGrynnResult<()> {
		if self.position != self.bytes.len() {
			return Err(invalid_catalog("catalog payload contains trailing bytes"));
		}
		Ok(())
	}
}

/// Creates the project-wide storage corruption error.
pub(super) fn invalid_catalog(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
