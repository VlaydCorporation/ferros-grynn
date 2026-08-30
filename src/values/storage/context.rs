use std::collections::BTreeMap;

use jiff::tz::{Offset, TimeZone};

use super::{
	DiskAtomRef,
	DiskGraphRef,
	FIRST_FIXED_TIME_ZONE_ID,
	FIXED_TIME_ZONE_BIAS,
	MAX_FIXED_TIME_ZONE_OFFSET,
	MAX_IANA_TIME_ZONE_ID,
	unresolved,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{AtomRef, GraphRef, PropertyDictionary, Text},
};

#[derive(Debug, Default)]
pub(super) struct StringDictionary {
	ids: BTreeMap<Text, u32>,
	names: Vec<Text>,
}

impl StringDictionary {
	pub(super) fn intern(
		&mut self,
		name: Text,
		first_id: u32,
		maximum_id: u32,
	) -> FerrosGrynnResult<u32> {
		if let Some(id) = self.ids.get(name.as_str()) {
			return Ok(*id);
		}
		let index = u32::try_from(self.names.len()).map_err(|_err| {
			FerrosGrynnError::storage_decode_limit_exceeded(
				"dictionary entries",
				u64::from(maximum_id),
				u64::MAX,
			)
		})?;
		let id = first_id.checked_add(index).filter(|id| *id <= maximum_id).ok_or_else(|| {
			FerrosGrynnError::storage_decode_limit_exceeded(
				"dictionary entries",
				u64::from(maximum_id - first_id + 1),
				self.names.len() as u64 + 1,
			)
		})?;
		self.names.push(name.clone());
		self.ids.insert(name, id);
		Ok(id)
	}

	pub(super) fn resolve(&self, id: u32, first_id: u32) -> Option<&Text> {
		id.checked_sub(first_id).and_then(|index| self.names.get(index as usize))
	}

	fn names(&self) -> impl ExactSizeIterator<Item = &Text> {
		self.names.iter()
	}
}

/// Mutable dictionary and reference context used while converting runtime
/// values.
#[derive(Debug, Default)]
pub struct EncodeContext {
	pub(super) properties: PropertyDictionary,
	pub(super) tables: StringDictionary,
	time_zones: StringDictionary,
	atoms_to_disk: BTreeMap<AtomRef, DiskAtomRef>,
	atoms_from_disk: BTreeMap<DiskAtomRef, AtomRef>,
	graphs_to_disk: BTreeMap<GraphRef, DiskGraphRef>,
	graphs_from_disk: BTreeMap<DiskGraphRef, GraphRef>,
}

impl EncodeContext {
	pub fn new(properties: PropertyDictionary) -> Self {
		Self {
			properties,
			..Self::default()
		}
	}

	pub const fn property_dictionary(&self) -> &PropertyDictionary {
		&self.properties
	}

	/// Registers one table name in the stable order persisted by LabelDict.
	pub(crate) fn register_table_name(&mut self, name: Text) -> FerrosGrynnResult<u32> {
		self.tables.intern(name, 0, u32::MAX)
	}

	/// Iterates table names in their persisted identifier order.
	pub(crate) fn table_names(&self) -> impl ExactSizeIterator<Item = &Text> {
		self.tables.names()
	}

	/// Resolves one table identifier while constructing an order key.
	pub(crate) fn table_name(&self, id: u32) -> Option<&Text> {
		self.tables.resolve(id, 0)
	}

	/// Registers one IANA time-zone name in its persisted identifier order.
	pub(crate) fn register_time_zone_name(&mut self, name: Text) -> FerrosGrynnResult<u32> {
		self.time_zones.intern(name, 1, MAX_IANA_TIME_ZONE_ID)
	}

	/// Iterates IANA time-zone names in their persisted identifier order.
	pub(crate) fn time_zone_names(&self) -> impl ExactSizeIterator<Item = &Text> {
		self.time_zones.names()
	}

	pub fn register_atom(&mut self, runtime: AtomRef, disk: DiskAtomRef) -> FerrosGrynnResult<()> {
		if runtime.kind() != disk.kind()
			|| self.atoms_to_disk.get(&runtime).is_some_and(|existing| *existing != disk)
			|| self.atoms_from_disk.get(&disk).is_some_and(|existing| existing != &runtime)
		{
			return Err(FerrosGrynnError::storage_context_conflict("atom", disk.to_string()));
		}
		self.atoms_to_disk.insert(runtime.clone(), disk);
		self.atoms_from_disk.insert(disk, runtime);
		Ok(())
	}

	/// Resolves a registered disk address to its stable runtime identity.
	pub(crate) fn registered_atom(&self, disk: DiskAtomRef) -> Option<&AtomRef> {
		self.atoms_from_disk.get(&disk)
	}

	pub fn register_graph(
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

	pub const fn decoder(&self) -> DecodeContext<'_> {
		DecodeContext {
			properties: &self.properties,
			tables: &self.tables,
			time_zones: &self.time_zones,
			atoms: &self.atoms_from_disk,
			graphs: &self.graphs_from_disk,
		}
	}

	pub(super) fn atom_to_disk(&self, value: &AtomRef) -> FerrosGrynnResult<DiskAtomRef> {
		self.atoms_to_disk
			.get(value)
			.copied()
			.ok_or_else(|| unresolved("atom", value.id().to_string()))
	}

	pub(super) fn graph_to_disk(&self, value: &GraphRef) -> FerrosGrynnResult<DiskGraphRef> {
		self.graphs_to_disk
			.get(value)
			.copied()
			.ok_or_else(|| unresolved("graph", value.id().to_string()))
	}

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
	pub(super) properties: &'a PropertyDictionary,
	pub(super) tables: &'a StringDictionary,
	time_zones: &'a StringDictionary,
	atoms: &'a BTreeMap<DiskAtomRef, AtomRef>,
	graphs: &'a BTreeMap<DiskGraphRef, GraphRef>,
}

impl DecodeContext<'_> {
	pub(super) fn atom_from_disk(&self, value: DiskAtomRef) -> FerrosGrynnResult<&AtomRef> {
		self.atoms.get(&value).ok_or_else(|| unresolved("atom", value.to_string()))
	}

	pub(super) fn graph_from_disk(&self, value: DiskGraphRef) -> FerrosGrynnResult<&GraphRef> {
		self.graphs.get(&value).ok_or_else(|| unresolved("graph", value.to_string()))
	}

	pub(super) fn time_zone(&self, id: u32) -> FerrosGrynnResult<TimeZone> {
		match id {
			0 => Ok(TimeZone::UTC),
			1..=MAX_IANA_TIME_ZONE_ID => {
				let name = self
					.time_zones
					.resolve(id, 1)
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
