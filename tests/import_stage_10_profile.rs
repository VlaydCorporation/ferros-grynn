#![cfg(windows)]
#![allow(
	clippy::expect_used,
	clippy::unwrap_used,
	reason = "the ignored profiling probe must stop on an invalid harness configuration"
)]

use std::{
	error::Error,
	fs::{self, OpenOptions},
	io::Write,
	path::{Path, PathBuf},
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	thread,
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ferros_grynn::{
	datastore::{
		ClassField,
		ClassIndex,
		ClassIndexKind,
		ClassSchema,
		Datastore,
		ImportIndexMode,
		ImportOptions,
		IndexScope,
		LocalOptions,
	},
	graph::AtomKind,
	values::{PropertyMap, RecordId, RecordIdPart, Type},
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

struct TestDirectory(PathBuf);

impl TestDirectory {
	fn new() -> Self {
		let nonce = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos())
			.unwrap_or(u64::MAX);
		Self(std::env::temp_dir().join(format!("fgdb-{nonce:016x}")))
	}
}

impl Drop for TestDirectory {
	fn drop(&mut self) {
		drop(fs::remove_dir_all(&self.0));
	}
}

#[test]
#[ignore = "storage-heavy Stage-10 profiling probe"]
fn stage10_atomic_import_profile() -> Result<(), Box<dyn Error>> {
	let items = environment_usize("FG_IMPORT_PROFILE_ITEMS", 20_000)?;
	let segment_size = environment_usize("FG_IMPORT_PROFILE_SEGMENT_SIZE", 4_096)?;
	let (mode_name, mode) = match std::env::var("FG_IMPORT_PROFILE_MODE")
		.unwrap_or_else(|_| "deferred".to_owned())
		.to_ascii_lowercase()
		.as_str()
	{
		"deferred" => ("deferred", ImportIndexMode::Deferred),
		"maintained" => ("maintained", ImportIndexMode::Maintained),
		value => return Err(format!("unsupported FG_IMPORT_PROFILE_MODE '{value}'").into()),
	};
	let backend = std::env::var("FG_IMPORT_PROFILE_BACKEND")
		.unwrap_or_else(|_| "local".to_owned())
		.to_ascii_lowercase();
	let directory = TestDirectory::new();
	let store = match backend.as_str() {
		"local" => {
			let mut options = LocalOptions::default();
			options.buffer_pool_pages = 64;
			Datastore::create_local(&directory.0, options)?
		}
		"memory" => Datastore::in_memory(),
		value => return Err(format!("unsupported FG_IMPORT_PROFILE_BACKEND '{value}'").into()),
	};
	let (group_key, email_key) = define_profile_schema(&store)?;
	let wal_before = directory_bytes(&directory.0.join("wal"));
	let rss_before = process_rss();
	let running = Arc::new(AtomicBool::new(true));
	let sampler = spawn_rss_sampler(running.clone(), rss_before);

	let mut options = ImportOptions::default();
	options.indexes = mode;
	options.segment_size = segment_size;
	let started = Instant::now();
	let result = store.import(options, |session| {
		for item in 0..items {
			session.apply_item(|transaction| {
				let mut properties = PropertyMap::with_capacity(2);
				properties.insert(
					group_key,
					i64::try_from(item % 1_024)
						.map_err(|_error| profile_error("profile group exceeds i64"))?,
				);
				properties.insert(email_key, format!("item-{item:08}@example.test"));
				transaction.create_atom(
					RecordId::new(
						"person",
						RecordIdPart::String(format!("item-{item:08}").into()),
					)?,
					properties,
				)
			})?;
		}
		Ok(())
	});
	let elapsed = started.elapsed();
	running.store(false, Ordering::Release);
	let rss_peak = sampler.join().expect("RSS sampler must not panic");
	let report = result?;
	let wal_bytes = directory_bytes(&directory.0.join("wal")).saturating_sub(wal_before);
	let database_bytes = directory_bytes(&directory.0);
	let index_timing_ns = report.index_timings().values().map(Duration::as_nanos).sum::<u128>();
	let elapsed_ns = elapsed.as_nanos();
	let items_per_second = u128::try_from(items)
		.unwrap_or(u128::MAX)
		.saturating_mul(1_000_000_000)
		.checked_div(elapsed_ns.max(1))
		.unwrap_or(0);
	let rss_delta = rss_peak.saturating_sub(rss_before);
	let timestamp_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
	let row = format!(
		concat!("{},{},{},{},{},{},", "{},{},{},{},{},{},", "{},{},{},{},{}"),
		timestamp_ms,
		backend,
		mode_name,
		items,
		segment_size,
		elapsed_ns,
		items_per_second,
		rss_before,
		rss_peak,
		rss_delta,
		database_bytes,
		wal_bytes,
		report.processed_items(),
		report.applied_items(),
		report.segments(),
		report.built_indexes().len(),
		index_timing_ns,
	);
	write_csv(&row)?;
	println!("FG_IMPORT_STAGE10,{row}");
	std::hint::black_box(store);
	Ok(())
}

fn define_profile_schema(
	store: &Datastore,
) -> Result<
	(ferros_grynn::values::PropertyKeyId, ferros_grynn::values::PropertyKeyId),
	Box<dyn Error>,
> {
	let mut schema = store.begin();
	let class = ClassSchema::new("person", AtomKind::Node, [
		ClassField::new("group", Type::Int),
		ClassField::new("email", Type::String),
	])?
	.with_index(ClassIndex::new("by_group", ClassIndexKind::Standard, IndexScope::Base, [
		"group",
	])?)?
	.with_index(ClassIndex::new("unique_email", ClassIndexKind::Unique, IndexScope::Base, [
		"email",
	])?)?
	.with_index(ClassIndex::new(
		"person_count",
		ClassIndexKind::Count,
		IndexScope::Base,
		std::iter::empty::<&str>(),
	)?)?;
	schema.define_class(class)?;
	schema.commit()?;
	let dictionary = store.property_dictionary();
	Ok((
		dictionary.get("group").ok_or("profile group key is absent")?,
		dictionary.get("email").ok_or("profile email key is absent")?,
	))
}

fn spawn_rss_sampler(running: Arc<AtomicBool>, initial: u64) -> thread::JoinHandle<u64> {
	thread::spawn(move || {
		let pid = Pid::from(usize::try_from(std::process::id()).unwrap());
		let mut system = System::new();
		let mut peak = initial;
		while running.load(Ordering::Acquire) {
			system.refresh_processes_specifics(
				ProcessesToUpdate::Some(&[pid]),
				true,
				ProcessRefreshKind::nothing().with_memory(),
			);
			if let Some(process) = system.process(pid) {
				peak = peak.max(process.memory());
			}
			thread::sleep(Duration::from_millis(1));
		}
		peak
	})
}

fn process_rss() -> u64 {
	let pid = Pid::from(usize::try_from(std::process::id()).unwrap());
	let mut system = System::new();
	system.refresh_processes_specifics(
		ProcessesToUpdate::Some(&[pid]),
		true,
		ProcessRefreshKind::nothing().with_memory(),
	);
	system.process(pid).map_or(0, sysinfo::Process::memory)
}

fn environment_usize(name: &str, default: usize) -> Result<usize, Box<dyn Error>> {
	match std::env::var(name) {
		Ok(value) => {
			value.parse::<usize>().map_err(|error| format!("invalid {name}: {error}").into())
		}
		Err(std::env::VarError::NotPresent) => Ok(default),
		Err(error) => Err(format!("invalid {name}: {error}").into()),
	}
}

fn write_csv(row: &str) -> Result<(), Box<dyn Error>> {
	let path = std::env::var_os("FG_IMPORT_PROFILE_OUTPUT")
		.map(PathBuf::from)
		.unwrap_or_else(|| PathBuf::from("target/import-stage-10/import_results.csv"));
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let header = !path.exists() || path.metadata()?.len() == 0;
	let mut file = OpenOptions::new().create(true).append(true).open(path)?;
	if header {
		writeln!(
			file,
			concat!(
				"timestamp_ms,backend,index_mode,items,segment_size,elapsed_ns,items_per_second,",
				"rss_before_bytes,rss_peak_bytes,rss_delta_bytes,database_bytes,wal_bytes,\
				 processed,",
				"applied,segments,built_indexes,index_timing_ns"
			)
		)?;
	}
	writeln!(file, "{row}")?;
	Ok(())
}

fn directory_bytes(root: &Path) -> u64 {
	let mut total = 0_u64;
	let mut directories = vec![root.to_path_buf()];
	while let Some(directory) = directories.pop() {
		let Ok(entries) = fs::read_dir(directory) else {
			continue;
		};
		for entry in entries.flatten() {
			let path = entry.path();
			if path.is_dir() {
				directories.push(path);
			} else if let Ok(metadata) = entry.metadata() {
				total = total.saturating_add(metadata.len());
			}
		}
	}
	total
}

fn profile_error(message: &str) -> ferros_grynn::gql_status::FerrosGrynnError {
	ferros_grynn::gql_status::FerrosGrynnError::invalid_argument(message)
}
