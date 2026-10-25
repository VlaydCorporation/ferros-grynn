use std::collections::BTreeMap;

use fg_runtime::__internal::{PropertyDictionaryInternal as _, TextInternal as _};
use jiff::tz::{Offset, TimeZone};

use super::{
	DiskGraphRef,
	FIRST_FIXED_TIME_ZONE_ID,
	FIXED_TIME_ZONE_BIAS,
	MAX_FIXED_TIME_ZONE_OFFSET,
	MAX_IANA_TIME_ZONE_ID,
	encode_budget::EncodeBudget,
	tree_allocation_bytes,
	unresolved,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{GraphRef, PropertyDictionary, PropertyKeyId, Text},
};

/// Bidirectional dictionary assigning compact identifiers to text values.
#[derive(Debug, Default)]
pub(super) struct StringDictionary {
	/// Mapping from names to the identifiers assigned by the dictionary.
	ids: BTreeMap<Text, u32>,
	/// Reverse mapping from assigned dictionary identifiers to names.
	names: Vec<Text>,
}

impl StringDictionary {
	/// Looks up borrowed names before copying, then admits the complete
	/// insertion peak.
	fn intern_bounded(
		&mut self,
		name: &str,
		first_id: u32,
		maximum_id: u32,
		budget: &mut EncodeBudget,
	) -> FerrosGrynnResult<u32> {
		if let Some(id) = self.ids.get(name) {
			return Ok(*id);
		}
		let id = self.next_id(first_id, maximum_id)?;
		let next = EncodeBudget::add(self.names.len(), 1)?;
		let text = Text::estimated_copy_heap_bytes(name.len());
		let text = EncodeBudget::add(text, text)?;
		// A full tree upper bound covers root/split allocations; subtracting
		// two upper bounds would not prove an insertion's allocation
		// peak.
		let tree = tree_allocation_bytes(next, size_of::<(Text, u32)>())?;
		let (growth, overlap) = if self.names.len() == self.names.capacity() {
			let old = EncodeBudget::array_bytes::<Text>(self.names.capacity())?;
			let new = EncodeBudget::array_bytes::<Text>(next)?;
			(new - old, EncodeBudget::add(old, old)?)
		} else {
			(0, 0)
		};
		budget
			.retain_context(EncodeBudget::add(EncodeBudget::add(text, tree)?, growth)?, overlap)?;
		self.names.try_reserve_exact(1).map_err(|_error| {
			FerrosGrynnError::invalid_storage_value_encoding(0, "time-zone names allocation failed")
		})?;
		let name = Text::from(name);
		self.names.push(name.clone());
		self.ids.insert(name, id);
		Ok(id)
	}

	/// Returns intern for `StringDictionary`.
	pub(super) fn intern(
		&mut self,
		name: Text,
		first_id: u32,
		maximum_id: u32,
	) -> FerrosGrynnResult<u32> {
		if let Some(id) = self.ids.get(name.as_str()) {
			return Ok(*id);
		}
		let id = self.next_id(first_id, maximum_id)?;
		self.names.push(name.clone());
		self.ids.insert(name, id);
		Ok(id)
	}

	/// Validates the next dense slot before any dictionary allocation.
	fn next_id(&self, first_id: u32, maximum_id: u32) -> FerrosGrynnResult<u32> {
		let index = u32::try_from(self.names.len()).map_err(|_err| {
			FerrosGrynnError::storage_decode_limit_exceeded(
				"dictionary entries",
				u64::MAX,
				u64::from(maximum_id),
			)
		})?;
		let id = first_id.checked_add(index).filter(|id| *id <= maximum_id).ok_or_else(|| {
			FerrosGrynnError::storage_decode_limit_exceeded(
				"dictionary entries",
				self.names.len() as u64 + 1,
				u64::from(maximum_id - first_id + 1),
			)
		})?;
		Ok(id)
	}

	/// Returns names for `StringDictionary`.
	fn names(&self) -> impl ExactSizeIterator<Item = &Text> {
		self.names.iter()
	}
}

#[cfg(test)]
/// New dictionary ownership and literal diagnostic boundaries.
mod bounded_tests;

/// Bidirectional mapping between class names and on-disk class identifiers.
#[derive(Debug, Default)]
pub(super) struct ClassDictionary {
	/// Mapping from names to the identifiers assigned by the dictionary.
	ids: BTreeMap<Text, u32>,
	/// Reverse mapping from assigned dictionary identifiers to names.
	names: BTreeMap<u32, Text>,
}

impl ClassDictionary {
	/// Returns register for `ClassDictionary`.
	fn register(&mut self, id: u32, name: Text) -> FerrosGrynnResult<()> {
		if id == 0 || id == u32::MAX {
			return Err(FerrosGrynnError::invalid_argument(
				"class identity must be in 1..u32::MAX",
			));
		}
		if self.ids.get(&name).is_some_and(|existing| *existing != id)
			|| self.names.get(&id).is_some_and(|existing| existing != &name)
		{
			return Err(FerrosGrynnError::storage_context_conflict("class", id.to_string()));
		}
		self.ids.insert(name.clone(), id);
		self.names.insert(id, name);
		Ok(())
	}

	/// Returns id for `ClassDictionary`.
	pub(super) fn id(&self, name: &Text) -> Option<u32> {
		self.ids.get(name).copied()
	}

	/// Returns resolve for `ClassDictionary`.
	pub(super) fn resolve(&self, id: u32) -> Option<&Text> {
		self.names.get(&id)
	}

	/// Returns entries for `ClassDictionary`.
	fn entries(&self) -> impl ExactSizeIterator<Item = (u32, &Text)> {
		self.names.iter().map(|(id, name)| (*id, name))
	}
}

/// Mutable dictionary and reference context used while converting runtime
/// values.
#[derive(Debug, Default)]
pub struct EncodeContext {
	/// Dictionary resolving interned property keys during value conversion.
	pub(super) properties: PropertyDictionary,
	/// Class-name dictionary used to resolve stored record identities.
	pub(super) classes: ClassDictionary,
	/// Dictionary resolving time-zone names used by stored timestamps.
	time_zones: StringDictionary,
	/// Mapping from owned graph references to compact on-disk references.
	graphs_to_disk: BTreeMap<GraphRef, DiskGraphRef>,
	/// Mapping from compact on-disk graph references to owned references.
	graphs_from_disk: BTreeMap<DiskGraphRef, GraphRef>,
}

impl EncodeContext {
	/// Estimates the initial property clone and both class/graph resolver maps
	/// from borrowed catalog metadata, without constructing a context.
	///
	/// # Errors
	/// Preserves checked allocation and dictionary-clone sizing failures.
	pub fn initial_retained_bytes<'a, Classes, Graphs>(
		properties: &PropertyDictionary,
		classes: Classes,
		graphs: Graphs,
	) -> FerrosGrynnResult<usize>
	where
		Classes: ExactSizeIterator<Item = &'a Text>,
		Graphs: ExactSizeIterator<Item = &'a GraphRef>,
	{
		let class_tree = tree_allocation_bytes(classes.len(), size_of::<(Text, u32)>())?;
		let graph_tree =
			tree_allocation_bytes(graphs.len(), size_of::<(GraphRef, DiskGraphRef)>())?;
		let trees = EncodeBudget::add(
			EncodeBudget::add(class_tree, class_tree)?,
			EncodeBudget::add(graph_tree, graph_tree)?,
		)?;
		let initial = EncodeBudget::add(properties.estimated_clone_heap_bytes()?, trees)?;
		let mut names = classes.chain(graphs.map(GraphRef::name));
		names.try_fold(initial, |bytes, name| {
			let clone = name.estimated_clone_heap_bytes();
			EncodeBudget::add(bytes, EncodeBudget::add(clone, clone)?)
		})
	}

	/// Admits only new IANA entries; UTC, fixed offsets and repeats need no
	/// copy.
	pub(super) fn intern_time_zone_bounded(
		&mut self,
		value: &jiff::Zoned,
		budget: &mut EncodeBudget,
	) -> FerrosGrynnResult<u32> {
		if let Some(name) = value.time_zone().iana_name().filter(|name| *name != "UTC") {
			self.time_zones.intern_bounded(name, 1, MAX_IANA_TIME_ZONE_ID, budget)
		} else {
			self.intern_time_zone(value)
		}
	}

	/// Constructs `EncodeContext` from the supplied initial state.
	pub fn new(properties: PropertyDictionary) -> Self {
		Self {
			properties,
			..Self::default()
		}
	}

	/// Borrows the stored properties of this `EncodeContext`.
	pub const fn property_dictionary(&self) -> &PropertyDictionary {
		&self.properties
	}

	/// Registers an exact dense property slot in an independent historical
	/// dictionary, preserving the primary dictionary's restore checks.
	///
	/// # Errors
	/// Rejects conflicting or invalid dictionary/reference mappings.
	pub fn register_property_name(
		&mut self,
		id: PropertyKeyId,
		name: Text,
	) -> FerrosGrynnResult<()> {
		self.properties.restore(id, name)
	}

	/// Registers one exact catalog class identity used by canonical RecordId
	/// values.
	///
	/// # Errors
	/// Rejects conflicting or invalid dictionary/reference mappings.
	pub fn register_class_name(&mut self, id: u32, name: Text) -> FerrosGrynnResult<()> {
		self.classes.register(id, name)
	}

	/// Iterates exact class identities and names in identifier order.
	pub fn class_names(&self) -> impl ExactSizeIterator<Item = (u32, &Text)> {
		self.classes.entries()
	}

	/// Iterates registered graph references in durable identity order.
	pub fn graph_references(&self) -> impl ExactSizeIterator<Item = (DiskGraphRef, &GraphRef)> {
		self.graphs_from_disk.iter().map(|(disk, runtime)| (*disk, runtime))
	}

	/// Resolves one class identifier while constructing an order key.
	pub fn class_name(&self, id: u32) -> Option<&Text> {
		self.classes.resolve(id)
	}

	/// Resolves one registered immutable class name to its durable identity.
	pub fn class_id(&self, name: &Text) -> Option<u32> {
		self.classes.id(name)
	}

	/// Restores a published prefix into an empty table after one linear
	/// admission. The complete tree reserve is retained once; sequential
	/// single-entry reservations would repeatedly charge the same table
	/// backing.
	///
	/// # Errors
	/// Rejects a nonempty target, overflow, allocation or encoding limits and
	/// preserves registration failures. Discard the private context on failure.
	pub fn restore_time_zone_names_bounded(
		&mut self,
		names: &[Text],
		maximum: usize,
	) -> FerrosGrynnResult<usize> {
		if !self.time_zones.names.is_empty() {
			return Err(FerrosGrynnError::invalid_argument(
				"time-zone restoration requires an empty dictionary",
			));
		}
		let text = names.iter().try_fold(0, |bytes, name| {
			EncodeBudget::add(bytes, name.estimated_clone_heap_bytes())
		})?;
		let text = EncodeBudget::add(text, text)?;
		let tree = tree_allocation_bytes(names.len(), size_of::<(Text, u32)>())?;
		let vector = EncodeBudget::array_bytes::<Text>(names.len())?;
		let retained = EncodeBudget::add(EncodeBudget::add(tree, vector)?, text)?;
		let mut budget = EncodeBudget::new(maximum);
		budget.retain_context(retained, 0)?;
		self.time_zones.names.try_reserve_exact(names.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_value_encoding(0, "time-zone names allocation failed")
		})?;
		for name in names {
			self.register_time_zone_name(name.clone())?;
		}
		Ok(budget.context_bytes())
	}

	/// Registers one IANA time-zone name in its persisted identifier order.
	///
	/// # Errors
	/// Rejects conflicting or invalid dictionary/reference mappings.
	pub fn register_time_zone_name(&mut self, name: Text) -> FerrosGrynnResult<u32> {
		self.time_zones.intern(name, 1, MAX_IANA_TIME_ZONE_ID)
	}

	/// Iterates IANA time-zone names in their persisted identifier order.
	pub fn time_zone_names(&self) -> impl ExactSizeIterator<Item = &Text> {
		self.time_zones.names()
	}

	/// Resolves a registered graph identity while constructing an order key.
	pub fn registered_graph(&self, disk: DiskGraphRef) -> Option<&GraphRef> {
		self.graphs_from_disk.get(&disk)
	}

	/// Registers graph for `EncodeContext`.
	///
	/// # Errors
	/// Rejects conflicting or invalid dictionary/reference mappings.
	pub fn register_graph(
		&mut self,
		runtime: GraphRef,
		disk: DiskGraphRef,
	) -> FerrosGrynnResult<()> {
		if runtime.id() != disk.graph_id() {
			return Err(FerrosGrynnError::storage_context_conflict("graph", disk.to_string()));
		}
		self.register_historical_graph(runtime, disk)
	}

	/// Registers a group-local historical slot without consulting the current
	/// graph catalog. The runtime identity and descriptor remain unchanged.
	///
	/// # Errors
	/// Rejects conflicting or invalid dictionary/reference mappings.
	pub fn register_historical_graph(
		&mut self,
		runtime: GraphRef,
		disk: DiskGraphRef,
	) -> FerrosGrynnResult<()> {
		if self.graphs_to_disk.get(&runtime).is_some_and(|existing| *existing != disk)
			|| self.graphs_from_disk.get(&disk).is_some_and(|existing| existing != &runtime)
		{
			return Err(FerrosGrynnError::storage_context_conflict("graph", disk.to_string()));
		}
		self.graphs_to_disk.insert(runtime.clone(), disk);
		self.graphs_from_disk.insert(disk, runtime);
		Ok(())
	}

	/// Returns decoder for `EncodeContext`.
	pub const fn decoder(&self) -> DecodeContext<'_> {
		DecodeContext {
			properties: &self.properties,
			classes: &self.classes,
			time_zones: self.time_zones.names.as_slice(),
			graphs: &self.graphs_from_disk,
		}
	}

	/// Returns graph to disk for `EncodeContext`.
	pub(super) fn graph_to_disk(&self, value: &GraphRef) -> FerrosGrynnResult<DiskGraphRef> {
		self.graphs_to_disk
			.get(value)
			.copied()
			.ok_or_else(|| unresolved("graph", value.name().to_string()))
	}

	/// Returns intern time zone for `EncodeContext`.
	pub(super) fn intern_time_zone(&mut self, value: &jiff::Zoned) -> FerrosGrynnResult<u32> {
		if let Some(name) = value.time_zone().iana_name() {
			if name == "UTC" {
				return Ok(0);
			}
			return self.time_zones.intern(name.into(), 1, MAX_IANA_TIME_ZONE_ID);
		}

		let offset = value.offset().seconds();
		if !(-FIXED_TIME_ZONE_BIAS..=MAX_FIXED_TIME_ZONE_OFFSET).contains(&offset) {
			return Err(FerrosGrynnError::invalid_storage_value_encoding(
				0,
				"fixed time-zone offset is outside the storage range",
			));
		}
		Ok((i64::from(FIRST_FIXED_TIME_ZONE_ID)
			+ i64::from(offset)
			+ i64::from(FIXED_TIME_ZONE_BIAS)) as u32)
	}
}

/// Borrowed resolver context used while moving storage values into runtime
/// values.
#[derive(Debug, Clone, Copy)]
pub struct DecodeContext<'a> {
	/// Dictionary resolving interned property keys during value conversion.
	pub(super) properties: &'a PropertyDictionary,
	/// Class-name dictionary used to resolve stored record identities.
	pub(super) classes: &'a ClassDictionary,
	/// Names in persisted identifier order; decoding needs no reverse map.
	time_zones: &'a [Text],
	/// Graph references or physical graph snapshots indexed for resolution.
	graphs: &'a BTreeMap<DiskGraphRef, GraphRef>,
}

impl DecodeContext<'_> {
	/// Borrows the validated table belonging to the exact physical source.
	/// Property, class and graph resolvers keep their caller-owned authority.
	pub const fn with_time_zone_names<'a>(&'a self, time_zones: &'a [Text]) -> DecodeContext<'a> {
		DecodeContext {
			properties: self.properties,
			classes: self.classes,
			time_zones,
			graphs: self.graphs,
		}
	}

	/// Returns graph from disk for `DecodeContext<'_>`.
	pub(super) fn graph_from_disk(&self, value: DiskGraphRef) -> FerrosGrynnResult<&GraphRef> {
		self.graphs.get(&value).ok_or_else(|| unresolved("graph", value.to_string()))
	}

	/// Returns time zone for `DecodeContext<'_>`.
	pub(super) fn time_zone(&self, id: u32) -> FerrosGrynnResult<TimeZone> {
		match id {
			0 => Ok(TimeZone::UTC),
			1..=MAX_IANA_TIME_ZONE_ID => {
				let name = self
					.time_zones
					.get((id - 1) as usize)
					.ok_or_else(|| unresolved("time zone", id.to_string()))?;
				TimeZone::get(name.as_str()).map_err(|source| {
					FerrosGrynnError::unresolved_storage_reference("time zone", name.to_string())
						.with_technical_source(source)
				})
			}
			FIRST_FIXED_TIME_ZONE_ID.. => {
				let offset = i64::from(id)
					- i64::from(FIRST_FIXED_TIME_ZONE_ID)
					- i64::from(FIXED_TIME_ZONE_BIAS);
				let offset = i32::try_from(offset).map_err(|_err| {
					FerrosGrynnError::invalid_storage_value_encoding(
						0,
						"fixed time-zone identifier overflows seconds",
					)
				})?;
				if !(-FIXED_TIME_ZONE_BIAS..=MAX_FIXED_TIME_ZONE_OFFSET).contains(&offset) {
					return Err(FerrosGrynnError::invalid_storage_value_encoding(
						0,
						"fixed time-zone identifier is outside the storage range",
					));
				}
				Offset::from_seconds(offset).map(TimeZone::fixed).map_err(|source| {
					FerrosGrynnError::invalid_storage_value_encoding(
						0,
						"fixed time-zone offset is invalid",
					)
					.with_technical_source(source)
				})
			}
		}
	}
}
