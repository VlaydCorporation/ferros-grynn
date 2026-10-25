//! Owns the measured database, independent oracles and bounded file cleanup.

use super::*;

/// Owns the sole datastore handle and a directory created by this process.
pub(super) struct Fixture {
	/// Taken before reopen or explicit cleanup so all native handles close
	/// first.
	store: Option<Datastore>,
	/// Exact owned child and canonical parent; failures retain evidence on
	/// disk.
	directory: Option<(PathBuf, PathBuf)>,
	/// Stable dataset identities, including unchanged anchors.
	records: Vec<RecordId>,
	/// Existing property key used by normal SDK setters.
	key: PropertyKeyId,
	/// Main inherited/overlay scope and an independently overridden scope.
	graphs: Option<[GraphRef; 2]>,
}

impl Fixture {
	/// Imports the same dataset through the ordinary bounded importer.
	pub(super) fn create(config: &Config) -> FerrosGrynnResult<Self> {
		let (store, directory) = if let Some(mode) = config.io_mode {
			fs::create_dir_all(&config.root).map_err(io_error)?;
			let parent = config.root.canonicalize().map_err(io_error)?;
			// The directory-derived database name is limited to 31 UTF-8 bytes.
			let directory =
				parent.join(format!("q05-{:016x}", Uuid::now_v7().as_inner().as_u128() as u64,));
			let mut options = LocalOptions::default();
			options.io_mode = mode;
			(Datastore::create_local(&directory, options)?, Some((directory, parent)))
		} else {
			(Datastore::in_memory(), None)
		};
		verify_mode(&store, config.io_mode)?;
		let mut schema = store.begin();
		let graphs = if config.effective() {
			Some([
				schema.define_graph("qmain", GraphKind::Graph)?,
				schema.define_graph("qfixed", GraphKind::Graph)?,
			])
		} else {
			None
		};
		let mut class =
			ClassSchema::new("item", AtomKind::Node, [ClassField::new("value", Type::Int)])?;
		if let Some(kind) = config.kind.scalar() {
			let scope = graphs.as_ref().map_or(IndexScope::Base, |graphs| IndexScope::Effective {
				graph: graphs[0].id(),
			});
			class = class.with_index(ClassIndex::new("by_value", kind, scope, ["value"])?)?;
		}
		if let Some(graphs) = &graphs {
			class = class.with_index(ClassIndex::new(
				"by_fixed",
				ClassIndexKind::Standard,
				IndexScope::Effective {
					graph: graphs[1].id(),
				},
				["value"],
			)?)?;
		}
		schema.define_class(class)?;
		schema.commit()?;
		let key = store
			.property_dictionary()
			.get("value")
			.ok_or_else(|| invalid("benchmark field is absent from the committed dictionary"))?;
		let records =
			(0..ROWS).map(|row| config.record(row, 0)).collect::<FerrosGrynnResult<Vec<_>>>()?;
		let mut options = ImportOptions::default();
		options.segment_size = 1024;
		let imported = store.import(options, |session| {
			for (row, record) in records.iter().enumerate() {
				session.apply_item(|tx| {
					let mut properties = PropertyMap::new();
					properties.insert(key, Value::from(config.value(row, 0)));
					tx.create_atom(record.clone(), properties)?;
					if let Some(graphs) = &graphs {
						tx.attach_node(graphs[0].id(), record, None, None)?;
						tx.attach_node(graphs[1].id(), record, None, None)?;
						tx.set_overlay(
							graphs[1].id(),
							record,
							key,
							Value::from(config.value(row, 0)),
						)?;
					}
					Ok(())
				})?;
			}
			Ok(())
		})?;
		if imported.applied_items() != ROWS as u64 {
			return Err(invalid("import cardinality differs"));
		}
		let mut fixture = Self {
			store: Some(store),
			directory,
			records,
			key,
			graphs,
		};
		fixture.checkpoint()?;
		fixture.reopen(config)?;
		fixture.validate(config, 0)?;
		Ok(fixture)
	}

	/// Borrows the active datastore; a missing handle is a probe error.
	pub(super) fn store(&self) -> FerrosGrynnResult<&Datastore> {
		self.store.as_ref().ok_or_else(|| invalid("fixture store is closed"))
	}

	/// Separates checkpoint cost from the explicitly measured commit interval.
	pub(super) fn checkpoint(&self) -> FerrosGrynnResult<Duration> {
		let started = Instant::now();
		if self.directory.is_some() {
			self.store()?.checkpoint(CheckpointMode::Restart)?;
		}
		Ok(started.elapsed())
	}

	/// Closes all handles before measuring cold datastore reopen.
	pub(super) fn reopen(&mut self, config: &Config) -> FerrosGrynnResult<Duration> {
		let Some((directory, _parent)) = &self.directory else {
			return Ok(Duration::ZERO);
		};
		drop(self.store.take());
		let mut options = LocalOptions::default();
		options.io_mode = config.io_mode.ok_or_else(|| invalid("local fixture has no I/O mode"))?;
		let started = Instant::now();
		let store = Datastore::open_local(directory, options)?;
		let elapsed = started.elapsed();
		verify_mode(&store, config.io_mode)?;
		self.store = Some(store);
		Ok(elapsed)
	}

	/// Checks every final SDK value and compares indexed results to a scan.
	pub(super) fn validate(&self, config: &Config, round: usize) -> FerrosGrynnResult<()> {
		let mut tx = self.store()?.begin();
		for row in 0..ROWS {
			let record = config.record(row, round)?;
			let atom = tx.atom(&record)?.ok_or_else(|| invalid("oracle atom absent"))?;
			let base_round = if config.entry == "overlay" {
				0
			} else {
				round
			};
			if atom.properties().get(self.key) != Some(&Value::from(config.value(row, base_round)))
			{
				return Err(invalid("SDK value oracle differs"));
			}
			if let Some(graphs) = &self.graphs
				&& (tx.effective_property(graphs[0].id(), &record, self.key)?
					!= Some(Value::from(config.value(row, round)))
					|| tx.effective_property(graphs[1].id(), &record, self.key)?
						!= Some(Value::from(config.value(row, 0))))
			{
				return Err(invalid("Effective scope oracle differs"));
			}
		}
		drop(tx);
		if config.kind == IndexKind::Primary {
			let mut expected = (0..ROWS)
				.map(|row| config.record(row, round))
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			expected.sort();
			let expected =
				Value::Array(expected.into_iter().map(Value::from).collect::<Vec<_>>().into());
			if execute_value(
				self.store()?,
				"SELECT VALUE id FROM item WITH NOINDEX ORDER BY id;",
				&Object::new(),
			)? != expected
			{
				return Err(invalid("Primary full identity set differs"));
			}
			for ordinal in 0..config.batch {
				self.probe_primary(config, config.target(ordinal), round)?;
			}
			return Ok(());
		}
		// Include vacated keys and the temporary unique key, not just final
		// hits.
		let mut keys = BTreeSet::from([-10, config.value(ROWS - 1, round)]);
		for previous in 0..=round {
			for ordinal in 0..config.batch {
				keys.insert(config.value(config.target(ordinal), previous));
			}
		}
		for value in keys {
			self.probe_value(config, round, value)?;
		}
		Ok(())
	}

	/// Checks the indexed query against the on-demand oracle outside timing.
	/// Eager runtime bindings may use RAM; cold reopen separately forces
	/// decode.
	pub(super) fn probe(&self, config: &Config, round: usize) -> FerrosGrynnResult<()> {
		if config.kind == IndexKind::Primary {
			return self.probe_primary(config, config.target(0), round);
		}
		self.probe_value(config, round, config.value(config.target(0), round))
	}

	/// Checks the changed identity and its vacated alternate, outside commit
	/// timing.
	///
	/// # Errors
	/// Returns SDK failures or a typed identity/value oracle mismatch.
	fn probe_primary(&self, config: &Config, row: usize, round: usize) -> FerrosGrynnResult<()> {
		let mut tx = self.store()?.begin();
		let record = config.record(row, round)?;
		let atom = tx.atom(&record)?.ok_or_else(|| invalid("Primary target absent"))?;
		if atom.properties().get(self.key) != Some(&Value::from(config.value(row, round))) {
			return Err(invalid("Primary target value differs"));
		}
		let alternate = config.record(row, round + 1)?;
		if alternate != record && tx.atom(&alternate)?.is_some() {
			return Err(invalid("Primary vacated identity remains"));
		}
		Ok(())
	}

	/// Checks both query paths against the deterministic final binding rows.
	fn probe_value(&self, config: &Config, round: usize, value: i64) -> FerrosGrynnResult<()> {
		let parameters = Object::from_iter([("value", Value::from(value))]);
		let scope = if self.graphs.is_some() {
			"IN GRAPH @qmain"
		} else {
			""
		};
		let indexed = query(self.store()?, scope, "WITH INDEX by_value", &parameters)?;
		let scanned = query(self.store()?, scope, "WITH NOINDEX", &parameters)?;
		let expected = Value::Array(
			self.records
				.iter()
				.enumerate()
				.filter(|(row, _record)| config.value(*row, round) == value)
				.map(|(_row, record)| Value::from(record.clone()))
				.collect::<Vec<_>>()
				.into(),
		);
		if indexed != expected || scanned != expected {
			return Err(invalid("indexed query differs from scan oracle"));
		}
		if self.graphs.is_some() {
			let indexed =
				query(self.store()?, "IN GRAPH @qfixed", "WITH INDEX by_fixed", &parameters)?;
			let scanned = query(self.store()?, "IN GRAPH @qfixed", "WITH NOINDEX", &parameters)?;
			let expected = Value::Array(
				self.records
					.iter()
					.enumerate()
					.filter(|(row, _)| config.value(*row, 0) == value)
					.map(|(_, record)| Value::from(record.clone()))
					.collect::<Vec<_>>()
					.into(),
			);
			if indexed != expected || scanned != expected {
				return Err(invalid("independent overridden index differs"));
			}
		}
		Ok(())
	}

	/// Stages SDK mutations outside the commit timer, including unique-key
	/// cycles.
	pub(super) fn commit(
		&self,
		config: &Config,
		round: usize,
	) -> FerrosGrynnResult<(Duration, Duration)> {
		let staged = Instant::now();
		let mut tx = self.store()?.begin();
		if config.kind == IndexKind::Primary {
			for ordinal in 0..config.batch {
				let row = config.target(ordinal);
				tx.delete_atom(&config.record(row, round - 1)?)?;
				let mut properties = PropertyMap::new();
				properties.insert(self.key, Value::from(config.value(row, round)));
				tx.create_atom(config.record(row, round)?, properties)?;
			}
		} else if config.kind == IndexKind::Unique && !config.fallback && config.batch > 1 {
			// Vacate one key so ordinary setters preserve uniqueness at every
			// step.
			self.set_value(&mut tx, config, 1, -10)?;
			for ordinal in (1..config.batch).rev() {
				let row = config.target(ordinal);
				self.set_value(&mut tx, config, row, config.value(row, round))?;
			}
			self.set_value(&mut tx, config, 1, config.value(1, round))?;
		} else {
			for ordinal in 0..config.batch {
				let row = config.target(ordinal);
				self.set_value(&mut tx, config, row, config.value(row, round))?;
			}
		}
		let staging = staged.elapsed();
		let started = Instant::now();
		std::hint::black_box(tx.commit()?);
		Ok((started.elapsed(), staging))
	}

	/// The same staged values enter through either Base or the selected
	/// overlay.
	///
	/// # Errors
	/// Returns a missing scope diagnostic or the ordinary SDK write error.
	fn set_value(
		&self,
		tx: &mut Transaction,
		config: &Config,
		row: usize,
		value: i64,
	) -> FerrosGrynnResult<()> {
		if config.entry == "overlay" {
			let graph = self.graphs.as_ref().ok_or_else(|| invalid("overlay scope absent"))?;
			tx.set_overlay(graph[0].id(), &self.records[row], self.key, Value::from(value))
		} else {
			tx.set_base_property(&self.records[row], self.key, Value::from(value))
				.map(|_previous| ())
		}
	}

	/// Observes only the selected binding's files, without SDK storage DTOs.
	pub(super) fn index_paths(&self, config: &Config) -> FerrosGrynnResult<BTreeSet<PathBuf>> {
		let mut paths = BTreeSet::new();
		if let Some((directory, _parent)) = &self.directory {
			let scope = if let Some(graphs) = &self.graphs {
				directory.join("graphs").join(format!("{:016x}", graphs[0].id().get()))
			} else {
				directory.join("atoms")
			};
			for generation in fs::read_dir(scope).map_err(io_error)? {
				let generation = generation.map_err(io_error)?;
				if !generation.file_type().map_err(io_error)?.is_dir() {
					continue;
				}
				let relative = match config.kind {
					IndexKind::Primary => "primary.fgidx",
					IndexKind::Standard => "idx/std_0000.fgidx",
					IndexKind::Unique => "idx/uniq_0000.fgidx",
				};
				let path = generation.path().join(relative);
				if path.try_exists().map_err(io_error)? {
					paths.insert(path);
				}
			}
			if paths.is_empty() {
				return Err(invalid("selected physical binding has no files"));
			}
		}
		Ok(paths)
	}

	/// Deletes only the unique child created by this fixture, after all handles
	/// close.
	pub(super) fn finish(mut self) -> FerrosGrynnResult<()> {
		drop(self.store.take());
		if let Some((directory, parent)) = self.directory {
			let absolute = directory.canonicalize().map_err(io_error)?;
			if absolute.parent() != Some(parent.as_path()) {
				return Err(invalid("cleanup escaped fixture parent"));
			}
			fs::remove_dir_all(absolute).map_err(io_error)?;
		}
		Ok(())
	}
}
