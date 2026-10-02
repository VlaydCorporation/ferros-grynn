//! Differential STR packing, malformed-page and external-spill controls.

use std::{collections::BTreeSet, num::NonZeroUsize};

use super::{
	GeometryBuildSpec,
	GeometryEntry,
	GeometryHeader,
	decode_geometry_header,
	search_geometry,
	stream_geometry_index,
};
use crate::{
	datastore::{ClassId, GeometryIndexOptions, IndexId, IndexScope, SchemaVersion},
	storage::{
		page::{AlignedPage, PageNo},
		with_external_sort_options,
	},
	values::{AtomKind, PropertyKeyId, Uuid, storage::DiskAtomRef},
};

/// Small published definition shared by format controls.
fn spec() -> GeometryBuildSpec {
	GeometryBuildSpec {
		index: IndexId::new(2).unwrap(),
		index_version: SchemaVersion::INITIAL,
		class: ClassId::new(1).unwrap(),
		field: PropertyKeyId::new(0),
		scope: IndexScope::Base,
		options: GeometryIndexOptions::default(),
		build_revision: 7,
	}
}

/// Preallocates stable final page slots before filling the test control.
fn build(rows: usize, scratch: &std::path::Path) -> Vec<AlignedPage> {
	let mut nodes = rows;
	let mut count = 1;
	while nodes > 0 {
		nodes = nodes.div_ceil(387);
		count += nodes;
		if nodes == 1 {
			break;
		}
	}
	let mut pages = Vec::with_capacity(count);
	pages.resize_with(count, AlignedPage::zeroed);
	let built = stream_geometry_index(
		spec(),
		scratch,
		|consumer| {
			for slot in (0..rows).rev() {
				let x = (slot % 1000) as f64 / 100.0;
				let y = (slot / 1000) as f64 / 100.0;
				consumer(GeometryEntry {
					mbr: [x, y, x, y],
					identity: DiskAtomRef::from_parts(AtomKind::Node, slot as u64).unwrap().raw(),
				})?;
			}
			Ok(())
		},
		|page_no, page| {
			pages[page_no.get() as usize].as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		},
	)
	.unwrap();
	assert_eq!(built.root, PageNo::new(0));
	assert_eq!(built.page_count, count as u64);
	pages
}

/// Selective-reader fixture that never bypasses page validation.
fn candidates(
	pages: &[AlignedPage],
	header: GeometryHeader,
	query: [f64; 4],
) -> crate::gql_status::FerrosGrynnResult<Vec<DiskAtomRef>> {
	let mut result = Vec::new();
	search_geometry(
		header,
		query,
		|_| Ok(()),
		|page_no, output| {
			output.as_bytes_mut().copy_from_slice(pages[page_no.get() as usize].as_bytes());
			Ok(())
		},
		|atom| {
			result.push(atom);
			Ok(())
		},
	)?;
	Ok(result)
}

#[test]
fn geometry_str_matches_scan_across_empty_leaf_and_three_levels() {
	for rows in [0, 1, 387, 388, 160_000] {
		let pages = build(rows, &std::env::temp_dir());
		let header = decode_geometry_header(&pages[0], pages.len() as u64).unwrap();
		assert_eq!(header.row_count, rows as u64);
		assert_eq!(header.spec, spec());
		if rows == 0 {
			assert_eq!(header.root, PageNo::NULL);
		}
		if rows == 160_000 {
			assert_eq!(header.height, 3);
		}
		let found = candidates(&pages, header, [1.0, 0.25, 2.0, 0.75]).unwrap();
		let unique = found.iter().map(|atom| atom.raw()).collect::<BTreeSet<_>>();
		assert_eq!(found.len(), unique.len());
		let expected = (0..rows)
			.filter(|slot| {
				(100..=200).contains(&(slot % 1000)) && (25..=75).contains(&(slot / 1000))
			})
			.map(|slot| DiskAtomRef::from_parts(AtomKind::Node, slot as u64).unwrap().raw())
			.collect::<BTreeSet<_>>();
		assert_eq!(unique, expected);
	}
}

#[test]
fn geometry_str_spill_is_deterministic_and_cleans_scratch() {
	let scratch = std::env::temp_dir().join(format!("fg-str-{}", Uuid::now_v7()));
	std::fs::create_dir(&scratch).unwrap();
	let expected = build(10_000, &scratch);
	let spilled = with_external_sort_options(
		NonZeroUsize::new(2 * 1024 * 1024 + 24 * 1024).unwrap(),
		NonZeroUsize::new(1).unwrap(),
		|| Ok(build(10_000, &scratch)),
	)
	.unwrap();
	assert_eq!(AlignedPage::slice_as_bytes(&expected), AlignedPage::slice_as_bytes(&spilled));
	assert!(std::fs::read_dir(scratch.join("tmp/index-sort")).unwrap().next().is_none());
	std::fs::remove_dir_all(&scratch).unwrap();
}

#[test]
fn geometry_str_rejects_duplicate_rows_and_malformed_nodes() {
	let entry = GeometryEntry {
		mbr: [0.0; 4],
		identity: DiskAtomRef::from_parts(AtomKind::Node, 0).unwrap().raw(),
	};
	assert!(
		stream_geometry_index(
			spec(),
			&std::env::temp_dir(),
			|consumer| {
				consumer(entry)?;
				consumer(entry)
			},
			|_, _| Ok(())
		)
		.is_err()
	);
	for corruption in 0..5 {
		let mut pages = build(1000, &std::env::temp_dir());
		let header = decode_geometry_header(&pages[0], pages.len() as u64).unwrap();
		let root = &mut pages[header.root.get() as usize];
		match corruption {
			0 => root.as_bytes_mut()[32..34].copy_from_slice(&409_u16.to_le_bytes()),
			1 => root.as_bytes_mut()[36] = 1,
			2 => root.as_bytes_mut()[48..56].copy_from_slice(&f64::NAN.to_le_bytes()),
			3 => root.as_bytes_mut()[80..88].copy_from_slice(&header.root.get().to_le_bytes()),
			_ => {
				let first = root.as_bytes()[80..88].to_vec();
				root.as_bytes_mut()[120..128].copy_from_slice(&first);
			}
		}
		root.seal().unwrap();
		assert!(
			candidates(&pages, header, [-180.0, -90.0, 180.0, 90.0]).is_err(),
			"corruption {corruption}"
		);
	}
}

#[test]
fn geometry_str_rejects_small_budget_before_feed_or_write() {
	let result = with_external_sort_options(
		NonZeroUsize::new(16 * 1024).unwrap(),
		NonZeroUsize::new(1).unwrap(),
		|| {
			stream_geometry_index(
				spec(),
				&std::env::temp_dir(),
				|_| panic!("feed must not run"),
				|_, _| panic!("write must not run"),
			)
		},
	);
	assert!(result.is_err());
}
