//! Immutable STR R-tree pages with snapshot-bound logical metadata.

#![allow(
	clippy::redundant_pub_crate,
	reason = "Physical index types retain an explicit crate boundary through storage re-exports"
)]

use std::{cell::Cell, collections::BTreeSet, path::Path};

use rapidhash::v3::rapidhash_v3;

use crate::{
	datastore::{ClassId, GeometryIndexOptions, IndexId, IndexScope, SchemaVersion},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		external_sort_run_budget,
		graph_index::{ExternalIndexSorter, StreamedIndex},
		page::{AlignedPage, PageFlags, PageLsn, PageNo, PageType},
	},
	values::{GeometryKind, PropertyKeyId, storage::DiskAtomRef},
};

/// Physical record width shared by leaf and internal nodes.
const RECORD_BYTES: usize = 40;
/// Both page kinds fit 408 records; builders apply the fixed 0.95 fill policy.
const CAPACITY: usize = 408;
/// Floor of 408 * 0.95, preserving a reproducible writer policy.
const FILL: usize = 387;
/// Semantic kernel/model/fill identity, independent of build revision.
const KERNEL: u64 = 0x5747_5338_3453_5452;

/// Exact bounded replacement pages.
mod delta;
/// RAM ownership of the shared page format.
mod resident;
#[cfg(test)]
mod tests;
pub(crate) use delta::{
	DecodedGeometryDelta,
	GeometryDeltaRecord,
	build_geometry_delta,
	decode_geometry_delta,
	geometry_delta_records,
};
pub use resident::{ResidentGeometryIndex, build_resident_geometry_indexes};

/// Exact candidates and physical work of one admitted tree walk.
#[derive(Default)]
pub struct GeometryCandidates {
	/// Stable storage addresses, mapped to RecordId at the datastore boundary.
	pub(crate) atoms: Vec<DiskAtomRef>,
	/// Tree node pages actually read, including resident hits.
	pub(crate) page_reads: u64,
	/// Replacement records consulted in this snapshot.
	pub(crate) delta_records: u64,
}

/// Applies exact replacements to a bounded page walk shared by both storage
/// backends.
pub(crate) fn search_geometry_generation(
	header: GeometryHeader,
	delta: &[GeometryDeltaRecord],
	query: [f64; 4],
	admit: &mut dyn FnMut(usize) -> FerrosGrynnResult<()>,
	mut read: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<GeometryCandidates> {
	admit(
		delta
			.len()
			.checked_mul(128)
			.ok_or_else(|| invalid("geometry replacement memory overflows"))?,
	)?;
	let replacements = delta
		.iter()
		.map(|record| (record.atom, record.mbr))
		.collect::<std::collections::BTreeMap<_, _>>();
	let mut candidates = BTreeSet::new();
	let admission = std::cell::RefCell::new(admit);
	let mut page_reads = 0_u64;
	search_geometry(
		header,
		query,
		|bytes| admission.borrow_mut()(bytes),
		|page, output| {
			page_reads += 1;
			read(page, output)
		},
		|atom| {
			if !replacements.contains_key(&atom) {
				admission.borrow_mut()(96)?;
				candidates.insert(atom);
			}
			Ok(())
		},
	)?;
	for (atom, mbr) in replacements {
		if mbr.is_some_and(|mbr| overlaps(mbr, query)) {
			admission.borrow_mut()(96)?;
			candidates.insert(atom);
		}
	}
	Ok(GeometryCandidates {
		atoms: candidates.into_iter().collect(),
		page_reads,
		delta_records: delta.len() as u64,
	})
}

/// Definition and snapshot revision encoded into the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GeometryBuildSpec {
	/// Published index identity.
	pub(crate) index: IndexId,
	/// Definition version.
	pub(crate) index_version: SchemaVersion,
	/// Owning class.
	pub(crate) class: ClassId,
	/// Indexed property.
	pub(crate) field: PropertyKeyId,
	/// BASE or graph-qualified EFFECTIVE scope.
	pub(crate) scope: IndexScope,
	/// Validated geometry family restriction.
	pub(crate) options: GeometryIndexOptions,
	/// MVCC revision represented by the base.
	pub(crate) build_revision: u64,
}

/// Validated header retained without loading the tree into resident memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GeometryHeader {
	/// Definition and snapshot contract.
	pub(crate) spec: GeometryBuildSpec,
	/// Root page, or NULL for an empty base.
	pub(crate) root: PageNo,
	/// Nonempty geometry population.
	pub(crate) row_count: u64,
	/// Total physical pages including the header.
	pub(crate) page_count: u64,
	/// Number of nonempty tree levels.
	pub(crate) height: u16,
}

/// One finite MBR plus stable row or child-page identity.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GeometryEntry {
	/// Inclusive lon/lat bounds.
	pub(crate) mbr: [f64; 4],
	/// Stable row reference in leaves or child page in internal nodes.
	pub(crate) identity: u64,
}

/// Checks coordinate model and ordering at every page/build boundary.
pub(crate) fn validate_mbr(mbr: [f64; 4]) -> FerrosGrynnResult<()> {
	if !mbr.iter().all(|coordinate| coordinate.is_finite())
		|| mbr[0] > mbr[2]
		|| mbr[1] > mbr[3]
		|| ![-180.0..=180.0, -90.0..=90.0, -180.0..=180.0, -90.0..=90.0]
			.into_iter()
			.zip(mbr)
			.all(|(range, value)| range.contains(&value))
	{
		return Err(invalid("geometry MBR is nonfinite, inverted or outside WGS-84"));
	}
	Ok(())
}

/// Builds an immutable base using bounded identity/X/Y sorts and streamed
/// pages.
pub(in crate::storage) fn stream_geometry_index(
	spec: GeometryBuildSpec,
	scratch: &Path,
	feed: impl FnOnce(&mut dyn FnMut(GeometryEntry) -> FerrosGrynnResult<()>) -> FerrosGrynnResult<()>,
	mut write: impl FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<StreamedIndex> {
	// Reserve merge readers, writer/tree pages and controls before retaining
	// payload. Six shares account for three simultaneous buffers and their
	// vector capacity slack.
	let budget = external_sort_run_budget()
		.checked_sub(2 * 1024 * 1024)
		.map(|remaining| remaining / 6)
		.filter(|budget| *budget >= 4096)
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument(
				"STR scratch budget must cover merge pages and three sort buffers (at least 2 MiB \
				 + 24 KiB)",
			)
		})?;
	let mut identities = ExternalIndexSorter::with_budget(scratch, budget);
	let mut row_count = 0_u64;
	feed(&mut |entry| {
		validate_mbr(entry.mbr)?;
		DiskAtomRef::from_raw(entry.identity)?;
		row_count =
			row_count.checked_add(1).ok_or_else(|| invalid("geometry row count overflows u64"))?;
		identities.push_raw(key(&entry, &entry.identity.to_be_bytes()), entry.identity)
	})?;
	let mut x = ExternalIndexSorter::with_budget(scratch, budget);
	let mut previous = None;
	identities.finish()?.visit_raw(|bytes, identity| {
		if previous == Some(identity) {
			return Err(invalid("duplicate geometry row identity"));
		}
		previous = Some(identity);
		let entry = entry_from_key(bytes, identity)?;
		x.push_raw(x_key(entry), identity)
	})?;
	let mut page = Box::new(AlignedPage::zeroed());
	page.reset(
		PageType::IndexGeometryRtreeHeader,
		PageFlags::empty(),
		PageLsn::ZERO,
		PageNo::new(0),
	);
	page.seal()?;
	write(PageNo::new(0), &page)?;
	let next_page = Cell::new(1_u64);
	let mut height = 0_u16;
	let mut population = row_count;
	let mut root = PageNo::NULL;
	while population > 0 {
		let pages = population.div_ceil(FILL as u64);
		let floor = pages.isqrt();
		let stripes = floor + u64::from(floor * floor < pages);
		let stripe_rows = pages
			.div_ceil(stripes)
			.checked_mul(FILL as u64)
			.ok_or_else(|| invalid("STR stripe size overflows"))?;
		let mut y = ExternalIndexSorter::with_budget(scratch, budget);
		let mut ordinal = 0_u64;
		x.finish()?.visit_raw(|bytes, identity| {
			let entry = entry_from_key(bytes, identity)?;
			let mut prefix = (ordinal / stripe_rows).to_be_bytes().to_vec();
			prefix.extend_from_slice(&ordered(entry.mbr[1] + (entry.mbr[3] - entry.mbr[1]) * 0.5));
			prefix.extend_from_slice(&identity.to_be_bytes());
			ordinal += 1;
			y.push_raw(key(&entry, &prefix), identity)
		})?;
		let mut parents = ExternalIndexSorter::with_budget(scratch, budget);
		let mut count = 0_usize;
		let mut bounds = None;
		let mut parent_count = 0_u64;
		let mut stripe = None;
		{
			let mut flush = |page: &mut AlignedPage,
			                 count: &mut usize,
			                 bounds: &mut Option<[f64; 4]>|
			 -> FerrosGrynnResult<()> {
				if *count == 0 {
					return Ok(());
				}
				page.as_bytes_mut()[32..34].copy_from_slice(&(*count as u16).to_le_bytes());
				page.seal()?;
				write(PageNo::new(next_page.get()), page)?;
				let entry = GeometryEntry {
					mbr: bounds.take().ok_or_else(|| invalid("STR node has no MBR"))?,
					identity: next_page.get(),
				};
				parents.push_raw(x_key(entry), entry.identity)?;
				root = PageNo::new(next_page.get());
				next_page.set(
					next_page
						.get()
						.checked_add(1)
						.ok_or_else(|| invalid("STR page count overflows"))?,
				);
				parent_count += 1;
				*count = 0;
				Ok(())
			};
			y.finish()?.visit_raw(|bytes, identity| {
				let current_stripe = number(bytes, 0)?;
				if count == FILL || stripe.is_some_and(|stripe| stripe != current_stripe) {
					flush(&mut page, &mut count, &mut bounds)?;
				}
				stripe = Some(current_stripe);
				if count == 0 {
					page.reset(
						if height == 0 {
							PageType::IndexGeometryRtreeLeaf
						} else {
							PageType::IndexGeometryRtreeInternal
						},
						if height == 0 {
							PageFlags::LEAF
						} else {
							PageFlags::empty()
						},
						PageLsn::ZERO,
						PageNo::new(next_page.get()),
					);
					page.as_bytes_mut()[34..36].copy_from_slice(&height.to_le_bytes());
				}
				let entry = entry_from_key(bytes, identity)?;
				let offset = (if height == 0 {
					40
				} else {
					48
				}) + count * RECORD_BYTES;
				encode_entry(&mut page.as_bytes_mut()[offset..offset + RECORD_BYTES], entry);
				bounds = Some(bounds.map_or(entry.mbr, |previous| union(previous, entry.mbr)));
				count += 1;
				Ok(())
			})?;
			flush(&mut page, &mut count, &mut bounds)?;
		}
		height = height
			.checked_add(1)
			.filter(|height| *height <= 64)
			.ok_or_else(|| invalid("STR height exceeds its limit"))?;
		if parent_count == 1 {
			break;
		}
		population = parent_count;
		x = parents;
	}
	let header = GeometryHeader {
		spec,
		root,
		row_count,
		page_count: next_page.get(),
		height,
	};
	encode_header(&mut page, header)?;
	write(PageNo::new(0), &page)?;
	Ok(StreamedIndex {
		root: PageNo::new(0),
		page_count: next_page.get(),
	})
}

/// Encodes definition and snapshot metadata into the final stable page buffer.
fn encode_header(page: &mut AlignedPage, header: GeometryHeader) -> FerrosGrynnResult<()> {
	page.reset(
		PageType::IndexGeometryRtreeHeader,
		PageFlags::empty(),
		PageLsn::ZERO,
		PageNo::new(0),
	);
	let bytes = page.as_bytes_mut();
	for (offset, value) in [
		(32, header.spec.index.get()),
		(36, header.spec.index_version.get()),
		(40, header.spec.class.get()),
		(44, header.spec.field.get()),
	] {
		bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
	}
	bytes[48..56].copy_from_slice(&header.spec.build_revision.to_le_bytes());
	if let IndexScope::Effective {
		graph,
	} = header.spec.scope
	{
		bytes[64] = 1;
		bytes[56..64].copy_from_slice(&graph.get().to_le_bytes());
	}
	bytes[65] = type_tag(header.spec.options.geometry_type());
	bytes[66..68].copy_from_slice(&1_u16.to_le_bytes());
	bytes[68..70].copy_from_slice(&950_u16.to_le_bytes());
	bytes[72..80].copy_from_slice(&KERNEL.to_le_bytes());
	for (offset, value) in
		[(80, header.root.get()), (88, header.row_count), (96, header.page_count)]
	{
		bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
	}
	bytes[104..106].copy_from_slice(&header.height.to_le_bytes());
	let fingerprint = fingerprint(bytes);
	bytes[112..120].copy_from_slice(&fingerprint.to_le_bytes());
	page.seal()
}

/// Validates the header without decoding any tree nodes.
pub(crate) fn decode_geometry_header(
	page: &AlignedPage,
	page_count: u64,
) -> FerrosGrynnResult<GeometryHeader> {
	let header = page.validate(PageNo::new(0))?;
	let bytes = page.as_bytes();
	if header.page_type() != PageType::IndexGeometryRtreeHeader
		|| !header.flags().is_empty()
		|| short(bytes, 66)? != 1
		|| short(bytes, 68)? != 950
		|| number(bytes, 72)? != KERNEL
		|| number(bytes, 112)? != fingerprint(bytes)
		|| bytes[70..72].iter().chain(&bytes[106..112]).chain(&bytes[120..]).any(|byte| *byte != 0)
	{
		return Err(invalid("invalid R-tree header or definition fingerprint"));
	}
	let scope = match (bytes[64], number(bytes, 56)?) {
		(0, 0) => IndexScope::Base,
		(1, graph) => IndexScope::Effective {
			graph: crate::graph::GraphId::new(graph)
				.ok_or_else(|| invalid("null geometry GraphId"))?,
		},
		_ => return Err(invalid("invalid geometry scope")),
	};
	let spec = GeometryBuildSpec {
		index: IndexId::new(word(bytes, 32)?).ok_or_else(|| invalid("null geometry IndexId"))?,
		index_version: SchemaVersion::new(word(bytes, 36)?)
			.ok_or_else(|| invalid("zero geometry version"))?,
		class: ClassId::new(word(bytes, 40)?).ok_or_else(|| invalid("null geometry ClassId"))?,
		field: PropertyKeyId::new(word(bytes, 44)?),
		scope,
		options: GeometryIndexOptions::new(decode_type(bytes[65])?),
		build_revision: number(bytes, 48)?,
	};
	let root = PageNo::new(number(bytes, 80)?);
	let row_count = number(bytes, 88)?;
	let height = short(bytes, 104)?;
	if number(bytes, 96)? != page_count
		|| page_count == 0
		|| height > 64
		|| (row_count == 0 && (root != PageNo::NULL || page_count != 1 || height != 0))
		|| (row_count > 0 && (root.get() == 0 || root.get() >= page_count || height == 0))
	{
		return Err(invalid("inconsistent R-tree root, count or height"));
	}
	Ok(GeometryHeader {
		spec,
		root,
		row_count,
		page_count,
		height,
	})
}

/// Visits only overlapping nodes and returns candidate identities for exact
/// refinement.
pub(crate) fn search_geometry(
	header: GeometryHeader,
	query: [f64; 4],
	mut admit: impl FnMut(usize) -> FerrosGrynnResult<()>,
	mut read: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	mut visit: impl FnMut(DiskAtomRef) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	validate_mbr(query)?;
	if header.root == PageNo::NULL {
		return Ok(());
	}
	admit(32 * 1024)?;
	let mut pending = vec![(header.root, header.height - 1)];
	let mut referenced = BTreeSet::from([header.root.get()]);
	let mut page = Box::new(AlignedPage::zeroed());
	while let Some((page_no, level)) = pending.pop() {
		admit(0)?;
		if page_no.get() == 0 || page_no.get() >= header.page_count {
			return Err(invalid("R-tree child is outside its file"));
		}
		read(page_no, &mut page)?;
		let page_header = page.validate(page_no)?;
		let leaf = level == 0;
		if page_header.page_type()
			!= if leaf {
				PageType::IndexGeometryRtreeLeaf
			} else {
				PageType::IndexGeometryRtreeInternal
			}
			|| page_header.flags()
				!= if leaf {
					PageFlags::LEAF
				} else {
					PageFlags::empty()
				} {
			return Err(invalid("R-tree node type or flags disagree with its level"));
		}
		let bytes = page.as_bytes();
		let count = usize::from(short(bytes, 32)?);
		let start = if leaf {
			40
		} else {
			48
		};
		if count == 0
			|| count > CAPACITY
			|| short(bytes, 34)? != level
			|| bytes[36..start]
				.iter()
				.chain(&bytes[start + count * RECORD_BYTES..])
				.any(|byte| *byte != 0)
		{
			return Err(invalid("invalid R-tree node count or reserved bytes"));
		}
		for record in bytes[start..start + count * RECORD_BYTES].as_chunks::<RECORD_BYTES>().0 {
			let entry = decode_entry(record)?;
			if leaf {
				DiskAtomRef::from_raw(entry.identity)?;
			} else {
				admit(128)?;
				if entry.identity == 0
					|| entry.identity >= page_no.get()
					|| !referenced.insert(entry.identity)
					|| referenced.len() as u64 >= header.page_count
				{
					return Err(invalid(
						"R-tree child order is cyclic, repeated or outside its file",
					));
				}
			}
			if overlaps(entry.mbr, query) {
				if leaf {
					visit(DiskAtomRef::from_raw(entry.identity)?)?;
				} else {
					pending.push((PageNo::new(entry.identity), level - 1));
				}
			}
		}
	}
	Ok(())
}

/// Inclusive MBR contact is a conservative spatial candidate test.
pub(crate) fn overlaps(left: [f64; 4], right: [f64; 4]) -> bool {
	left[0] <= right[2] && right[0] <= left[2] && left[1] <= right[3] && right[1] <= left[3]
}
/// Combines child rectangles without changing coordinate precision.
fn union(left: [f64; 4], right: [f64; 4]) -> [f64; 4] {
	[left[0].min(right[0]), left[1].min(right[1]), left[2].max(right[2]), left[3].max(right[3])]
}
/// Encodes finite float order for external sorting.
fn ordered(value: f64) -> [u8; 8] {
	let bits = value.to_bits();
	(if bits >> 63 != 0 {
		!bits
	} else {
		bits ^ (1 << 63)
	})
	.to_be_bytes()
}
/// Forms center-X keys with stable identity ties.
fn x_key(entry: GeometryEntry) -> Vec<u8> {
	let mut prefix = ordered(entry.mbr[0] + (entry.mbr[2] - entry.mbr[0]) * 0.5).to_vec();
	prefix.extend_from_slice(&entry.identity.to_be_bytes());
	key(&entry, &prefix)
}
/// Appends opaque MBR bytes after the complete sortable prefix.
fn key(entry: &GeometryEntry, prefix: &[u8]) -> Vec<u8> {
	let mut result = prefix.to_vec();
	for value in entry.mbr {
		result.extend_from_slice(&value.to_le_bytes());
	}
	result
}
/// Reads the opaque rectangle from a sorted key's suffix.
fn entry_from_key(bytes: &[u8], identity: u64) -> FerrosGrynnResult<GeometryEntry> {
	let offset = bytes.len().checked_sub(32).ok_or_else(|| invalid("truncated STR sort key"))?;
	let mbr = [
		number(bytes, offset)?,
		number(bytes, offset + 8)?,
		number(bytes, offset + 16)?,
		number(bytes, offset + 24)?,
	]
	.map(f64::from_bits);
	validate_mbr(mbr)?;
	Ok(GeometryEntry {
		mbr,
		identity,
	})
}
/// Writes the exact 40-byte node record.
fn encode_entry(output: &mut [u8], entry: GeometryEntry) {
	for (slot, value) in output[..32].as_chunks_mut::<8>().0.iter_mut().zip(entry.mbr) {
		slot.copy_from_slice(&value.to_le_bytes());
	}
	output[32..40].copy_from_slice(&entry.identity.to_le_bytes());
}
/// Validates one physical node record.
fn decode_entry(bytes: &[u8]) -> FerrosGrynnResult<GeometryEntry> {
	let mbr = [number(bytes, 0)?, number(bytes, 8)?, number(bytes, 16)?, number(bytes, 24)?]
		.map(f64::from_bits);
	validate_mbr(mbr)?;
	Ok(GeometryEntry {
		mbr,
		identity: number(bytes, 32)?,
	})
}
/// Hashes definition bytes while excluding mutable build/root metadata.
fn fingerprint(bytes: &[u8]) -> u64 {
	let mut definition = [0_u8; 40];
	definition[..16].copy_from_slice(&bytes[32..48]);
	definition[16..].copy_from_slice(&bytes[56..80]);
	rapidhash_v3(&definition)
}
/// Checked integer reads also serve malformed external-sort keys.
fn number(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u64> {
	Ok(u64::from_le_bytes(
		bytes
			.get(offset..offset + 8)
			.and_then(|bytes| bytes.try_into().ok())
			.ok_or_else(|| invalid("truncated geometry integer"))?,
	))
}
/// Checked 32-bit metadata read.
fn word(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u32> {
	Ok(u32::from_le_bytes(
		bytes
			.get(offset..offset + 4)
			.and_then(|bytes| bytes.try_into().ok())
			.ok_or_else(|| invalid("truncated geometry integer"))?,
	))
}
/// Checked 16-bit metadata read.
fn short(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u16> {
	Ok(u16::from_le_bytes(
		bytes
			.get(offset..offset + 2)
			.and_then(|bytes| bytes.try_into().ok())
			.ok_or_else(|| invalid("truncated geometry integer"))?,
	))
}
/// Canonical optional TYPE discriminator.
fn type_tag(kind: Option<GeometryKind>) -> u8 {
	match kind {
		None => 0,
		Some(GeometryKind::Point) => 1,
		Some(GeometryKind::MultiPoint) => 2,
		Some(GeometryKind::LineString) => 3,
		Some(GeometryKind::MultiLineString) => 4,
		Some(GeometryKind::Polygon) => 5,
		Some(GeometryKind::MultiPolygon) => 6,
		Some(GeometryKind::Collection) => 7,
	}
}
/// Rejects unknown TYPE tags rather than applying a default.
fn decode_type(tag: u8) -> FerrosGrynnResult<Option<GeometryKind>> {
	match tag {
		0 => Ok(None),
		1 => Ok(Some(GeometryKind::Point)),
		2 => Ok(Some(GeometryKind::MultiPoint)),
		3 => Ok(Some(GeometryKind::LineString)),
		4 => Ok(Some(GeometryKind::MultiLineString)),
		5 => Ok(Some(GeometryKind::Polygon)),
		6 => Ok(Some(GeometryKind::MultiPolygon)),
		7 => Ok(Some(GeometryKind::Collection)),
		_ => Err(invalid("invalid geometry TYPE")),
	}
}
/// Redacted storage diagnostic shared by checked codecs.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
