//! Primary mutations must validate persisted ordering before binary search.

use super::*;
use crate::test_assertions::{AssertOption as _, AssertResult as _};

/// A valid checksum does not authorize searching an unordered primary leaf.
#[test]
fn primary_mutation_rejects_unordered_existing_leaf() {
	let entries = [
		PrimaryIndexEntry {
			key: b"beta".to_vec(),
			atom: GlobalAtomId::from_storage(1).assert_ok(),
		},
		PrimaryIndexEntry {
			key: b"alpha".to_vec(),
			atom: GlobalAtomId::from_storage(2).assert_ok(),
		},
	];
	let mut page = Box::new(AlignedPage::zeroed());
	encode_unique_leaf(&mut page, PageNo::new(0), PageLsn::ZERO, None, &entries).assert_ok();
	let result = mutate_primary_index_leaves(
		|page_no, output| {
			assert_eq!(page_no, PageNo::new(0));
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		},
		PageNo::new(0),
		&[PrimaryIndexMutation {
			key: b"zeta".to_vec(),
			atom: GlobalAtomId::from_storage(3).assert_ok(),
			insert: true,
		}],
		PageLsn::ZERO,
	);
	assert!(result.is_err(), "unordered primary keys must be rejected before mutation");
}

/// Reusing a RecordId removes its old address before inserting a new
/// generation.
#[test]
fn global_primary_mutation_reuses_record_after_generation_change() {
	let old = GlobalAtomId::from_storage((37_u64 << 41) | 2).assert_ok();
	let replacement = GlobalAtomId::from_storage((38_u64 << 41) | (1_u64 << 40) | 2).assert_ok();
	let mut built = build_primary_index(
		&[PrimaryIndexEntry {
			key: b"record".to_vec(),
			atom: old,
		}],
		PageLsn::ZERO,
	)
	.assert_ok();
	let changes = mutate_primary_index_leaves(
		|page_no, output| {
			output.as_bytes_mut().copy_from_slice(built.pages[page_no.get() as usize].as_bytes());
			Ok(())
		},
		built.root,
		&[
			PrimaryIndexMutation {
				key: b"record".to_vec(),
				atom: replacement,
				insert: true,
			},
			PrimaryIndexMutation {
				key: b"record".to_vec(),
				atom: old,
				insert: false,
			},
		],
		PageLsn::ZERO,
	)
	.assert_ok()
	.assert_some();
	for (page_no, page) in changes {
		built.pages[page_no.get() as usize].as_bytes_mut().copy_from_slice(page.as_bytes());
	}
	let actual = scan_primary_index::<GlobalAtomId>(
		|page_no, output| {
			output.as_bytes_mut().copy_from_slice(built.pages[page_no.get() as usize].as_bytes());
			Ok(())
		},
		built.root,
	)
	.assert_ok();
	assert_eq!(actual, vec![PrimaryIndexEntry {
		key: b"record".to_vec(),
		atom: replacement
	}]);
	let rebuilt = build_primary_index(&actual, PageLsn::ZERO).assert_ok();
	assert_eq!(built.root, rebuilt.root);
	assert_eq!(built.pages[0].as_bytes(), rebuilt.pages[0].as_bytes());
}

/// Valid canonical addresses still require an exact source generation match.
#[test]
fn primary_mutation_rejects_wrong_source_generation() {
	let expected = GlobalAtomId::from_storage(7).assert_ok();
	let wrong = GlobalAtomId::from_storage((37_u64 << 41) | 7).assert_ok();
	let built = build_primary_index(
		&[PrimaryIndexEntry {
			key: b"record".to_vec(),
			atom: wrong,
		}],
		PageLsn::ZERO,
	)
	.assert_ok();
	let result = mutate_primary_index_leaves(
		|number, output| {
			output.as_bytes_mut().copy_from_slice(built.pages[number.get() as usize].as_bytes());
			Ok(())
		},
		built.root,
		&[PrimaryIndexMutation {
			key: b"record".to_vec(),
			atom: expected,
			insert: false,
		}],
		PageLsn::ZERO,
	);
	assert!(result.is_err(), "a checksummed wrong-generation posting cannot authorize removal");
}

/// An early separator fallback cannot hide corruption in a later leaf.
#[test]
fn primary_mutation_validates_source_before_bulk_fallback() {
	let entries = (0..2000_u64)
		.map(|slot| PrimaryIndexEntry {
			key: format!("record:{slot:08}").into_bytes(),
			atom: GlobalAtomId::from_storage(if slot == 1999 {
				(37_u64 << 41) | (slot + 1)
			} else {
				slot + 1
			})
			.assert_ok(),
		})
		.collect::<Vec<_>>();
	let built = build_primary_index(&entries, PageLsn::ZERO).assert_ok();
	assert_ne!(built.root, PageNo::new(0));
	let mutations = [0_u64, 1999].map(|slot| PrimaryIndexMutation {
		key: format!("record:{slot:08}").into_bytes(),
		atom: GlobalAtomId::from_storage(slot + 1).assert_ok(),
		insert: false,
	});
	let result = mutate_primary_index_leaves(
		|number, output| {
			output.as_bytes_mut().copy_from_slice(built.pages[number.get() as usize].as_bytes());
			Ok(())
		},
		built.root,
		&mutations,
		PageLsn::ZERO,
	);
	assert!(result.is_err(), "a fallback does not authorize a later wrong-generation posting");
}
