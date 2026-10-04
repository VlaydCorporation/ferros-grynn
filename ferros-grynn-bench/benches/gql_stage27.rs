//! Stage-27 bounded capture/read Criterion workloads and atomic import probes.
//! FG27_ROWS: 10000/100000/1000000; FG27_LOCAL=1 selects local Sync.
//! FG27_PROBE=1 measures one ON/OFF import and validates its entire history.

use std::{
	hint::black_box,
	path::PathBuf,
	time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion};
use ferros_grynn::{
	changefeed::{
		ChangeIdentity,
		ChangefeedConfig,
		ChangefeedGapReason,
		ChangefeedId,
		ChangefeedReadItem,
		ChangefeedReadMode,
		ChangefeedReadOptions,
		ChangefeedSince,
		ChangefeedSource,
		Versionstamp,
	},
	datastore::{
		ClassField,
		ClassSchema,
		Datastore,
		ImportChangefeedMode,
		ImportOptions,
		InMemoryOptions,
		LocalOptions,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult, Status},
	graph::AtomKind,
	values::{PropertyKeyId, PropertyMap, RecordId, Type, Uuid},
};

/// Failure terminates measurement instead of creating a successful sample.
fn require<T>(result: FerrosGrynnResult<T>) -> T {
	match result {
		Ok(value) => value,
		Err(error) => {
			eprintln!("Stage-27 benchmark failed: {error}");
			std::process::exit(1);
		}
	}
}

/// Keeps the local directory until every datastore handle has been released.
struct Fixture {
	store: Option<Datastore>,
	directory: Option<PathBuf>,
	records: Vec<RecordId>,
	key: PropertyKeyId,
	feed: ChangefeedId,
	since: Versionstamp,
	value: i64,
}

impl Drop for Fixture {
	fn drop(&mut self) {
		drop(self.store.take());
		if let Some(directory) = self.directory.take() {
			drop(std::fs::remove_dir_all(directory));
		}
	}
}

impl Fixture {
	/// Borrows the retained store without exposing a storage handle.
	fn store(&self) -> FerrosGrynnResult<&Datastore> {
		self.store
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("benchmark store released"))
	}
}

/// Configures a source before optional import so ON/OFF compare identical
/// inputs.
fn fixture(rows: usize, full: bool, local: bool, populate: bool) -> FerrosGrynnResult<Fixture> {
	let directory = local.then(|| {
		std::env::temp_dir()
			.join(format!("cf27-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64))
	});
	let store = if let Some(directory) = &directory {
		let mut options = LocalOptions::default();
		options.max_snapshot_age = Duration::from_secs(3600);
		Datastore::create_local(directory, options)?
	} else {
		let mut options = InMemoryOptions::default();
		options.max_snapshot_age = Duration::from_secs(3600);
		Datastore::in_memory_with(options)
	};
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"value",
		Type::Int,
	)])?)?;
	let retention = "P1D"
		.parse::<ferros_grynn::values::Duration>()
		.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))?;
	let feed = tx.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new(retention)?.with_full(full),
	)?;
	let since = tx.commit()?.versionstamp();
	let key = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("benchmark field absent"))?;
	let records = (0..rows)
		.map(|row| format!("item:r{row}").parse::<RecordId>())
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let mut fixture = Fixture {
		store: Some(store),
		directory,
		records,
		key,
		feed,
		since,
		value: 0,
	};
	if populate {
		import(&fixture, ImportChangefeedMode::Off)?;
		fixture.since = fixture.store()?.begin().commit()?.versionstamp();
	}
	Ok(fixture)
}

/// Uses private segments without permitting configuration DDL inside import.
fn import(fixture: &Fixture, mode: ImportChangefeedMode) -> FerrosGrynnResult<()> {
	let mut options = ImportOptions::default();
	options.segment_size = 2048;
	options.changefeed = mode;
	let report = fixture.store()?.import(options, |session| {
		for record in &fixture.records {
			session.apply_item(|tx| {
				let mut properties = PropertyMap::new();
				properties.insert(fixture.key, 0);
				tx.create_atom(record.clone(), properties)
			})?;
		}
		Ok(())
	})?;
	if report.applied_items() != fixture.records.len() as u64 {
		return Err(FerrosGrynnError::invalid_argument("import cardinality differs"));
	}
	Ok(())
}

/// Keeps mutation cardinality fixed across different background datasets.
fn mutate(fixture: &mut Fixture) -> FerrosGrynnResult<()> {
	fixture.value = 1 - fixture.value;
	let mut tx = fixture.store()?.begin();
	for record in fixture.records.iter().take(64) {
		tx.set_base_property(record, fixture.key, fixture.value.into())?;
	}
	black_box(tx.commit()?);
	Ok(())
}

/// Validates complete history, returning logical observations after EOF.
fn read(
	fixture: &Fixture,
	mode: ChangefeedReadMode,
	expected: u64,
) -> FerrosGrynnResult<ferros_grynn::changefeed::ChangefeedReadStatistics> {
	let mut tx = fixture.store()?.begin();
	let options = ChangefeedReadOptions::new(ChangefeedSince::Versionstamp(fixture.since))
		.with_mode(mode)
		.with_budgets(256 * 1024 * 1024, 100_000_000)?;
	let mut reader = tx.changes(fixture.feed, options)?;
	let mut entries = 0_u64;
	while let Some(item) = reader.next_batch()? {
		match item {
			ChangefeedReadItem::Commit(batch) => {
				entries += batch.entries().len() as u64;
				black_box(batch);
			}
			_ => {
				return Err(FerrosGrynnError::invalid_argument("unexpected benchmark history GAP"));
			}
		}
	}
	if entries != expected {
		return Err(FerrosGrynnError::invalid_argument("history cardinality differs"));
	}
	Ok(reader.statistics())
}

/// Checks one atomic import's exact source, images, execution order, or GAP.
/// Read timings include this validation and are not cold-I/O measurements.
fn read_import(
	fixture: &Fixture,
	capture: ImportChangefeedMode,
	stamp: Versionstamp,
) -> FerrosGrynnResult<ferros_grynn::changefeed::ChangefeedReadStatistics> {
	let mut tx = fixture.store()?.begin();
	let options = ChangefeedReadOptions::new(ChangefeedSince::Versionstamp(fixture.since))
		.with_mode(ChangefeedReadMode::Full)
		.with_budgets(256 * 1024 * 1024, 100_000_000)?;
	let mut reader = tx.changes(fixture.feed, options)?;
	match reader.next_batch()? {
		Some(ChangefeedReadItem::Commit(batch)) if capture == ImportChangefeedMode::On => {
			if batch.source() != fixture.feed
				|| batch.versionstamp() != stamp
				|| batch.entries().len() != fixture.records.len()
			{
				return Err(FerrosGrynnError::invalid_argument(
					"import batch source, stamp or cardinality differs",
				));
			}
			for (ordinal, (entry, record)) in
				batch.entries().iter().zip(&fixture.records).enumerate()
			{
				let image = entry.after().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("import after image absent")
				})?;
				if entry.ordinal() != ordinal as u64
					|| entry.operation() != ordinal as u64
					|| entry.before().is_some()
					|| !matches!(image.identity(), ChangeIdentity::Record(atom) if atom.id() == record && atom.kind() == AtomKind::Node)
					|| image.properties().get("value") != Some(&0_i64.into())
					|| image.properties().len() != 1
				{
					return Err(FerrosGrynnError::invalid_argument(
						"import FULL identity, ordinal or image differs",
					));
				}
			}
		}
		Some(ChangefeedReadItem::Gap(gap))
			if capture == ImportChangefeedMode::Off
				&& gap.source() == fixture.feed
				&& gap.reason() == ChangefeedGapReason::ImportSuppressed
				&& gap.from() == Some(stamp)
				&& gap.through() == Some(stamp) => {}
		_ => {
			return Err(FerrosGrynnError::invalid_argument(
				"import did not publish its exact commit or suppression GAP",
			));
		}
	}
	if reader.next_batch()?.is_some() {
		return Err(FerrosGrynnError::invalid_argument("import published additional history"));
	}
	Ok(reader.statistics())
}

/// Measures currently retained WAL files, independently of feed page counters.
fn wal_bytes(fixture: &Fixture) -> FerrosGrynnResult<u64> {
	let Some(directory) = &fixture.directory else {
		return Ok(0);
	};
	let io_error = |error| {
		FerrosGrynnError::storage_io_failure("inspect benchmark WAL bytes")
			.with_technical_source(error)
	};
	let mut bytes = 0_u64;
	for entry in std::fs::read_dir(directory.join("wal")).map_err(io_error)? {
		let metadata = entry.map_err(io_error)?.metadata().map_err(io_error)?;
		if metadata.is_file() {
			bytes = bytes.checked_add(metadata.len()).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("benchmark WAL bytes overflow")
			})?;
		}
	}
	Ok(bytes)
}

/// Measures bounded asynchronous maintenance, including one-millisecond
/// polling.
fn purge(fixture: &Fixture) -> FerrosGrynnResult<u128> {
	use ferros_grynn::datastore::MaintenanceJobState;
	let store = fixture.store()?;
	let info = store.begin().changefeed_info(fixture.feed)?;
	let retention = "PT0.000000001S"
		.parse::<ferros_grynn::values::Duration>()
		.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))?;
	let mut tx = store.begin();
	let started = Instant::now();
	tx.configure_changefeed(info.source(), ChangefeedConfig::new(retention)?.with_full(true))?;
	tx.commit()?;
	let id = store.purge_changefeeds(None)?;
	loop {
		let job = store
			.job(id)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("purge benchmark job absent"))?;
		match job.state() {
			MaintenanceJobState::Completed => break,
			MaintenanceJobState::Failed
			| MaintenanceJobState::Cancelled
			| MaintenanceJobState::Interrupted => {
				return Err(FerrosGrynnError::invalid_argument(format!(
					"purge benchmark job {:?}, status {:?}: {}",
					job.state(),
					job.terminal_status(),
					job.diagnostic().unwrap_or("no diagnostic recorded")
				)));
			}
			_ => {}
		}
		if started.elapsed() > Duration::from_secs(60) {
			return Err(FerrosGrynnError::invalid_argument("purge benchmark deadline exceeded"));
		}
		std::thread::sleep(Duration::from_millis(1));
	}
	if store.begin().changefeed_info(fixture.feed)?.purged_through() < info.last_versionstamp() {
		return Err(FerrosGrynnError::invalid_argument("purge benchmark retained expired prefix"));
	}
	Ok(started.elapsed().as_nanos())
}

/// Reports one complete import observation without treating rejection as
/// success.
fn probe(rows: usize, local: bool) -> FerrosGrynnResult<()> {
	for capture in [ImportChangefeedMode::Off, ImportChangefeedMode::On] {
		let fixture = fixture(rows, true, local, false)?;
		let before = fixture.store()?.begin().changefeed_info(fixture.feed)?;
		let started = Instant::now();
		let result = import(&fixture, capture);
		let elapsed = started.elapsed().as_nanos();
		if let Err(error) = result {
			if rows == 10_000
				|| !matches!(
					error.status().status(),
					Status::StorageDecodeLimitExceeded { .. }
						| Status::TransactionWriteSetLimitExceeded { .. }
				) {
				return Err(error);
			}
			let mut tx = fixture.store()?.begin();
			for record in &fixture.records {
				if tx.atom(record)?.is_some() {
					return Err(FerrosGrynnError::invalid_argument(
						"failed import published a partial primary",
					));
				}
			}
			if tx.changefeed_info(fixture.feed)? != before {
				return Err(FerrosGrynnError::invalid_argument(
					"failed import changed feed metadata or commit stamp",
				));
			}
			let mut history = tx.changes(
				fixture.feed,
				ChangefeedReadOptions::new(ChangefeedSince::Versionstamp(fixture.since)),
			)?;
			if history.next_batch()?.is_some() {
				return Err(FerrosGrynnError::invalid_argument(
					"failed import published history or GAP",
				));
			}
			println!(
				concat!(
					"stage27_probe\trows={}\tlocal={}\tmode={:?}\tstatus=rejected",
					"\telapsed_ns={}\terror={}"
				),
				rows, local, capture, elapsed, error
			);
			continue;
		}
		let stamp = fixture.store()?.begin().commit()?.versionstamp();
		let cold = Instant::now();
		let statistics = read_import(&fixture, capture, stamp)?;
		let cold_ns = cold.elapsed().as_nanos();
		let warm = Instant::now();
		let repeated = read_import(&fixture, capture, stamp)?;
		let warm_ns = warm.elapsed().as_nanos();
		if repeated.entries != statistics.entries {
			return Err(FerrosGrynnError::invalid_argument("warm history differs"));
		}
		let capture_stats = fixture.store()?.changefeed_statistics()?;
		let wal_bytes = wal_bytes(&fixture)?;
		let purge_ns = purge(&fixture)?;
		println!(
			concat!(
				"stage27_probe\trows={}\tlocal={}\tmode={:?}\tstatus=ok",
				"\timport_ns={}\tfirst_read_ns={}\twarm_read_ns={}\tpages={}",
				"\tread_bytes={}\treader_admitted_peak={}\tcapture_admitted_peak={}",
				"\tfeed_bytes={}\tgroups={}\twal_bytes={}\tttl_to_purge_complete_ns={}"
			),
			rows,
			local,
			capture,
			elapsed,
			cold_ns,
			warm_ns,
			statistics.page_reads,
			statistics.bytes_read,
			statistics.peak_scratch_bytes,
			capture_stats.peak_capture_scratch_bytes,
			capture_stats.retained_feed_bytes,
			statistics.batches,
			wal_bytes,
			purge_ns
		);
	}
	Ok(())
}

/// Runs separate immutable read and bounded capture workloads.
fn run() -> FerrosGrynnResult<()> {
	let rows = match std::env::var("FG27_ROWS") {
		Ok(value) => value
			.parse::<usize>()
			.map_err(|error| FerrosGrynnError::invalid_argument(error.to_string()))?,
		Err(std::env::VarError::NotPresent) => 10_000,
		Err(error) => return Err(FerrosGrynnError::invalid_argument(error.to_string())),
	};
	if ![10_000, 100_000, 1_000_000].contains(&rows) {
		return Err(FerrosGrynnError::invalid_argument(
			"FG27_ROWS must be 10000, 100000 or 1000000",
		));
	}
	let local = std::env::var("FG27_LOCAL").is_ok_and(|value| value == "1");
	if std::env::var("FG27_PROBE").is_ok_and(|value| value == "1") {
		return probe(rows, local);
	}
	let mut criterion = Criterion::default().configure_from_args();
	let mut group = criterion.benchmark_group(if local {
		"gql_stage27_sync"
	} else {
		"gql_stage27_ram"
	});
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(500));
	group.measurement_time(Duration::from_secs(2));
	for full in [false, true] {
		let mut capture = fixture(rows, full, local, true)?;
		let capture_start = capture.store()?.changefeed_statistics()?;
		group.bench_function(
			BenchmarkId::new(
				if full {
					"capture_full_64"
				} else {
					"capture_net_64"
				},
				rows,
			),
			|bench| bench.iter(|| require(mutate(black_box(&mut capture)))),
		);
		let capture_end = capture.store()?.changefeed_statistics()?;
		println!(
			concat!(
				"stage27_rolling_capture\trows={}\tlocal={}\tfull={}",
				"\tstart_groups={}\tend_groups={}",
				"\tstart_feed_bytes={}\tend_feed_bytes={}",
				"\tcapture_admitted_peak={}"
			),
			rows,
			local,
			full,
			capture_start.published_groups,
			capture_end.published_groups,
			capture_start.retained_feed_bytes,
			capture_end.retained_feed_bytes,
			capture_end.peak_capture_scratch_bytes
		);
		let mut history = fixture(rows, full, local, true)?;
		mutate(&mut history)?;
		group.bench_function(
			BenchmarkId::new(
				if full {
					"read_full_64"
				} else {
					"read_net_64"
				},
				rows,
			),
			|bench| {
				bench.iter(|| {
					black_box(require(read(
						black_box(&history),
						if full {
							ChangefeedReadMode::Full
						} else {
							ChangefeedReadMode::Net
						},
						rows.min(64) as u64,
					)))
				})
			},
		);
	}
	group.finish();
	criterion.final_summary();
	Ok(())
}

/// Keeps all failed measurements explicit.
fn main() {
	require(run());
}
