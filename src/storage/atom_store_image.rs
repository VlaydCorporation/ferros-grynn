//! Complete immutable AtomStore generation, WAL publication and checkpoint-COW
//! metadata codec.

use std::collections::BTreeMap;

use super::{
	atom_page::{ATOM_SLOTS_PER_PAGE, AtomHotPage, AtomHotSlot, PropertyRef, atom_location},
	atom_store_superblock::AtomStoreSuperblock,
	catalog::FileCatalog,
	file_registry::FileRegistry,
	graph_index::{BuiltPrimaryIndex, PrimaryIndexEntry, build_primary_index, scan_primary_index},
	image_io::{backfill_pages, read_pages, read_pages_cached, stage_pages},
	metadata_cow::{select_checkpoint_page, write_checkpoint_page},
	page::{AlignedPage, PageLsn, PageNo},
	property_dictionary::{
		BuiltPropertyDictionary,
		build_property_dictionary,
		open_property_dictionary,
	},
	property_page::{
		LARGE_CHUNK_BYTES,
		MAX_INLINE_BYTES,
		PropertyPageBuilder,
		append_large_property,
		decode_large_property,
		decode_property_map,
		decode_record_id,
		encode_property_map,
		encode_record_id,
		read_property_record,
	},
	types::{CheckpointGeneration, DataFileId, DataFileKind, Lsn, PageKey, StorageSnapshot},
	wal::{CheckpointTicket, FrameWal, TxHandle},
};
use crate::{
	datastore::{AtomStoreRoot, BaseAtom, DataSpaceId, GlobalAtomId, SchemaCatalog},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind},
	values::{
		PropertyDictionary,
		PropertyMap,
		storage::{DiskGraphRef, EncodeContext, codec::DecodeLimits},
	},
};

/// Database checkpoint authority used to validate AtomStore COW candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AtomStoreCheckpointAuthority {
	/// Highest checkpoint generation authorized by database metadata.
	pub(super) generation: CheckpointGeneration,
	/// Highest home-file backfill boundary authorized by database metadata.
	pub(super) backfilled_lsn: Lsn,
	/// Highest durable checkpoint record authorized by database metadata.
	pub(super) checkpoint_record_lsn: Lsn,
}

impl AtomStoreCheckpointAuthority {
	/// Authority used before the first durable database checkpoint.
	pub(super) const EMPTY: Self = Self {
		generation: CheckpointGeneration::ZERO,
		backfilled_lsn: Lsn::ZERO,
		checkpoint_record_lsn: Lsn::ZERO,
	};

	/// Creates authority from the database checkpoint commit point.
	pub(in crate::storage) const fn new(
		generation: CheckpointGeneration,
		backfilled_lsn: Lsn,
		checkpoint_record_lsn: Lsn,
	) -> Self {
		Self {
			generation,
			backfilled_lsn,
			checkpoint_record_lsn,
		}
	}

	/// Returns whether a logical WAL version carries an authorized checkpoint
	/// tuple.
	fn accepts_logical(self, superblock: &AtomStoreSuperblock) -> bool {
		superblock.checkpoint_generation <= self.generation
			&& superblock.backfilled_lsn <= self.backfilled_lsn
			&& superblock.checkpoint_record_lsn <= self.checkpoint_record_lsn
	}

	/// Returns whether a home COW candidate was materialized by its declared
	/// checkpoint rather than by a later logical transaction.
	fn accepts_checkpoint(self, superblock: &AtomStoreSuperblock) -> bool {
		self.accepts_logical(superblock)
			&& superblock.page_lsn.get() <= superblock.backfilled_lsn.get()
	}
}

/// Exact physical bindings needed to publish and reopen one AtomStore image.
#[derive(Debug, Clone, Copy)]
pub(super) struct AtomStoreFileSet {
	/// Logical AtomStore superblock file.
	pub(super) superblock: DataFileId,
	/// Global atom hot-row file.
	pub(super) atoms_hot: DataFileId,
	/// Global property-record file.
	pub(super) properties: DataFileId,
	/// Database-global property-key dictionary file.
	pub(super) property_dictionary: DataFileId,
	/// Global `RecordId -> GlobalAtomId` primary-index file.
	pub(super) primary: DataFileId,
	/// Root page of the global primary index.
	pub(super) primary_root: PageNo,
}

impl AtomStoreFileSet {
	/// Resolves singleton AtomStore files from `FGFC v5` and an explicit
	/// physical binding for the global primary index.
	pub(super) fn from_catalog(
		catalog: &FileCatalog,
		primary: DataFileId,
		primary_root: PageNo,
	) -> FerrosGrynnResult<Self> {
		if primary.kind() != Some(DataFileKind::Index) || primary_root == PageNo::NULL {
			return Err(invalid("global primary has an invalid physical binding"));
		}
		let primary_exists = catalog
			.files
			.iter()
			.any(|file| file.space_id == DataSpaceId::Database && file.file_id == primary);
		if !primary_exists {
			return Err(invalid("global primary file is absent from FileCatalog"));
		}
		Ok(Self {
			superblock: catalog_singleton(catalog, DataFileKind::AtomStoreSuperblock)?,
			atoms_hot: catalog_singleton(catalog, DataFileKind::AtomsHot)?,
			properties: catalog_singleton(catalog, DataFileKind::Properties)?,
			property_dictionary: catalog_singleton(catalog, DataFileKind::PropertyDictionary)?,
			primary,
			primary_root,
		})
	}
}

/// Complete logical AtomStore version staged by one database WAL transaction.
pub(super) struct DurableAtomStoreImage {
	/// Logical metadata published last in the WAL transaction.
	pub(super) superblock: AtomStoreSuperblock,
	/// Atom, property, and primary-index data pages.
	pub(super) atoms: BuiltAtomStoreImage,
	/// Property-key dictionary pages for the same logical version.
	pub(super) dictionary: BuiltPropertyDictionary,
}

/// Builds one self-consistent initial durable AtomStore image.
pub(super) fn build_durable_atom_store_image(
	atoms: &AtomStoreRoot,
	schema: &SchemaCatalog,
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<DurableAtomStoreImage> {
	let mut context = value_context(schema, dictionary.clone())?;
	build_durable_atom_store_image_with_context(atoms, schema, &mut context)
}

/// Builds one AtomStore image while preserving database-wide value
/// dictionaries shared with graph and parameter encoders.
pub(super) fn build_durable_atom_store_image_with_context(
	atoms: &AtomStoreRoot,
	schema: &SchemaCatalog,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<DurableAtomStoreImage> {
	let image = build_atom_store_image_with_context(atoms, schema, context, PageLsn::ZERO)?;
	let dictionary = build_property_dictionary(context.property_dictionary(), PageLsn::ZERO)?;
	let mut node_count = 0_u64;
	let mut edge_count = 0_u64;
	for atom in atoms.atoms() {
		match atom.kind() {
			AtomKind::Node => {
				node_count = node_count
					.checked_add(1)
					.ok_or_else(|| invalid("AtomStore node count overflows u64"))?;
			}
			AtomKind::Edge => {
				edge_count = edge_count
					.checked_add(1)
					.ok_or_else(|| invalid("AtomStore edge count overflows u64"))?;
			}
		}
	}
	let superblock = AtomStoreSuperblock {
		page_lsn: PageLsn::ZERO,
		checkpoint_generation: CheckpointGeneration::ZERO,
		backfilled_lsn: Lsn::ZERO,
		checkpoint_record_lsn: Lsn::ZERO,
		atom_capacity: u64::try_from(atoms.slot_capacity())
			.map_err(|_error| invalid("AtomStore capacity exceeds u64"))?,
		node_count,
		edge_count,
		atoms_hot_pages: u64::try_from(image.atoms_hot.len())
			.map_err(|_error| invalid("AtomStore hot-page count exceeds u64"))?,
		properties_pages: u64::try_from(image.properties.len())
			.map_err(|_error| invalid("AtomStore property-page count exceeds u64"))?,
		property_dictionary_pages: u64::try_from(dictionary.pages.len())
			.map_err(|_error| invalid("property dictionary page count exceeds u64"))?,
	};
	superblock.encode()?;
	Ok(DurableAtomStoreImage {
		superblock,
		atoms: image,
		dictionary,
	})
}

/// Stages one complete AtomStore generation plus exactly one superblock frame.
pub(super) fn stage_durable_atom_store_image(
	tx: &mut TxHandle,
	files: AtomStoreFileSet,
	image: &DurableAtomStoreImage,
) -> FerrosGrynnResult<()> {
	validate_image_ceilings(image)?;
	stage_pages(tx, DataSpaceId::Database, files.atoms_hot, &image.atoms.atoms_hot)?;
	stage_pages(tx, DataSpaceId::Database, files.properties, &image.atoms.properties)?;
	stage_pages(tx, DataSpaceId::Database, files.property_dictionary, &image.dictionary.pages)?;
	stage_pages(tx, DataSpaceId::Database, files.primary, &image.atoms.primary.pages)?;
	tx.stage_page(
		PageKey::new(DataSpaceId::Database, files.superblock, PageNo::new(0)),
		&image.superblock.encode()?,
	)
}

/// Stages metadata only when its logical fields changed.
pub(super) fn stage_atom_store_superblock_if_changed(
	tx: &mut TxHandle,
	file: DataFileId,
	previous: AtomStoreSuperblock,
	next: AtomStoreSuperblock,
) -> FerrosGrynnResult<bool> {
	if previous.same_logical_metadata(next) {
		return Ok(false);
	}
	tx.stage_page(PageKey::new(DataSpaceId::Database, file, PageNo::new(0)), &next.encode()?)?;
	Ok(true)
}

/// Reopens one AtomStore version through WAL-first snapshot resolution.
pub(super) fn open_durable_atom_store_image(
	wal: &FrameWal,
	registry: &FileRegistry,
	engine: Option<&super::hybrid_engine::HybridReadEngine>,
	snapshot: StorageSnapshot,
	authority: AtomStoreCheckpointAuthority,
	files: AtomStoreFileSet,
	schema: &SchemaCatalog,
) -> FerrosGrynnResult<(AtomStoreRoot, PropertyDictionary, AtomStoreSuperblock)> {
	let superblock = read_atom_store_superblock(wal, registry, snapshot, authority, files)?;
	let load = |file_id, page_count| match engine {
		Some(engine) => read_pages_cached(
			wal,
			registry,
			engine,
			snapshot,
			authority.generation,
			authority.backfilled_lsn,
			DataSpaceId::Database,
			file_id,
			page_count,
		),
		None => read_pages(wal, registry, snapshot, DataSpaceId::Database, file_id, page_count),
	};
	let atoms_hot = load(files.atoms_hot, superblock.atoms_hot_pages)?;
	let properties = load(files.properties, superblock.properties_pages)?;
	let dictionary_pages = load(files.property_dictionary, superblock.property_dictionary_pages)?;
	let dictionary =
		open_property_dictionary(&dictionary_pages, superblock.property_dictionary_pages)?;
	let primary_file = registry.resolve_file(DataSpaceId::Database, files.primary)?;
	let home_primary_pages = primary_file.file()?.page_count();
	let primary_pages = home_primary_pages.max(
		wal.visible_file_page_count(snapshot, DataSpaceId::Database, files.primary).unwrap_or(0),
	);
	if primary_pages == 0 || files.primary_root.get() >= primary_pages {
		return Err(invalid("global primary root exceeds its snapshot-visible file"));
	}
	let primary = BuiltPrimaryIndex {
		root: files.primary_root,
		pages: load(files.primary, primary_pages)?,
	};
	let atoms = open_atom_store_image(
		BuiltAtomStoreImage {
			atoms_hot,
			properties,
			primary,
		},
		usize::try_from(superblock.atom_capacity)
			.map_err(|_error| invalid("AtomStore capacity exceeds usize"))?,
		schema,
		&dictionary,
	)?;
	validate_opened_counts(&atoms, superblock)?;
	Ok((atoms, dictionary, superblock))
}

/// Publishes one checkpointed logical AtomStore superblock through COW.
///
/// The caller must replace database checkpoint authority afterwards and only
/// then call `FrameWal::publish_checkpoint`.
pub(super) fn write_atom_store_checkpoint_page(
	registry: &FileRegistry,
	files: AtomStoreFileSet,
	logical: AtomStoreSuperblock,
	authority: AtomStoreCheckpointAuthority,
) -> FerrosGrynnResult<AtomStoreSuperblock> {
	let checkpointed = logical.at_checkpoint(
		authority.generation,
		authority.backfilled_lsn,
		authority.checkpoint_record_lsn,
	)?;
	let path = registry.resolve_file(DataSpaceId::Database, files.superblock)?.path().to_path_buf();
	write_checkpoint_page(&path, &checkpointed.encode()?)?;
	Ok(checkpointed)
}

/// Backfills only regular AtomStore frames selected by one checkpoint ticket.
///
/// The logical superblock frame is returned without writing its home page;
/// callers finish `CheckpointEnd`, publish it through metadata COW, then
/// replace database authority and finally prune the WAL.
pub(super) fn backfill_atom_store_checkpoint(
	wal: &FrameWal,
	registry: &FileRegistry,
	files: AtomStoreFileSet,
	ticket: &CheckpointTicket,
) -> FerrosGrynnResult<Option<AtomStoreSuperblock>> {
	let regular_files =
		[files.atoms_hot, files.properties, files.property_dictionary, files.primary];
	let mut logical_superblock = None;
	for frame in ticket.frames() {
		let key = frame.key();
		if key.space_id() != DataSpaceId::Database {
			continue;
		}
		if key.file_id() == files.superblock {
			if key.page_no() != PageNo::new(0) || logical_superblock.is_some() {
				return Err(invalid("checkpoint selected an invalid AtomStore superblock frame"));
			}
			let mut page = AlignedPage::zeroed();
			wal.read_frame(*frame, &mut page)?;
			logical_superblock = Some(AtomStoreSuperblock::decode(&page)?);
			continue;
		}
	}
	backfill_pages(wal, registry, DataSpaceId::Database, &regular_files, ticket)?;
	Ok(logical_superblock)
}

/// Immutable physical files produced for one AtomStore generation.
pub(super) struct BuiltAtomStoreImage {
	/// Dense global atom hot pages.
	pub(super) atoms_hot: Vec<AlignedPage>,
	/// Slotted and large property-record pages.
	pub(super) properties: Vec<AlignedPage>,
	/// Global primary-index tree.
	pub(super) primary: BuiltPrimaryIndex,
}

/// Builds the AtomStore data and primary-index files below superblock ceilings.
pub(super) fn build_atom_store_image(
	atoms: &AtomStoreRoot,
	schema: &SchemaCatalog,
	dictionary: &mut PropertyDictionary,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<BuiltAtomStoreImage> {
	let mut context = value_context(schema, dictionary.clone())?;
	let image = build_atom_store_image_with_context(atoms, schema, &mut context, page_lsn)?;
	*dictionary = context.property_dictionary().clone();
	Ok(image)
}

/// Builds AtomStore pages in an already seeded database-wide value context.
fn build_atom_store_image_with_context(
	atoms: &AtomStoreRoot,
	schema: &SchemaCatalog,
	context: &mut EncodeContext,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<BuiltAtomStoreImage> {
	let page_count = atoms.slot_capacity().div_ceil(ATOM_SLOTS_PER_PAGE as usize);
	let mut hot_pages = (0..page_count)
		.map(|page| {
			u64::try_from(page)
				.map(PageNo::new)
				.map_err(|_error| invalid("AtomHot page count exceeds u64"))
				.and_then(|page| AtomHotPage::new(page, page_lsn))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;

	for slot in 0..atoms.slot_capacity() {
		let generation = atoms
			.slot_generation(slot)
			.ok_or_else(|| invalid("AtomStore allocator has a missing slot generation"))?;
		let slot = u64::try_from(slot).map_err(|_error| invalid("AtomStore slot exceeds u64"))?;
		let id =
			GlobalAtomId::from_raw(AtomId::from_parts(AtomKind::Node, slot, generation)?.raw());
		write_hot_slot(&mut hot_pages, id, AtomHotSlot::Tombstone)?;
	}

	let mut properties = PropertyStoreBuilder::default();
	let mut primary = Vec::with_capacity(atoms.atoms().len());
	for atom in atoms.atoms() {
		validate_atom_schema(atom, schema)?;
		let record = encode_record_id(atom.record_id(), context)?;
		let record_ref = properties.insert(&record, page_lsn)?;
		let property_ref = if atom.properties().is_empty() {
			PropertyRef::None
		} else {
			properties.insert(&encode_property_map(atom.properties(), context)?, page_lsn)?
		};
		write_hot_slot(
			&mut hot_pages,
			atom.id(),
			AtomHotSlot::live(atom.kind(), atom.class_id(), record_ref, property_ref)?,
		)?;
		primary.push(PrimaryIndexEntry {
			key: record,
			atom: atom.id(),
		});
	}
	primary.sort_unstable_by(|left, right| left.key.cmp(&right.key));
	Ok(BuiltAtomStoreImage {
		atoms_hot: hot_pages.into_iter().map(|page| *page.finish()).collect(),
		properties: properties.finish()?,
		primary: build_primary_index(&primary, page_lsn)?,
	})
}

/// Reopens one complete AtomStore image using the capacity published by its
/// snapshot-visible superblock.
pub(super) fn open_atom_store_image(
	image: BuiltAtomStoreImage,
	atom_capacity: usize,
	schema: &SchemaCatalog,
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<AtomStoreRoot> {
	let BuiltAtomStoreImage {
		atoms_hot,
		properties,
		primary,
	} = image;
	let expected_pages = atom_capacity.div_ceil(ATOM_SLOTS_PER_PAGE as usize);
	if atoms_hot.len() != expected_pages {
		return Err(invalid("AtomHot page count disagrees with AtomStore capacity"));
	}
	let mut primary_by_atom = BTreeMap::<GlobalAtomId, Vec<u8>>::new();
	for entry in scan_primary_index::<GlobalAtomId>(
		|page_no, target| copy_page(&primary.pages, page_no, target, "global primary"),
		primary.root,
	)? {
		if primary_by_atom.insert(entry.atom, entry.key).is_some() {
			return Err(invalid("global primary maps multiple RecordIds to one GlobalAtomId"));
		}
	}

	let context = value_context(schema, dictionary.clone())?;
	let decoder = context.decoder();
	let mut result = AtomStoreRoot::default();
	for (page_index, page) in atoms_hot.into_iter().enumerate() {
		let page_no = PageNo::new(
			u64::try_from(page_index)
				.map_err(|_error| invalid("AtomHot page index exceeds u64"))?,
		);
		let page = AtomHotPage::from_page(Box::new(page), page_no)?;
		for within_page in 0..ATOM_SLOTS_PER_PAGE {
			let global_slot = page_no
				.get()
				.checked_mul(ATOM_SLOTS_PER_PAGE)
				.and_then(|base| base.checked_add(within_page))
				.ok_or_else(|| invalid("AtomHot slot address overflows u64"))?;
			let (id, slot) = page.read(within_page)?;
			if global_slot >= atom_capacity as u64 {
				if id.generation() != 0 || slot != AtomHotSlot::Tombstone {
					return Err(invalid("AtomHot tail contains a non-canonical unused slot"));
				}
				continue;
			}
			match slot {
				AtomHotSlot::Tombstone => {
					result.restore_free_slot(global_slot, id.generation())?;
				}
				AtomHotSlot::Live {
					kind,
					class_id,
					record_id,
					properties: property_ref,
				} => {
					let record_bytes = read_record(&properties, record_id)?
						.ok_or_else(|| invalid("live atom has no RecordId record"))?;
					let expected_key = primary_by_atom
						.remove(&id)
						.ok_or_else(|| invalid("live atom is absent from global primary"))?;
					if expected_key != record_bytes {
						return Err(invalid("global primary key disagrees with AtomHot RecordId"));
					}
					let record_id =
						decode_record_id(&record_bytes, &decoder, DecodeLimits::default())?;
					let class = schema
						.class_by_id(class_id)
						.ok_or_else(|| invalid("AtomHot slot references an absent class"))?;
					if class.kind() != kind || id.kind() != Some(kind) {
						return Err(invalid("AtomHot identity and class kind disagree"));
					}
					let properties = match read_record(&properties, property_ref)? {
						Some(record) => {
							decode_property_map(&record, &decoder, DecodeLimits::default())?
						}
						None => PropertyMap::new(),
					};
					result.insert_at(id, &class, record_id, properties)?;
				}
			}
		}
	}
	if !primary_by_atom.is_empty() {
		return Err(invalid("global primary references an absent AtomHot slot"));
	}
	Ok(result)
}

/// Resolves exactly one database-global catalog file of a required kind.
fn catalog_singleton(catalog: &FileCatalog, kind: DataFileKind) -> FerrosGrynnResult<DataFileId> {
	let mut matches = catalog
		.files
		.iter()
		.filter(|file| file.space_id == DataSpaceId::Database && file.file_id.kind() == Some(kind));
	let file = matches
		.next()
		.ok_or_else(|| invalid(format!("database FileCatalog has no {kind:?} file")))?;
	if matches.next().is_some() {
		return Err(invalid(format!("database FileCatalog has multiple {kind:?} files")));
	}
	Ok(file.file_id)
}

/// Confirms that superblock ceilings match the staged physical image.
fn validate_image_ceilings(image: &DurableAtomStoreImage) -> FerrosGrynnResult<()> {
	image.superblock.encode()?;
	if usize::try_from(image.superblock.atoms_hot_pages).ok() != Some(image.atoms.atoms_hot.len())
		|| usize::try_from(image.superblock.properties_pages).ok()
			!= Some(image.atoms.properties.len())
		|| usize::try_from(image.superblock.property_dictionary_pages).ok()
			!= Some(image.dictionary.pages.len())
	{
		return Err(invalid("AtomStore superblock page ceilings disagree with its image"));
	}
	let primary_pages = u64::try_from(image.atoms.primary.pages.len())
		.map_err(|_error| invalid("global primary page count exceeds u64"))?;
	if primary_pages == 0 || image.atoms.primary.root.get() >= primary_pages {
		return Err(invalid("global primary root exceeds its image"));
	}
	open_property_dictionary(&image.dictionary.pages, image.superblock.property_dictionary_pages)?;
	let primary_count = scan_primary_index::<GlobalAtomId>(
		|page_no, target| copy_page(&image.atoms.primary.pages, page_no, target, "global primary"),
		image.atoms.primary.root,
	)?
	.len();
	if u64::try_from(primary_count).ok() != Some(image.superblock.live_count()?) {
		return Err(invalid("global primary count disagrees with AtomStoreSuperblock"));
	}
	Ok(())
}

/// Resolves the logical superblock from WAL first, then authorized home COW.
fn read_atom_store_superblock(
	wal: &FrameWal,
	registry: &FileRegistry,
	snapshot: StorageSnapshot,
	authority: AtomStoreCheckpointAuthority,
	files: AtomStoreFileSet,
) -> FerrosGrynnResult<AtomStoreSuperblock> {
	let key = PageKey::new(DataSpaceId::Database, files.superblock, PageNo::new(0));
	let visible = wal.visible_frames(&[key], snapshot);
	if let Some(frame) = visible.frames()[0] {
		let mut page = AlignedPage::zeroed();
		visible.read_frame(frame, &mut page)?;
		let value = AtomStoreSuperblock::decode(&page)?;
		if !authority.accepts_logical(&value) {
			return Err(invalid("WAL AtomStoreSuperblock checkpoint tuple exceeds authority"));
		}
		return Ok(value);
	}
	let path = registry.resolve_file(DataSpaceId::Database, files.superblock)?.path().to_path_buf();
	select_checkpoint_page(
		&path,
		AtomStoreSuperblock::decode,
		|value| authority.accepts_checkpoint(value),
		|value| (value.checkpoint_generation, value.backfilled_lsn, value.checkpoint_record_lsn),
		"AtomStoreSuperblock",
	)
	.map(|(value, _page)| value)
}

/// Checks reopened live counts and capacity against published metadata.
fn validate_opened_counts(
	atoms: &AtomStoreRoot,
	superblock: AtomStoreSuperblock,
) -> FerrosGrynnResult<()> {
	let mut node_count = 0_u64;
	let mut edge_count = 0_u64;
	for atom in atoms.atoms() {
		match atom.kind() {
			AtomKind::Node => {
				node_count = node_count
					.checked_add(1)
					.ok_or_else(|| invalid("reopened AtomStore node count overflows u64"))?;
			}
			AtomKind::Edge => {
				edge_count = edge_count
					.checked_add(1)
					.ok_or_else(|| invalid("reopened AtomStore edge count overflows u64"))?;
			}
		}
	}
	if node_count != superblock.node_count || edge_count != superblock.edge_count {
		return Err(invalid("reopened AtomStore counts disagree with its superblock"));
	}
	Ok(())
}

/// Builds a storage-value context from the current schema and dictionary.
pub(super) fn value_context(
	schema: &SchemaCatalog,
	dictionary: PropertyDictionary,
) -> FerrosGrynnResult<EncodeContext> {
	let mut context = EncodeContext::new(dictionary);
	for class in schema.classes() {
		let id = class.id().ok_or_else(|| invalid("catalog contains an unpublished class"))?;
		context.register_class_name(id.get(), class.name().clone())?;
	}
	for graph in schema.graphs() {
		let reference = graph.reference().clone();
		context.register_graph(reference.clone(), DiskGraphRef::new(reference.id()))?;
	}
	Ok(context)
}

/// Confirms that one atom references a present class of the same kind.
fn validate_atom_schema(atom: &BaseAtom, schema: &SchemaCatalog) -> FerrosGrynnResult<()> {
	let class = schema
		.class_by_id(atom.class_id())
		.ok_or_else(|| invalid("base atom references an absent class"))?;
	if class.name().as_str() != atom.record_id().class() || class.kind() != atom.kind() {
		return Err(invalid("base atom identity disagrees with its class"));
	}
	Ok(())
}

/// Writes one global atom slot into its exact hot page and in-page position.
fn write_hot_slot(
	pages: &mut [AtomHotPage],
	id: GlobalAtomId,
	slot: AtomHotSlot,
) -> FerrosGrynnResult<()> {
	let (page_no, _within_page) = atom_location(id.slot());
	let index = usize::try_from(page_no.get())
		.map_err(|_error| invalid("AtomHot page number exceeds usize"))?;
	pages
		.get_mut(index)
		.ok_or_else(|| invalid("global atom address exceeds AtomHot capacity"))?
		.write(id, slot)
}

/// Resolves an optional inline or large property-store record.
fn read_record(
	pages: &[AlignedPage],
	reference: PropertyRef,
) -> FerrosGrynnResult<Option<Vec<u8>>> {
	match reference {
		PropertyRef::None => Ok(None),
		PropertyRef::Inline {
			page,
			slot,
			length,
		} => {
			if length as usize > MAX_INLINE_BYTES {
				return Err(invalid("inline property reference exceeds the inline limit"));
			}
			let page_ref = page_at(pages, page, "property")?;
			read_property_record(page_ref, page, slot, length).map(|record| Some(record.to_vec()))
		}
		PropertyRef::Large {
			first_page,
			length,
		} => {
			let length_usize = usize::try_from(length)
				.map_err(|_error| invalid("large property length exceeds usize"))?;
			if length_usize <= MAX_INLINE_BYTES {
				return Err(invalid("large property reference fits the inline format"));
			}
			let count = length_usize.div_ceil(LARGE_CHUNK_BYTES);
			let start = usize::try_from(first_page.get())
				.map_err(|_error| invalid("large property page exceeds usize"))?;
			let end = start
				.checked_add(count)
				.ok_or_else(|| invalid("large property page range overflows usize"))?;
			let chain = pages
				.get(start..end)
				.ok_or_else(|| invalid("large property chain exceeds its file"))?;
			decode_large_property(first_page, chain, length, length_usize).map(Some)
		}
	}
}

/// Resolves one page number within a fully materialized file image.
fn page_at<'a>(
	pages: &'a [AlignedPage],
	page_no: PageNo,
	file: &str,
) -> FerrosGrynnResult<&'a AlignedPage> {
	let index = usize::try_from(page_no.get())
		.map_err(|_error| invalid(format!("{file} page number exceeds usize")))?;
	pages.get(index).ok_or_else(|| invalid(format!("{file} page exceeds its file")))
}

/// Copies one page from a materialized image into a read callback target.
fn copy_page(
	pages: &[AlignedPage],
	page_no: PageNo,
	target: &mut AlignedPage,
	file: &str,
) -> FerrosGrynnResult<()> {
	target.as_bytes_mut().copy_from_slice(page_at(pages, page_no, file)?.as_bytes());
	Ok(())
}

#[derive(Default)]
/// Append-only builder for inline and large property records.
struct PropertyStoreBuilder {
	/// Next page number assigned to an appended property page.
	next_page: u64,
	/// Current unsealed slotted page, if any.
	current: Option<(PageNo, PropertyPageBuilder)>,
	/// Final property-file frames in exact page-number order.
	pages: Vec<AlignedPage>,
}

impl PropertyStoreBuilder {
	/// Stores one encoded record and returns its durable physical reference.
	fn insert(&mut self, record: &[u8], page_lsn: PageLsn) -> FerrosGrynnResult<PropertyRef> {
		let length =
			u32::try_from(record.len()).map_err(|_error| invalid("property record exceeds u32"))?;
		if record.len() > MAX_INLINE_BYTES {
			self.finish_current()?;
			let first_page = PageNo::new(self.next_page);
			let page_count = append_large_property(&mut self.pages, first_page, page_lsn, record)?;
			self.next_page = self
				.next_page
				.checked_add(
					u64::try_from(page_count)
						.map_err(|_error| invalid("large property page count exceeds u64"))?,
				)
				.ok_or_else(|| invalid("property page clock is exhausted"))?;
			return PropertyRef::large(first_page, length);
		}

		if self.current.is_none() {
			self.start_page(page_lsn)?;
		}
		if let Some((page_no, builder)) = self.current.as_mut() {
			let index = usize::try_from(page_no.get())
				.map_err(|_error| invalid("property page number exceeds usize"))?;
			let page = self
				.pages
				.get_mut(index)
				.ok_or_else(|| invalid("current property page is absent"))?;
			if let Some(slot) = builder.try_insert(page, record)? {
				return PropertyRef::inline(*page_no, slot, length);
			}
		}
		self.finish_current()?;
		self.start_page(page_lsn)?;
		let (page_no, builder) =
			self.current.as_mut().ok_or_else(|| invalid("property page was not started"))?;
		let index = usize::try_from(page_no.get())
			.map_err(|_error| invalid("property page number exceeds usize"))?;
		let page =
			self.pages.get_mut(index).ok_or_else(|| invalid("current property page is absent"))?;
		let slot = builder
			.try_insert(page, record)?
			.ok_or_else(|| invalid("inline record does not fit an empty property page"))?;
		PropertyRef::inline(*page_no, slot, length)
	}

	/// Starts the next slotted property page.
	fn start_page(&mut self, page_lsn: PageLsn) -> FerrosGrynnResult<()> {
		let page_no = PageNo::new(self.next_page);
		let index = usize::try_from(page_no.get())
			.map_err(|_error| invalid("property page number exceeds usize"))?;
		if index != self.pages.len() {
			return Err(invalid("property pages are not contiguous"));
		}
		self.next_page = self
			.next_page
			.checked_add(1)
			.ok_or_else(|| invalid("property page clock is exhausted"))?;
		self.pages.push(AlignedPage::zeroed());
		let page =
			self.pages.last_mut().ok_or_else(|| invalid("property page allocation failed"))?;
		self.current = Some((page_no, PropertyPageBuilder::new(page, page_no, page_lsn)));
		Ok(())
	}

	/// Seals and retains the current slotted page when present.
	fn finish_current(&mut self) -> FerrosGrynnResult<()> {
		if let Some((page_no, builder)) = self.current.take() {
			let index = usize::try_from(page_no.get())
				.map_err(|_error| invalid("property page number exceeds usize"))?;
			let page = self
				.pages
				.get_mut(index)
				.ok_or_else(|| invalid("current property page is absent"))?;
			builder.finish(page)?;
		}
		Ok(())
	}

	/// Seals the builder and returns a contiguous page-number-ordered image.
	fn finish(mut self) -> FerrosGrynnResult<Vec<AlignedPage>> {
		self.finish_current()?;
		if u64::try_from(self.pages.len()).ok() != Some(self.next_page) {
			return Err(invalid("property pages are not contiguous"));
		}
		Ok(self.pages)
	}
}

/// Creates the typed corruption error used by AtomStore image validation.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests;
