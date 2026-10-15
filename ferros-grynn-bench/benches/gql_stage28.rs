//! Maintained Stage-28 LIVE matrix with whole-commit result oracles.
//! FG28_ROWS=10000/100000/1000000; FG28_CASE selects one fresh process fixture.
//! FG28_LOCAL=1 selects Sync; FG28_SMOKE=1 uses64 rows for harness checks only.
//! FG28_BUDGET=large-work uses explicit100M query work with unchanged256MiB
//! scratch and queue defaults; its Criterion group is separate from defaults.
//! Criterion latency excludes setup; external process RSS includes
//! setup/import.

use std::{
	hint::black_box,
	path::PathBuf,
	time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion, Throughput};
use ferros_grynn::{
	changefeed::{ChangefeedConfig, ChangefeedSource, Versionstamp},
	datastore::{
		CheckpointMode,
		ClassField,
		ClassId,
		ClassSchema,
		Datastore,
		ImportChangefeedMode,
		ImportOptions,
		InMemoryOptions,
		LocalOptions,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult, Status},
	graph::AtomKind,
	live::{
		LiveBinaryOperator,
		LiveBindingId,
		LiveBindingOptions,
		LiveDelivery,
		LiveExpr,
		LiveField,
		LiveIdentity,
		LiveNext,
		LiveOptions,
		LivePath,
		LivePayload,
		LiveProjection,
		LiveQuery,
		LiveSource,
		LiveSubscription,
	},
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Type, Uuid, Value},
};

/// Fails a benchmark instead of turning errors or partial results into samples.
fn require<T>(result: FerrosGrynnResult<T>) -> T {
	match result {
		Ok(value) => value,
		Err(error) => {
			eprintln!("Stage-28 benchmark failed: {error}");
			std::process::exit(1);
		}
	}
}

/// Admits only explicit resource statuses, never semantic or I/O failures.
fn resource(error: &FerrosGrynnError) -> bool {
	let mut current = Some(error);
	while let Some(error) = current {
		if matches!(
			error.status().status(),
			Status::StorageDecodeLimitExceeded { .. }
				| Status::TransactionWriteSetLimitExceeded { .. }
				| Status::TraversalResourceLimitExceeded { .. }
		) {
			return true;
		}
		current = error.cause();
	}
	false
}

/// Sums actual files under a dedicated owned WAL/history subtree.
fn file_bytes(directory: &std::path::Path) -> FerrosGrynnResult<u64> {
	if !directory.try_exists().map_err(io)? {
		return Ok(0);
	}
	let mut directories = vec![directory.to_owned()];
	let mut bytes = 0_u64;
	let mut entries = 0_u64;
	while let Some(directory) = directories.pop() {
		for entry in std::fs::read_dir(directory).map_err(io)? {
			let entry = entry.map_err(io)?;
			entries += 1;
			if entries > 100000 {
				return Err(FerrosGrynnError::invalid_argument("benchmark file count exceeds cap"));
			}
			let kind = entry.file_type().map_err(io)?;
			if kind.is_dir() {
				directories.push(entry.path());
			} else if kind.is_file() {
				bytes = bytes
					.checked_add(entry.metadata().map_err(io)?.len())
					.ok_or_else(|| FerrosGrynnError::operation_overflow("benchmark file bytes"))?;
			}
		}
	}
	Ok(bytes)
}

/// Verifies exact identity/images and the actual issued whole-commit position.
fn single(
	delivery: &LiveDelivery,
	record: &RecordId,
	before: i64,
	after: i64,
	stamp: Versionstamp,
) -> FerrosGrynnResult<()> {
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("benchmark expected exact batch"));
	};
	if batch.versionstamp() != stamp
		|| delivery.token().versionstamp() != stamp
		|| batch.id() != delivery.token().live_id()
		|| batch.version() != delivery.token().version()
		|| batch.events().len() != 1
	{
		return Err(FerrosGrynnError::invalid_argument("benchmark commit envelope differs"));
	}
	let event = &batch.events()[0];
	if event.ordinal() != 0
		|| !matches!(event.identity(),LiveIdentity::Record(id) if id==record)
		|| event.before() != Some(&before.into())
		|| event.after() != Some(&after.into())
	{
		return Err(FerrosGrynnError::invalid_argument("benchmark exact row oracle differs"));
	}
	Ok(())
}

/// Converts fixture I/O failures into existing typed diagnostics.
fn io(error: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument("benchmark fixture I/O failed").with_technical_source(error)
}

/// A single fresh fixture owns registrations and its optional disposable
/// database.
struct Fixture {
	/// Datastore retained by the fixture or operation.
	store: Option<Datastore>,
	/// Directory retaining the local files used by the scenario.
	directory: Option<PathBuf>,
	/// Directory retaining the local files used by the scenario.
	root: Option<PathBuf>,
	/// Configured row count of the fixture or measurement.
	rows: usize,
	/// Class selected by the schema declaration, query target or fixture.
	class: ClassId,
	/// Stable class-qualified record identity used by this operation.
	hot: Vec<RecordId>,
	/// Stable class-qualified record identity used by this operation.
	boss: RecordId,
	/// Interned property-name identifier used for value lookup.
	value_key: PropertyKeyId,
	/// Interned property-name identifier used for value lookup.
	hot_key: PropertyKeyId,
	/// Interned property-name identifier used for value lookup.
	manager_key: PropertyKeyId,
	/// Payload retained for the operation or its expected result.
	value: i64,
	/// Value preceding the mutation whose change image is being checked.
	previous: i64,
	/// Version stamp associated with the committed fixture state.
	stamp: Versionstamp,
	/// Optional value captured before applying the fixture mutation.
	before_image: Option<Value>,
	/// Resolved binding connecting the fixture or reader to its registered
	/// resource.
	binding: Option<LiveBindingId>,
	/// Live subscription handles retained for the scenario lifetime.
	handles: Vec<LiveSubscription>,
}

impl Drop for Fixture {
	/// Runs cleanup for this `Fixture` before its fields are released.
	fn drop(&mut self) {
		self.handles.clear();
		drop(self.store.take());
		if let (Some(directory), Some(root)) = (self.directory.take(), self.root.take()) {
			// Delete only the unique directory created beneath this fixture's
			// canonical root.
			if let Ok(directory) = directory.canonicalize()
				&& directory.starts_with(&root)
				&& directory != root
			{
				drop(std::fs::remove_dir_all(directory));
			}
		}
	}
}

impl Fixture {
	/// Borrows the public datastore without retaining an unrelated snapshot.
	fn store(&self) -> FerrosGrynnResult<&Datastore> {
		self.store
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("benchmark store released"))
	}

	/// Streams background records with no captures before any LIVE
	/// registrations.
	fn populate(&self) -> FerrosGrynnResult<()> {
		let mut options = ImportOptions::default();
		options.segment_size = 2048;
		self.store()?.import(options, |session| {
			for row in 0..self.rows {
				let record: RecordId = format!("item:r{row}").parse()?;
				session.apply_item(|tx| {
					let mut properties = PropertyMap::from_iter([
						(self.value_key, 0.into()),
						(self.hot_key, (row < 64).into()),
					]);
					if row < 64 {
						properties.insert(self.manager_key, Value::from(self.boss.clone()));
					}
					tx.create_atom(record, properties)
				})?;
			}
			Ok(())
		})?;
		Ok(())
	}

	/// Builds a bounded-result query while retaining the actual class-rescan
	/// cost.
	fn query(&self, case: &str) -> FerrosGrynnResult<LiveQuery> {
		let query =
			LiveQuery::new(LiveSource::Class(self.class)).with_filter(LiveExpr::field("hot")?);
		Ok(if case == "fetch" {
			query
				.with_projection(LiveProjection::Fields(vec![
					LiveField::new(LiveExpr::field("value")?).with_alias(LivePath::field("val")?),
					LiveField::new(LiveExpr::field("manager")?),
				]))
				.with_fetch(vec![LivePath::field("manager")?])
		} else {
			query.with_projection(LiveProjection::Value(LiveExpr::field("value")?))
		})
	}

	/// Alternates source data or one reactive literal; commits remain
	/// indivisible.
	fn mutate(&mut self, case: &str) -> FerrosGrynnResult<()> {
		self.previous = self.value;
		self.value = 1 - self.value;
		let mut tx = self.store()?.begin();
		if case == "fetch" {
			tx.set_base_property(&self.boss, self.value_key, self.value.into())?;
		} else if case == "let_param" {
			tx.replace_live_binding_value(
				self.binding.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("benchmark binding absent")
				})?,
				self.value.into(),
			)?;
		} else {
			for record in &self.hot {
				tx.set_base_property(record, self.value_key, self.value.into())?;
			}
		}
		self.stamp = black_box(tx.commit()?).versionstamp();
		Ok(())
	}

	/// Uses ordinary SELECT for FETCH; scalar scores are independent arithmetic
	/// oracles.
	fn oracle(&self, case: &str, value: i64) -> FerrosGrynnResult<Value> {
		Ok(if case == "fetch" {
			let mut rows = self.store()?.execute(
				"SELECT properties.value AS val, properties.manager AS manager FROM item:r0 FETCH \
				 manager;",
				&Object::new(),
			)?;
			let result = rows
				.pop()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("FETCH oracle absent"))?
				.result?
				.into_parts()
				.0;
			result
				.as_array()
				.and_then(|rows| rows.get(0))
				.cloned()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("FETCH oracle row absent"))?
		} else {
			Value::from(match case {
				"let_data" => 2 * value + 1,
				"let_param" => value + 1,
				_ => value,
			})
		})
	}

	/// Prepares the previous image outside every commit timer.
	fn prepare(&mut self, case: &str) -> FerrosGrynnResult<()> {
		self.before_image = Some(self.oracle(case, self.value)?);
		Ok(())
	}

	/// Classifies a default-limit fault before any successful timing samples.
	fn evaluation_fault(&mut self, case: &str) -> FerrosGrynnResult<bool> {
		let mut faults = 0;
		for handle in &mut self.handles {
			if let LiveNext::Delivery(delivery) = handle.next()?
				&& let LivePayload::Fault {
					diagnostic,
					..
				} = delivery.payload()
			{
				if !resource(diagnostic) {
					return Err(diagnostic.clone());
				}
				faults += 1;
			}
		}
		if faults > 0 {
			let mut tx = self.store()?.begin();
			if case == "let_param" {
				let id = self.binding.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("benchmark binding absent")
				})?;
				if tx.live_binding_value(id)? != self.value.into() {
					return Err(FerrosGrynnError::invalid_argument(
						"fault blocked binding publication",
					));
				}
			} else {
				let records = if case == "fetch" {
					std::slice::from_ref(&self.boss)
				} else {
					self.hot.as_slice()
				};
				for record in records {
					if tx
						.atom(record)?
						.and_then(|atom| atom.properties().get(self.value_key).cloned())
						!= Some(self.value.into())
					{
						return Err(FerrosGrynnError::invalid_argument(
							"fault blocked primary publication",
						));
					}
				}
			}
			println!(
				concat!(
					"stage28_probe\tcase={}\trows={}\t",
					"status=evaluation_resource_fault\tfaults={}\tstamp={}"
				),
				case,
				self.rows,
				faults,
				self.stamp.get()
			);
		}
		Ok(faults > 0)
	}

	/// Checks exact before/after, identity order and commit stamp before ACK.
	fn validate(&mut self, case: &str) -> FerrosGrynnResult<()> {
		let expected = self.oracle(case, self.value)?;
		let before = self.before_image.as_ref().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("benchmark previous oracle absent")
		})?;
		for handle in &mut self.handles {
			let LiveNext::Delivery(delivery) = handle.next()? else {
				return Err(FerrosGrynnError::invalid_argument("benchmark delivery absent"));
			};
			let LivePayload::Batch(batch) = delivery.payload() else {
				return Err(FerrosGrynnError::invalid_argument(
					"benchmark GAP/fault is not a successful sample",
				));
			};
			if batch.versionstamp() != self.stamp
				|| delivery.token().versionstamp() != self.stamp
				|| batch.id() != delivery.token().live_id()
				|| batch.version() != delivery.token().version()
				|| batch.events().len() != 64
				|| batch.events().windows(2).any(|pair| pair[0].identity() >= pair[1].identity())
			{
				return Err(FerrosGrynnError::invalid_argument(
					"benchmark whole-commit identities differ",
				));
			}
			for event in batch.events() {
				let LiveIdentity::Record(record) = event.identity() else {
					return Err(FerrosGrynnError::invalid_argument(
						"benchmark row identity differs",
					));
				};
				if !self.hot.contains(record)
					|| event.after() != Some(&expected)
					|| event.before() != Some(before)
				{
					return Err(FerrosGrynnError::invalid_argument(
						"benchmark before/after oracle differs",
					));
				}
			}
			handle.ack(delivery.token())?;
			handle.ack(delivery.token())?;
		}
		Ok(())
	}

	/// Logs non-authoritative counters outside the measured work.
	fn statistics(&self, case: &str, phase: &str) -> FerrosGrynnResult<()> {
		let live = self.store()?.live_statistics()?;
		let source = self.store()?.changefeed_statistics()?;
		let (wal, feed) = if let Some(directory) = &self.directory {
			(
				Some(file_bytes(&directory.join("wal"))?),
				Some(file_bytes(&directory.join("changefeed"))?),
			)
		} else {
			(None, None)
		};
		println!(
			concat!(
				"stage28_metrics\tcase={}\tphase={}\trows={}\tlocal={}\t",
				"wal_bytes={:?}\tfeed_bytes={:?}\tlive={:?}\tsource={:?}"
			),
			case,
			phase,
			self.rows,
			self.directory.is_some(),
			wal,
			feed,
			live,
			source
		);
		Ok(())
	}
}

/// Creates all schema/primary inputs before enabling the source history.
fn fixture(rows: usize, local: bool) -> FerrosGrynnResult<Fixture> {
	let root = if local {
		let root =
			std::env::current_dir().map_err(io)?.join("target/stage28-baseline/l11-databases");
		std::fs::create_dir_all(&root).map_err(io)?;
		Some(root.canonicalize().map_err(io)?)
	} else {
		None
	};
	let directory = root.as_ref().map(|root| {
		root.join(format!("live28-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64))
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
	let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [
		ClassField::new("value", Type::Int),
		ClassField::new("hot", Type::Bool),
		ClassField::new("manager", Type::Option(Box::new(Type::Record))),
	])?)?;
	tx.define_class(ClassSchema::new("manager", AtomKind::Node, [ClassField::new(
		"value",
		Type::Int,
	)])?)?;
	tx.commit()?;
	let key = |name: &str| {
		store
			.property_dictionary()
			.get(name)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("benchmark property key absent"))
	};
	let value_key = key("value")?;
	let hot_key = key("hot")?;
	let manager_key = key("manager")?;
	let mut tx = store.begin();
	let boss: RecordId = "manager:boss".parse()?;
	tx.create_atom(boss.clone(), PropertyMap::from_iter([(value_key, 0.into())]))?;
	tx.commit()?;
	let hot = (0..64)
		.map(|row| format!("item:r{row}").parse())
		.collect::<FerrosGrynnResult<Vec<RecordId>>>()?;
	let fixture = Fixture {
		store: Some(store),
		directory,
		root,
		rows,
		class,
		hot,
		boss,
		value_key,
		hot_key,
		manager_key,
		value: 0,
		previous: 0,
		stamp: Versionstamp::new(0),
		before_image: None,
		binding: None,
		handles: Vec::new(),
	};
	fixture.populate()?;
	let mut tx = fixture.store()?.begin();
	tx.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	tx.commit()?;
	Ok(fixture)
}

/// Defines a two-level reactive DAG; data and literal updates share the same
/// query.
fn register(
	fixture: &mut Fixture,
	case: &str,
	subscribers: usize,
	options: &LiveOptions,
) -> FerrosGrynnResult<()> {
	let mut query = fixture.query(case)?;
	if case == "let_data" || case == "let_param" {
		let mut tx = fixture.store()?.begin();
		let first = tx.define_live_binding(
			LiveExpr::Select(Box::new(
				LiveQuery::new(LiveSource::Record(fixture.hot[0].clone()))
					.with_only(true)
					.with_projection(LiveProjection::Value(LiveExpr::field("value")?)),
			)),
			Object::new(),
			LiveBindingOptions::default(),
		)?;
		let second = tx.define_live_binding(
			LiveExpr::Binding(first).binary(LiveBinaryOperator::Add, LiveExpr::Literal(1.into())),
			Object::new(),
			LiveBindingOptions::default(),
		)?;
		tx.commit()?;
		query = query.with_projection(LiveProjection::Value(
			LiveExpr::field("value")?.binary(LiveBinaryOperator::Add, LiveExpr::Binding(second)),
		));
		fixture.binding = Some(first);
	}
	for _ in 0..subscribers {
		fixture.handles.push(fixture.store()?.subscribe_live(query.clone(), options.clone())?);
	}
	Ok(())
}

/// Reopens only engine caches; this does not claim cold OS/device caches.
fn reopen(fixture: &mut Fixture) -> FerrosGrynnResult<()> {
	fixture.handles.clear();
	fixture.store()?.checkpoint(CheckpointMode::Restart)?;
	drop(fixture.store.take());
	let directory = fixture
		.directory
		.as_ref()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("cold reopen requires local fixture"))?;
	let mut options = LocalOptions::default();
	options.max_snapshot_age = Duration::from_secs(3600);
	fixture.store = Some(Datastore::open_local(directory, options)?);
	Ok(())
}

/// Historical pull reads saved before/after despite a newer primary value.
fn replay(fixture: &mut Fixture, criterion: &mut Criterion) -> FerrosGrynnResult<()> {
	let handle = fixture.store()?.subscribe_live(
		LiveQuery::new(LiveSource::Record(fixture.hot[0].clone()))
			.with_projection(LiveProjection::Value(LiveExpr::field("value")?)),
		LiveOptions::default(),
	)?;
	let cursor = handle.cursor().clone();
	drop(handle);
	let mut first_stamp = Versionstamp::new(0);
	for value in [1_i64, 2] {
		let mut tx = fixture.store()?.begin();
		tx.set_base_property(&fixture.hot[0], fixture.value_key, value.into())?;
		let stamp = tx.commit()?.versionstamp();
		if value == 1 {
			first_stamp = stamp;
		}
	}
	if fixture.directory.is_some() {
		let started = Instant::now();
		reopen(fixture)?;
		println!(
			"stage28_probe\tcase=replay\tstate=reopen\trows={}\telapsed_ns={}",
			fixture.rows,
			started.elapsed().as_nanos()
		);
	}
	let pull = |fixture: &Fixture| -> FerrosGrynnResult<()> {
		let mut handle = fixture.store()?.resume_live(&cursor)?;
		let LiveNext::Delivery(delivery) = handle.next()? else {
			return Err(FerrosGrynnError::invalid_argument("replay delivery absent"));
		};
		single(&delivery, &fixture.hot[0], 0, 1, first_stamp)?;
		Ok(())
	};
	let started = Instant::now();
	pull(fixture)?;
	println!(
		"stage28_probe\tcase=replay\tstate=first_engine_pull\trows={}\tlocal={}\telapsed_ns={}",
		fixture.rows,
		fixture.directory.is_some(),
		started.elapsed().as_nanos()
	);
	fixture.statistics("replay", "before")?;
	let mut group = criterion.benchmark_group("gql_stage28");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(500));
	group.measurement_time(Duration::from_secs(2));
	group.bench_function(BenchmarkId::new("replay_attach_next", fixture.rows), |bench| {
		bench.iter(|| require(pull(black_box(fixture))))
	});
	group.finish();
	fixture.statistics("replay", "after")
}

/// Times only ACK; mutation and first issue are outside each accumulated
/// duration.
fn ack(fixture: &mut Fixture, criterion: &mut Criterion) -> FerrosGrynnResult<()> {
	let mut handle = fixture.store()?.subscribe_live(
		LiveQuery::new(LiveSource::Record(fixture.hot[0].clone()))
			.with_projection(LiveProjection::Value(LiveExpr::field("value")?)),
		LiveOptions::default(),
	)?;
	fixture.statistics("ack", "before")?;
	let mut group = criterion.benchmark_group("gql_stage28");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(500));
	group.measurement_time(Duration::from_secs(2));
	group.bench_function(BenchmarkId::new("whole_commit_ack", fixture.rows), |bench| {
		bench.iter_custom(|iterations| {
			let mut elapsed = Duration::ZERO;
			for _ in 0..iterations {
				let previous = fixture.value;
				fixture.value = 1 - fixture.value;
				let mut tx = require(fixture.store()).begin();
				require(tx.set_base_property(
					&fixture.hot[0],
					fixture.value_key,
					fixture.value.into(),
				));
				let stamp = require(tx.commit()).versionstamp();
				let delivery = match require(handle.next()) {
					LiveNext::Delivery(delivery) => delivery,
					_ => require(Err(FerrosGrynnError::invalid_argument("ACK delivery absent"))),
				};
				require(single(&delivery, &fixture.hot[0], previous, fixture.value, stamp));
				let started = Instant::now();
				require(handle.ack(delivery.token()));
				elapsed += started.elapsed();
				require(handle.ack(delivery.token()));
			}
			elapsed
		})
	});
	group.finish();
	fixture.statistics("ack", "after")
}

/// Probes atomic import under ON/OFF; resource rejection must preserve primary.
fn import_probe(fixture: &mut Fixture, on: bool) -> FerrosGrynnResult<()> {
	let mut handle = fixture.store()?.subscribe_live(
		LiveQuery::new(LiveSource::Record(fixture.hot[0].clone()))
			.with_projection(LiveProjection::Value(LiveExpr::field("value")?)),
		LiveOptions::default(),
	)?;
	let before = handle.cursor().clone();
	let mut options = ImportOptions::default();
	options.segment_size = 2048;
	options.changefeed = if on {
		ImportChangefeedMode::On
	} else {
		ImportChangefeedMode::Off
	};
	let started = Instant::now();
	let result = fixture.store()?.import(options, |session| {
		for row in 0..fixture.rows {
			let record: RecordId = format!("item:r{row}").parse()?;
			session.apply_item(|tx| tx.set_base_property(&record, fixture.value_key, 1.into()))?;
		}
		Ok(())
	});
	let elapsed = started.elapsed().as_nanos();
	match result {
		Err(error) if resource(&error) => {
			let mut tx = fixture.store()?.begin();
			for row in 0..fixture.rows {
				let record: RecordId = format!("item:r{row}").parse()?;
				if tx
					.atom(&record)?
					.and_then(|atom| atom.properties().get(fixture.value_key).cloned())
					!= Some(0.into())
				{
					return Err(FerrosGrynnError::invalid_argument(
						"failed import published partial primary",
					));
				}
			}
			if tx.commit()?.versionstamp() != before.versionstamp() {
				return Err(FerrosGrynnError::invalid_argument(
					"failed import advanced commit clock",
				));
			}
			println!(
				concat!(
					"stage28_probe\tcase=import\trows={}\ton={}\t",
					"status=resource_rejected\telapsed_ns={}\terror={}"
				),
				fixture.rows, on, elapsed, error
			);
		}
		Err(error) => return Err(error),
		Ok(_) => {
			let mut tx = fixture.store()?.begin();
			for row in 0..fixture.rows {
				let record: RecordId = format!("item:r{row}").parse()?;
				if tx
					.atom(&record)?
					.and_then(|atom| atom.properties().get(fixture.value_key).cloned())
					!= Some(1.into())
				{
					return Err(FerrosGrynnError::invalid_argument(
						"successful import final primary differs",
					));
				}
			}
			let stamp = tx.commit()?.versionstamp();
			let LiveNext::Delivery(delivery) = handle.next()? else {
				return Err(FerrosGrynnError::invalid_argument("import live net delivery absent"));
			};
			single(&delivery, &fixture.hot[0], 0, 1, stamp)?;
			drop(delivery);
			drop(handle);
			// Saved result coverage follows import ON/OFF independently of the
			// active volatile batch.
			let mut history = fixture.store()?.resume_live(&before)?;
			let LiveNext::Delivery(saved) = history.next()? else {
				return Err(FerrosGrynnError::invalid_argument(
					"import historical delivery absent",
				));
			};
			if on {
				single(&saved, &fixture.hot[0], 0, 1, stamp)?;
			} else if !matches!(saved.payload(),LivePayload::Gap(gap) if gap.reason()==ferros_grynn::live::LiveGapReason::ImportSuppressed)
			{
				return Err(FerrosGrynnError::invalid_argument(
					"OFF import suppression GAP missing",
				));
			}
			history.ack(saved.token())?;
			println!(
				"stage28_probe\tcase=import\trows={}\ton={on}\tstatus=ok\telapsed_ns={elapsed}",
				fixture.rows
			);
		}
	}
	fixture.statistics(
		if on {
			"import_on"
		} else {
			"import_off"
		},
		"after",
	)
}

/// Tiny registration and oversized whole-delivery budgets reject explicitly.
fn resource_probe(fixture: &mut Fixture) -> FerrosGrynnResult<()> {
	let point = LiveQuery::new(LiveSource::Record(fixture.hot[0].clone()));
	let options = LiveOptions::default().with_budgets(1, 1)?;
	match fixture.store()?.subscribe_live(point.clone(), options) {
		Err(error) if resource(&error) => {}
		Err(error) => return Err(error),
		Ok(_) => {
			return Err(FerrosGrynnError::invalid_argument("tiny registration budget admitted"));
		}
	}
	let mut handle =
		fixture.store()?.subscribe_live(point, LiveOptions::default().with_queue(1, 1)?)?;
	let mut tx = fixture.store()?.begin();
	tx.set_base_property(&fixture.hot[0], fixture.value_key, 1.into())?;
	tx.commit()?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("oversized control absent"));
	};
	if !matches!(delivery.payload(),LivePayload::Fault {diagnostic,..} if resource(diagnostic)) {
		return Err(FerrosGrynnError::invalid_argument("oversized commit was silently admitted"));
	}
	if fixture
		.store()?
		.begin()
		.atom(&fixture.hot[0])?
		.and_then(|atom| atom.properties().get(fixture.value_key).cloned())
		!= Some(1.into())
	{
		return Err(FerrosGrynnError::invalid_argument("fault blocked primary publication"));
	}
	println!("stage28_probe\tcase=resource\trows={}\tstatus=expected_rejection", fixture.rows);
	fixture.statistics("resource", "after")
}

/// One controlled case per process permits honest peak-RSS and cold/warm
/// labels.
fn run() -> FerrosGrynnResult<()> {
	let smoke = std::env::var("FG28_SMOKE").is_ok_and(|value| value == "1");
	let rows = if smoke {
		64
	} else {
		std::env::var("FG28_ROWS").unwrap_or_else(|_| "10000".into()).parse::<usize>().map_err(
			|error| {
				FerrosGrynnError::invalid_argument("FG28_ROWS is invalid")
					.with_technical_source(error)
			},
		)?
	};
	if !smoke && ![10000, 100000, 1000000].contains(&rows) {
		return Err(FerrosGrynnError::invalid_argument("FG28_ROWS must be10K/100K/1M"));
	}
	let case = std::env::var("FG28_CASE").unwrap_or_else(|_| "scan_1".into());
	let local = std::env::var("FG28_LOCAL").is_ok_and(|value| value == "1");
	let large_work = match std::env::var("FG28_BUDGET") {
		Ok(value) if value == "large-work" => true,
		Err(std::env::VarError::NotPresent) => false,
		_ => {
			return Err(FerrosGrynnError::invalid_argument(
				"FG28_BUDGET must be large-work or absent",
			));
		}
	};
	let options = if large_work {
		LiveOptions::default().with_budgets(256 * 1024 * 1024, 100_000_000)?
	} else {
		LiveOptions::default()
	};
	if large_work
		&& !matches!(
			case.as_str(),
			"scan_1" | "scan_10" | "scan_100" | "fetch" | "let_data" | "let_param"
		) {
		return Err(FerrosGrynnError::invalid_argument(
			"large-work requires a class LIVE workload",
		));
	}
	let mut fixture = fixture(rows, local)?;
	let mut criterion = Criterion::default().configure_from_args();
	println!(
		concat!(
			"stage28_contract\tcase={}\trows={}\tlocal={}\tsmoke={}\t",
			"work={}\tscratch={}\t",
			"queue_events={}\tqueue_bytes={}"
		),
		case,
		rows,
		local,
		smoke,
		options.work(),
		options.scratch_bytes(),
		options.queue_entries(),
		options.queue_bytes()
	);
	match case.as_str() {
		"replay" => replay(&mut fixture, &mut criterion)?,
		"ack" => ack(&mut fixture, &mut criterion)?,
		"import_on" => import_probe(&mut fixture, true)?,
		"import_off" => import_probe(&mut fixture, false)?,
		"resource" => resource_probe(&mut fixture)?,
		"scan_1" | "scan_10" | "scan_100" | "fetch" | "let_data" | "let_param" => {
			let subscribers = match case.as_str() {
				"scan_10" => 10,
				"scan_100" => 100,
				_ => 1,
			};
			if let Err(error) = register(&mut fixture, &case, subscribers, &options) {
				if resource(&error) {
					println!(
						concat!(
							"stage28_probe\tcase={}\trows={}\tlocal={}\t",
							"status=registration_resource_rejected\terror={}"
						),
						case, rows, local, error
					);
					return Ok(());
				}
				return Err(error);
			}
			fixture.prepare(&case)?;
			fixture.mutate(&case)?;
			if fixture.evaluation_fault(&case)? {
				fixture.statistics(&case, "fault")?;
				return Ok(());
			}
			fixture.validate(&case)?;
			fixture.statistics(&case, "before")?;
			let mut group = criterion.benchmark_group(if large_work {
				"gql_stage28_large_work"
			} else {
				"gql_stage28"
			});
			group.sample_size(10);
			group.warm_up_time(Duration::from_millis(500));
			group.measurement_time(Duration::from_secs(2));
			group.throughput(Throughput::Elements((subscribers * 64) as u64));
			group.bench_function(BenchmarkId::new(&case, rows), |bench| {
				bench.iter_custom(|iterations| {
					let mut elapsed = Duration::ZERO;
					for _ in 0..iterations {
						require(fixture.prepare(&case));
						let started = Instant::now();
						require(fixture.mutate(&case));
						elapsed += started.elapsed();
						require(fixture.validate(&case));
					}
					elapsed
				})
			});
			group.finish();
			fixture.statistics(&case, "after")?;
		}
		_ => return Err(FerrosGrynnError::invalid_argument("FG28_CASE is unknown")),
	}
	criterion.final_summary();
	Ok(())
}

/// Keeps errors observable at the command boundary.
fn main() {
	require(run());
}
