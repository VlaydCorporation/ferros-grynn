//! Versioned correspondence records framed by existing slotted/overflow pages.

use std::{collections::BTreeMap, sync::Arc};

use super::{
	catalog::PhysicalIndexBinding,
	graph_index::{
		BuiltPrimaryIndex,
		PrimaryAddress,
		PrimaryIndexEntry,
		build_primary_index,
		lookup_primary_index,
		visit_primary_index,
	},
	history_codec::{CodecBudget, Decoder, append_blob, values},
	page::{AlignedPage, PageLsn, PageNo},
	property_page::{
		LARGE_CHUNK_BYTES,
		MAX_INLINE_BYTES,
		PropertyPageBuilder,
		append_large_property,
		decode_large_property,
		read_raw_property_record,
	},
};
use crate::{
	bridge::{
		BridgeDatabase,
		BridgeDatabaseId,
		BridgeDatabaseRef,
		BridgeEntityRef,
		BridgeMapping,
		BridgeMappingId,
		BridgeMappingRef,
		BridgeMappingRole,
		BridgeMappingState,
		BridgeMappingSubject,
		BridgeMirrorRequest,
	},
	datastore::BridgeRegistry,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
	values::{PropertyDictionary, RecordId, RecordIdPart, Text, Value, storage::EncodeContext},
};

/// Bounded independent record, rather than a sixteen-MiB registry blob.
const MAX_RECORD_BYTES: usize = 4 * 1024 * 1024;

/// Exact roots retained by the logical catalog; no mapping rows live in FGDC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeCatalogBinding {
	/// Logical generation qualifying immutable physical roots.
	pub(crate) generation: u64,
	/// UUID naming this immutable generation directory.
	pub(crate) directory: uuid::Uuid,
	/// Exact number of authoritative descriptor and mapping records.
	pub(crate) count: u64,
	/// Slotted authoritative record pages.
	pub(in crate::storage) heap: PhysicalIndexBinding,
	/// Logical identity index pointing to authoritative heap records.
	pub(in crate::storage) primary: PhysicalIndexBinding,
	/// Local subject and database index returning authoritative row identities.
	pub(in crate::storage) direct: PhysicalIndexBinding,
	/// External identity and role index returning authoritative row identities.
	pub(in crate::storage) reverse: PhysicalIndexBinding,
}

/// Checked heap page/slot pointer, never interpreted as a physical atom ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MappingLocator(u64);

impl MappingLocator {
	/// Constructs the owned state used by this bounded operation.
	fn new(page: PageNo, slot: u16) -> FerrosGrynnResult<Self> {
		if page.get() >= 1_u64 << 48 {
			return Err(invalid("mapping heap address exceeds 48 bits"));
		}
		Ok(Self((page.get() << 16) | u64::from(slot)))
	}

	/// One active aligned slotted page.
	fn page(self) -> PageNo {
		PageNo::new(self.0 >> 16)
	}

	/// Returns the slotted heap record ordinal.
	fn slot(self) -> u16 {
		self.0 as u16
	}
}

impl PrimaryAddress for MappingLocator {
	fn from_raw(raw: u64) -> FerrosGrynnResult<Self> {
		if raw == u64::MAX {
			return Err(invalid("reserved mapping locator"));
		}
		Ok(Self(raw))
	}

	fn raw(self) -> u64 {
		self.0
	}

	fn kind(self) -> Option<crate::graph::AtomKind> {
		None
	}

	fn order_key(self) -> u64 {
		self.0
	}
}

/// Separate page vectors permit existing generic B+trees to keep native roots.
pub(super) struct BuiltMappings {
	/// Slotted authoritative record pages.
	pub(super) heap: Vec<AlignedPage>,
	/// Logical identity index pointing to authoritative heap records.
	pub(super) primary: BuiltPrimaryIndex,
	/// Local subject and database index returning authoritative row identities.
	pub(super) direct: BuiltPrimaryIndex,
	/// External identity and role index returning authoritative row identities.
	pub(super) reverse: BuiltPrimaryIndex,
}

/// Streams the heap and three sorted indexes under one installed build budget.
pub(super) fn build_streamed(
	registry: &BridgeRegistry,
	owner: uuid::Uuid,
	root: &std::path::Path,
	directory: uuid::Uuid,
) -> FerrosGrynnResult<[super::graph_index::StreamedIndex; 4]> {
	use super::{
		graph_index::{ExternalIndexSorter, StreamedIndex},
		scoped_index_image::{write_streamed_file, write_streamed_unique_file},
	};
	let total = super::external_sort_run_budget();
	let scratch = MAX_RECORD_BYTES * 16;
	// Recovery retains authority plus one bounded codec workspace. Publication
	// must fit the same configured allowance, including secondary keys.
	let mut recovery_budget = CodecBudget::new(total, scratch)?;
	let sorter_budget =
		total.checked_sub(scratch).filter(|bytes| *bytes >= 4 * 1024 * 1024).ok_or_else(|| {
			invalid("mapping builder budget cannot hold one bounded record and sort lanes")
		})? / 4;
	let prefix = format!("atoms/{directory}/bridges");
	let mut primary = ExternalIndexSorter::with_budget(root, sorter_budget);
	let mut direct = ExternalIndexSorter::with_budget(root, sorter_budget);
	let mut reverse = ExternalIndexSorter::with_budget(root, sorter_budget);
	let heap = write_streamed_file(root, &format!("{prefix}/records.fgb"), |file| {
		let mut heap = StreamingHeap::new(file)?;
		for database in registry.databases.values() {
			if database.reference.datastore != owner {
				return Err(invalid("bridge database origin mismatch"));
			}
			let value = database_value(database)?;
			admit_recoverable(&value, None, &mut recovery_budget)?;
			let locator = heap.push(&encode_value(&value)?)?;
			primary.push(database_key(database.reference.id), locator)?;
		}
		for row in registry.rows() {
			super::check_external_sort_cancelled()?;
			let value = mapping_value(row)?;
			admit_recoverable(&value, Some(row), &mut recovery_budget)?;
			let locator = heap.push(&encode_value(&value)?)?;
			primary.push(mapping_key(row.reference.id), locator)?;
			if row.state != BridgeMappingState::Tombstoned {
				direct.push(direct_key(row)?, locator)?;
				reverse.push(reverse_key(row)?, locator)?;
			}
		}
		heap.finish()?;
		Ok(StreamedIndex {
			root: PageNo::new(0),
			page_count: file.page_count(),
		})
	})?;
	let index = |sorter: ExternalIndexSorter, name: &str| {
		let records = sorter.finish()?;
		let memory = std::num::NonZeroUsize::new(sorter_budget)
			.ok_or_else(|| invalid("mapping sort allowance is zero"))?;
		super::with_external_sort_options(memory, std::num::NonZeroUsize::MIN, || {
			write_streamed_unique_file::<MappingLocator>(
				&records,
				root,
				&format!("{prefix}/{name}"),
			)
		})
	};
	Ok([
		heap,
		index(primary, "primary.fgidx")?,
		index(direct, "local.fgidx")?,
		index(reverse, "external.fgidx")?,
	])
}

/// Only the active slotted page and one bounded overflow record remain
/// resident.
struct StreamingHeap<'a> {
	/// Direct file receiving the immutable page stream.
	file: &'a super::direct::DirectFile,
	/// Active slotted page address within this heap.
	current: PageNo,
	/// One active aligned slotted page.
	page: AlignedPage,
	/// Slotted-page writer holding the current page metadata.
	builder: PropertyPageBuilder,
}
impl<'a> StreamingHeap<'a> {
	/// Constructs the owned state used by this bounded operation.
	fn new(file: &'a super::direct::DirectFile) -> FerrosGrynnResult<Self> {
		file.resize_pages(1)?;
		let mut page = AlignedPage::zeroed();
		let current = PageNo::new(0);
		let builder = PropertyPageBuilder::new(&mut page, current, PageLsn::ZERO);
		Ok(Self {
			file,
			current,
			page,
			builder,
		})
	}

	/// Appends one admitted record using existing slotted and overflow formats.
	fn push(&mut self, bytes: &[u8]) -> FerrosGrynnResult<MappingLocator> {
		if bytes.len() > MAX_RECORD_BYTES {
			return Err(invalid("mapping record exceeds bounded payload"));
		}
		let overflow;
		let record = if bytes.len() > MAX_INLINE_BYTES {
			let first = PageNo::new(self.file.page_count());
			let mut pages = Vec::new();
			append_large_property(&mut pages, first, PageLsn::ZERO, bytes)?;
			self.file.write_pages(first, &pages)?;
			let mut locator = b"FGBL\x01\0\0\0".to_vec();
			locator.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
			locator.extend_from_slice(&first.get().to_le_bytes());
			overflow = locator;
			overflow.as_slice()
		} else {
			bytes
		};
		let slot = match self.builder.try_insert(&mut self.page, record)? {
			Some(slot) => slot,
			None => {
				let current = PageNo::new(self.file.page_count());
				self.file.resize_pages(
					current
						.get()
						.checked_add(1)
						.ok_or_else(|| invalid("mapping heap page count overflows"))?,
				)?;
				let mut next = AlignedPage::zeroed();
				let builder = PropertyPageBuilder::new(&mut next, current, PageLsn::ZERO);
				let old = std::mem::replace(&mut self.builder, builder);
				old.finish(&mut self.page)?;
				self.file.write_page(self.current, &self.page)?;
				self.page = next;
				self.current = current;
				self.builder
					.try_insert(&mut self.page, record)?
					.ok_or_else(|| invalid("mapping record cannot fit empty slotted page"))?
			}
		};
		MappingLocator::new(self.current, slot)
	}

	/// Finalizes the bounded representation and checks exact consumption.
	fn finish(mut self) -> FerrosGrynnResult<()> {
		self.builder.finish(&mut self.page)?;
		self.file.write_page(self.current, &self.page)
	}
}

/// Pure generation builder using the production property heap and B+tree codec.
pub(super) fn build(
	registry: &BridgeRegistry,
	owner: uuid::Uuid,
) -> FerrosGrynnResult<BuiltMappings> {
	let mut heap = Heap::new();
	let mut primary = Vec::new();
	let mut direct = Vec::new();
	let mut reverse = Vec::new();
	for database in registry.databases.values() {
		if database.reference.datastore != owner {
			return Err(invalid("bridge database origin mismatch"));
		}
		let locator = heap.push(&encode_value(&database_value(database)?)?)?;
		primary.push(PrimaryIndexEntry {
			key: database_key(database.reference.id),
			atom: locator,
		});
	}
	for row in registry.rows() {
		let locator = heap.push(&encode_value(&mapping_value(row)?)?)?;
		primary.push(PrimaryIndexEntry {
			key: mapping_key(row.reference.id),
			atom: locator,
		});
		if row.state != BridgeMappingState::Tombstoned {
			direct.push(PrimaryIndexEntry {
				key: direct_key(row)?,
				atom: locator,
			});
			reverse.push(PrimaryIndexEntry {
				key: reverse_key(row)?,
				atom: locator,
			});
		}
	}
	primary.sort_unstable_by(|left, right| left.key.cmp(&right.key));
	direct.sort_unstable_by(|left, right| left.key.cmp(&right.key));
	reverse.sort_unstable_by(|left, right| left.key.cmp(&right.key));
	Ok(BuiltMappings {
		heap: heap.finish()?,
		primary: build_primary_index(&primary, PageLsn::ZERO)?,
		direct: build_primary_index(&direct, PageLsn::ZERO)?,
		reverse: build_primary_index(&reverse, PageLsn::ZERO)?,
	})
}

/// One open slotted page plus existing overflow chains.
struct Heap {
	/// Owned aligned pages for the bounded buffered builder.
	pages: Vec<AlignedPage>,
	/// Active slotted page address within this heap.
	current: usize,
	/// Slotted-page writer holding the current page metadata.
	builder: PropertyPageBuilder,
}

impl Heap {
	/// Constructs the owned state used by this bounded operation.
	fn new() -> Self {
		let mut page = AlignedPage::zeroed();
		let builder = PropertyPageBuilder::new(&mut page, PageNo::new(0), PageLsn::ZERO);
		Self {
			pages: vec![page],
			current: 0,
			builder,
		}
	}

	/// Appends one admitted record using existing slotted and overflow formats.
	fn push(&mut self, bytes: &[u8]) -> FerrosGrynnResult<MappingLocator> {
		if bytes.len() > MAX_RECORD_BYTES {
			return Err(invalid("mapping record exceeds bounded payload"));
		}
		let overflow;
		let record = if bytes.len() > MAX_INLINE_BYTES {
			let first = PageNo::new(self.pages.len() as u64);
			append_large_property(&mut self.pages, first, PageLsn::ZERO, bytes)?;
			let mut locator = b"FGBL\x01\0\0\0".to_vec();
			locator.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
			locator.extend_from_slice(&first.get().to_le_bytes());
			overflow = locator;
			overflow.as_slice()
		} else {
			bytes
		};
		let slot = match self.builder.try_insert(&mut self.pages[self.current], record)? {
			Some(slot) => slot,
			None => {
				let index = self.pages.len();
				let mut page = AlignedPage::zeroed();
				let next =
					PropertyPageBuilder::new(&mut page, PageNo::new(index as u64), PageLsn::ZERO);
				let old = std::mem::replace(&mut self.builder, next);
				old.finish(&mut self.pages[self.current])?;
				self.pages.push(page);
				self.current = index;
				self.builder
					.try_insert(&mut self.pages[index], record)?
					.ok_or_else(|| invalid("mapping inline frame does not fit an empty page"))?
			}
		};
		MappingLocator::new(PageNo::new(self.current as u64), slot)
	}

	/// Finalizes the bounded representation and checks exact consumption.
	fn finish(mut self) -> FerrosGrynnResult<Vec<AlignedPage>> {
		self.builder.finish(&mut self.pages[self.current])?;
		Ok(self.pages)
	}
}

/// Loads authority from primary entries and verifies both secondary indexes.
pub(super) fn recover(
	owner: uuid::Uuid,
	binding: BridgeCatalogBinding,
	mut read: impl FnMut(PhysicalIndexBinding, PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<BridgeRegistry> {
	if binding.generation == 0 || binding.directory.is_nil() {
		return Err(invalid("invalid mapping generation descriptor"));
	}
	let read = std::cell::RefCell::new(&mut read);
	let budget = std::cell::RefCell::new(CodecBudget::new(super::external_sort_run_budget(), 0)?);
	let mut registry = BridgeRegistry::default();
	let mut count = 0_u64;
	let mut live = 0_u64;
	visit_primary_index::<MappingLocator>(
		|number, page| {
			budget.borrow_mut().admit(64)?;
			read.borrow_mut()(binding.primary, number, page)
		},
		binding.primary.root,
		|key, locator| {
			count = count.checked_add(1).ok_or_else(|| invalid("mapping count overflows"))?;
			if count > binding.count {
				return Err(invalid("mapping primary exceeds declared count"));
			}
			let remaining = {
				let budget = budget.borrow();
				budget.maximum.saturating_sub(budget.used)
			};
			let value = read_value(
				locator,
				binding.heap,
				&mut |root, number, page| read.borrow_mut()(root, number, page),
				remaining,
			)?;
			budget.borrow_mut().admit(
				value
					.estimated_heap_bytes()
					.checked_add(1024)
					.ok_or_else(|| invalid("mapping recovery admission overflows"))?,
			)?;
			let mut fields = Fields::new(value)?;
			match fields.number()? {
				0 => {
					let database = decode_database(owner, &mut fields)?;
					fields.finish()?;
					if key != database_key(database.reference.id) {
						return Err(invalid("mapping database primary key mismatch"));
					}
					registry.install_database(Arc::new(database))?;
				}
				1 => {
					let row = decode_mapping(owner, &mut fields)?;
					fields.finish()?;
					if key != mapping_key(row.reference.id) {
						return Err(invalid("mapping row primary key mismatch"));
					}
					if row.state != BridgeMappingState::Tombstoned {
						live += 1;
						budget.borrow_mut().admit(
							row.subject
								.record()
								.estimated_heap_bytes()
								.saturating_add(row.external.record.estimated_heap_bytes()),
						)?;
					}
					registry.install(Arc::new(row))?;
				}
				_ => return Err(invalid("unknown mapping record tag")),
			}
			Ok(())
		},
	)?;
	if count != binding.count {
		return Err(invalid("mapping count disagrees with primary index"));
	}
	for (index, direct) in [(binding.direct, true), (binding.reverse, false)] {
		let mut count = 0_u64;
		visit_primary_index::<MappingLocator>(
			|number, page| {
				budget.borrow_mut().admit(64)?;
				read.borrow_mut()(index, number, page)
			},
			index.root,
			|key, locator| {
				count = count
					.checked_add(1)
					.ok_or_else(|| invalid("mapping secondary count overflows"))?;
				if count > live {
					return Err(invalid("mapping secondary exceeds live authority"));
				}
				let remaining = {
					let budget = budget.borrow();
					budget.maximum.saturating_sub(budget.used)
				};
				let value = read_value(
					locator,
					binding.heap,
					&mut |root, number, page| read.borrow_mut()(root, number, page),
					remaining,
				)?;
				let mut fields = Fields::new(value)?;
				if fields.number()? != 1 {
					return Err(invalid("secondary points to a database descriptor"));
				}
				let row = decode_mapping(owner, &mut fields)?;
				fields.finish()?;
				if row.state == BridgeMappingState::Tombstoned
					|| registry.get(row.reference.id).map(Arc::as_ref) != Some(&row)
				{
					return Err(invalid("mapping secondary points outside live authority"));
				}
				let expected = if direct {
					direct_key(&row)?
				} else {
					reverse_key(&row)?
				};
				if key != expected {
					return Err(invalid("mapping secondary key disagrees with authority"));
				}
				let authoritative = lookup_primary_index::<MappingLocator>(
					|number, page| read.borrow_mut()(binding.primary, number, page),
					binding.primary.root,
					&mapping_key(row.reference.id),
				)?;
				if authoritative != Some(locator) {
					return Err(invalid("mapping secondary locator disagrees with primary"));
				}
				Ok(())
			},
		)?;
		if count != live {
			return Err(invalid("mapping secondary omits live authority"));
		}
	}
	registry.generation = binding.generation;
	registry.physical = Some(binding);
	registry.touched.clear();
	Ok(registry)
}

/// Decodes one capped heap record under the remaining recovery allowance.
fn read_value(
	locator: MappingLocator,
	heap: PhysicalIndexBinding,
	read: &mut impl FnMut(PhysicalIndexBinding, PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	maximum: usize,
) -> FerrosGrynnResult<Value> {
	let mut page = AlignedPage::zeroed();
	read(heap, locator.page(), &mut page)?;
	let record = read_raw_property_record(&page, locator.page(), locator.slot())?;
	if record.starts_with(b"FGBL") {
		let mut decoder = Decoder::new(record);
		decoder.take(4)?;
		if decoder.take(4)? != [1, 0, 0, 0] {
			return Err(invalid("invalid mapping overflow version"));
		}
		let length = decoder.u32()?;
		let first = PageNo::new(decoder.u64()?);
		decoder.finish()?;
		if length as usize > maximum / 4 {
			return Err(invalid("mapping overflow exceeds remaining recovery budget"));
		}
		if length as usize > MAX_RECORD_BYTES || length as usize <= MAX_INLINE_BYTES {
			return Err(invalid("invalid mapping overflow length"));
		}
		let count = (length as usize).div_ceil(LARGE_CHUNK_BYTES);
		let mut pages = Vec::new();
		pages.try_reserve_exact(count).map_err(|error| {
			invalid("mapping overflow allocation failed").with_technical_source(error)
		})?;
		for offset in 0..count {
			let mut page = AlignedPage::zeroed();
			read(
				heap,
				PageNo::new(
					first
						.get()
						.checked_add(offset as u64)
						.ok_or_else(|| invalid("mapping overflow address overflows"))?,
				),
				&mut page,
			)?;
			pages.push(page);
		}
		decode_value(
			&decode_large_property(first, &pages, length, MAX_RECORD_BYTES)?,
			maximum.saturating_sub(length as usize * 3),
		)
	} else {
		decode_value(record, maximum)
	}
}

/// Encodes one independently versioned bounded mapping value envelope.
fn encode_value(value: &Value) -> FerrosGrynnResult<Vec<u8>> {
	let mut budget = CodecBudget::new(MAX_RECORD_BYTES * 16, 0)?;
	let mut context = EncodeContext::new(PropertyDictionary::new());
	let value = values::encode(value, &mut context, &mut budget)?;
	let dictionary = values::encode_dictionary(&context, &mut budget)?;
	let mut bytes = b"FGBR\x01\0\0\0".to_vec();
	append_blob(&mut bytes, &dictionary, &mut budget)?;
	append_blob(&mut bytes, &value, &mut budget)?;
	if bytes.len() > MAX_RECORD_BYTES {
		return Err(invalid("mapping record exceeds its payload cap"));
	}
	Ok(bytes)
}

/// Rejects malformed framing before canonical typed value allocation.
fn decode_value(bytes: &[u8], maximum: usize) -> FerrosGrynnResult<Value> {
	if bytes.len() > MAX_RECORD_BYTES {
		return Err(invalid("mapping record exceeds its payload cap"));
	}
	let mut decoder = Decoder::new(bytes);
	if decoder.take(8)? != b"FGBR\x01\0\0\0" {
		return Err(invalid("invalid mapping record marker/version"));
	}
	let dictionary = decoder.blob()?;
	let payload = decoder.blob()?;
	decoder.finish()?;
	let mut budget = CodecBudget::new(maximum.min(MAX_RECORD_BYTES * 16), bytes.len())?;
	let context = values::decode_dictionary(dictionary, &mut budget)?;
	values::decode(payload, &context, &mut budget)
}

/// Preserves the native record namespace and typed key in the envelope.
fn record_value(record: &RecordId) -> Value {
	Value::Array(vec![Value::from(record.class()), record.id().clone().into_value()].into())
}
/// Preserves the complete unsigned GraphId domain as eight LE bytes.
fn graph_value(graph: GraphId) -> Value {
	Value::Bytes(graph.get().to_le_bytes().to_vec().into())
}
/// Encodes the explicit atom or graph-occurrence role and identity.
fn subject_value(subject: &BridgeMappingSubject) -> Value {
	match subject {
		BridgeMappingSubject::Atom(record) => {
			Value::Array(vec![Value::from(0_u64), record_value(record)].into())
		}
		BridgeMappingSubject::Relation {
			graph,
			edge,
		} => Value::Array(vec![Value::from(1_u64), graph_value(*graph), record_value(edge)].into()),
	}
}
/// Rejects oversized descriptors before their owning envelope is copied.
fn database_value(database: &BridgeDatabase) -> FerrosGrynnResult<Value> {
	let bytes =
		database
			.classes
			.iter()
			.fold(4096_usize, |bytes, (class, table)| {
				bytes.saturating_add(class.len()).saturating_add(table.len()).saturating_add(256)
			})
			.saturating_add(database.graphs.values().fold(0_usize, |bytes, table| {
				bytes.saturating_add(table.len()).saturating_add(256)
			}));
	if bytes > MAX_RECORD_BYTES {
		return Err(invalid("bridge descriptor exceeds record cap"));
	}
	Ok(Value::Array(
		vec![
			Value::from(0_u64),
			Value::from(database.reference.id.get()),
			Value::String(database.alias.clone()),
			Value::String(database.provider.clone()),
			Value::String(database.namespace.clone()),
			Value::String(database.database.clone()),
			Value::Object(
				database
					.classes
					.iter()
					.map(|(class, table)| (class.clone(), Value::String(table.clone())))
					.collect(),
			),
			Value::Array(
				database
					.graphs
					.iter()
					.map(|(graph, table)| {
						Value::Array(vec![graph_value(*graph), Value::String(table.clone())].into())
					})
					.collect(),
			),
		]
		.into(),
	))
}
/// Checks borrowed payloads and record keys before making an owning envelope.
fn mapping_value(row: &BridgeMapping) -> FerrosGrynnResult<Value> {
	let mut budget = CodecBudget::new(MAX_RECORD_BYTES * 16, 0)?;
	let mut context = EncodeContext::new(PropertyDictionary::new());
	let remaining = std::cell::Cell::new(MAX_RECORD_BYTES - 4096);
	let observe = |bytes: u64| {
		let bytes = usize::try_from(bytes)
			.map_err(|error| invalid("mapping payload overflows").with_technical_source(error))?;
		remaining.set(
			remaining
				.get()
				.checked_sub(bytes)
				.ok_or_else(|| invalid("mapping payload exceeds record cap"))?,
		);
		Ok(())
	};
	for record in [row.subject.record(), &row.external.record] {
		values::prepare_record(record, &mut context, &mut budget, 4, Some(&observe))?;
	}
	if let Some(intent) = &row.mirror {
		for record in [&intent.relation, &intent.source, &intent.target] {
			values::prepare_record(record, &mut context, &mut budget, 4, Some(&observe))?;
		}
		for (name, value) in &intent.properties {
			observe(name.len() as u64)?;
			values::prepare(value, &mut context, &mut budget, 4, Some(&observe))?;
		}
	}
	let mirror = row.mirror.as_ref().map_or(Value::None, |value| {
		Value::Array(
			vec![
				record_value(&value.relation),
				record_value(&value.source),
				record_value(&value.target),
				Value::Object(value.properties.clone()),
			]
			.into(),
		)
	});
	Ok(Value::Array(
		vec![
			Value::from(1_u64),
			Value::Uuid(row.reference.id.0.into()),
			Value::from(row.external.database.id.get()),
			subject_value(&row.subject),
			record_value(&row.external.record),
			Value::from(match row.state {
				BridgeMappingState::Pending => 0_u64,
				BridgeMappingState::Ready => 1,
				BridgeMappingState::Tombstoned => 2,
			}),
			mirror,
		]
		.into(),
	))
}

/// Registration and persistence share exactly the same representable record.
pub fn validate_mapping_record(row: &BridgeMapping) -> FerrosGrynnResult<()> {
	let value = mapping_value(row)?;
	super::graph_index::validate_unique_index_key(&direct_key(row)?)?;
	super::graph_index::validate_unique_index_key(&reverse_key(row)?)?;
	encode_value(&value)?;
	Ok(())
}

/// Registration uses the same independently bounded descriptor as recovery.
pub fn validate_database_record(database: &BridgeDatabase) -> FerrosGrynnResult<()> {
	encode_value(&database_value(database)?)?;
	Ok(())
}

/// Leaves recovery scratch available after all retained authoritative records.
fn admit_recoverable(
	value: &Value,
	row: Option<&BridgeMapping>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	let bytes = value
		.estimated_heap_bytes()
		.checked_mul(2)
		.and_then(|bytes| bytes.checked_add(4096))
		.ok_or_else(|| invalid("mapping recoverability admission overflows"))?;
	budget.admit(bytes)?;
	if let Some(row) = row.filter(|row| row.state != BridgeMappingState::Tombstoned) {
		budget.admit(
			row.subject
				.record()
				.estimated_heap_bytes()
				.saturating_add(row.external.record.estimated_heap_bytes()),
		)?;
	}
	Ok(())
}
/// Forms the descriptor primary key in its separate key namespace.
fn database_key(id: BridgeDatabaseId) -> Vec<u8> {
	let mut key = vec![0];
	key.extend_from_slice(&id.get().to_be_bytes());
	key
}
/// Forms the non-reusable mapping UUID primary key.
fn mapping_key(id: BridgeMappingId) -> Vec<u8> {
	let mut key = vec![1];
	key.extend_from_slice(id.0.as_bytes());
	key
}
/// Qualifies the local subject key by database and role.
fn direct_key(row: &BridgeMapping) -> FerrosGrynnResult<Vec<u8>> {
	encode_value(&Value::Array(
		vec![subject_value(&row.subject), Value::from(row.external.database.id.get())].into(),
	))
}
/// Qualifies the native identity key by database and role.
fn reverse_key(row: &BridgeMapping) -> FerrosGrynnResult<Vec<u8>> {
	encode_value(&Value::Array(
		vec![
			Value::from(row.external.database.id.get()),
			Value::from(match row.subject.role() {
				BridgeMappingRole::Atom => 0_u64,
				BridgeMappingRole::Relation => 1,
			}),
			record_value(&row.external.record),
		]
		.into(),
	))
}

/// Exact-consumption reader for one decoded authoritative value array.
struct Fields(std::vec::IntoIter<Value>);
impl Fields {
	/// Constructs the owned state used by this bounded operation.
	fn new(value: Value) -> FerrosGrynnResult<Self> {
		match value {
			Value::Array(array) => Ok(Self(array.into_iter().collect::<Vec<_>>().into_iter())),
			_ => Err(invalid("mapping payload is not an array")),
		}
	}

	/// Consumes one field, rejecting a truncated record.
	fn value(&mut self) -> FerrosGrynnResult<Value> {
		self.0.next().ok_or_else(|| invalid("mapping payload field is absent"))
	}

	/// Accepts only a nonnegative integral tag, without numeric coercion.
	fn number(&mut self) -> FerrosGrynnResult<u64> {
		match self.value()? {
			Value::Number(crate::values::Number::Int(number)) => {
				u64::try_from(number).map_err(|error| {
					invalid("mapping integer field is negative").with_technical_source(error)
				})
			}
			_ => Err(invalid("mapping numeric field is not an integer")),
		}
	}

	/// Reads one typed bounded text field.
	fn text(&mut self) -> FerrosGrynnResult<Text> {
		match self.value()? {
			Value::String(text) if text.len() <= 4096 && !text.as_str().contains('\0') => Ok(text),
			_ => Err(invalid("mapping text field is invalid")),
		}
	}

	/// Reads the full nonzero unsigned GraphId domain.
	fn graph(&mut self) -> FerrosGrynnResult<GraphId> {
		let Value::Bytes(bytes) = self.value()? else {
			return Err(invalid("mapping graph identity is not a byte field"));
		};
		let bytes: [u8; 8] = bytes.as_slice().try_into().map_err(|error| {
			invalid("mapping graph identity is not eight bytes").with_technical_source(error)
		})?;
		GraphId::new(u64::from_le_bytes(bytes))
			.ok_or_else(|| invalid("mapping graph identity is zero"))
	}

	/// Reads a checked native namespace and typed record key.
	fn record(&mut self) -> FerrosGrynnResult<RecordId> {
		let mut record = Self::new(self.value()?)?;
		let class = record.text()?;
		let key = RecordIdPart::try_from_value(record.value()?)?;
		record.finish()?;
		let record = RecordId::new(class, key)?;
		crate::bridge::types::validate_record(&record)?;
		Ok(record)
	}

	/// Finalizes the bounded representation and checks exact consumption.
	fn finish(mut self) -> FerrosGrynnResult<()> {
		if self.0.next().is_some() {
			return Err(invalid("mapping payload has trailing fields"));
		}
		Ok(())
	}
}
/// Rejects zero and non-u32 external database identities.
fn database_id(fields: &mut Fields) -> FerrosGrynnResult<BridgeDatabaseId> {
	let raw = u32::try_from(fields.number()?).map_err(|error| {
		invalid("mapping database identity exceeds u32").with_technical_source(error)
	})?;
	BridgeDatabaseId::new(raw).ok_or_else(|| invalid("mapping database identity is zero"))
}
/// Validates owned descriptor fields and injective table correspondences.
fn decode_database(owner: uuid::Uuid, fields: &mut Fields) -> FerrosGrynnResult<BridgeDatabase> {
	let id = database_id(fields)?;
	let alias = fields.text()?;
	let provider = fields.text()?;
	let namespace = fields.text()?;
	let database = fields.text()?;
	if alias.is_empty() || provider.is_empty() {
		return Err(invalid("bridge identity name is empty"));
	}
	let Value::Object(raw_classes) = fields.value()? else {
		return Err(invalid("bridge class mapping is not an object"));
	};
	let mut classes = BTreeMap::new();
	for (class, table) in raw_classes {
		let Value::String(table) = table else {
			return Err(invalid("bridge class table is not text"));
		};
		if class.is_empty()
			|| table.is_empty()
			|| class.len() > 4096
			|| class.contains('\0')
			|| table.len() > 4096
			|| table.contains('\0')
			|| classes.values().any(|value| value == &table)
		{
			return Err(invalid("bridge class/table mapping is invalid"));
		}
		classes.insert(class, table);
	}
	let Value::Array(raw_graphs) = fields.value()? else {
		return Err(invalid("bridge graph mappings are not an array"));
	};
	let mut graphs = BTreeMap::new();
	for entry in raw_graphs {
		let mut entry = Fields::new(entry)?;
		let graph = entry.graph()?;
		let table = entry.text()?;
		entry.finish()?;
		if table.is_empty()
			|| graphs.values().any(|value| value == &table)
			|| graphs.insert(graph, table).is_some()
		{
			return Err(invalid("bridge graph/table mapping is invalid"));
		}
	}
	Ok(BridgeDatabase {
		reference: BridgeDatabaseRef {
			datastore: owner,
			id,
		},
		alias,
		provider,
		namespace,
		database,
		classes,
		graphs,
	})
}
/// Validates the authoritative mapping identity, state and saved intent.
fn decode_mapping(owner: uuid::Uuid, fields: &mut Fields) -> FerrosGrynnResult<BridgeMapping> {
	let Value::Uuid(id) = fields.value()? else {
		return Err(invalid("mapping identity is not a UUID"));
	};
	if id.as_inner().is_nil() {
		return Err(invalid("mapping identity is nil"));
	}
	let reference = BridgeMappingRef {
		datastore: owner,
		id: BridgeMappingId(*id.as_inner()),
	};
	let database = BridgeDatabaseRef {
		datastore: owner,
		id: database_id(fields)?,
	};
	let mut subject = Fields::new(fields.value()?)?;
	let subject_value = match subject.number()? {
		0 => BridgeMappingSubject::Atom(subject.record()?),
		1 => {
			let graph = subject.graph()?;
			BridgeMappingSubject::Relation {
				graph,
				edge: subject.record()?,
			}
		}
		_ => return Err(invalid("invalid mapping subject role")),
	};
	subject.finish()?;
	let external = BridgeEntityRef {
		database,
		record: fields.record()?,
	};
	let state = match fields.number()? {
		0 => BridgeMappingState::Pending,
		1 => BridgeMappingState::Ready,
		2 => BridgeMappingState::Tombstoned,
		_ => return Err(invalid("invalid mapping state")),
	};
	let mirror = match fields.value()? {
		Value::None => None,
		value => {
			let mut intent = Fields::new(value)?;
			let relation = intent.record()?;
			let source = intent.record()?;
			let target = intent.record()?;
			let Value::Object(properties) = intent.value()? else {
				return Err(invalid("mapping intent properties are not an object"));
			};
			intent.finish()?;
			if relation != external.record || subject_value.role() != BridgeMappingRole::Relation {
				return Err(invalid("mapping intent identity mismatch"));
			}
			Some(BridgeMirrorRequest {
				relation,
				source,
				target,
				properties,
			})
		}
	};
	if state == BridgeMappingState::Pending && mirror.is_none() {
		return Err(invalid("Pending mapping has no recoverable operation"));
	}
	Ok(BridgeMapping {
		reference,
		subject: subject_value,
		external,
		state,
		mirror: mirror.map(Arc::new),
	})
}
/// Constructs the common typed format error at this trust boundary.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::storage::types::{DataFileId, DataFileKind};

	fn fixture() -> FerrosGrynnResult<(uuid::Uuid, BridgeRegistry, BridgeMappingRef)> {
		let owner = uuid::Uuid::now_v7();
		let database = BridgeDatabaseRef {
			datastore: owner,
			id: BridgeDatabaseId::new(1).ok_or_else(|| invalid("test identity"))?,
		};
		let mut registry = BridgeRegistry::default();
		registry.install_database(Arc::new(BridgeDatabase {
			reference: database,
			alias: "sdb".into(),
			provider: "fake".into(),
			namespace: "ns".into(),
			database: "db".into(),
			classes: BTreeMap::from([("person".into(), "profile".into())]),
			graphs: BTreeMap::from([(
				GraphId::new(u64::MAX).ok_or_else(|| invalid("test graph"))?,
				"relation".into(),
			)]),
		}))?;
		let reference = BridgeMappingRef {
			datastore: owner,
			id: BridgeMappingId(uuid::Uuid::now_v7()),
		};
		registry.install(Arc::new(BridgeMapping {
			reference,
			subject: BridgeMappingSubject::Atom("person:a".parse()?),
			external: BridgeEntityRef {
				database,
				record: "profile:a".parse()?,
			},
			state: BridgeMappingState::Ready,
			mirror: None,
		}))?;
		Ok((owner, registry, reference))
	}
	fn binding(
		registry: &BridgeRegistry,
		built: &BuiltMappings,
	) -> FerrosGrynnResult<BridgeCatalogBinding> {
		let file = |id, root| {
			Ok::<_, FerrosGrynnError>(PhysicalIndexBinding {
				file_id: DataFileId::new(id, DataFileKind::BridgeMappings)
					.ok_or_else(|| invalid("test file identity"))?,
				root,
			})
		};
		Ok(BridgeCatalogBinding {
			generation: registry.generation,
			directory: uuid::Uuid::now_v7(),
			count: 2,
			heap: file(1, PageNo::new(0))?,
			primary: file(2, built.primary.root)?,
			direct: file(3, built.direct.root)?,
			reverse: file(4, built.reverse.root)?,
		})
	}
	fn read(
		built: &BuiltMappings,
		root: PhysicalIndexBinding,
		number: PageNo,
		output: &mut AlignedPage,
	) -> FerrosGrynnResult<()> {
		let pages = match root.file_id.instance_id() {
			1 => &built.heap,
			2 => &built.primary.pages,
			3 => &built.direct.pages,
			4 => &built.reverse.pages,
			_ => return Err(invalid("test unknown file")),
		};
		let page = pages
			.get(
				usize::try_from(number.get())
					.map_err(|error| invalid("test page index").with_technical_source(error))?,
			)
			.ok_or_else(|| invalid("test page out of bounds"))?;
		output.as_bytes_mut().copy_from_slice(page.as_bytes());
		Ok(())
	}
	#[test]
	fn bridge_codec_registry_roundtrip_and_unsigned_graph_identity() -> FerrosGrynnResult<()> {
		let (owner, registry, reference) = fixture()?;
		let built = build(&registry, owner)?;
		let binding = binding(&registry, &built)?;
		let recovered =
			recover(owner, binding, |root, number, output| read(&built, root, number, output))?;
		assert_eq!(recovered.get(reference.id), registry.get(reference.id));
		assert_eq!(recovered.databases, registry.databases);
		Ok(())
	}
	#[test]
	fn bridge_codec_declared_count_stops_before_extra_heap_allocation() -> FerrosGrynnResult<()> {
		let (owner, registry, _) = fixture()?;
		let built = build(&registry, owner)?;
		let mut binding = binding(&registry, &built)?;
		binding.count = 1;
		let reads = std::cell::Cell::new(0_usize);
		assert!(
			recover(owner, binding, |root, number, output| {
				reads.set(reads.get() + 1);
				read(&built, root, number, output)
			})
			.is_err()
		);
		assert!(reads.get() <= 5);
		Ok(())
	}
	#[test]
	fn bridge_codec_rejects_versions_lengths_and_fractional_tags() -> FerrosGrynnResult<()> {
		let (_, registry, _) = fixture()?;
		let database =
			registry.databases.values().next().ok_or_else(|| invalid("test descriptor"))?;
		let mut bytes = encode_value(&database_value(database)?)?;
		bytes[4] = 2;
		assert!(decode_value(&bytes, 1024 * 1024).is_err());
		let mut bytes = encode_value(&database_value(database)?)?;
		bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
		assert!(decode_value(&bytes, 1024 * 1024).is_err());
		assert!(Fields::new(Value::Array(vec![Value::from(1.5_f64)].into()))?.number().is_err());
		Ok(())
	}

	#[test]
	fn bridge_codec_preflights_borrowed_payloads() -> FerrosGrynnResult<()> {
		let (_, registry, reference) = fixture()?;
		let mut row =
			registry.get(reference.id).ok_or_else(|| invalid("test mapping"))?.as_ref().clone();
		row.mirror = Some(Arc::new(BridgeMirrorRequest {
			relation: "relation:r".parse()?,
			source: "profile:a".parse()?,
			target: "profile:b".parse()?,
			properties: crate::values::Object::from_iter([(
				"payload",
				Value::Bytes(vec![0; MAX_RECORD_BYTES + 1].into()),
			)]),
		}));
		assert!(mapping_value(&row).is_err());
		Ok(())
	}

	#[test]
	fn bridge_codec_registration_matches_index_and_descriptor_caps() -> FerrosGrynnResult<()> {
		assert!(super::super::graph_index::validate_unique_index_key(&vec![1; 16_330]).is_ok());
		assert!(super::super::graph_index::validate_unique_index_key(&vec![1; 16_331]).is_err());
		let (_, registry, reference) = fixture()?;
		let mut row =
			registry.get(reference.id).ok_or_else(|| invalid("test row absent"))?.as_ref().clone();
		row.external.record = RecordId::from_value(
			"profile",
			Value::Array(vec![Value::from("x".repeat(16_384))].into()),
		)?;
		assert!(validate_mapping_record(&row).is_err());
		let mut database = registry
			.databases
			.values()
			.next()
			.ok_or_else(|| invalid("test descriptor absent"))?
			.as_ref()
			.clone();
		database.graphs.clear();
		for ordinal in 1..=1030 {
			let graph = GraphId::new(ordinal).ok_or_else(|| invalid("test graph identity"))?;
			database.graphs.insert(graph, format!("{ordinal:04}{}", "x".repeat(4092)).into());
		}
		assert!(validate_database_record(&database).is_err());
		Ok(())
	}
}
