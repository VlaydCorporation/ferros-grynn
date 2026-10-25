//! Bounded exact replacement/tombstone pages attached to an immutable STR base.

use std::collections::BTreeMap;

use fg_runtime::__internal::GeometryInternal as _;

use super::{
	GeometryBuildSpec,
	GeometryEntry,
	GeometryHeader,
	decode_entry,
	decode_geometry_header,
	encode_entry,
	encode_header,
	fingerprint,
	invalid,
	number,
	short,
	validate_mbr,
};
use crate::{
	gql_status::FerrosGrynnResult,
	storage::page::{AlignedPage, PageFlags, PageLsn, PageNo, PageType},
	values::storage::DiskAtomRef,
};

/// Encoded identity, replacement bounds and action width.
const WIDTH: usize = 48;
/// Records fitting in one delta page.
const CAPACITY: usize = 340;
/// Hard replacement population cap before base compaction.
const MAX_RECORDS: usize = 4096;

/// Derives one exact replacement set shared by resident and durable
/// generations.
#[allow(
	clippy::mutable_key_type,
	reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash identities"
)]
pub(crate) fn geometry_delta_records(
	previous: &crate::datastore::DatabaseRoot,
	next: &crate::datastore::DatabaseRoot,
	changed: &std::collections::BTreeSet<crate::values::RecordId>,
	base: GeometryHeader,
	prior: impl Iterator<Item = GeometryDeltaRecord>,
) -> FerrosGrynnResult<Option<BTreeMap<DiskAtomRef, GeometryDeltaRecord>>> {
	let mut records = prior.map(|record| (record.atom, record)).collect::<BTreeMap<_, _>>();
	if records.len() > MAX_RECORDS {
		return Err(invalid("geometry delta exceeds its record cap"));
	}
	let row = |root: &crate::datastore::DatabaseRoot,
	           record: &crate::values::RecordId|
	 -> FerrosGrynnResult<Option<(DiskAtomRef, Option<[f64; 4]>)>> {
		let Some((atom, value)) =
			scoped_index_value(root, record, base.spec.class, base.spec.scope, base.spec.field)?
		else {
			return Ok(None);
		};
		let mbr = match value {
			None | Some(crate::values::Value::None | crate::values::Value::Null) => None,
			Some(crate::values::Value::Geometry(geometry)) => {
				base.spec.options.validate(geometry)?;
				geometry.mbr()?
			}
			Some(_) => return Err(invalid("geometry delta field contains a non-geometry value")),
		};
		Ok(Some((atom, mbr)))
	};
	let limit = (usize::try_from(base.row_count)
		.map_err(|_error| invalid("geometry population exceeds usize"))?
		/ 1000)
		.clamp(1, MAX_RECORDS);
	for record in changed {
		crate::storage::check_external_sort_cancelled()?;
		let old = row(previous, record)?;
		let new = row(next, record)?;
		if old == new {
			continue;
		}
		if let Some((atom, Some(_))) = old {
			records.insert(atom, GeometryDeltaRecord {
				atom,
				mbr: None,
			});
		}
		if let Some((atom, Some(mbr))) = new {
			records.insert(atom, GeometryDeltaRecord {
				atom,
				mbr: Some(mbr),
			});
		}
		if records.len() > limit {
			return Ok(None);
		}
	}
	Ok(Some(records))
}

/// Resolves one qualified source row using the same BASE/EFFECTIVE semantics as
/// builds.
pub(in crate::storage) fn scoped_index_value<'a>(
	root: &'a crate::datastore::DatabaseRoot,
	record: &crate::values::RecordId,
	class: crate::datastore::ClassId,
	scope: crate::datastore::IndexScope,
	key: crate::values::PropertyKeyId,
) -> FerrosGrynnResult<Option<(DiskAtomRef, Option<&'a crate::values::Value>)>> {
	let Some(global) = root.resolve_atom(record) else {
		return Ok(None);
	};
	let atom = root
		.atom(global)
		.ok_or_else(|| invalid("primary identity resolves an absent base atom"))?;
	if atom.class_id() != class {
		return Ok(None);
	}
	match scope {
		crate::datastore::IndexScope::Base => Ok(Some((
			DiskAtomRef::from_parts(atom.kind(), atom.id().slot())?,
			atom.properties().get(key),
		))),
		crate::datastore::IndexScope::Effective {
			graph,
		} => {
			let Some(local) = root.membership(graph, global) else {
				return Ok(None);
			};
			let occurrence = root
				.occurrence(graph, local)
				.ok_or_else(|| invalid("graph membership resolves an absent occurrence"))?;
			Ok(Some((
				DiskAtomRef::from_parts(atom.kind(), local.slot())?,
				occurrence.effective_value(atom.properties().get(key), key),
			)))
		}
	}
}

/// A missing rectangle is an exact removal from the base generation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GeometryDeltaRecord {
	/// Stable scope-local row identity.
	pub(crate) atom: DiskAtomRef,
	/// Replacement bounds, or a tombstone.
	pub(crate) mbr: Option<[f64; 4]>,
}

/// Checked replacement generation; records are ordered by stable row identity.
#[derive(Debug)]
pub(crate) struct DecodedGeometryDelta {
	/// Exact records in stable identity order.
	pub(crate) records: Vec<GeometryDeltaRecord>,
}

/// Allocates final aligned slots only after checking the bounded record count.
pub(crate) fn build_geometry_delta(
	base: GeometryBuildSpec,
	revision: u64,
	records: BTreeMap<DiskAtomRef, GeometryDeltaRecord>,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	if records.len() > MAX_RECORDS || revision < base.build_revision {
		return Err(invalid("geometry delta exceeds its record cap or precedes its base"));
	}
	let count = 1 + records.len().div_ceil(CAPACITY);
	let mut pages = Vec::with_capacity(count);
	pages.resize_with(count, AlignedPage::zeroed);
	encode_header(&mut pages[0], GeometryHeader {
		spec: base,
		root: if records.is_empty() {
			PageNo::NULL
		} else {
			PageNo::new(1)
		},
		row_count: records.len() as u64,
		page_count: count as u64,
		height: u16::from(!records.is_empty()),
	})?;
	let header = pages[0].as_bytes_mut();
	header[66..68].copy_from_slice(&2_u16.to_le_bytes());
	header[120..128].copy_from_slice(&revision.to_le_bytes());
	let hash = fingerprint(header);
	header[112..120].copy_from_slice(&hash.to_le_bytes());
	pages[0].seal()?;
	for (ordinal, record) in records.values().enumerate() {
		let page_no = 1 + ordinal / CAPACITY;
		let slot = ordinal % CAPACITY;
		let page = &mut pages[page_no];
		if slot == 0 {
			page.reset(
				PageType::IndexGeometryRtreeLeaf,
				PageFlags::LEAF,
				PageLsn::ZERO,
				PageNo::new(page_no as u64),
			);
		}
		if let Some(mbr) = record.mbr {
			validate_mbr(mbr)?;
		}
		let start = 40 + slot * WIDTH;
		encode_entry(&mut page.as_bytes_mut()[start..start + 40], GeometryEntry {
			mbr: record.mbr.unwrap_or([0.0; 4]),
			identity: record.atom.raw(),
		});
		page.as_bytes_mut()[start + 40] = u8::from(record.mbr.is_some());
		page.as_bytes_mut()[32..34].copy_from_slice(&((slot + 1) as u16).to_le_bytes());
	}
	for page in &mut pages[1..] {
		page.seal()?;
	}
	Ok(pages)
}

/// Rejects partial, oversized, reordered or incompatible delta generations.
pub(crate) fn decode_geometry_delta(
	pages: &[AlignedPage],
	expected: GeometryBuildSpec,
) -> FerrosGrynnResult<DecodedGeometryDelta> {
	let first = pages.first().ok_or_else(|| invalid("geometry delta has no header"))?;
	first.validate(PageNo::new(0))?;
	let bytes = first.as_bytes();
	if short(bytes, 66)? != 2
		|| number(bytes, 112)? != fingerprint(bytes)
		|| bytes[128..].iter().any(|byte| *byte != 0)
	{
		return Err(invalid("invalid geometry delta header"));
	}
	let revision = number(bytes, 120)?;
	let mut control = Box::new(AlignedPage::zeroed());
	control.as_bytes_mut().copy_from_slice(bytes);
	control.as_bytes_mut()[66..68].copy_from_slice(&1_u16.to_le_bytes());
	control.as_bytes_mut()[120..128].fill(0);
	let hash = fingerprint(control.as_bytes());
	control.as_bytes_mut()[112..120].copy_from_slice(&hash.to_le_bytes());
	control.seal()?;
	let header = decode_geometry_header(&control, pages.len() as u64)?;
	if header.spec != expected
		|| revision < expected.build_revision
		|| header.row_count > MAX_RECORDS as u64
		|| pages.len() != 1 + (header.row_count as usize).div_ceil(CAPACITY)
		|| header.height != u16::from(header.row_count != 0)
		|| (header.row_count != 0 && header.root != PageNo::new(1))
	{
		return Err(invalid("geometry delta metadata disagrees with its base"));
	}
	let mut records = Vec::with_capacity(header.row_count as usize);
	for (ordinal, page) in pages.iter().enumerate().skip(1) {
		let page_header = page.validate(PageNo::new(ordinal as u64))?;
		let bytes = page.as_bytes();
		let count = usize::from(short(bytes, 32)?);
		let remaining = header.row_count as usize - records.len();
		if page_header.page_type() != PageType::IndexGeometryRtreeLeaf
			|| page_header.flags() != PageFlags::LEAF
			|| count != remaining.min(CAPACITY)
			|| bytes[34..40].iter().chain(&bytes[40 + count * WIDTH..]).any(|byte| *byte != 0)
		{
			return Err(invalid("invalid geometry delta record page"));
		}
		for bytes in bytes[40..40 + count * WIDTH].as_chunks::<WIDTH>().0 {
			let entry = decode_entry(&bytes[..40])?;
			let atom = DiskAtomRef::from_raw(entry.identity)?;
			if records.last().is_some_and(|previous: &GeometryDeltaRecord| previous.atom >= atom)
				|| bytes[41..].iter().any(|byte| *byte != 0)
			{
				return Err(invalid("geometry delta identities or reserved bytes are invalid"));
			}
			let mbr = match bytes[40] {
				0 if entry.mbr.iter().all(num_traits::Zero::is_zero) => None,
				1 => Some(entry.mbr),
				_ => return Err(invalid("invalid geometry delta action")),
			};
			records.push(GeometryDeltaRecord {
				atom,
				mbr,
			});
		}
	}
	Ok(DecodedGeometryDelta {
		records,
	})
}
