use std::{error::Error, sync::Arc};

use rapidhash::RapidHashSet;

use super::support::{CombinedLayout, Fixture, TestDirectory};
use crate::{
	datastore::DatabaseRoot,
	storage::{
		atom_store_image::{
			AtomStoreCheckpointAuthority,
			backfill_atom_store_checkpoint,
			build_durable_atom_store_image,
			open_durable_atom_store_image,
			stage_durable_atom_store_image,
			value_context,
			write_atom_store_checkpoint_page,
		},
		direct::DirectFileOpenMode,
		file_registry::FileRegistry,
		graph_pages::GraphArenaBindings,
		graph_store::{
			GraphCheckpointAuthority,
			GraphFileSet,
			ReverseMembershipFile,
			backfill_graph_checkpoint,
			backfill_reverse_membership_checkpoint,
			build_durable_graph_image,
			build_reverse_membership_image,
			open_durable_graph_image,
			open_reverse_membership_image,
			stage_durable_graph_image,
			stage_reverse_membership_image,
			validate_reverse_memberships,
			write_graph_checkpoint_page,
		},
		types::{CheckpointGeneration, StorageSnapshot},
		wal::{FrameWal, RecoverySeed},
	},
};

/// AtomStore, graph space, and both membership directions share one commit
/// boundary.
#[test]
fn one_wal_transaction_publishes_atom_store_graph_and_memberships() -> Result<(), Box<dyn Error>> {
	let directory = TestDirectory::new()?;
	let fixture = Fixture::build()?;
	let (atom, graph, reverse) = fixture.images()?;
	let empty_database = fixture.empty_database()?;
	let empty_atom = build_durable_atom_store_image(
		&empty_database.atoms,
		&empty_database.schema,
		&empty_database.dictionary,
	)?;
	let mut empty_context =
		value_context(&empty_database.schema, empty_database.dictionary.as_ref().clone())?;
	let empty_graph =
		build_durable_graph_image(&empty_database, fixture.graph_id, &mut empty_context)?;
	let empty_reverse = build_reverse_membership_image(&empty_database)?;
	let layout = CombinedLayout::create(
		directory.as_path(),
		fixture.graph_id,
		&empty_atom,
		&empty_graph,
		&empty_reverse,
	)?;
	let registry = FileRegistry::open(
		layout.catalog.registrations(directory.as_path()),
		DirectFileOpenMode::Synchronous,
		&RapidHashSet::default(),
	)?;
	let wal_path = directory.as_path().join("wal");
	let wal = FrameWal::create_new(&wal_path)?;
	let before = wal.snapshot();

	let mut aborted = wal.begin_tx()?;
	stage_durable_atom_store_image(&mut aborted, layout.atoms, &atom)?;
	stage_durable_graph_image(&mut aborted, layout.graph, &graph)?;
	stage_reverse_membership_image(&mut aborted, layout.reverse, &reverse)?;
	drop(aborted);
	let old = open_snapshot(
		&wal,
		&registry,
		before,
		AtomStoreCheckpointAuthority::EMPTY,
		GraphCheckpointAuthority::EMPTY,
		layout.atoms,
		layout.graph,
		layout.reverse,
		&fixture,
		graph.arena.clone(),
	)?;
	assert_eq!(old.atoms.atoms().count(), 0);
	assert_eq!(old.graph(fixture.graph_id).unwrap().occurrences().count(), 0);

	let mut tx = wal.begin_tx()?;
	stage_durable_atom_store_image(&mut tx, layout.atoms, &atom)?;
	stage_durable_graph_image(&mut tx, layout.graph, &graph)?;
	stage_reverse_membership_image(&mut tx, layout.reverse, &reverse)?;
	wal.commit_tx(tx)?;
	let opened = open_snapshot(
		&wal,
		&registry,
		wal.snapshot(),
		AtomStoreCheckpointAuthority::EMPTY,
		GraphCheckpointAuthority::EMPTY,
		layout.atoms,
		layout.graph,
		layout.reverse,
		&fixture,
		graph.arena.clone(),
	)?;
	assert_opened(&opened, &fixture);

	drop(wal);
	let recovered = FrameWal::open(&wal_path)?;
	let reopened = open_snapshot(
		&recovered,
		&registry,
		recovered.snapshot(),
		AtomStoreCheckpointAuthority::EMPTY,
		GraphCheckpointAuthority::EMPTY,
		layout.atoms,
		layout.graph,
		layout.reverse,
		&fixture,
		graph.arena,
	)?;
	assert_opened(&reopened, &fixture);
	Ok(())
}

/// One checkpoint backfills all data spaces before publishing both metadata
/// pages.
#[test]
fn checkpoint_recovers_atom_and_graph_spaces_from_authorized_cow() -> Result<(), Box<dyn Error>> {
	let directory = TestDirectory::new()?;
	let fixture = Fixture::build()?;
	let (atom, graph, reverse) = fixture.images()?;
	let empty_database = fixture.empty_database()?;
	let empty_atom = build_durable_atom_store_image(
		&empty_database.atoms,
		&empty_database.schema,
		&empty_database.dictionary,
	)?;
	let mut empty_context =
		value_context(&empty_database.schema, empty_database.dictionary.as_ref().clone())?;
	let empty_graph =
		build_durable_graph_image(&empty_database, fixture.graph_id, &mut empty_context)?;
	let empty_reverse = build_reverse_membership_image(&empty_database)?;
	let layout = CombinedLayout::create(
		directory.as_path(),
		fixture.graph_id,
		&empty_atom,
		&empty_graph,
		&empty_reverse,
	)?;
	let registry = FileRegistry::open(
		layout.catalog.registrations(directory.as_path()),
		DirectFileOpenMode::Synchronous,
		&RapidHashSet::default(),
	)?;
	let wal_path = directory.as_path().join("wal");
	let wal = FrameWal::create_new(&wal_path)?;
	let mut tx = wal.begin_tx()?;
	stage_durable_atom_store_image(&mut tx, layout.atoms, &atom)?;
	stage_durable_graph_image(&mut tx, layout.graph, &graph)?;
	stage_reverse_membership_image(&mut tx, layout.reverse, &reverse)?;
	let outcome = wal.commit_tx(tx)?;

	let ticket = wal.begin_checkpoint(outcome.commit_lsn(), CheckpointGeneration::ZERO)?;
	let atom_metadata = backfill_atom_store_checkpoint(&wal, &registry, layout.atoms, &ticket)?
		.ok_or("checkpoint omitted AtomStoreSuperblock")?;
	let graph_metadata = backfill_graph_checkpoint(&wal, &registry, layout.graph, &ticket)?
		.ok_or("checkpoint omitted GraphSuperblock")?;
	backfill_reverse_membership_checkpoint(&wal, &registry, layout.reverse, &ticket)?;
	let checkpoint = wal.finish_checkpoint(ticket)?;
	let atom_authority = AtomStoreCheckpointAuthority {
		generation: checkpoint.generation(),
		backfilled_lsn: checkpoint.target(),
		checkpoint_record_lsn: checkpoint.checkpoint_record_lsn(),
	};
	let graph_authority = GraphCheckpointAuthority {
		generation: checkpoint.generation(),
		backfilled_lsn: checkpoint.target(),
		checkpoint_record_lsn: checkpoint.checkpoint_record_lsn(),
	};
	write_atom_store_checkpoint_page(&registry, layout.atoms, atom_metadata, atom_authority)?;
	write_graph_checkpoint_page(&registry, layout.graph, graph_metadata, graph_authority)?;
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

	let recovered = FrameWal::open_with_seed(&wal_path, seed)?;
	let opened = open_snapshot(
		&recovered,
		&registry,
		recovered.snapshot(),
		atom_authority,
		graph_authority,
		layout.atoms,
		layout.graph,
		layout.reverse,
		&fixture,
		graph.arena,
	)?;
	assert_opened(&opened, &fixture);
	Ok(())
}

#[allow(
	clippy::too_many_arguments,
	reason = "the integration helper mirrors the durable trust boundary"
)]
fn open_snapshot(
	wal: &FrameWal,
	registry: &FileRegistry,
	snapshot: StorageSnapshot,
	atom_authority: AtomStoreCheckpointAuthority,
	graph_authority: GraphCheckpointAuthority,
	atom_files: crate::storage::atom_store_image::AtomStoreFileSet,
	graph_files: GraphFileSet,
	reverse_file: ReverseMembershipFile,
	fixture: &Fixture,
	arena: GraphArenaBindings,
) -> Result<DatabaseRoot, Box<dyn Error>> {
	let (atoms, dictionary, _atom_metadata) = open_durable_atom_store_image(
		wal,
		registry,
		None,
		snapshot,
		atom_authority,
		atom_files,
		&fixture.database.schema,
	)?;
	let mut database = DatabaseRoot::default();
	database.schema = fixture.database.schema.clone();
	database.dictionary = Arc::new(dictionary.clone());
	database.atoms = Arc::new(atoms);
	let context = value_context(&database.schema, dictionary)?;
	let (graph, _graph_metadata) = open_durable_graph_image(
		wal,
		registry,
		None,
		snapshot,
		graph_authority,
		graph_files,
		fixture.reference.clone(),
		arena,
		&database,
		&context.decoder(),
	)?;
	database.install_opened_graph(graph)?;
	let reverse = open_reverse_membership_image(
		wal,
		registry,
		None,
		snapshot,
		atom_authority.generation,
		atom_authority.backfilled_lsn,
		reverse_file,
	)?;
	validate_reverse_memberships(&reverse, &database)?;
	database.rebuild_derived_state()?;
	Ok(database)
}

fn assert_opened(opened: &DatabaseRoot, fixture: &Fixture) {
	assert_eq!(opened.atoms.atoms().count(), fixture.database.atoms.atoms().count());
	assert_eq!(
		opened.graph(fixture.graph_id).unwrap().occurrences().count(),
		fixture.database.graph(fixture.graph_id).unwrap().occurrences().count()
	);
	assert_eq!(opened.membership(fixture.graph_id, fixture.left), Some(fixture.left_local));
	let class = opened.atom(fixture.left).unwrap().class_id();
	assert_eq!(
		opened.indexes.graph_class(fixture.graph_id, class),
		fixture.database.indexes.graph_class(fixture.graph_id, class)
	);
	assert_eq!(opened.statistics.graph(fixture.graph_id).unwrap().node_count, 2);
	assert_eq!(
		opened
			.effective_property(
				fixture.graph_id,
				fixture.left_local,
				opened.dictionary.get("name").unwrap(),
			)
			.unwrap(),
		Some(&crate::values::Value::from("private"))
	);
}
