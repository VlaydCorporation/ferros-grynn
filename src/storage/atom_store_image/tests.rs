use std::{
	error::Error,
	fs::{self, OpenOptions},
	io::{self, Seek, SeekFrom, Write},
	path::{Path, PathBuf},
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
};

use rapidhash::RapidHashSet;

use super::*;
use crate::{
	datastore::{ClassId, ClassSchema, DatabaseRoot},
	storage::{
		catalog::{CatalogPath, FileCatalogEntry},
		direct::{DirectFile, DirectFileOpenMode},
		file_registry::DataFileRole,
		metadata_cow::{previous_path, temporary_path},
		page::{PageFlags, PageType},
		wal::RecoverySeed,
	},
	values::{AtomRef, PropertyKeyId, RecordIdPart, Value},
};

static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
	fn new() -> io::Result<Self> {
		let suffix = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
		let name = format!(".ferros-grynn-atom-store-{}-{suffix}", std::process::id());
		Ok(Self(std::env::current_dir()?.join(name)))
	}

	fn as_path(&self) -> &Path {
		&self.0
	}
}

impl Drop for TestDirectory {
	fn drop(&mut self) {
		drop(fs::remove_dir_all(&self.0));
	}
}

#[test]
fn atom_store_image_round_trips_global_ids_primary_and_properties() -> FerrosGrynnResult<()> {
	let mut root = DatabaseRoot::default();
	let person = install_class(&mut root, 1, "person", AtomKind::Node)?;
	let relation = install_class(&mut root, 2, "relation", AtomKind::Edge)?;
	let name = Arc::make_mut(&mut root.dictionary).intern("name")?;
	let link = Arc::make_mut(&mut root.dictionary).intern("link")?;

	let first_record = crate::values::RecordId::new("person", RecordIdPart::Int(1))?;
	let mut first_properties = PropertyMap::new();
	first_properties.insert(name, "Ada");
	let first = root.insert_atom(&person, first_record.clone(), first_properties)?;

	let edge_record = crate::values::RecordId::new("relation", RecordIdPart::Int(9))?;
	let edge = GlobalAtomId::from_raw(
		AtomId::from_parts(AtomKind::Edge, ATOM_SLOTS_PER_PAGE + 1, 7)?.raw(),
	);
	let mut edge_properties = PropertyMap::new();
	edge_properties.insert(link, Value::from(AtomRef::new(AtomKind::Node, first_record)));
	edge_properties.insert(PropertyKeyId::new(2), Value::from("x".repeat(MAX_INLINE_BYTES + 1)));
	root.insert_atom_at(edge, &relation, edge_record, edge_properties)?;
	Arc::make_mut(&mut root.atoms).restore_free_slot(ATOM_SLOTS_PER_PAGE, 4)?;

	let capacity = root.atoms.slot_capacity();
	let mut dictionary = root.dictionary.as_ref().clone();
	let image =
		build_atom_store_image(&root.atoms, &root.schema, &mut dictionary, PageLsn::new(19))?;
	assert_eq!(image.atoms_hot.len(), 2);
	let reopened = open_atom_store_image(image, capacity, &root.schema, &dictionary)?;
	assert_eq!(reopened.atoms().collect::<Vec<_>>(), root.atoms.atoms().collect::<Vec<_>>());
	assert_eq!(reopened.resolve(&"person:1".parse()?), Some(first));

	let next = reopened.clone().insert(&person, "person:2".parse()?, PropertyMap::new())?;
	assert_eq!((next.slot(), next.generation()), (ATOM_SLOTS_PER_PAGE, 4));
	Ok(())
}

#[test]
fn durable_commit_preserves_snapshots_and_recovers_wal_frames() -> Result<(), Box<dyn Error>> {
	let directory = TestDirectory::new()?;
	let mut root = DatabaseRoot::default();
	let person = install_class(&mut root, 1, "person", AtomKind::Node)?;
	let name = Arc::make_mut(&mut root.dictionary).intern("name")?;
	let mut properties = PropertyMap::new();
	properties.insert(name, "Ada");
	let record: crate::values::RecordId = "person:1".parse()?;
	let atom = root.insert_atom(&person, record.clone(), properties)?;
	let image = build_durable_atom_store_image(&root.atoms, &root.schema, &root.dictionary)?;

	let empty = build_durable_atom_store_image(
		&AtomStoreRoot::default(),
		&SchemaCatalog::default(),
		&PropertyDictionary::new(),
	)?;
	let (catalog, files) = create_layout(directory.as_path(), &empty)?;
	let registry = FileRegistry::open(
		catalog.registrations(directory.as_path()),
		DirectFileOpenMode::Synchronous,
		&RapidHashSet::default(),
	)?;
	let wal_path = directory.as_path().join("wal");
	let wal = FrameWal::create_new(&wal_path)?;
	let before = wal.snapshot();

	let mut aborted = wal.begin_tx()?;
	stage_durable_atom_store_image(&mut aborted, files, &image)?;
	drop(aborted);
	let (uncommitted, dictionary, _) = open_durable_atom_store_image(
		&wal,
		&registry,
		None,
		wal.snapshot(),
		AtomStoreCheckpointAuthority::EMPTY,
		files,
		&root.schema,
	)?;
	assert_eq!(uncommitted.atoms().count(), 0);
	assert!(dictionary.is_empty());

	let mut tx = wal.begin_tx()?;
	stage_durable_atom_store_image(&mut tx, files, &image)?;
	stage_durable_atom_store_image(&mut tx, files, &image)?;
	let outcome = wal.commit_tx(tx)?;
	let (old, old_dictionary, _) = open_durable_atom_store_image(
		&wal,
		&registry,
		None,
		before,
		AtomStoreCheckpointAuthority::EMPTY,
		files,
		&root.schema,
	)?;
	assert_eq!(old.atoms().count(), 0);
	assert!(old_dictionary.is_empty());

	corrupt_byte(registry.resolve_file(DataSpaceId::Database, files.superblock)?.path(), 80)?;
	let (opened, dictionary, metadata) = open_durable_atom_store_image(
		&wal,
		&registry,
		None,
		wal.snapshot(),
		AtomStoreCheckpointAuthority::EMPTY,
		files,
		&root.schema,
	)?;
	assert_eq!(opened.resolve(&record), Some(atom));
	assert_eq!(dictionary.get("name"), Some(name));
	assert_eq!(metadata.live_count()?, 1);
	assert_eq!(
		wal.recovery_frames()
			.iter()
			.filter(|frame| frame.key().file_id() == files.superblock)
			.count(),
		1
	);

	let mut unchanged = wal.begin_tx()?;
	let mut ignored_lsn = metadata;
	ignored_lsn.page_lsn = PageLsn::new(u64::MAX);
	assert!(!stage_atom_store_superblock_if_changed(
		&mut unchanged,
		files.superblock,
		metadata,
		ignored_lsn,
	)?);
	assert_eq!(wal.commit_tx(unchanged)?.commit_lsn(), outcome.commit_lsn());

	drop(wal);
	let reopened_wal = FrameWal::open(&wal_path)?;
	let (reopened, reopened_dictionary, reopened_metadata) = open_durable_atom_store_image(
		&reopened_wal,
		&registry,
		None,
		reopened_wal.snapshot(),
		AtomStoreCheckpointAuthority::EMPTY,
		files,
		&root.schema,
	)?;
	assert_eq!(reopened.resolve(&record), Some(atom));
	assert_eq!(reopened_dictionary.get("name"), Some(name));
	assert_eq!(reopened_metadata.live_count()?, 1);
	Ok(())
}

#[test]
fn checkpoint_backfills_data_and_recovers_only_authorized_cow_candidates()
-> Result<(), Box<dyn Error>> {
	let directory = TestDirectory::new()?;
	let mut root = DatabaseRoot::default();
	let person = install_class(&mut root, 1, "person", AtomKind::Node)?;
	let name = Arc::make_mut(&mut root.dictionary).intern("name")?;
	let mut properties = PropertyMap::new();
	properties.insert(name, "Grace");
	let record: crate::values::RecordId = "person:2".parse()?;
	let atom = root.insert_atom(&person, record.clone(), properties)?;
	let image = build_durable_atom_store_image(&root.atoms, &root.schema, &root.dictionary)?;
	let empty = build_durable_atom_store_image(
		&AtomStoreRoot::default(),
		&SchemaCatalog::default(),
		&PropertyDictionary::new(),
	)?;
	let (catalog, files) = create_layout(directory.as_path(), &empty)?;
	let registry = FileRegistry::open(
		catalog.registrations(directory.as_path()),
		DirectFileOpenMode::Synchronous,
		&RapidHashSet::default(),
	)?;
	let wal_path = directory.as_path().join("wal");
	let wal = FrameWal::create_new(&wal_path)?;
	let mut tx = wal.begin_tx()?;
	stage_durable_atom_store_image(&mut tx, files, &image)?;
	let outcome = wal.commit_tx(tx)?;

	let ticket = wal.begin_checkpoint(outcome.commit_lsn(), CheckpointGeneration::ZERO)?;
	let logical = backfill_atom_store_checkpoint(&wal, &registry, files, &ticket)?
		.ok_or_else(|| io::Error::other("checkpoint omitted the AtomStore superblock"))?;
	let checkpoint = wal.finish_checkpoint(ticket)?;
	let authority = AtomStoreCheckpointAuthority {
		generation: checkpoint.generation(),
		backfilled_lsn: checkpoint.target(),
		checkpoint_record_lsn: checkpoint.checkpoint_record_lsn(),
	};
	let checkpointed = write_atom_store_checkpoint_page(&registry, files, logical, authority)?;
	assert_eq!(checkpointed.page_lsn, logical.page_lsn);
	wal.publish_checkpoint(&checkpoint)?;
	let clocks = checkpoint.clocks();
	let seed = RecoverySeed::new(
		checkpoint.generation(),
		checkpoint.target(),
		checkpoint.checkpoint_record_lsn(),
		clocks.durable_visible_lsn,
		clocks.next_lsn,
		clocks.commit_ts,
		clocks.next_tx_id,
		clocks.next_file_instance_id,
	);
	drop(wal);

	let reopened_wal = FrameWal::open_with_seed(&wal_path, seed)?;
	let (opened, dictionary, metadata) = open_durable_atom_store_image(
		&reopened_wal,
		&registry,
		None,
		reopened_wal.snapshot(),
		authority,
		files,
		&root.schema,
	)?;
	assert_eq!(opened.resolve(&record), Some(atom));
	assert_eq!(dictionary.get("name"), Some(name));
	assert_eq!(metadata, checkpointed);

	let next_authority = AtomStoreCheckpointAuthority {
		generation: CheckpointGeneration::new(authority.generation.get() + 1),
		backfilled_lsn: authority.backfilled_lsn,
		checkpoint_record_lsn: Lsn::new(authority.checkpoint_record_lsn.get() + 1),
	};
	let ahead = write_atom_store_checkpoint_page(&registry, files, logical, next_authority)?;
	let (_, _, selected_previous) = open_durable_atom_store_image(
		&reopened_wal,
		&registry,
		None,
		reopened_wal.snapshot(),
		authority,
		files,
		&root.schema,
	)?;
	assert_eq!(selected_previous, checkpointed);

	let third_authority = AtomStoreCheckpointAuthority {
		generation: CheckpointGeneration::new(next_authority.generation.get() + 1),
		backfilled_lsn: next_authority.backfilled_lsn,
		checkpoint_record_lsn: Lsn::new(next_authority.checkpoint_record_lsn.get() + 1),
	};
	let temporary =
		temporary_path(registry.resolve_file(DataSpaceId::Database, files.superblock)?.path());
	let staged_temporary = logical.at_checkpoint(
		third_authority.generation,
		third_authority.backfilled_lsn,
		third_authority.checkpoint_record_lsn,
	)?;
	write_direct_pages(&temporary, &[staged_temporary.encode()?])?;
	let (_, _, ignored_temporary) = open_durable_atom_store_image(
		&reopened_wal,
		&registry,
		None,
		reopened_wal.snapshot(),
		third_authority,
		files,
		&root.schema,
	)?;
	assert_eq!(ignored_temporary, ahead);

	let superblock_path =
		registry.resolve_file(DataSpaceId::Database, files.superblock)?.path().to_path_buf();
	assert!(previous_path(&superblock_path).exists());
	corrupt_byte(&superblock_path, 88)?;
	let (_, _, recovered_previous) = open_durable_atom_store_image(
		&reopened_wal,
		&registry,
		None,
		reopened_wal.snapshot(),
		next_authority,
		files,
		&root.schema,
	)?;
	assert_eq!(recovered_previous, checkpointed);

	let next_record: crate::values::RecordId = "person:3".parse()?;
	let next_atom = root.insert_atom(&person, next_record.clone(), PropertyMap::new())?;
	let mut next_image =
		build_durable_atom_store_image(&root.atoms, &root.schema, &root.dictionary)?;
	next_image.superblock = next_image.superblock.at_checkpoint(
		authority.generation,
		authority.backfilled_lsn,
		authority.checkpoint_record_lsn,
	)?;
	let mut post_checkpoint_tx = reopened_wal.begin_tx()?;
	stage_durable_atom_store_image(&mut post_checkpoint_tx, files, &next_image)?;
	reopened_wal.commit_tx(post_checkpoint_tx)?;
	let (post_checkpoint, _, post_checkpoint_metadata) = open_durable_atom_store_image(
		&reopened_wal,
		&registry,
		None,
		reopened_wal.snapshot(),
		authority,
		files,
		&root.schema,
	)?;
	assert_eq!(post_checkpoint.resolve(&next_record), Some(next_atom));
	assert!(post_checkpoint_metadata.page_lsn.get() > authority.backfilled_lsn.get());
	Ok(())
}

fn install_class(
	root: &mut DatabaseRoot,
	id: u32,
	name: &str,
	kind: AtomKind,
) -> FerrosGrynnResult<Arc<ClassSchema>> {
	let mut class = ClassSchema::new(name, kind, [])?;
	class.id = Some(ClassId::new(id).ok_or_else(|| invalid("test class identity is invalid"))?);
	root.install_class(class, u64::from(id))?;
	root.schema.class(name).ok_or_else(|| invalid("installed test class is absent"))
}

fn create_layout(
	root: &Path,
	empty: &DurableAtomStoreImage,
) -> FerrosGrynnResult<(FileCatalog, AtomStoreFileSet)> {
	let superblock = data_file_id(1, DataFileKind::AtomStoreSuperblock)?;
	let atoms_hot = data_file_id(2, DataFileKind::AtomsHot)?;
	let properties = data_file_id(3, DataFileKind::Properties)?;
	let property_dictionary = data_file_id(4, DataFileKind::PropertyDictionary)?;
	let primary = data_file_id(5, DataFileKind::Index)?;
	let atoms_freelist = data_file_id(6, DataFileKind::Freelist)?;
	let properties_freelist = data_file_id(7, DataFileKind::Freelist)?;
	let mut catalog = FileCatalog::bootstrap()?;
	catalog.next_file_instance_id = 8;
	catalog.files.extend([
		catalog_entry(superblock, DataFileRole::Superblock, "atoms/g/superblock.fgb", None)?,
		catalog_entry(atoms_hot, DataFileRole::WalBackfilled, "atoms/g/atoms_hot.fgb", None)?,
		catalog_entry(properties, DataFileRole::WalBackfilled, "atoms/g/properties.fgb", None)?,
		catalog_entry(
			property_dictionary,
			DataFileRole::WalBackfilled,
			"atoms/g/property_keys.fgb",
			None,
		)?,
		catalog_entry(primary, DataFileRole::WalBackfilled, "atoms/g/primary.fgidx", None)?,
		catalog_entry(
			atoms_freelist,
			DataFileRole::WalBackfilled,
			"atoms/g/freelist_atoms.fgb",
			Some(atoms_hot),
		)?,
		catalog_entry(
			properties_freelist,
			DataFileRole::WalBackfilled,
			"atoms/g/freelist_props.fgb",
			Some(properties),
		)?,
	]);
	catalog.files.sort_unstable_by_key(|entry| (entry.space_id, entry.file_id));
	catalog.encode()?;

	for entry in &catalog.files {
		let path = entry.path.resolve(root);
		match entry.file_id.kind() {
			Some(DataFileKind::AtomStoreSuperblock) => {
				write_direct_pages(&path, &[empty.superblock.encode()?])?;
			}
			Some(DataFileKind::PropertyDictionary) => {
				write_direct_pages(&path, &empty.dictionary.pages)?;
			}
			Some(DataFileKind::Index) => {
				write_direct_pages(&path, &empty.atoms.primary.pages)?;
			}
			Some(DataFileKind::Freelist) => {
				let page = AlignedPage::new(
					PageType::Freelist,
					PageFlags::empty(),
					PageLsn::ZERO,
					PageNo::new(0),
				);
				write_direct_pages(&path, &[page])?;
			}
			_ => write_direct_pages(&path, &[])?,
		}
	}
	let files = AtomStoreFileSet::from_catalog(&catalog, primary, empty.atoms.primary.root)?;
	Ok((catalog, files))
}

fn catalog_entry(
	file_id: DataFileId,
	role: DataFileRole,
	path: &str,
	companion_of: Option<DataFileId>,
) -> FerrosGrynnResult<FileCatalogEntry> {
	Ok(FileCatalogEntry {
		space_id: DataSpaceId::Database,
		file_id,
		companion_of,
		role,
		path: CatalogPath::parse(path)?,
	})
}

fn data_file_id(instance: u64, kind: DataFileKind) -> FerrosGrynnResult<DataFileId> {
	DataFileId::new(instance, kind).ok_or_else(|| invalid("test data-file identity is invalid"))
}

fn write_direct_pages(path: &Path, pages: &[AlignedPage]) -> FerrosGrynnResult<()> {
	let parent = path.parent().ok_or_else(|| invalid("test data file has no parent"))?;
	fs::create_dir_all(parent).map_err(|source| {
		FerrosGrynnError::storage_io_failure("create AtomStore test directory")
			.with_technical_source(source)
	})?;
	let file = DirectFile::create_new(path)?;
	if !pages.is_empty() {
		file.write_pages(PageNo::new(0), pages)?;
	}
	file.sync_data()
}

fn corrupt_byte(path: &Path, offset: u64) -> io::Result<()> {
	let mut file = OpenOptions::new().read(true).write(true).open(path)?;
	file.seek(SeekFrom::Start(offset))?;
	file.write_all(&[0xff])?;
	file.sync_data()
}
