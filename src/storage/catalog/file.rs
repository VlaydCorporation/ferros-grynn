use std::{
	collections::{BTreeMap, BTreeSet},
	path::{Component, Path, PathBuf},
};

use super::SYSTEM_CATALOG_PATH;
use crate::{
	datastore::DataSpaceId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		file_registry::{DataFileRegistration, DataFileRole},
		types::{DataFileId, DataFileKind},
	},
};

/// File-catalog format magic.
const MAGIC: [u8; 4] = *b"FGFC";
/// Only accepted file-catalog version.
const VERSION: u16 = 5;
/// Fixed byte width of the catalog header.
const HEADER_BYTES: usize = 24;
/// Fixed byte width of one entry before its path bytes.
const ENTRY_HEADER_BYTES: usize = 32;
/// Defensive upper bound for one encoded catalog.
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
/// Defensive upper bound for one canonical relative path.
const MAX_PATH_BYTES: usize = 1024;

/// Validated slash-separated path below one trusted database root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::storage) struct CatalogPath(Box<str>);

impl CatalogPath {
	/// Parses one canonical relative path without accepting Windows aliases.
	pub(in crate::storage) fn parse(value: &str) -> FerrosGrynnResult<Self> {
		if value.is_empty()
			|| value.len() > MAX_PATH_BYTES
			|| value.chars().any(|character| matches!(character, '\\' | ':' | '\0'))
			|| value.split('/').any(invalid_component)
		{
			return Err(invalid("file catalog contains a non-canonical relative path"));
		}
		let path = Path::new(value);
		if path.is_absolute()
			|| path.components().any(|component| !matches!(component, Component::Normal(_)))
		{
			return Err(invalid("file catalog path escapes the database directory"));
		}
		Ok(Self(value.into()))
	}

	/// Returns the canonical persisted form.
	pub(in crate::storage) fn as_str(&self) -> &str {
		&self.0
	}

	/// Resolves validated components below a trusted root.
	pub(in crate::storage) fn resolve(&self, root: &Path) -> PathBuf {
		self.0.split('/').fold(root.to_path_buf(), |path, component| path.join(component))
	}

	/// Returns the generation directory shared by companion files.
	fn generation_directory(&self) -> Option<&str> {
		self.0.rsplit_once('/').map(|(directory, _name)| directory)
	}
}

/// One exact entry in the sealed physical-file registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct FileCatalogEntry {
	/// Logical data space owning the file.
	pub(in crate::storage) space_id: DataSpaceId,
	/// Exact physical file identity.
	pub(in crate::storage) file_id: DataFileId,
	/// Mutable data file served by this freelist, when applicable.
	pub(in crate::storage) companion_of: Option<DataFileId>,
	/// Checkpoint publication role.
	pub(in crate::storage) role: DataFileRole,
	/// Canonical path relative to the database root.
	pub(in crate::storage) path: CatalogPath,
}

/// Canonical `FGFC v5` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct FileCatalog {
	/// Next unused physical file-instance identity.
	pub(in crate::storage) next_file_instance_id: u64,
	/// Entries sorted by `(DataSpaceId, DataFileId)`.
	pub(in crate::storage) files: Vec<FileCatalogEntry>,
}

impl FileCatalog {
	/// Creates the minimal catalog containing the shared catalog page file.
	pub(in crate::storage) fn bootstrap() -> FerrosGrynnResult<Self> {
		Ok(Self {
			next_file_instance_id: 1,
			files: vec![FileCatalogEntry {
				space_id: DataSpaceId::Database,
				file_id: DataFileId::for_kind(DataFileKind::SystemCatalog),
				companion_of: None,
				role: DataFileRole::WalBackfilled,
				path: CatalogPath::parse(SYSTEM_CATALOG_PATH)?,
			}],
		})
	}

	/// Encodes exact `FGFC v5` bytes.
	pub(in crate::storage) fn encode(&self) -> FerrosGrynnResult<Vec<u8>> {
		validate(self)?;
		let count = u32::try_from(self.files.len())
			.map_err(|_error| invalid("file catalog contains more than u32::MAX files"))?;
		let mut output = Vec::with_capacity(HEADER_BYTES + self.files.len() * ENTRY_HEADER_BYTES);
		output.extend_from_slice(&MAGIC);
		output.extend_from_slice(&VERSION.to_le_bytes());
		output.extend_from_slice(&0_u16.to_le_bytes());
		output.extend_from_slice(&count.to_le_bytes());
		output.extend_from_slice(&0_u32.to_le_bytes());
		output.extend_from_slice(&self.next_file_instance_id.to_le_bytes());
		for file in &self.files {
			let path_len = u16::try_from(file.path.as_str().len())
				.map_err(|_error| invalid("file catalog path exceeds u16"))?;
			output.extend_from_slice(&file.space_id.raw().to_le_bytes());
			output.extend_from_slice(&file.file_id.get().to_le_bytes());
			output.extend_from_slice(&file.companion_of.map_or(0, DataFileId::get).to_le_bytes());
			output.extend_from_slice(&path_len.to_le_bytes());
			output.push(role_byte(file.role));
			output.extend_from_slice(&[0; 5]);
			output.extend_from_slice(file.path.as_str().as_bytes());
		}
		if output.len() > MAX_CATALOG_BYTES {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"file catalog bytes",
				MAX_CATALOG_BYTES as u64,
				output.len() as u64,
			));
		}
		Ok(output)
	}

	/// Decodes only exact `FGFC v5` bytes and validates the complete registry.
	pub(in crate::storage) fn decode(bytes: &[u8]) -> FerrosGrynnResult<Self> {
		if bytes.len() > MAX_CATALOG_BYTES {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"file catalog bytes",
				MAX_CATALOG_BYTES as u64,
				bytes.len() as u64,
			));
		}
		let mut decoder = Decoder::new(bytes);
		if decoder.read_exact(4)? != MAGIC {
			return Err(invalid("invalid file catalog marker"));
		}
		let version = decoder.read_u16()?;
		if version != VERSION {
			return Err(FerrosGrynnError::unsupported_storage_format(
				u64::from(version),
				u64::from(VERSION),
			));
		}
		decoder.expect_zero(2, "file catalog header reserved bytes")?;
		let count = usize::try_from(decoder.read_u32()?)
			.map_err(|_error| invalid("file catalog count does not fit usize"))?;
		decoder.expect_zero(4, "file catalog header reserved bytes")?;
		let next_file_instance_id = decoder.read_u64()?;
		let minimum = count
			.checked_mul(ENTRY_HEADER_BYTES)
			.and_then(|count| HEADER_BYTES.checked_add(count))
			.ok_or_else(|| invalid("file catalog entry area overflows usize"))?;
		if minimum > bytes.len() {
			return Err(invalid("file catalog count exceeds its payload"));
		}
		let mut files = Vec::with_capacity(count);
		for _ in 0..count {
			let space_id = DataSpaceId::from_raw(decoder.read_u64()?)
				.ok_or_else(|| invalid("file catalog contains an invalid data-space identity"))?;
			let file_id = DataFileId::from_raw(decoder.read_u64()?)
				.ok_or_else(|| invalid("file catalog contains an unknown data-file identity"))?;
			let raw_companion = decoder.read_u64()?;
			let companion_of = if raw_companion == 0 {
				None
			} else {
				Some(DataFileId::from_raw(raw_companion).ok_or_else(|| {
					invalid("file catalog contains an unknown companion identity")
				})?)
			};
			let path_len = usize::from(decoder.read_u16()?);
			let role = role_from_byte(decoder.read_u8()?)?;
			decoder.expect_zero(5, "file catalog entry reserved bytes")?;
			if path_len > MAX_PATH_BYTES {
				return Err(invalid("file catalog path exceeds its format limit"));
			}
			let path = std::str::from_utf8(decoder.read_exact(path_len)?)
				.map_err(|_error| invalid("file catalog path is not UTF-8"))?;
			files.push(FileCatalogEntry {
				space_id,
				file_id,
				companion_of,
				role,
				path: CatalogPath::parse(path)?,
			});
		}
		decoder.finish()?;
		let catalog = Self {
			next_file_instance_id,
			files,
		};
		validate(&catalog)?;
		Ok(catalog)
	}

	/// Resolves cataloged paths into registry bootstrap values.
	pub(in crate::storage) fn registrations(&self, root: &Path) -> Vec<DataFileRegistration> {
		self.files
			.iter()
			.map(|file| DataFileRegistration {
				space_id: file.space_id,
				file_id: file.file_id,
				role: file.role,
				path: file.path.resolve(root),
			})
			.collect()
	}
}

/// Validates cross-entry identities, roles, and freelist bindings.
fn validate(catalog: &FileCatalog) -> FerrosGrynnResult<()> {
	if catalog.next_file_instance_id == 0
		|| catalog.next_file_instance_id > DataFileId::EXHAUSTED_INSTANCE_ID
	{
		return Err(invalid("file catalog next-file clock is outside its format domain"));
	}
	let mut previous = None;
	let mut entries = BTreeMap::new();
	for file in &catalog.files {
		let key = (file.space_id, file.file_id);
		if previous.is_some_and(|previous| previous >= key) {
			return Err(invalid("file catalog entries are not in strict identity order"));
		}
		previous = Some(key);
		if file.file_id.instance_id() >= catalog.next_file_instance_id {
			return Err(invalid("file catalog contains an identity beyond its next-file clock"));
		}
		validate_space_kind(file.space_id, file.file_id.kind())?;
		let superblock = matches!(
			file.file_id.kind(),
			Some(DataFileKind::AtomStoreSuperblock | DataFileKind::GraphSuperblock)
		);
		if superblock != (file.role == DataFileRole::Superblock) {
			return Err(invalid("file catalog role does not match its file kind"));
		}
		if file.file_id.kind() == Some(DataFileKind::Freelist) {
			if file.companion_of.is_none() {
				return Err(invalid("freelist entry has no companion target"));
			}
		} else if file.companion_of.is_some() {
			return Err(invalid("non-freelist entry has a companion target"));
		}
		if entries.insert(key, file).is_some() {
			return Err(invalid("file catalog contains duplicate identities"));
		}
	}

	let mut companions = BTreeSet::new();
	for file in &catalog.files {
		let Some(target_id) = file.companion_of else {
			continue;
		};
		let target = entries
			.get(&(file.space_id, target_id))
			.ok_or_else(|| invalid("freelist companion target is absent from its data space"))?;
		if !requires_freelist(file.space_id, target_id.kind()) {
			return Err(invalid("freelist companion target cannot own mutable pages"));
		}
		if file.path.generation_directory() != target.path.generation_directory() {
			return Err(invalid("freelist and target belong to different generations"));
		}
		if !companions.insert((file.space_id, target_id)) {
			return Err(invalid("one data file has more than one freelist companion"));
		}
	}
	for file in &catalog.files {
		if requires_freelist(file.space_id, file.file_id.kind())
			&& !companions.contains(&(file.space_id, file.file_id))
		{
			return Err(invalid("mutable data file has no freelist companion"));
		}
	}
	Ok(())
}

/// Validates that one physical file kind belongs to its declared data space.
fn validate_space_kind(space: DataSpaceId, kind: Option<DataFileKind>) -> FerrosGrynnResult<()> {
	let valid = match (space, kind) {
		(_, None) => false,
		(DataSpaceId::Database, Some(kind)) => matches!(
			kind,
			DataFileKind::SystemCatalog
				| DataFileKind::AtomStoreSuperblock
				| DataFileKind::AtomsHot
				| DataFileKind::Properties
				| DataFileKind::Freelist
				| DataFileKind::Index
				| DataFileKind::ClassStatistics
				| DataFileKind::PropertyStatistics
				| DataFileKind::PropertyDictionary
		),
		(DataSpaceId::Graph(_), Some(kind)) => matches!(
			kind,
			DataFileKind::GraphSuperblock
				| DataFileKind::NodesHot
				| DataFileKind::EdgesHot
				| DataFileKind::AdjacencyOverflow
				| DataFileKind::CrossLevelAdjacencyOverflow
				| DataFileKind::Overlays
				| DataFileKind::Subgraphs
				| DataFileKind::Labels
				| DataFileKind::Freelist
				| DataFileKind::Statistics
				| DataFileKind::LabelStatistics
				| DataFileKind::PropertyStatistics
				| DataFileKind::Index
				| DataFileKind::TensorCache
		),
	};
	if !valid {
		return Err(invalid("data-file kind does not belong to its data space"));
	}
	Ok(())
}

/// Reports whether the current format requires a companion freelist.
fn requires_freelist(space: DataSpaceId, kind: Option<DataFileKind>) -> bool {
	match space {
		DataSpaceId::Database => {
			matches!(kind, Some(DataFileKind::AtomsHot | DataFileKind::Properties))
		}
		DataSpaceId::Graph(_) => matches!(
			kind,
			Some(
				DataFileKind::NodesHot
					| DataFileKind::EdgesHot
					| DataFileKind::AdjacencyOverflow
					| DataFileKind::CrossLevelAdjacencyOverflow
					| DataFileKind::Overlays
					| DataFileKind::Subgraphs
			)
		),
	}
}

/// Rejects traversal, empty segments, and Windows reserved path aliases.
fn invalid_component(component: &str) -> bool {
	if component.is_empty()
		|| matches!(component, "." | "..")
		|| component.ends_with('.')
		|| component.ends_with(' ')
	{
		return true;
	}
	let stem = component.split('.').next().unwrap_or(component).to_ascii_uppercase();
	matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
		|| (stem.len() == 4
			&& (stem.starts_with("COM") || stem.starts_with("LPT"))
			&& matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

/// Encodes a checkpoint role into its canonical byte.
const fn role_byte(role: DataFileRole) -> u8 {
	match role {
		DataFileRole::WalBackfilled => 1,
		DataFileRole::Superblock => 2,
	}
}

/// Decodes one canonical checkpoint-role byte.
fn role_from_byte(value: u8) -> FerrosGrynnResult<DataFileRole> {
	match value {
		1 => Ok(DataFileRole::WalBackfilled),
		2 => Ok(DataFileRole::Superblock),
		_ => Err(invalid("file catalog contains an unknown checkpoint role")),
	}
}

/// Builds the canonical invalid-file error for this codec.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

/// Bounds-checked cursor over one encoded catalog payload.
struct Decoder<'a> {
	/// Complete immutable payload.
	bytes: &'a [u8],
	/// Next unread byte offset.
	position: usize,
}

impl<'a> Decoder<'a> {
	/// Creates a cursor at the start of `bytes`.
	const fn new(bytes: &'a [u8]) -> Self {
		Self {
			bytes,
			position: 0,
		}
	}

	/// Reads exactly `count` bytes and advances the cursor.
	fn read_exact(&mut self, count: usize) -> FerrosGrynnResult<&'a [u8]> {
		let end = self
			.position
			.checked_add(count)
			.ok_or_else(|| invalid("file catalog offset overflows usize"))?;
		let value = self
			.bytes
			.get(self.position..end)
			.ok_or_else(|| invalid("file catalog payload is truncated"))?;
		self.position = end;
		Ok(value)
	}

	/// Reads one byte.
	fn read_u8(&mut self) -> FerrosGrynnResult<u8> {
		Ok(self.read_exact(1)?[0])
	}

	/// Reads one little-endian `u16`.
	fn read_u16(&mut self) -> FerrosGrynnResult<u16> {
		let bytes: [u8; 2] = self
			.read_exact(2)?
			.try_into()
			.map_err(|_error| invalid("file catalog u16 is truncated"))?;
		Ok(u16::from_le_bytes(bytes))
	}

	/// Reads one little-endian `u32`.
	fn read_u32(&mut self) -> FerrosGrynnResult<u32> {
		let bytes: [u8; 4] = self
			.read_exact(4)?
			.try_into()
			.map_err(|_error| invalid("file catalog u32 is truncated"))?;
		Ok(u32::from_le_bytes(bytes))
	}

	/// Reads one little-endian `u64`.
	fn read_u64(&mut self) -> FerrosGrynnResult<u64> {
		let bytes: [u8; 8] = self
			.read_exact(8)?
			.try_into()
			.map_err(|_error| invalid("file catalog u64 is truncated"))?;
		Ok(u64::from_le_bytes(bytes))
	}

	/// Consumes a reserved range and requires every byte to be zero.
	fn expect_zero(&mut self, count: usize, field: &str) -> FerrosGrynnResult<()> {
		if self.read_exact(count)?.iter().any(|byte| *byte != 0) {
			return Err(invalid(format!("{field} are non-zero")));
		}
		Ok(())
	}

	/// Rejects unconsumed trailing bytes.
	fn finish(self) -> FerrosGrynnResult<()> {
		if self.position != self.bytes.len() {
			return Err(invalid("file catalog contains trailing bytes"));
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::graph::GraphId;

	#[test]
	fn fgfc_v5_has_exact_space_header_and_rejects_v4() -> FerrosGrynnResult<()> {
		let catalog = FileCatalog::bootstrap()?;
		let bytes = catalog.encode()?;
		let mut expected = vec![
			b'F', b'G', b'F', b'C', 5, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0,
		];
		expected.extend_from_slice(&0_u64.to_le_bytes());
		expected.extend_from_slice(
			&DataFileId::for_kind(DataFileKind::SystemCatalog).get().to_le_bytes(),
		);
		expected.extend_from_slice(&0_u64.to_le_bytes());
		expected.extend_from_slice(&18_u16.to_le_bytes());
		expected.push(1);
		expected.extend_from_slice(&[0; 5]);
		expected.extend_from_slice(SYSTEM_CATALOG_PATH.as_bytes());
		assert_eq!(bytes, expected);
		assert_eq!(FileCatalog::decode(&bytes)?, catalog);

		let mut old = bytes;
		old[4..6].copy_from_slice(&4_u16.to_le_bytes());
		assert_eq!(
			FileCatalog::decode(&old).err().map(|error| error.status().code().to_string()),
			Some("FG-51S01".to_owned())
		);
		Ok(())
	}

	#[test]
	fn companions_are_exact_and_space_local() -> FerrosGrynnResult<()> {
		let graph = DataSpaceId::Graph(GraphId::new(7).unwrap());
		let atoms =
			file(1, DataFileKind::AtomsHot, DataSpaceId::Database, "atoms/g/atoms_hot.fgb", None)?;
		let atom_free = file(
			2,
			DataFileKind::Freelist,
			DataSpaceId::Database,
			"atoms/g/freelist_atoms.fgb",
			Some(atoms.file_id),
		)?;
		let props = file(
			3,
			DataFileKind::Properties,
			DataSpaceId::Database,
			"atoms/g/properties.fgb",
			None,
		)?;
		let prop_free = file(
			4,
			DataFileKind::Freelist,
			DataSpaceId::Database,
			"atoms/g/freelist_props.fgb",
			Some(props.file_id),
		)?;
		let nodes = file(5, DataFileKind::NodesHot, graph, "graphs/7/g/nodes_hot.fgb", None)?;
		let node_free = file(
			6,
			DataFileKind::Freelist,
			graph,
			"graphs/7/g/freelist_nodes.fgb",
			Some(nodes.file_id),
		)?;
		let node_free_id = node_free.file_id;
		let mut files = vec![atoms, atom_free, props, prop_free, nodes, node_free];
		files.sort_unstable_by_key(|file| (file.space_id, file.file_id));
		let catalog = FileCatalog {
			next_file_instance_id: 7,
			files,
		};
		assert_eq!(FileCatalog::decode(&catalog.encode()?)?, catalog);

		let mut wrong_space = catalog;
		let freelist = wrong_space
			.files
			.iter_mut()
			.find(|file| file.file_id == node_free_id)
			.ok_or_else(|| invalid("test freelist is absent"))?;
		freelist.space_id = DataSpaceId::Database;
		wrong_space.files.sort_unstable_by_key(|file| (file.space_id, file.file_id));
		assert!(wrong_space.encode().is_err());
		Ok(())
	}

	#[test]
	fn property_dictionary_is_database_global_and_has_no_freelist() -> FerrosGrynnResult<()> {
		let mut catalog = FileCatalog::bootstrap()?;
		let dictionary = file(
			1,
			DataFileKind::PropertyDictionary,
			DataSpaceId::Database,
			"atoms/g/property_keys.fgb",
			None,
		)?;
		let dictionary_id = dictionary.file_id;
		catalog.next_file_instance_id = 2;
		catalog.files.push(dictionary);
		catalog.files.sort_unstable_by_key(|entry| (entry.space_id, entry.file_id));
		assert_eq!(FileCatalog::decode(&catalog.encode()?)?, catalog);

		let mut companion = catalog.clone();
		companion
			.files
			.iter_mut()
			.find(|entry| entry.file_id == dictionary_id)
			.ok_or_else(|| invalid("test property dictionary is absent"))?
			.companion_of = Some(DataFileId::for_kind(DataFileKind::SystemCatalog));
		assert!(companion.encode().is_err());

		let mut graph_local = catalog;
		graph_local
			.files
			.iter_mut()
			.find(|entry| entry.file_id == dictionary_id)
			.ok_or_else(|| invalid("test property dictionary is absent"))?
			.space_id = DataSpaceId::Graph(
			GraphId::new(1).ok_or_else(|| invalid("test graph identity is invalid"))?,
		);
		graph_local.files.sort_unstable_by_key(|entry| (entry.space_id, entry.file_id));
		assert!(graph_local.encode().is_err());
		Ok(())
	}

	fn file(
		instance: u64,
		kind: DataFileKind,
		space_id: DataSpaceId,
		path: &str,
		companion_of: Option<DataFileId>,
	) -> FerrosGrynnResult<FileCatalogEntry> {
		Ok(FileCatalogEntry {
			space_id,
			file_id: DataFileId::new(instance, kind)
				.ok_or_else(|| invalid("test data-file identity is invalid"))?,
			companion_of,
			role: DataFileRole::WalBackfilled,
			path: CatalogPath::parse(path)?,
		})
	}

	#[test]
	fn path_rejects_traversal_and_windows_aliases() {
		for invalid_path in [
			"",
			"/absolute",
			"../escape",
			"schema/../escape",
			"schema\\catalog.fgb",
			"C:/outside",
			"schema/NUL.fgb",
			"schema/catalog.fgb.",
		] {
			assert!(CatalogPath::parse(invalid_path).is_err(), "accepted {invalid_path:?}");
		}
	}
}
