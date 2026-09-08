//! Durable database and physical-file catalog pages.

use std::{
	collections::{BTreeMap, BTreeSet},
	path::{Component, Path, PathBuf},
};

use super::{
	direct::DirectFile,
	file_registry::{DataFileRegistration, DataFileRole},
	graph_pages::{LARGE_PROPERTY_SLOT, PropertyLocation},
	page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
	types::{DataFileId, DataFileKind, GraphId, Lsn},
};
use crate::{
	datastore::{IndexId, TableIndexKind, TableKind},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{
		Type,
		VectorKind,
		graph::{AtomKind, GraphKind},
	},
};

/// Catalog page and value codecs.
mod codec;
/// Durable catalog definitions and update plans.
mod definitions;

#[allow(
	clippy::wildcard_imports,
	reason = "catalog definitions share the private format codec"
)]
use codec::*;
#[cfg(test)]
use definitions::read_catalogs;
use definitions::{CatalogKind, SchemaTypeTag};
pub(super) use definitions::{
	CatalogPath,
	CatalogUpdatePages,
	DatabaseCatalog,
	FileCatalog,
	FileCatalogEntry,
	GraphCatalogEntry,
	ParameterCatalogEntry,
	SystemIndexCatalogEntry,
	TableCatalogEntry,
	TableFieldCatalogEntry,
	TableIndexCatalogEntry,
	encode_bootstrap_catalogs,
	encode_catalog_update,
	read_catalogs_with,
};

/// Trusted relative path of the format-v1 system catalog.
pub(super) const SYSTEM_CATALOG_PATH: &str = "schema/catalog.fgb";
/// Stable singleton identity of the system catalog file.
pub(super) const SYSTEM_CATALOG_FILE_ID: DataFileId =
	DataFileId::for_kind(DataFileKind::SystemCatalog);

/// Catalog-page marker stored after the common page header.
const CATALOG_PAGE_MAGIC: [u8; 4] = *b"FGC1";
/// Catalog-page codec version.
const CATALOG_PAGE_VERSION: u8 = 1;
/// Catalog kind byte offset.
const CATALOG_KIND_OFFSET: usize = PAGE_HEADER_SIZE + CATALOG_PAGE_MAGIC.len();
/// Catalog codec-version byte offset.
const CATALOG_VERSION_OFFSET: usize = CATALOG_KIND_OFFSET + size_of::<u8>();
/// Reserved bytes after the catalog version.
const CATALOG_RESERVED_A_OFFSET: usize = CATALOG_VERSION_OFFSET + size_of::<u8>();
/// Next page in the same catalog chain.
const CATALOG_NEXT_PAGE_OFFSET: usize = CATALOG_RESERVED_A_OFFSET + size_of::<u16>();
/// Bytes used by this page's payload chunk.
const CATALOG_CHUNK_LEN_OFFSET: usize = CATALOG_NEXT_PAGE_OFFSET + size_of::<u64>();
/// Complete logical catalog length repeated on every page.
const CATALOG_TOTAL_LEN_OFFSET: usize = CATALOG_CHUNK_LEN_OFFSET + size_of::<u32>();
/// Reserved bytes before the payload chunk.
const CATALOG_RESERVED_B_OFFSET: usize = CATALOG_TOTAL_LEN_OFFSET + size_of::<u32>();
/// First byte of a catalog payload chunk.
const CATALOG_PAYLOAD_OFFSET: usize = CATALOG_RESERVED_B_OFFSET + size_of::<u64>();
/// Bytes available to one catalog payload chunk.
const CATALOG_PAYLOAD_BYTES: usize = PAGE_SIZE - CATALOG_PAYLOAD_OFFSET;
/// Maximum logical catalog accepted before allocation.
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;

/// Database-catalog payload marker.
const DATABASE_CATALOG_MAGIC: [u8; 4] = *b"FGDC";
/// File-catalog payload marker.
const FILE_CATALOG_MAGIC: [u8; 4] = *b"FGFC";
/// Database-catalog payload codec version.
const DATABASE_CATALOG_PAYLOAD_VERSION: u16 = 4;
/// Physical-file catalog payload codec version.
const FILE_CATALOG_PAYLOAD_VERSION: u16 = 4;
/// Maximum persisted graph name in UTF-8 bytes.
const MAX_GRAPH_NAME_BYTES: usize = 128;
/// Maximum canonical `RecordId` text retained for a graph identity.
const MAX_GRAPH_RECORD_ID_BYTES: usize = 4096;
/// Bytes before the two variable UTF-8 fields in one graph entry.
const GRAPH_CATALOG_FIXED_BYTES: usize = 136;
/// Maximum trusted relative file path in UTF-8 bytes.
const MAX_CATALOG_PATH_BYTES: usize = 1024;
/// Maximum one catalog-owned schema name in UTF-8 bytes.
const MAX_SCHEMA_NAME_BYTES: usize = 1024;
/// Maximum recursive type nesting accepted from storage.
const MAX_TYPE_DEPTH: usize = 64;
/// Maximum number of members in one schema type node.
const MAX_TYPE_MEMBERS: usize = 4096;

#[cfg(test)]
mod tests {
	use std::{
		fs,
		path::{Path, PathBuf},
		sync::atomic::{AtomicU64, Ordering},
	};

	use super::*;

	/// Unique suffix for catalog filesystem fixtures.
	static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

	/// Exact temporary directory removed during unwind as well as success.
	struct TestDirectory(PathBuf);

	impl TestDirectory {
		/// Creates one empty fixture directory below the current workspace.
		fn new(label: &str) -> FerrosGrynnResult<Self> {
			let suffix = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
			let path = std::env::current_dir()
				.map_err(|source| test_io("resolve catalog test directory", source))?
				.join(format!(".ferros-grynn-{label}-{}-{suffix}", std::process::id()));
			fs::create_dir(&path)
				.map_err(|source| test_io("create catalog test directory", source))?;
			Ok(Self(path))
		}

		/// Returns the exact fixture path.
		fn path(&self) -> &Path {
			&self.0
		}
	}

	impl Drop for TestDirectory {
		/// Best-effort cleanup owns only the uniquely created fixture
		/// directory.
		fn drop(&mut self) {
			drop(fs::remove_dir_all(&self.0));
		}
	}

	/// Preserves one native test I/O source behind the storage status boundary.
	fn test_io(operation: &str, source: std::io::Error) -> FerrosGrynnError {
		FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
	}

	/// Both logical catalogs round-trip through checksummed chained pages.
	#[test]
	fn bootstrap_catalogs_round_trip_through_direct_file() -> FerrosGrynnResult<()> {
		let directory = TestDirectory::new("catalog-roundtrip")?;
		let path = directory.path().join("catalog.fgb");
		let database = DatabaseCatalog {
			next_graph_id: 2,
			next_schema_id: 8,
			graphs: vec![GraphCatalogEntry {
				graph_id: GraphId::new(1),
				schema_id: 1,
				schema_version: 1,
				label_index: Some(SystemIndexCatalogEntry {
					index_id: IndexId::from_storage(2)?,
					schema_version: 1,
				}),
				adjacency_index: Some(SystemIndexCatalogEntry {
					index_id: IndexId::from_storage(3)?,
					schema_version: 1,
				}),
				parent_graph_id: None,
				subgraph_slot: None,
				parent_atom: None,
				record_id: "graph:people".to_owned(),
				name: "people".to_owned(),
				kind: GraphKind::Graph,
				created_lsn: Lsn::new(3),
				identity: [7; 16],
				revision: 9,
				primary_index_root: PageNo::new(4),
				primary_index_file_id: DataFileId::for_kind(DataFileKind::Index),
				properties: Some(PropertyLocation::Inline {
					page_no: PageNo::new(2),
					slot: 1,
					length: 17,
				}),
			}],
			property_keys: vec!["name".to_owned(), "tags".to_owned()],
			tables: vec![TableCatalogEntry {
				schema_id: 4,
				schema_version: 1,
				name: "person".to_owned(),
				graph_id: GraphId::new(1),
				kind: TableKind::Normal,
				fields: vec![
					TableFieldCatalogEntry {
						schema_id: 6,
						schema_version: 1,
						name: "name".to_owned(),
						value_type: Type::String,
						property_key: 0,
					},
					TableFieldCatalogEntry {
						schema_id: 7,
						schema_version: 1,
						name: "tags".to_owned(),
						value_type: Type::Option(Box::new(Type::Array(Box::new(Type::String)))),
						property_key: 1,
					},
				],
				indexes: Vec::new(),
				primary_index: SystemIndexCatalogEntry {
					index_id: IndexId::from_storage(5)?,
					schema_version: 1,
				},
			}],
			parameters: Vec::new(),
		};
		let files = FileCatalog::bootstrap()?;
		let encoded = encode_bootstrap_catalogs(&database, &files)?;
		let file = DirectFile::create_new(&path)?;
		file.write_pages(PageNo::new(0), &encoded.pages)?;
		file.sync_data()?;

		let actual = read_catalogs(&file, encoded.database_root, encoded.file_root)?;
		assert_eq!(actual, (database, files));
		Ok(())
	}

	/// Catalog paths reject every form that can escape or alias the DB root.
	#[test]
	fn catalog_path_rejects_traversal_and_windows_aliases() {
		for invalid in [
			"",
			"/absolute",
			"../escape",
			"schema/../escape",
			"schema\\catalog.fgb",
			"C:/outside",
			"schema/NUL.fgb",
			"schema/catalog.fgb.",
		] {
			assert!(CatalogPath::parse(invalid).is_err(), "accepted {invalid:?}");
		}
		assert_eq!(
			CatalogPath::parse(SYSTEM_CATALOG_PATH).ok().map(|path| path.as_str().to_owned()),
			Some(SYSTEM_CATALOG_PATH.to_owned())
		);
	}

	#[test]
	fn file_catalog_v4_has_canonical_fixed_header_and_rejects_v3() -> FerrosGrynnResult<()> {
		let catalog = FileCatalog::bootstrap()?;
		let bytes = catalog.encode()?;
		let mut expected = vec![
			b'F', b'G', b'F', b'C', 4, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0,
		];
		expected.extend_from_slice(&0_u64.to_le_bytes());
		expected.extend_from_slice(&1_u64.to_le_bytes());
		expected.extend_from_slice(&0_u64.to_le_bytes());
		expected.extend_from_slice(&18_u16.to_le_bytes());
		expected.push(role_byte(DataFileRole::WalBackfilled));
		expected.extend_from_slice(&[0; 5]);
		expected.extend_from_slice(SYSTEM_CATALOG_PATH.as_bytes());
		assert_eq!(bytes, expected);
		assert_eq!(FileCatalog::decode(&bytes)?, catalog);

		let mut version_three = bytes.clone();
		version_three[4..6].copy_from_slice(&3_u16.to_le_bytes());
		assert_eq!(
			FileCatalog::decode(&version_three)
				.err()
				.map(|error| error.status().code().to_string()),
			Some("FG-51S01".to_owned())
		);
		assert!(FileCatalog::decode(&bytes[..bytes.len() - 1]).is_err());
		let mut reserved = bytes;
		reserved[51] = 1;
		assert!(FileCatalog::decode(&reserved).is_err());
		let mut invalid_companion = expected;
		invalid_companion[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
		assert!(FileCatalog::decode(&invalid_companion).is_err());
		Ok(())
	}

	#[test]
	fn file_catalog_v4_validates_one_to_one_freelist_bindings() -> FerrosGrynnResult<()> {
		let valid = valid_file_catalog()?;
		let bytes = valid.encode()?;
		assert_eq!(FileCatalog::decode(&bytes)?, valid);

		let mut missing_target = valid.clone();
		let freelist = missing_target
			.files
			.iter_mut()
			.find(|file| file.file_id.kind() == Some(DataFileKind::Freelist))
			.ok_or_else(|| invalid_catalog("test freelist is absent"))?;
		freelist.companion_of = Some(test_file_id(63, DataFileKind::NodesHot)?);
		assert!(missing_target.encode().is_err());

		let mut other_graph = valid.clone();
		let freelist = other_graph
			.files
			.iter_mut()
			.find(|file| file.file_id.kind() == Some(DataFileKind::Freelist))
			.ok_or_else(|| invalid_catalog("test freelist is absent"))?;
		freelist.graph_id = GraphId::new(2);
		other_graph.files.sort_unstable_by_key(|file| (file.graph_id, file.file_id));
		assert!(other_graph.encode().is_err());

		let mut duplicate = valid.clone();
		let mut repeated = duplicate
			.files
			.iter()
			.find(|file| file.file_id.kind() == Some(DataFileKind::Freelist))
			.cloned()
			.ok_or_else(|| invalid_catalog("test freelist is absent"))?;
		repeated.file_id = test_file_id(63, DataFileKind::Freelist)?;
		duplicate.files.push(repeated);
		duplicate.files.sort_unstable_by_key(|file| (file.graph_id, file.file_id));
		duplicate.next_file_instance_id = 64;
		assert!(duplicate.encode().is_err());

		let mut wrong_name = valid.clone();
		let freelist = wrong_name
			.files
			.iter_mut()
			.find(|file| file.file_id.kind() == Some(DataFileKind::Freelist))
			.ok_or_else(|| invalid_catalog("test freelist is absent"))?;
		freelist.path = CatalogPath::parse("graphs/0000000000000001/generation/freelist.fgb")?;
		assert!(wrong_name.encode().is_err());

		let mut wrong_generation = valid.clone();
		let freelist = wrong_generation
			.files
			.iter_mut()
			.find(|file| file.file_id.kind() == Some(DataFileKind::Freelist))
			.ok_or_else(|| invalid_catalog("test freelist is absent"))?;
		freelist.path =
			CatalogPath::parse("graphs/0000000000000001/other-generation/freelist_nodes.fgb")?;
		assert!(wrong_generation.encode().is_err());

		let mut non_freelist_companion = valid.clone();
		let target = non_freelist_companion
			.files
			.iter_mut()
			.find(|file| file.file_id.kind() == Some(DataFileKind::NodesHot))
			.ok_or_else(|| invalid_catalog("test target is absent"))?;
		target.companion_of = Some(target.file_id);
		assert!(non_freelist_companion.encode().is_err());

		let mut empty_binding = valid.clone();
		let freelist = empty_binding
			.files
			.iter_mut()
			.find(|file| file.file_id.kind() == Some(DataFileKind::Freelist))
			.ok_or_else(|| invalid_catalog("test freelist is absent"))?;
		freelist.companion_of = None;
		assert!(empty_binding.encode().is_err());

		let mut missing_companion = valid;
		let position = missing_companion
			.files
			.iter()
			.position(|file| file.file_id.kind() == Some(DataFileKind::Freelist))
			.ok_or_else(|| invalid_catalog("test freelist is absent"))?;
		missing_companion.files.remove(position);
		assert!(missing_companion.encode().is_err());
		Ok(())
	}

	fn valid_file_catalog() -> FerrosGrynnResult<FileCatalog> {
		let graph_id = GraphId::new(1);
		let generation = "graphs/0000000000000001/generation";
		let mut files = FileCatalog::bootstrap()?.files;
		let mut instance = 1;
		for (target_kind, freelist_name) in crate::storage::store::file_layout::FREELIST_FILES {
			let target_id = test_file_id(instance, target_kind)?;
			instance += 1;
			let filename = crate::storage::store::file_layout::graph_filename(target_kind)
				.ok_or_else(|| invalid_catalog("test target filename is absent"))?;
			files.push(FileCatalogEntry {
				graph_id,
				file_id: target_id,
				companion_of: None,
				role: DataFileRole::WalBackfilled,
				path: CatalogPath::parse(&format!("{generation}/{filename}"))?,
			});
			files.push(FileCatalogEntry {
				graph_id,
				file_id: test_file_id(instance, DataFileKind::Freelist)?,
				companion_of: Some(target_id),
				role: DataFileRole::WalBackfilled,
				path: CatalogPath::parse(&format!("{generation}/{freelist_name}"))?,
			});
			instance += 1;
		}
		files.sort_unstable_by_key(|file| (file.graph_id, file.file_id));
		Ok(FileCatalog {
			next_file_instance_id: instance,
			files,
		})
	}

	fn test_file_id(instance: u64, kind: DataFileKind) -> FerrosGrynnResult<DataFileId> {
		DataFileId::new(instance, kind)
			.ok_or_else(|| invalid_catalog("test file identity exceeds the format domain"))
	}

	/// Decode validates reserved bytes after checksum validation.
	#[test]
	fn catalog_page_rejects_non_zero_reserved_bytes() -> FerrosGrynnResult<()> {
		let files = FileCatalog::bootstrap()?;
		let mut pages = encode_page_chain(
			CatalogKind::Files,
			PageNo::new(0),
			PageNo::new(1),
			&files.encode()?,
			PageLsn::ZERO,
		)?;
		pages[0].1.as_bytes_mut()[CATALOG_RESERVED_B_OFFSET] = 1;
		pages[0].1.seal()?;
		let directory = TestDirectory::new("catalog-reserved")?;
		let path = directory.path().join("catalog.fgb");
		let file = DirectFile::create_new(path)?;
		let pages = pages.into_iter().map(|(_page_no, page)| page).collect::<Vec<_>>();
		file.write_pages(PageNo::new(0), &pages)?;
		let mut read_page =
			|page_no, page: &mut AlignedPage| file.read_page(page_no, page).map(|_header| ());
		assert!(decode_page_chain(&mut read_page, PageNo::new(0), CatalogKind::Files).is_err());
		Ok(())
	}

	/// Retaining both over-aligned roots in their page-chain vectors preserves
	/// complete headers and checksums across the update boundary.
	#[test]
	fn catalog_update_root_container_preserves_aligned_pages() -> FerrosGrynnResult<()> {
		let mut database = DatabaseCatalog::empty();
		database.property_keys.push("name".to_owned());
		let files = FileCatalog::bootstrap()?;
		let update = encode_catalog_update(
			&database,
			&files,
			PageNo::new(0),
			PageNo::new(1),
			PageNo::new(2),
		)?;
		for pages in [&update.database_pages, &update.file_pages] {
			let (page_no, page) = pages
				.first()
				.ok_or_else(|| invalid_catalog("catalog update test root is absent"))?;
			assert_eq!(page.validate(*page_no)?.page_type(), PageType::SchemaRoot);
		}
		Ok(())
	}

	#[test]
	fn nested_catalog_reserves_local_slot_and_rejects_root_wide_collisions() -> FerrosGrynnResult<()>
	{
		let root = nested_graph_entry(1, None, None)?;
		let slot_zero = DatabaseCatalog {
			next_graph_id: 3,
			next_schema_id: 4,
			graphs: vec![root.clone(), nested_graph_entry(2, Some(1), Some(0))?],
			property_keys: Vec::new(),
			tables: Vec::new(),
			parameters: Vec::new(),
		};
		assert!(slot_zero.encode().is_err());

		let duplicate = DatabaseCatalog {
			next_graph_id: 4,
			next_schema_id: 4,
			graphs: vec![
				root,
				nested_graph_entry(2, Some(1), Some(1))?,
				nested_graph_entry(3, Some(2), Some(1))?,
			],
			property_keys: Vec::new(),
			tables: Vec::new(),
			parameters: Vec::new(),
		};
		assert!(duplicate.encode().is_err());
		Ok(())
	}

	fn nested_graph_entry(
		graph_id: u64,
		parent_graph_id: Option<u64>,
		subgraph_slot: Option<u32>,
	) -> FerrosGrynnResult<GraphCatalogEntry> {
		Ok(GraphCatalogEntry {
			graph_id: GraphId::new(graph_id),
			schema_id: if parent_graph_id.is_some() {
				0
			} else {
				1
			},
			schema_version: if parent_graph_id.is_some() {
				0
			} else {
				1
			},
			label_index: if parent_graph_id.is_some() {
				None
			} else {
				Some(SystemIndexCatalogEntry {
					index_id: IndexId::from_storage(2)?,
					schema_version: 1,
				})
			},
			adjacency_index: if parent_graph_id.is_some() {
				None
			} else {
				Some(SystemIndexCatalogEntry {
					index_id: IndexId::from_storage(3)?,
					schema_version: 1,
				})
			},
			parent_graph_id: parent_graph_id.map(GraphId::new),
			subgraph_slot,
			parent_atom: parent_graph_id
				.map(|_| {
					crate::values::storage::DiskAtomRef::from_parts(
						crate::values::runtime::graph::AtomKind::Node,
						0,
					)
				})
				.transpose()?,
			record_id: format!("graph:{graph_id}"),
			name: format!("graph-{graph_id}"),
			kind: GraphKind::Graph,
			created_lsn: Lsn::new(1),
			identity: [0; 16],
			revision: 0,
			primary_index_root: PageNo::new(1),
			primary_index_file_id: DataFileId::for_kind(DataFileKind::Index),
			properties: None,
		})
	}
}
