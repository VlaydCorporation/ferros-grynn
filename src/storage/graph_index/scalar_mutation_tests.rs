//! Scalar leaf admission must finish before following external postings.

use super::*;
use crate::test_assertions::AssertResult as _;

/// An unchanged external posting in the same leaf must trigger bounded
/// fallback.
#[test]
fn standard_mutation_defers_external_neighbour_before_posting_read() {
	let entries = [
		ScalarIndexEntry {
			key: b"alpha".to_vec(),
			atoms: (0..9).map(node).collect(),
		},
		ScalarIndexEntry {
			key: b"beta".to_vec(),
			atoms: vec![node(10)],
		},
	];
	let built = build_scalar_index(&entries, ScalarIndexMode::Standard, PageLsn::ZERO).assert_ok();
	assert_eq!(built.root, PageNo::new(0));
	let result = mutate_scalar_index_leaves(
		|page_no, output| {
			if page_no != built.root {
				return Err(invalid_index("posting read forbidden before bounded fallback"));
			}
			output.as_bytes_mut().copy_from_slice(built.pages[0].as_bytes());
			Ok(())
		},
		built.root,
		ScalarIndexMode::Standard,
		&[
			ScalarIndexMutation {
				key: b"beta".to_vec(),
				atom: node(10),
				insert: false,
			},
			ScalarIndexMutation {
				key: b"beta".to_vec(),
				atom: node(11),
				insert: true,
			},
		],
		PageLsn::ZERO,
	)
	.assert_ok();
	assert!(result.is_none(), "external layout requires the selected streaming fallback");
}

/// A checksummed but unordered leaf is corruption, not input to binary search.
#[test]
fn scalar_mutation_rejects_unordered_existing_leaf() {
	let entries = [
		ScalarIndexEntry {
			key: b"beta".to_vec(),
			atoms: vec![node(1)],
		},
		ScalarIndexEntry {
			key: b"alpha".to_vec(),
			atoms: vec![node(2)],
		},
	];
	for mode in [ScalarIndexMode::Standard, ScalarIndexMode::Unique] {
		let mut page = Box::new(AlignedPage::zeroed());
		match mode {
			ScalarIndexMode::Standard => encode_standard_leaf(
				&mut page,
				PageNo::new(0),
				PageLsn::ZERO,
				None,
				&entries,
				&BTreeMap::new(),
			)
			.assert_ok(),
			ScalarIndexMode::Unique => {
				let unique = entries
					.iter()
					.map(|entry| PrimaryIndexEntry {
						key: entry.key.clone(),
						atom: entry.atoms[0],
					})
					.collect::<Vec<_>>();
				encode_unique_leaf(&mut page, PageNo::new(0), PageLsn::ZERO, None, &unique)
					.assert_ok();
			}
		}
		let result = mutate_scalar_index_leaves(
			|page_no, output| {
				assert_eq!(page_no, PageNo::new(0));
				output.as_bytes_mut().copy_from_slice(page.as_bytes());
				Ok(())
			},
			PageNo::new(0),
			mode,
			&[ScalarIndexMutation {
				key: b"zeta".to_vec(),
				atom: node(3),
				insert: true,
			}],
			PageLsn::ZERO,
		);
		assert!(result.is_err(), "unordered {mode:?} keys must be rejected before mutation");
	}
}

/// Creates a valid local address for the fixed test corpus.
fn node(slot: u64) -> DiskAtomRef {
	DiskAtomRef::from_parts(AtomKind::Node, slot).assert_ok()
}
