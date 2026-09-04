//! Fixed-size aligned pages and the format-v1 page header codec.

use bitflags::bitflags;
use rapidhash::v3::{DEFAULT_RAPID_SECRETS, RapidStreamHasherV3, rapidhash_v3};

use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Size of every data and index page, in bytes.
pub const PAGE_SIZE: usize = 16_384;

/// Size of the header shared by every page, in bytes.
pub const PAGE_HEADER_SIZE: usize = 32;

/// Current provisional disk format version.
pub(super) const STORAGE_FORMAT_VERSION: u8 = 1;

/// Byte offset of the page magic.
const MAGIC_OFFSET: usize = 0;

/// Byte offset of the page type.
const PAGE_TYPE_OFFSET: usize = MAGIC_OFFSET + size_of::<u32>();

/// Byte offset of the page flags.
const FLAGS_OFFSET: usize = PAGE_TYPE_OFFSET + size_of::<u16>();

/// Byte offset of the page LSN.
const PAGE_LSN_OFFSET: usize = FLAGS_OFFSET + size_of::<u16>();

/// Byte offset of the checksum field.
const CHECKSUM_OFFSET: usize = PAGE_LSN_OFFSET + size_of::<u64>();

/// Byte offset of the physical page number.
pub(super) const PAGE_NO_OFFSET: usize = CHECKSUM_OFFSET + size_of::<u64>();

/// Exclusive end of the common page header.
const PAGE_HEADER_END: usize = PAGE_NO_OFFSET + size_of::<u64>();

/// A validated four-byte page-family marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(super) struct PageMagic(u32);

impl PageMagic {
	/// Adjacency overflow marker (`b"FAO\x01"`).
	pub(super) const ADJACENCY_OVERFLOW: Self = Self(0x01_4f_41_46);
	/// Changefeed marker (`b"FCF\x01"`).
	pub(super) const CHANGEFEED: Self = Self(0x01_46_43_46);
	/// Cross-level adjacency overflow marker (`b"FAC\x01"`).
	pub(super) const CROSS_LEVEL_ADJACENCY_OVERFLOW: Self = Self(0x01_43_41_46);
	/// Edge hot-page marker (`b"FHE\x01"`).
	pub(super) const EDGE_HOT: Self = Self(0x01_45_48_46);
	/// Freelist marker (`b"FFL\x01"`).
	pub(super) const FREELIST: Self = Self(0x01_4c_46_46);
	/// B+tree-index marker (`b"FIB\x01"`).
	pub(super) const INDEX_BTREE: Self = Self(0x01_42_49_46);
	/// Count-index marker (`b"FIC\x01"`).
	pub(super) const INDEX_COUNT: Self = Self(0x01_43_49_46);
	/// Edge-endpoint-index marker (`b"FIE\x01"`).
	pub(super) const INDEX_EDGE_ENDPOINT: Self = Self(0x01_45_49_46);
	/// Full-text-index marker (`b"FIT\x01"`).
	pub(super) const INDEX_FULLTEXT: Self = Self(0x01_54_49_46);
	/// Hash-index marker (`b"FIH\x01"`).
	pub(super) const INDEX_HASH: Self = Self(0x01_48_49_46);
	/// Label-index marker (`b"FIL\x01"`).
	pub(super) const INDEX_LABEL: Self = Self(0x01_4c_49_46);
	/// Neighbourhood-index marker (`b"FIN\x01"`).
	pub(super) const INDEX_NEIGHBOURHOOD: Self = Self(0x01_4e_49_46);
	/// Path-index marker (`b"FIP\x01"`).
	pub(super) const INDEX_PATH: Self = Self(0x01_50_49_46);
	/// Reachability-index marker (`b"FIA\x01"`).
	pub(super) const INDEX_REACHABILITY: Self = Self(0x01_41_49_46);
	/// Geometry R-tree marker (`b"FIR\x01"`).
	pub(super) const INDEX_RTREE: Self = Self(0x01_52_49_46);
	/// Vector-index marker (`b"FIV\x01"`).
	pub(super) const INDEX_VECTOR: Self = Self(0x01_56_49_46);
	/// Label dictionary marker (`b"FLD\x01"`).
	pub(super) const LABEL_DICTIONARY: Self = Self(0x01_44_4c_46);
	/// Node hot-page marker (`b"FHN\x01"`).
	pub(super) const NODE_HOT: Self = Self(0x01_4e_48_46);
	/// Property heap marker (`b"FHP\x01"`).
	pub(super) const PROPERTY_HEAP: Self = Self(0x01_50_48_46);
	/// Schema catalog marker (`b"FCH\x01"`).
	pub(super) const SCHEMA: Self = Self(0x01_48_43_46);
	/// Statistics marker (`b"FST\x01"`).
	pub(super) const STATISTICS: Self = Self(0x01_54_53_46);
	/// Subgraph directory marker (`b"FSG\x01"`).
	pub(super) const SUBGRAPH_DIRECTORY: Self = Self(0x01_47_53_46);
	/// Storage, namespace, database, and graph superblock marker
	/// (`b"FSB\x01"`).
	pub(super) const SUPERBLOCK: Self = Self(0x01_42_53_46);
	/// Tensor-cache marker (`b"FTC\x01"`).
	pub(super) const TENSOR: Self = Self(0x01_43_54_46);

	/// Returns the format-defined raw little-endian value.
	pub(super) const fn get(self) -> u32 {
		self.0
	}

	/// Returns the format version encoded in the most-significant byte.
	pub(super) const fn format_version(self) -> u8 {
		(self.0 >> (u32::BITS - u8::BITS)) as u8
	}
}

/// Declares the closed format-v1 page-type set and its required magic.
macro_rules! define_page_types {
	(
		$(
			$(#[$metadata:meta])*
			$variant:ident = $value:literal => $magic:ident;
		)+
	) => {
		/// A validated format-v1 page type.
		#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
		#[repr(u16)]
		pub enum PageType {
			$(
				$(#[$metadata])*
				$variant = $value,
			)+
		}

		impl PageType {
			/// Returns the format-defined raw little-endian value.
			pub(super) const fn get(self) -> u16 {
				self as u16
			}

			/// Decodes only page types assigned by format v1.
			pub(super) const fn from_raw(value: u16) -> Option<Self> {
				match value {
					$($value => Some(Self::$variant),)+
					_ => None,
				}
			}

			/// Returns the only valid page-family magic for this page type.
			const fn magic(self) -> PageMagic {
				match self {
					$(Self::$variant => PageMagic::$magic,)+
				}
			}

			/// All page types assigned by format v1.
			#[cfg(test)]
			const ALL: &'static [Self] = &[$(Self::$variant,)+];
		}
	};
}

define_page_types! {
	/// Global storage superblock.
	StorageSuperblock = 0x0001 => SUPERBLOCK;
	/// Namespace superblock.
	NamespaceSuperblock = 0x0002 => SUPERBLOCK;
	/// Database superblock.
	DatabaseSuperblock = 0x0003 => SUPERBLOCK;
	/// Structure-of-arrays node hot page.
	NodeHot = 0x0004 => NODE_HOT;
	/// Structure-of-arrays edge hot page.
	EdgeHot = 0x0005 => EDGE_HOT;
	/// Same-level adjacency overflow page.
	AdjacencyOverflow = 0x0006 => ADJACENCY_OVERFLOW;
	/// Cross-level adjacency overflow page.
	CrossLevelAdjacencyOverflow = 0x0007 => CROSS_LEVEL_ADJACENCY_OVERFLOW;
	/// Slotted property heap page.
	PropHeap = 0x000a => PROPERTY_HEAP;
	/// Large-property heap page.
	PropLargeHeap = 0x000b => PROPERTY_HEAP;
	/// Subgraph directory page.
	SubgraphDir = 0x000c => SUBGRAPH_DIRECTORY;
	/// Page freelist.
	Freelist = 0x000d => FREELIST;
	/// Label dictionary root page.
	LabelDict = 0x000e => LABEL_DICTIONARY;
	/// Label dictionary entry page.
	LabelDictEntry = 0x000f => LABEL_DICTIONARY;
	/// Hash-index page.
	HashIndex = 0x0010 => INDEX_HASH;
	/// Graph superblock.
	GraphSuperblock = 0x0011 => SUPERBLOCK;
	/// Schema catalog root page.
	SchemaRoot = 0x0012 => SCHEMA;
	/// Statistics catalog root page.
	StatisticsRoot = 0x0013 => STATISTICS;
	/// Per-label statistics page.
	LabelStatistic = 0x0014 => STATISTICS;
	/// Property histogram page.
	PropertyHistogram = 0x0015 => STATISTICS;
	/// Degree histogram page.
	DegreeHistogram = 0x0016 => STATISTICS;
	/// Internal B+tree page.
	IndexBtreeInternal = 0x0017 => INDEX_BTREE;
	/// Non-unique B+tree leaf page.
	IndexNonUniqueBtreeLeaf = 0x0018 => INDEX_BTREE;
	/// Unique B+tree leaf page.
	IndexUniqueBtreeLeaf = 0x0019 => INDEX_BTREE;
	/// Posting-list B+tree leaf page.
	IndexPostingBtreeLeaf = 0x001a => INDEX_BTREE;
	/// Sorted label-index array page.
	IndexLabelArray = 0x001b => INDEX_LABEL;
	/// Full-text index header page.
	IndexFulltextHeader = 0x001c => INDEX_FULLTEXT;
	/// Full-text dictionary page.
	IndexFulltextDict = 0x001d => INDEX_FULLTEXT;
	/// Full-text hash page.
	IndexFulltextHash = 0x001e => INDEX_FULLTEXT;
	/// Full-text posting page.
	IndexFulltextPosting = 0x001f => INDEX_FULLTEXT;
	/// Vector-index header page.
	IndexVector = 0x0020 => INDEX_VECTOR;
	/// Vector-index data page.
	IndexVectorData = 0x0021 => INDEX_VECTOR;
	/// Geometry R-tree header page.
	IndexGeometryRtreeHeader = 0x0022 => INDEX_RTREE;
	/// Geometry R-tree internal page.
	IndexGeometryRtreeInternal = 0x0023 => INDEX_RTREE;
	/// Geometry R-tree leaf page.
	IndexGeometryRtreeLeaf = 0x0024 => INDEX_RTREE;
	/// Single-table count-index page.
	IndexCountSingle = 0x0025 => INDEX_COUNT;
	/// Reachability-index header page.
	IndexReachabilityHeader = 0x0026 => INDEX_REACHABILITY;
	/// Reachability tree-cover page.
	IndexReachabilityTreeCover = 0x0027 => INDEX_REACHABILITY;
	/// GRAIL reachability page.
	IndexReachabilityGrail = 0x0028 => INDEX_REACHABILITY;
	/// Ferrari reachability page.
	IndexReachabilityFerrari = 0x0029 => INDEX_REACHABILITY;
	/// BFL reachability page.
	IndexReachabilityBfl = 0x002a => INDEX_REACHABILITY;
	/// Reachability landmark-list page.
	IndexReachabilityLandmarkList = 0x002b => INDEX_REACHABILITY;
	/// Neighbourhood-index header page.
	IndexNeighbourhoodHeader = 0x002c => INDEX_NEIGHBOURHOOD;
	/// Exact-neighbourhood page.
	IndexNeighbourhoodExact = 0x002d => INDEX_NEIGHBOURHOOD;
	/// Neighbourhood sketch page.
	IndexNeighbourhoodSketch = 0x002e => INDEX_NEIGHBOURHOOD;
	/// Neighbourhood landmark-list page.
	IndexNeighbourhoodLandmarkList = 0x002f => INDEX_NEIGHBOURHOOD;
	/// Path-index header page.
	IndexPathHeader = 0x0030 => INDEX_PATH;
	/// Path-index landmark-list page.
	IndexPathLandmarkList = 0x0031 => INDEX_PATH;
	/// Path-pattern catalog page.
	IndexPathPatternCatalog = 0x0032 => INDEX_PATH;
	/// Edge-endpoint-index header page.
	IndexEdgeEndpointHeader = 0x0033 => INDEX_EDGE_ENDPOINT;
	/// Edge-endpoint-index entry page.
	IndexEdgeEndpointEntry = 0x0034 => INDEX_EDGE_ENDPOINT;
	/// Changefeed header page.
	ChangefeedHeader = 0x0035 => CHANGEFEED;
	/// Changefeed entry page.
	ChangefeedEntry = 0x0036 => CHANGEFEED;
	/// Tensor-cache page.
	TensorCache = 0x0037 => TENSOR;
}

bitflags! {
	/// Valid format-v1 flags persisted in the common page header.
	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
	pub struct PageFlags: u16 {
		/// Page is dirty in the runtime buffer pool.
		const DIRTY = 0x0001;
		/// Page payload is compressed.
		const COMPRESSED = 0x0002;
		/// Page continues an overflow chain.
		const OVERFLOW = 0x0004;
		/// Page is a B+tree leaf.
		const LEAF = 0x0008;
		/// Page belongs to the schema catalog.
		const SCHEMA = 0x0010;
		/// Page belongs to statistics storage.
		const STATISTICS = 0x0020;
	}
}

/// The WAL position whose image is stored in a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PageLsn(u64);

impl PageLsn {
	/// LSN used for a page that has not yet been written by WAL replay.
	pub(super) const ZERO: Self = Self(0);

	/// Creates a page LSN from the storage engine's WAL position.
	pub const fn new(value: u64) -> Self {
		Self(value)
	}

	/// Returns the persisted integer value.
	pub const fn get(self) -> u64 {
		self.0
	}
}

/// A physical page number within one data or index file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PageNo(u64);

impl PageNo {
	/// Sentinel used by disk structures to denote the absence of a page.
	pub(super) const NULL: Self = Self(u64::MAX);

	/// Creates a physical page number.
	pub const fn new(value: u64) -> Self {
		Self(value)
	}

	/// Returns the persisted integer value.
	pub const fn get(self) -> u64 {
		self.0
	}
}

/// A zero-initialized page whose address satisfies Direct I/O alignment.
#[repr(C, align(16384))]
pub struct AlignedPage {
	/// Complete page bytes, including the header.
	bytes: [u8; PAGE_SIZE],
}

impl AlignedPage {
	/// Creates a zeroed page with a valid typed and sealed header.
	pub fn new(page_type: PageType, flags: PageFlags, page_lsn: PageLsn, page_no: PageNo) -> Self {
		let mut page = Self::zeroed();
		page.reset(page_type, flags, page_lsn, page_no);
		page
	}

	/// Reinitializes reusable aligned storage as a zero-filled sealed page.
	pub(super) fn reset(
		&mut self,
		page_type: PageType,
		flags: PageFlags,
		page_lsn: PageLsn,
		page_no: PageNo,
	) {
		self.bytes.fill(0);
		let mut header = PageHeader::new(page_type, flags, page_lsn, page_no);
		header.write_to(self);
		header.checksum = page_checksum_mut(self.as_bytes_mut());
		header.write_to(self);
	}

	/// Creates an entirely zeroed page for an exact read operation.
	pub const fn zeroed() -> Self {
		Self {
			bytes: [0; PAGE_SIZE],
		}
	}

	/// Returns the complete immutable page bytes.
	pub const fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
		&self.bytes
	}

	/// Returns the complete mutable page bytes.
	///
	/// Call [`Self::seal`] after changing persisted bytes.
	pub const fn as_bytes_mut(&mut self) -> &mut [u8; PAGE_SIZE] {
		&mut self.bytes
	}

	/// Views a contiguous slice of aligned pages as one immutable byte extent.
	pub fn slice_as_bytes(pages: &[Self]) -> &[u8] {
		let byte_len = size_of_val(pages);
		let bytes = pages.as_ptr().cast::<u8>();
		// SAFETY: the same layout proof as in `slice_as_bytes_mut` applies;
		// this conversion only exposes the initialized allocation immutably.
		unsafe { std::slice::from_raw_parts(bytes, byte_len) }
	}

	/// Views a contiguous slice of aligned pages as one mutable byte extent.
	///
	/// This is the only raw-memory conversion needed by batched Direct I/O.
	/// The returned slice has the same lifetime and exclusive borrow as
	/// `pages`.
	pub fn slice_as_bytes_mut(pages: &mut [Self]) -> &mut [u8] {
		let byte_len = size_of_val(pages);
		let bytes = pages.as_mut_ptr().cast::<u8>();
		// SAFETY: a slice stores `AlignedPage` values contiguously; the const
		// assertions below prove that every element is exactly `PAGE_SIZE`
		// initialized bytes with no trailing inter-element padding. `u8`
		// accepts every bit pattern, `bytes` inherits the slice allocation
		// and alignment, and the exclusive output borrow cannot outlive the
		// exclusive input borrow.
		unsafe { std::slice::from_raw_parts_mut(bytes, byte_len) }
	}

	/// Revalidates the typed header and writes a new format-v1 checksum.
	///
	/// # Errors
	///
	/// Returns an error when raw header bytes contain an unsupported format,
	/// unknown page type, mismatched magic, or reserved flag bit.
	pub fn seal(&mut self) -> FerrosGrynnResult<()> {
		let raw_header = RawPageHeader::decode(self.as_bytes());
		let mut header = raw_header.validate(PageNo::new(raw_header.page_no))?;
		header.checksum = page_checksum_mut(self.as_bytes_mut());
		header.write_to(self);
		Ok(())
	}

	/// Decodes and validates the complete page.
	///
	/// # Errors
	///
	/// Returns an error for an unsupported format version, unknown page type,
	/// mismatched page-family magic, non-zero reserved flag bits, a mismatched
	/// page number, or a checksum mismatch.
	pub fn validate(&self, expected_page_no: PageNo) -> FerrosGrynnResult<PageHeader> {
		validate_page_bytes(self.as_bytes(), expected_page_no)
	}

	/// Copies a validated page, assigns its frame LSN, and reseals it.
	///
	/// WAL is the only caller allowed to assign the final page LSN. Keeping the
	/// operation here prevents the persisted header and checksum from drifting
	/// apart.
	///
	/// # Errors
	///
	/// Returns an error if the source page is invalid for `expected_page_no`.
	#[cfg(test)]
	pub(super) fn copy_with_lsn(
		&self,
		expected_page_no: PageNo,
		page_lsn: PageLsn,
	) -> FerrosGrynnResult<Self> {
		let mut page = Self {
			bytes: self.bytes,
		};
		page.assign_lsn(expected_page_no, page_lsn)?;
		Ok(page)
	}

	/// Assigns the WAL frame LSN and reseals reusable exclusive page storage.
	///
	/// # Errors
	///
	/// Returns an error if the current page is invalid for `expected_page_no`.
	pub(super) fn assign_lsn(
		&mut self,
		expected_page_no: PageNo,
		page_lsn: PageLsn,
	) -> FerrosGrynnResult<()> {
		let mut header = self.validate(expected_page_no)?;
		header.page_lsn = page_lsn;
		header.checksum = 0;
		header.write_to(self);
		header.checksum = page_checksum_mut(self.as_bytes_mut());
		header.write_to(self);
		Ok(())
	}
}

/// Validates one complete page image without imposing Direct-I/O alignment.
pub fn validate_page_bytes(
	bytes: &[u8; PAGE_SIZE],
	expected_page_no: PageNo,
) -> FerrosGrynnResult<PageHeader> {
	let header = RawPageHeader::decode(bytes).validate(expected_page_no)?;
	let expected_checksum = page_checksum(bytes);
	validate_checksum(header, expected_checksum, expected_page_no)
}

/// Compares one decoded checksum against its complete page image.
fn validate_checksum(
	header: PageHeader,
	expected_checksum: u64,
	expected_page_no: PageNo,
) -> FerrosGrynnResult<PageHeader> {
	if header.checksum != expected_checksum {
		return Err(FerrosGrynnError::invalid_storage_page(
			expected_page_no.get(),
			format!(
				"checksum mismatch: stored {:#018x}, computed {expected_checksum:#018x}",
				header.checksum
			),
		));
	}
	Ok(header)
}

/// The validated common header of a format-v1 page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageHeader {
	/// Validated page-family magic.
	magic: PageMagic,
	/// Assigned format-v1 page type.
	page_type: PageType,
	/// Persisted flags with no unknown bits.
	flags: PageFlags,
	/// WAL position of the page image.
	page_lsn: PageLsn,
	/// RapidHash V3 checksum of the complete page with this field zeroed.
	checksum: u64,
	/// Physical page number.
	page_no: PageNo,
}

impl PageHeader {
	/// Creates a valid header; the page-family magic is derived from its type.
	const fn new(
		page_type: PageType,
		flags: PageFlags,
		page_lsn: PageLsn,
		page_no: PageNo,
	) -> Self {
		Self {
			magic: page_type.magic(),
			page_type,
			flags,
			page_lsn,
			checksum: 0,
			page_no,
		}
	}

	/// Returns the validated page-family magic.
	pub(super) const fn magic(self) -> PageMagic {
		self.magic
	}

	/// Returns the validated page type.
	pub(super) const fn page_type(self) -> PageType {
		self.page_type
	}

	/// Returns the validated persisted flags.
	pub(super) const fn flags(self) -> PageFlags {
		self.flags
	}

	/// Returns the frame LSN persisted in the page.
	pub const fn page_lsn(self) -> PageLsn {
		self.page_lsn
	}

	/// Returns the stored page checksum.
	pub const fn checksum(self) -> u64 {
		self.checksum
	}

	/// Returns the physical page number.
	pub const fn page_no(self) -> PageNo {
		self.page_no
	}

	/// Writes every header field using the normative little-endian layout.
	fn write_to(self, page: &mut AlignedPage) {
		let bytes = page.as_bytes_mut();
		bytes[MAGIC_OFFSET..PAGE_TYPE_OFFSET].copy_from_slice(&self.magic.get().to_le_bytes());
		bytes[PAGE_TYPE_OFFSET..FLAGS_OFFSET].copy_from_slice(&self.page_type.get().to_le_bytes());
		bytes[FLAGS_OFFSET..PAGE_LSN_OFFSET].copy_from_slice(&self.flags.bits().to_le_bytes());
		bytes[PAGE_LSN_OFFSET..CHECKSUM_OFFSET].copy_from_slice(&self.page_lsn.get().to_le_bytes());
		bytes[CHECKSUM_OFFSET..PAGE_NO_OFFSET].copy_from_slice(&self.checksum.to_le_bytes());
		bytes[PAGE_NO_OFFSET..PAGE_HEADER_END].copy_from_slice(&self.page_no.get().to_le_bytes());
	}
}

/// Untrusted integer fields decoded directly from page bytes.
#[derive(Debug, Clone, Copy)]
struct RawPageHeader {
	/// Unvalidated page-family marker.
	magic: u32,
	/// Unvalidated page-type discriminant.
	page_type: u16,
	/// Unvalidated persisted flag bits.
	flags: u16,
	/// Raw persisted page LSN.
	page_lsn: u64,
	/// Raw persisted checksum.
	checksum: u64,
	/// Raw physical page number.
	page_no: u64,
}

impl RawPageHeader {
	/// Decodes fixed offsets without alignment-dependent casts.
	fn decode(bytes: &[u8; PAGE_SIZE]) -> Self {
		Self {
			magic: read_u32_le(bytes, MAGIC_OFFSET),
			page_type: read_u16_le(bytes, PAGE_TYPE_OFFSET),
			flags: read_u16_le(bytes, FLAGS_OFFSET),
			page_lsn: read_u64_le(bytes, PAGE_LSN_OFFSET),
			checksum: read_u64_le(bytes, CHECKSUM_OFFSET),
			page_no: read_u64_le(bytes, PAGE_NO_OFFSET),
		}
	}

	/// Converts raw fields into the validated typed header boundary.
	fn validate(self, expected_page_no: PageNo) -> FerrosGrynnResult<PageHeader> {
		let format_version = (self.magic >> (u32::BITS - u8::BITS)) as u8;
		if format_version != STORAGE_FORMAT_VERSION {
			return Err(FerrosGrynnError::unsupported_storage_format(
				u64::from(format_version),
				u64::from(STORAGE_FORMAT_VERSION),
			));
		}

		let page_type = PageType::from_raw(self.page_type).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_page(
				expected_page_no.get(),
				format!("unknown or reserved page type {:#06x}", self.page_type),
			)
		})?;
		let magic = page_type.magic();
		if self.magic != magic.get() {
			return Err(FerrosGrynnError::invalid_storage_page(
				expected_page_no.get(),
				format!("page magic {:#010x} does not match type {:?}", self.magic, page_type),
			));
		}

		let flags = PageFlags::from_bits(self.flags).ok_or_else(|| {
			let reserved_flags = self.flags & !PageFlags::all().bits();
			FerrosGrynnError::invalid_storage_page(
				expected_page_no.get(),
				format!("reserved page flag bits are non-zero: {reserved_flags:#06x}"),
			)
		})?;
		let page_no = PageNo::new(self.page_no);
		if page_no != expected_page_no {
			return Err(FerrosGrynnError::invalid_storage_page(
				expected_page_no.get(),
				format!("header contains page number {}", page_no.get()),
			));
		}

		Ok(PageHeader {
			magic,
			page_type,
			flags,
			page_lsn: PageLsn::new(self.page_lsn),
			checksum: self.checksum,
			page_no,
		})
	}
}

/// Computes the portable format-v1 checksum through the optimized bulk path.
fn page_checksum(bytes: &[u8; PAGE_SIZE]) -> u64 {
	const ZERO_CHECKSUM: [u8; size_of::<u64>()] = [0; size_of::<u64>()];

	let mut hasher = RapidStreamHasherV3::new(&DEFAULT_RAPID_SECRETS);
	hasher.write(&bytes[..CHECKSUM_OFFSET]);
	hasher.write(&ZERO_CHECKSUM);
	hasher.write(&bytes[PAGE_NO_OFFSET..]);
	hasher.finish()
}

/// Computes the checksum while preparing a page for a new sealed header.
fn page_checksum_mut(bytes: &mut [u8; PAGE_SIZE]) -> u64 {
	bytes[CHECKSUM_OFFSET..PAGE_NO_OFFSET].fill(0);
	rapidhash_v3(bytes)
}

/// Reads one little-endian `u16` from a format-defined fixed offset.
fn read_u16_le(bytes: &[u8; PAGE_SIZE], offset: usize) -> u16 {
	let mut encoded = [0; size_of::<u16>()];
	encoded.copy_from_slice(&bytes[offset..offset + size_of::<u16>()]);
	u16::from_le_bytes(encoded)
}

/// Reads one little-endian `u32` from a format-defined fixed offset.
fn read_u32_le(bytes: &[u8; PAGE_SIZE], offset: usize) -> u32 {
	let mut encoded = [0; size_of::<u32>()];
	encoded.copy_from_slice(&bytes[offset..offset + size_of::<u32>()]);
	u32::from_le_bytes(encoded)
}

/// Reads one little-endian `u64` from a format-defined fixed offset.
fn read_u64_le(bytes: &[u8; PAGE_SIZE], offset: usize) -> u64 {
	let mut encoded = [0; size_of::<u64>()];
	encoded.copy_from_slice(&bytes[offset..offset + size_of::<u64>()]);
	u64::from_le_bytes(encoded)
}

const _: () = {
	assert!(PAGE_HEADER_END == PAGE_HEADER_SIZE);
	assert!(size_of::<PageMagic>() == size_of::<u32>());
	assert!(size_of::<PageType>() == size_of::<u16>());
	assert!(size_of::<PageFlags>() == size_of::<u16>());
	assert!(size_of::<PageLsn>() == size_of::<u64>());
	assert!(size_of::<PageNo>() == size_of::<u64>());
	assert!(size_of::<AlignedPage>() == PAGE_SIZE);
	assert!(align_of::<AlignedPage>() == PAGE_SIZE);
};

#[cfg(test)]
#[allow(
	clippy::panic_in_result_fn,
	reason = "fallible test setup returns Result while assertions report contract failures"
)]
mod tests {
	use super::*;

	#[test]
	fn aligned_page_has_exact_size_and_alignment() {
		let page = AlignedPage::zeroed();
		assert_eq!(size_of_val(&page), PAGE_SIZE);
		assert_eq!((page.as_bytes().as_ptr() as usize) % PAGE_SIZE, 0);
	}

	#[test]
	fn aligned_page_slice_is_one_contiguous_byte_extent() {
		let mut pages = (0..3).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		let start = pages.as_ptr() as usize;
		assert_eq!(start % PAGE_SIZE, 0);

		let bytes = AlignedPage::slice_as_bytes_mut(&mut pages);
		assert_eq!(bytes.len(), 3 * PAGE_SIZE);
		bytes[PAGE_SIZE] = 7;
		assert_eq!(pages[1].as_bytes()[0], 7);
		assert_eq!(AlignedPage::slice_as_bytes(&pages)[PAGE_SIZE], 7);
	}

	#[test]
	fn all_page_types_round_trip_their_typed_values() {
		for page_type in PageType::ALL {
			assert_eq!(PageType::from_raw(page_type.get()), Some(*page_type));
			assert_eq!(page_type.magic().format_version(), STORAGE_FORMAT_VERSION);
		}
		assert_eq!(PageType::from_raw(0x0008), None);
		assert_eq!(PageType::from_raw(0x0009), None);
		assert_eq!(PageType::from_raw(0x0038), None);
	}

	#[test]
	fn page_codec_round_trips_little_endian_header() -> FerrosGrynnResult<()> {
		let page =
			AlignedPage::new(PageType::NodeHot, PageFlags::LEAF, PageLsn::new(42), PageNo::new(7));
		let header = page.validate(PageNo::new(7))?;

		assert_eq!(header.magic(), PageMagic::NODE_HOT);
		assert_eq!(header.page_type(), PageType::NodeHot);
		assert_eq!(header.flags(), PageFlags::LEAF);
		assert_eq!(header.page_lsn(), PageLsn::new(42));
		assert_ne!(header.checksum(), 0);
		assert_eq!(header.page_no(), PageNo::new(7));
		Ok(())
	}

	#[test]
	fn page_checksum_rejects_payload_corruption() -> FerrosGrynnResult<()> {
		let mut page = AlignedPage::new(
			PageType::NodeHot,
			PageFlags::empty(),
			PageLsn::new(1),
			PageNo::new(0),
		);
		page.as_bytes_mut()[PAGE_HEADER_SIZE] = 0xff;

		let error = page
			.validate(PageNo::new(0))
			.err()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_page(0, "corruption was accepted"))?;
		assert_eq!(error.status().code().to_string(), "FG-22S08");
		Ok(())
	}

	#[test]
	fn page_codec_rejects_wrong_page_number_and_invalid_raw_fields() {
		let page = AlignedPage::new(
			PageType::NodeHot,
			PageFlags::empty(),
			PageLsn::new(1),
			PageNo::new(3),
		);
		assert_eq!(
			page.validate(PageNo::new(4)).err().map(|error| error.status().code().to_string()),
			Some("FG-22S08".to_owned())
		);

		let mut reserved_flags = AlignedPage::new(
			PageType::NodeHot,
			PageFlags::empty(),
			PageLsn::new(1),
			PageNo::new(3),
		);
		reserved_flags.as_bytes_mut()[FLAGS_OFFSET..PAGE_LSN_OFFSET]
			.copy_from_slice(&0x8000_u16.to_le_bytes());
		assert_eq!(
			reserved_flags
				.validate(PageNo::new(3))
				.err()
				.map(|error| error.status().code().to_string()),
			Some("FG-22S08".to_owned())
		);

		let mut unknown_type = AlignedPage::new(
			PageType::NodeHot,
			PageFlags::empty(),
			PageLsn::new(1),
			PageNo::new(3),
		);
		unknown_type.as_bytes_mut()[PAGE_TYPE_OFFSET..FLAGS_OFFSET]
			.copy_from_slice(&0x0008_u16.to_le_bytes());
		assert_eq!(
			unknown_type
				.validate(PageNo::new(3))
				.err()
				.map(|error| error.status().code().to_string()),
			Some("FG-22S08".to_owned())
		);

		let mut mismatched_magic = AlignedPage::new(
			PageType::NodeHot,
			PageFlags::empty(),
			PageLsn::new(1),
			PageNo::new(3),
		);
		mismatched_magic.as_bytes_mut()[MAGIC_OFFSET..PAGE_TYPE_OFFSET]
			.copy_from_slice(&PageMagic::EDGE_HOT.get().to_le_bytes());
		assert_eq!(
			mismatched_magic
				.validate(PageNo::new(3))
				.err()
				.map(|error| error.status().code().to_string()),
			Some("FG-22S08".to_owned())
		);

		let mut unsupported_version = AlignedPage::new(
			PageType::NodeHot,
			PageFlags::empty(),
			PageLsn::new(1),
			PageNo::new(3),
		);
		let family_bytes = PageMagic::NODE_HOT.get() & (u32::MAX >> u8::BITS);
		let version_two_magic = family_bytes | (u32::from(2_u8) << (u32::BITS - u8::BITS));
		unsupported_version.as_bytes_mut()[MAGIC_OFFSET..PAGE_TYPE_OFFSET]
			.copy_from_slice(&version_two_magic.to_le_bytes());
		assert_eq!(
			unsupported_version
				.validate(PageNo::new(3))
				.err()
				.map(|error| error.status().code().to_string()),
			Some("FG-51S01".to_owned())
		);
	}

	#[test]
	fn rapid_hash_v3_format_vectors_are_stable() -> FerrosGrynnResult<()> {
		let zero_page = AlignedPage::zeroed();
		assert_eq!(page_checksum(zero_page.as_bytes()), 0x3796_d028_25bf_3d75);

		let page = AlignedPage::new(
			PageType::StorageSuperblock,
			PageFlags::empty(),
			PageLsn::ZERO,
			PageNo::new(0),
		);
		assert_eq!(page.validate(PageNo::new(0))?.checksum(), 0x3fa0_f2d4_2971_e713);
		Ok(())
	}
}
