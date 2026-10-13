//! Public registry scale probe; setup is excluded from measured operations.

use std::{
	hint::black_box,
	time::{Duration, Instant},
};

use ferros_grynn::{
	bridge::{
		BridgeDatabaseOptions,
		BridgeDatabaseRef,
		BridgeEntityRef,
		BridgeMappingRef,
		BridgeMappingRole,
		BridgeMappingState,
		BridgeMappingSubject,
		BridgeMirrorRequest,
	},
	datastore::{ClassSchema, Datastore, ImportOptions, InMemoryOptions},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, EdgeTopology, GraphId, GraphKind},
	values::{Object, PropertyMap, RecordId, Value},
};

/// Immutable fixture metadata reused by indexed hit/miss and aborted reserves.
struct Fixture {
	/// Public datastore with exactly the requested authoritative mapping count.
	store: Datastore,
	/// External database uniqueness domain.
	database: BridgeDatabaseRef,
	/// Graph containing spare local relation occurrences.
	graph: GraphId,
	/// Deterministic indexed subjects, entities and expected references.
	hits: Vec<(BridgeMappingSubject, BridgeEntityRef, BridgeMappingRef)>,
	/// Unmapped edge occurrences reused only in aborted transactions.
	spares: Vec<RecordId>,
}

/// Seeds atoms through production import, then bounded ordinary mapping
/// commits.
fn fixture(count: usize) -> FerrosGrynnResult<Fixture> {
	let mut limits = InMemoryOptions::default();
	limits.max_write_set_bytes = 2 * 1024 * 1024 * 1024;
	limits.max_snapshot_age = Duration::from_secs(1200);
	let store = Datastore::in_memory_with(limits);
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let graph = tx.define_graph("g", GraphKind::Graph)?.id();
	let database = tx.register_bridge_database(
		BridgeDatabaseOptions::new("sdb", "fake", "ns", "db")?
			.map_class("person", "profile")?
			.map_graph(graph, "relation")?,
	)?;
	tx.commit()?;
	let mut options = ImportOptions::default();
	options.segment_size = 2048;
	store.import(options, |session| {
		for ordinal in 0..count {
			session.apply_item(|tx| {
				tx.create_atom(
					RecordId::from_value("person", Value::from(ordinal))?,
					PropertyMap::new(),
				)
			})?;
		}
		Ok(())
	})?;
	let mut tx = store.begin();
	let a =
		tx.attach_node(graph, &RecordId::from_value("person", Value::from(0_usize))?, None, None)?;
	let b =
		tx.attach_node(graph, &RecordId::from_value("person", Value::from(1_usize))?, None, None)?;
	let mut spares = Vec::new();
	for ordinal in 0..64_usize {
		let edge = RecordId::from_value("link", Value::from(ordinal))?;
		tx.create_atom(edge.clone(), PropertyMap::new())?;
		tx.attach_edge(graph, &edge, None, EdgeTopology::directed([a], [b]), None, None)?;
		spares.push(edge);
	}
	tx.commit()?;
	let indices = (0..64).map(|i| (i * 104729) % count).collect::<Vec<_>>();
	let mut hits = Vec::new();
	for start in (0..count).step_by(128) {
		let mut tx = store.begin();
		for ordinal in start..count.min(start + 128) {
			let subject =
				BridgeMappingSubject::Atom(RecordId::from_value("person", Value::from(ordinal))?);
			let external = BridgeEntityRef {
				database,
				record: RecordId::from_value("profile", Value::from(ordinal))?,
			};
			let reference = tx.register_bridge_mapping(subject.clone(), external.clone())?;
			if indices.contains(&ordinal) {
				hits.push((subject, external, reference));
			}
		}
		tx.commit()?;
		if start % 100_096 == 0 {
			eprintln!("registry setup {count}: {} committed", count.min(start + 128));
		}
	}
	assert_eq!(hits.len(), 64);
	Ok(Fixture {
		store,
		database,
		graph,
		hits,
		spares,
	})
}

/// Reports the mean, uncertainty and p97.5 of independent fixed-size batches.
fn measure(
	count: usize,
	name: &str,
	mut operation: impl FnMut(usize) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	for i in 0..64 {
		operation(i)?;
	}
	let mut samples = Vec::new();
	for round in 0..40 {
		let start = Instant::now();
		for i in 0..64 {
			operation((i + round) % 64)?;
		}
		samples.push(start.elapsed().as_nanos() as f64 / 64.0);
	}
	let mean = samples.iter().sum::<f64>() / samples.len() as f64;
	let deviation = (samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>()
		/ (samples.len() - 1) as f64)
		.sqrt();
	samples.sort_by(f64::total_cmp);
	println!(
		"{count},{name},{mean:.2},{:.2},{:.2}",
		1.96 * deviation / (samples.len() as f64).sqrt(),
		samples[38]
	);
	Ok(())
}

/// Runs indexed correctness assertions on the same paths being measured.
fn probe(count: usize) -> FerrosGrynnResult<()> {
	let start = Instant::now();
	let fixture = fixture(count)?;
	eprintln!("registry {count} setup_seconds={:.3}", start.elapsed().as_secs_f64());
	let missing = RecordId::from_value("profile", Value::from(count + 1))?;
	measure(count, "direct_hit", |i| {
		let (subject, _, reference) = &fixture.hits[i];
		assert_eq!(
			black_box(fixture.store.begin().bridge_mapping_local(fixture.database, subject)?)
				.map(|row| row.reference()),
			Some(*reference)
		);
		Ok(())
	})?;
	measure(count, "reverse_hit", |i| {
		let (_, external, reference) = &fixture.hits[i];
		assert_eq!(
			black_box(
				fixture.store.begin().bridge_mapping_external(external, BridgeMappingRole::Atom)?
			)
			.map(|row| row.reference()),
			Some(*reference)
		);
		Ok(())
	})?;
	measure(count, "direct_miss", |i| {
		assert!(
			black_box(fixture.store.begin().bridge_mapping_local(
				fixture.database,
				&BridgeMappingSubject::Relation {
					graph: fixture.graph,
					edge: fixture.spares[i].clone()
				}
			)?)
			.is_none()
		);
		Ok(())
	})?;
	measure(count, "reverse_miss", |_| {
		assert!(
			black_box(fixture.store.begin().bridge_mapping_external(
				&BridgeEntityRef {
					database: fixture.database,
					record: missing.clone()
				},
				BridgeMappingRole::Atom
			)?)
			.is_none()
		);
		Ok(())
	})?;
	let mut reservation_bytes = 0;
	measure(count, "reservation_abort", |i| {
		let mut tx = fixture.store.begin();
		let subject = BridgeMappingSubject::Relation {
			graph: fixture.graph,
			edge: fixture.spares[i].clone(),
		};
		let entity = BridgeEntityRef {
			database: fixture.database,
			record: RecordId::from_value("relation", Value::from(i))?,
		};
		let reference =
			tx.reserve_bridge_mapping(subject, entity.clone(), BridgeMirrorRequest {
				relation: entity.record,
				source: "profile:0".parse()?,
				target: "profile:1".parse()?,
				properties: Object::new(),
			})?;
		assert_eq!(
			tx.bridge_mapping(reference)?.map(|row| row.state()),
			Some(BridgeMappingState::Pending)
		);
		reservation_bytes = reservation_bytes.max(black_box(tx.write_set_bytes()));
		drop(tx);
		assert!(fixture.store.begin().bridge_mapping(reference)?.is_none());
		Ok(())
	})?;
	eprintln!("registry {count} max_reservation_write_bytes={reservation_bytes}");
	Ok(())
}

/// Explicit scale arguments permit reproducible separate process runs.
fn main() -> FerrosGrynnResult<()> {
	println!("mappings,operation,mean_ns,ci95_half_width_ns,p97_5_batch_ns");
	let arguments = std::env::args().skip(1).collect::<Vec<_>>();
	if arguments.is_empty() {
		for count in [10_000, 100_000, 1_000_000] {
			probe(count)?;
		}
	} else {
		for argument in arguments {
			let count = argument.parse::<usize>().map_err(|error| {
				FerrosGrynnError::invalid_argument("registry scale must be an integer")
					.with_technical_source(error)
			})?;
			if count < 64 {
				return Err(FerrosGrynnError::invalid_argument(
					"registry scale must be at least 64",
				));
			}
			probe(count)?;
		}
	}
	Ok(())
}
