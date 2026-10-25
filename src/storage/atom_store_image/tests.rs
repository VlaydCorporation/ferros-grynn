use std::{
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

/// Process-local suffix counter that separates temporary atom-store-image test
/// directories.
static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// Appending uses absolute record references without retaining old pages.
#[test]
fn property_append_image_preserves_absolute_page_numbers() {
	use crate::test_assertions::AssertResult as _;

	let mut builder = PropertyStoreBuilder::at_page(17);
	let first = builder.insert(b"first", PageLsn::ZERO).assert_ok();
	let large = vec![7; MAX_INLINE_BYTES + 1];
	let second = builder.insert(&large, PageLsn::ZERO).assert_ok();
	let pages = builder.finish().assert_ok();
	for (index, page) in pages.iter().enumerate() {
		page.validate(PageNo::new(17 + index as u64)).assert_ok();
	}
	assert_eq!(first, PropertyRef::inline(PageNo::new(17), 0, 5).assert_ok());
	assert_eq!(read_property_record(&pages[0], PageNo::new(17), 0, 5).assert_ok(), b"first");
	assert_eq!(second, PropertyRef::large(PageNo::new(18), large.len() as u32).assert_ok());
	let decoded =
		decode_large_property(PageNo::new(18), &pages[1..], large.len() as u32, large.len())
			.assert_ok();
	assert_eq!(decoded, large);
}

/// Packing predictions preserve literal inline/overflow references and bytes.
#[test]
fn property_append_prediction_matches_boundaries_and_tail_closure() {
	use crate::test_assertions::AssertResult as _;

	let lengths = [1, 3, 8176, 8176, 8177, 16336, 16337, 1];
	let expected_growth = [1, 0, 0, 1, 1, 1, 2, 1];
	let expected = [
		PropertyRef::inline(PageNo::new(17), 0, 1).assert_ok(),
		PropertyRef::inline(PageNo::new(17), 1, 3).assert_ok(),
		PropertyRef::inline(PageNo::new(17), 2, 8176).assert_ok(),
		PropertyRef::inline(PageNo::new(18), 0, 8176).assert_ok(),
		PropertyRef::large(PageNo::new(19), 8177).assert_ok(),
		PropertyRef::large(PageNo::new(20), 16336).assert_ok(),
		PropertyRef::large(PageNo::new(21), 16337).assert_ok(),
		PropertyRef::inline(PageNo::new(23), 0, 1).assert_ok(),
	];
	let mut builder = PropertyStoreBuilder::at_page(17);
	for (index, length) in lengths.into_iter().enumerate() {
		let pages = builder.pages.len();
		assert_eq!(builder.additional_pages(length).assert_ok(), expected_growth[index]);
		assert_eq!(builder.pages.len(), pages);
		assert_eq!(builder.insert(&vec![7; length], PageLsn::ZERO).assert_ok(), expected[index]);
		assert_eq!(builder.pages.len() - pages, expected_growth[index]);
	}
	assert!(builder.additional_pages(usize::MAX).is_err());
	let pages = builder.finish().assert_ok();
	assert_eq!(pages.len(), 7);
	for (index, page) in pages.iter().enumerate() {
		page.validate(PageNo::new(17 + index as u64)).assert_ok();
	}
	assert_eq!(read_property_record(&pages[0], PageNo::new(17), 1, 3).assert_ok(), &[7; 3]);
	assert_eq!(
		decode_large_property(PageNo::new(21), &pages[4..6], 16337, 16337).assert_ok(),
		vec![7; 16337]
	);
}

/// Bounded reservation preserves packing bytes and rejects before mutation.
#[test]
fn property_append_bounded_reservation_preserves_bytes_and_reallocation_peak() {
	use crate::{storage::page::PAGE_SIZE, test_assertions::AssertResult as _};

	let mut control = PropertyStoreBuilder::at_page(17);
	let mut bounded = PropertyStoreBuilder::at_page(17);
	for length in [1, 3, 8176, 8176, 8177, 16336, 16337, 1] {
		let record = vec![7; length];
		assert_eq!(
			bounded.insert_bounded(&record, PageLsn::new(13), usize::MAX).assert_ok(),
			control.insert(&record, PageLsn::new(13)).assert_ok()
		);
	}
	let expected = control.finish().assert_ok();
	let actual = bounded.finish().assert_ok();
	assert_eq!(actual.len(), expected.len());
	for (left, right) in actual.iter().zip(&expected) {
		assert_eq!(left.as_bytes(), right.as_bytes());
	}
	let mut bounded = PropertyStoreBuilder::at_page(17);
	assert!(bounded.insert_bounded(&[7], PageLsn::ZERO, PAGE_SIZE - 1).is_err());
	assert_eq!(bounded.page_count(), 0);
	assert_eq!(bounded.allocation_bytes().assert_ok(), 0);
	bounded.insert_bounded(&[7], PageLsn::ZERO, PAGE_SIZE).assert_ok();
	let capacity = bounded.pages.capacity();
	// Force a non-power-of-two request, so geometric Vec growth cannot pass.
	let count = capacity + 3;
	let record = vec![9; count * LARGE_CHUNK_BYTES];
	let peak = (capacity + count + 1) * PAGE_SIZE;
	assert!(bounded.insert_bounded(&record, PageLsn::ZERO, peak - 1).is_err());
	assert_eq!(bounded.page_count(), 1);
	assert_eq!(bounded.pages.capacity(), capacity);
	let reference = bounded.insert_bounded(&record, PageLsn::ZERO, peak).assert_ok();
	assert_eq!(reference, PropertyRef::large(PageNo::new(18), record.len() as u32).assert_ok());
	assert_eq!(bounded.pages.capacity(), count + 1);
	assert_eq!(bounded.finish().assert_ok().len(), count + 1);
}

/// An admitted buffer retains amortized growth without changing durable bytes.
#[test]
fn property_append_bounded_growth_is_amortized() {
	use crate::test_assertions::AssertResult as _;

	let record = vec![7; MAX_INLINE_BYTES + 1];
	let mut control = PropertyStoreBuilder::at_page(17);
	let mut bounded = PropertyStoreBuilder::at_page(17);
	let mut changes = 0;
	for _ in 0..64 {
		let capacity = bounded.pages.capacity();
		assert_eq!(
			bounded.insert_bounded(&record, PageLsn::new(13), usize::MAX).assert_ok(),
			control.insert(&record, PageLsn::new(13)).assert_ok()
		);
		changes += usize::from(bounded.pages.capacity() != capacity);
	}
	assert!(changes <= 8, "64 single-page appends changed capacity {changes} times");
	for (actual, expected) in bounded.finish().assert_ok().iter().zip(control.finish().assert_ok())
	{
		assert_eq!(actual.as_bytes(), expected.as_bytes());
	}
}

/// Tight envelopes admit the required capacity when doubling would not fit.
#[test]
fn property_append_bounded_growth_uses_admitted_capacity() {
	use crate::{gql_status::Status, storage::page::PAGE_SIZE, test_assertions::AssertResult as _};

	let record = vec![7; MAX_INLINE_BYTES + 1];
	let mut bounded = PropertyStoreBuilder::at_page(17);
	for _ in 0..2 {
		bounded.insert_bounded(&record, PageLsn::ZERO, usize::MAX).assert_ok();
	}
	assert_eq!(bounded.pages.capacity(), 2);
	let peak = 5 * PAGE_SIZE;
	let error = bounded.insert_bounded(&record, PageLsn::ZERO, peak - 1).assert_err();
	assert!(matches!(error.status().status(),
		Status::StorageDecodeLimitExceeded { actual, maximum, .. }
		if *actual == peak as u64 && *maximum == (peak - 1) as u64
	));
	assert_eq!(bounded.page_count(), 2);
	assert_eq!(bounded.pages.capacity(), 2);
	bounded.insert_bounded(&record, PageLsn::ZERO, peak).assert_ok();
	assert_eq!(bounded.page_count(), 3);
	assert_eq!(bounded.pages.capacity(), 3);
	assert_eq!(bounded.finish().assert_ok().len(), 3);
}

/// Temporary directory owner used to keep test files alive for their scenario.
struct TestDirectory(
	/// Path retained for the lifetime of the temporary directory owner.
	PathBuf,
);

impl TestDirectory {
	/// Constructs `TestDirectory` from the supplied initial state.
	fn new() -> io::Result<Self> {
		let suffix = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
		let name = format!(".ferros-grynn-atom-store-{}-{suffix}", std::process::id());
		Ok(Self(std::env::current_dir()?.join(name)))
	}

	/// Borrows the wrapped path representation.
	fn as_path(&self) -> &Path {
		&self.0
	}
}

impl Drop for TestDirectory {
	/// Runs cleanup for this `TestDirectory` before its fields are released.
	fn drop(&mut self) {
		drop(fs::remove_dir_all(&self.0));
	}
}

/// Verifies atom store image round trips global ids primary and properties.
#[test]
fn atom_store_image_round_trips_global_ids_primary_and_properties() {
	use crate::test_assertions::AssertResult as _;

	let mut root = DatabaseRoot::default();
	let person = install_class(&mut root, 1, "person", AtomKind::Node).assert_ok();
	let relation = install_class(&mut root, 2, "relation", AtomKind::Edge).assert_ok();
	let name = Arc::make_mut(&mut root.dictionary).intern("name").assert_ok();
	let link = Arc::make_mut(&mut root.dictionary).intern("link").assert_ok();

	let first_record = crate::values::RecordId::new("person", RecordIdPart::Int(1)).assert_ok();
	let mut first_properties = PropertyMap::new();
	first_properties.insert(name, "Ada");
	let first = root.insert_atom(&person, first_record.clone(), first_properties).assert_ok();

	let edge_record = crate::values::RecordId::new("relation", RecordIdPart::Int(9)).assert_ok();
	let edge = GlobalAtomId::from_raw(
		AtomId::from_parts(AtomKind::Edge, ATOM_SLOTS_PER_PAGE + 1, 7).assert_ok().raw(),
	);
	let mut edge_properties = PropertyMap::new();
	edge_properties.insert(link, Value::from(AtomRef::new(AtomKind::Node, first_record)));
	edge_properties.insert(PropertyKeyId::new(2), Value::from("x".repeat(MAX_INLINE_BYTES + 1)));
	root.insert_atom_at(edge, &relation, edge_record, edge_properties).assert_ok();
	Arc::make_mut(&mut root.atoms).restore_free_slot(ATOM_SLOTS_PER_PAGE, 4).assert_ok();

	let capacity = root.atoms.slot_capacity();
	let mut dictionary = root.dictionary.as_ref().clone();
	let image =
		build_atom_store_image(&root.atoms, &root.schema, &mut dictionary, PageLsn::new(19))
			.assert_ok();
	assert_eq!(image.atoms_hot.len(), 2);
	let mut reopened =
		open_atom_store_image(image, capacity, &root.schema, &dictionary, &[]).assert_ok();
	assert_eq!(reopened.atoms().collect::<Vec<_>>(), root.atoms.atoms().collect::<Vec<_>>());
	assert_eq!(reopened.resolve(&"person:1".parse().assert_ok()), Some(first));

	let next =
		reopened.insert(&person, "person:2".parse().assert_ok(), PropertyMap::new()).assert_ok();
	assert_eq!((next.slot(), next.generation()), (ATOM_SLOTS_PER_PAGE, 4));
}

/// Verifies durable commit preserves snapshots and recovers wal frames.
#[test]
fn durable_commit_preserves_snapshots_and_recovers_wal_frames() {
	use crate::test_assertions::AssertResult as _;

	let directory = TestDirectory::new().assert_ok();
	let mut root = DatabaseRoot::default();
	let person = install_class(&mut root, 1, "person", AtomKind::Node).assert_ok();
	let name = Arc::make_mut(&mut root.dictionary).intern("name").assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(name, "Ada");
	let record: crate::values::RecordId = "person:1".parse().assert_ok();
	let atom = root.insert_atom(&person, record.clone(), properties).assert_ok();
	let image =
		build_durable_atom_store_image(&root.atoms, &root.schema, &root.dictionary).assert_ok();

	let empty = build_durable_atom_store_image(
		&AtomStoreRoot::default(),
		&SchemaCatalog::default(),
		&PropertyDictionary::new(),
	)
	.assert_ok();
	let (catalog, files) = create_layout(directory.as_path(), &empty).assert_ok();
	let registry = FileRegistry::open(
		catalog.registrations(directory.as_path()),
		DirectFileOpenMode::Synchronous,
		&RapidHashSet::default(),
	)
	.assert_ok();
	let wal_path = directory.as_path().join("wal");
	let wal = FrameWal::create_new(&wal_path).assert_ok();
	let before = wal.snapshot();

	let mut aborted = wal.begin_tx().assert_ok();
	stage_durable_atom_store_image(&mut aborted, files, &image).assert_ok();
	drop(aborted);
	let (uncommitted, dictionary, _) = open_durable_atom_store_image(
		&wal,
		&registry,
		None,
		wal.snapshot(),
		AtomStoreCheckpointAuthority::EMPTY,
		files,
		&root.schema,
		&[],
	)
	.assert_ok();
	assert_eq!(uncommitted.atoms().count(), 0);
	assert!(dictionary.is_empty());

	let mut tx = wal.begin_tx().assert_ok();
	stage_durable_atom_store_image(&mut tx, files, &image).assert_ok();
	stage_durable_atom_store_image(&mut tx, files, &image).assert_ok();
	let outcome = wal.commit_tx(tx).assert_ok();
	let (old, old_dictionary, _) = open_durable_atom_store_image(
		&wal,
		&registry,
		None,
		before,
		AtomStoreCheckpointAuthority::EMPTY,
		files,
		&root.schema,
		&[],
	)
	.assert_ok();
	assert_eq!(old.atoms().count(), 0);
	assert!(old_dictionary.is_empty());

	corrupt_byte(
		registry.resolve_file(DataSpaceId::Database, files.superblock).assert_ok().path(),
		80,
	)
	.assert_ok();
	let (opened, dictionary, metadata) = open_durable_atom_store_image(
		&wal,
		&registry,
		None,
		wal.snapshot(),
		AtomStoreCheckpointAuthority::EMPTY,
		files,
		&root.schema,
		&[],
	)
	.assert_ok();
	assert_eq!(opened.resolve(&record), Some(atom));
	assert_eq!(dictionary.get("name"), Some(name));
	assert_eq!(metadata.live_count().assert_ok(), 1);
	assert_eq!(
		wal.recovery_frames()
			.iter()
			.filter(|frame| frame.key().file_id() == files.superblock)
			.count(),
		1
	);

	let mut unchanged = wal.begin_tx().assert_ok();
	let mut ignored_lsn = metadata;
	ignored_lsn.page_lsn = PageLsn::new(u64::MAX);
	assert!(
		!stage_atom_store_superblock_if_changed(
			&mut unchanged,
			files.superblock,
			metadata,
			ignored_lsn,
		)
		.assert_ok()
	);
	assert_eq!(wal.commit_tx(unchanged).assert_ok().commit_lsn(), outcome.commit_lsn());

	drop(wal);
	let reopened_wal = FrameWal::open(&wal_path).assert_ok();
	let (reopened, reopened_dictionary, reopened_metadata) = open_durable_atom_store_image(
		&reopened_wal,
		&registry,
		None,
		reopened_wal.snapshot(),
		AtomStoreCheckpointAuthority::EMPTY,
		files,
		&root.schema,
		&[],
	)
	.assert_ok();
	assert_eq!(reopened.resolve(&record), Some(atom));
	assert_eq!(reopened_dictionary.get("name"), Some(name));
	assert_eq!(reopened_metadata.live_count().assert_ok(), 1);
}

/// Verifies checkpoint backfills data and recovers only authorized cow
/// candidates.
#[test]
fn checkpoint_backfills_data_and_recovers_only_authorized_cow_candidates() {
	use crate::test_assertions::AssertResult as _;

	let directory = TestDirectory::new().assert_ok();
	let mut root = DatabaseRoot::default();
	let person = install_class(&mut root, 1, "person", AtomKind::Node).assert_ok();
	let name = Arc::make_mut(&mut root.dictionary).intern("name").assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(name, "Grace");
	let record: crate::values::RecordId = "person:2".parse().assert_ok();
	let atom = root.insert_atom(&person, record.clone(), properties).assert_ok();
	let image =
		build_durable_atom_store_image(&root.atoms, &root.schema, &root.dictionary).assert_ok();
	let empty = build_durable_atom_store_image(
		&AtomStoreRoot::default(),
		&SchemaCatalog::default(),
		&PropertyDictionary::new(),
	)
	.assert_ok();
	let (catalog, files) = create_layout(directory.as_path(), &empty).assert_ok();
	let registry = FileRegistry::open(
		catalog.registrations(directory.as_path()),
		DirectFileOpenMode::Synchronous,
		&RapidHashSet::default(),
	)
	.assert_ok();
	let wal_path = directory.as_path().join("wal");
	let wal = FrameWal::create_new(&wal_path).assert_ok();
	let mut tx = wal.begin_tx().assert_ok();
	stage_durable_atom_store_image(&mut tx, files, &image).assert_ok();
	let outcome = wal.commit_tx(tx).assert_ok();

	let ticket = wal.begin_checkpoint(outcome.commit_lsn(), CheckpointGeneration::ZERO).assert_ok();
	let logical = backfill_atom_store_checkpoint(&wal, &registry, files, &ticket)
		.assert_ok()
		.ok_or_else(|| io::Error::other("checkpoint omitted the AtomStore superblock"))
		.assert_ok();
	let checkpoint = wal.finish_checkpoint(ticket).assert_ok();
	let authority = AtomStoreCheckpointAuthority {
		generation: checkpoint.generation(),
		backfilled_lsn: checkpoint.target(),
		checkpoint_record_lsn: checkpoint.checkpoint_record_lsn(),
	};
	let checkpointed =
		write_atom_store_checkpoint_page(&registry, files, logical, authority).assert_ok();
	assert_eq!(checkpointed.page_lsn, logical.page_lsn);
	wal.publish_checkpoint(&checkpoint).assert_ok();
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

	let reopened_wal = FrameWal::open_with_seed(&wal_path, seed).assert_ok();
	let (opened, dictionary, metadata) = open_durable_atom_store_image(
		&reopened_wal,
		&registry,
		None,
		reopened_wal.snapshot(),
		authority,
		files,
		&root.schema,
		&[],
	)
	.assert_ok();
	assert_eq!(opened.resolve(&record), Some(atom));
	assert_eq!(dictionary.get("name"), Some(name));
	assert_eq!(metadata, checkpointed);

	let next_authority = AtomStoreCheckpointAuthority {
		generation: CheckpointGeneration::new(authority.generation.get() + 1),
		backfilled_lsn: authority.backfilled_lsn,
		checkpoint_record_lsn: Lsn::new(authority.checkpoint_record_lsn.get() + 1),
	};
	let ahead =
		write_atom_store_checkpoint_page(&registry, files, logical, next_authority).assert_ok();
	let (_, _, selected_previous) = open_durable_atom_store_image(
		&reopened_wal,
		&registry,
		None,
		reopened_wal.snapshot(),
		authority,
		files,
		&root.schema,
		&[],
	)
	.assert_ok();
	assert_eq!(selected_previous, checkpointed);

	let third_authority = AtomStoreCheckpointAuthority {
		generation: CheckpointGeneration::new(next_authority.generation.get() + 1),
		backfilled_lsn: next_authority.backfilled_lsn,
		checkpoint_record_lsn: Lsn::new(next_authority.checkpoint_record_lsn.get() + 1),
	};
	let temporary = temporary_path(
		registry.resolve_file(DataSpaceId::Database, files.superblock).assert_ok().path(),
	);
	let staged_temporary = logical
		.at_checkpoint(
			third_authority.generation,
			third_authority.backfilled_lsn,
			third_authority.checkpoint_record_lsn,
		)
		.assert_ok();
	write_direct_pages(&temporary, &[staged_temporary.encode().assert_ok()]).assert_ok();
	let (_, _, ignored_temporary) = open_durable_atom_store_image(
		&reopened_wal,
		&registry,
		None,
		reopened_wal.snapshot(),
		third_authority,
		files,
		&root.schema,
		&[],
	)
	.assert_ok();
	assert_eq!(ignored_temporary, ahead);

	let superblock_path = registry
		.resolve_file(DataSpaceId::Database, files.superblock)
		.assert_ok()
		.path()
		.to_path_buf();
	assert!(previous_path(&superblock_path).exists());
	corrupt_byte(&superblock_path, 88).assert_ok();
	let (_, _, recovered_previous) = open_durable_atom_store_image(
		&reopened_wal,
		&registry,
		None,
		reopened_wal.snapshot(),
		next_authority,
		files,
		&root.schema,
		&[],
	)
	.assert_ok();
	assert_eq!(recovered_previous, checkpointed);

	let next_record: crate::values::RecordId = "person:3".parse().assert_ok();
	let next_atom = root.insert_atom(&person, next_record.clone(), PropertyMap::new()).assert_ok();
	let mut next_image =
		build_durable_atom_store_image(&root.atoms, &root.schema, &root.dictionary).assert_ok();
	next_image.superblock = next_image
		.superblock
		.at_checkpoint(
			authority.generation,
			authority.backfilled_lsn,
			authority.checkpoint_record_lsn,
		)
		.assert_ok();
	let mut post_checkpoint_tx = reopened_wal.begin_tx().assert_ok();
	stage_durable_atom_store_image(&mut post_checkpoint_tx, files, &next_image).assert_ok();
	reopened_wal.commit_tx(post_checkpoint_tx).assert_ok();
	let (post_checkpoint, _, post_checkpoint_metadata) = open_durable_atom_store_image(
		&reopened_wal,
		&registry,
		None,
		reopened_wal.snapshot(),
		authority,
		files,
		&root.schema,
		&[],
	)
	.assert_ok();
	assert_eq!(post_checkpoint.resolve(&next_record), Some(next_atom));
	assert!(post_checkpoint_metadata.page_lsn.get() > authority.backfilled_lsn.get());
}

/// Returns install class for this module.
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

/// Creates layout for this module.
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

/// Returns catalog entry for this module.
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

/// Returns data file id for this module.
fn data_file_id(instance: u64, kind: DataFileKind) -> FerrosGrynnResult<DataFileId> {
	DataFileId::new(instance, kind).ok_or_else(|| invalid("test data-file identity is invalid"))
}

/// Writes direct pages for this module.
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

/// Returns corrupt byte for this module.
fn corrupt_byte(path: &Path, offset: u64) -> io::Result<()> {
	let mut file = OpenOptions::new().read(true).write(true).open(path)?;
	file.seek(SeekFrom::Start(offset))?;
	file.write_all(&[0xff])?;
	file.sync_data()
}
