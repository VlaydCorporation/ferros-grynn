//! Plain overflow bytes remain canonical when the destination frame is reused.
use super::*;
use crate::{graph::AtomKind, test_assertions::AssertResult as _};

/// Checks the fixed format independently of the convenience returning encoder.
#[test]
fn adjacency_into_matches_format_v1_and_clears_reused_padding() {
	for count in [0, 1, OVERFLOW_REFS_PER_PAGE] {
		let entries = (0..count)
			.map(|index| {
				DiskAtomRef::from_parts(AtomKind::Edge, u64::try_from(index).assert_ok())
					.assert_ok()
			})
			.collect::<Vec<_>>();
		for next in [None, Some(PageNo::new(71))] {
			let mut destination = Box::new(AlignedPage::zeroed());
			destination.as_bytes_mut().fill(0xa5);
			encode_adjacency_overflow_into(
				&mut destination,
				PageNo::new(37),
				PageLsn::new(11),
				next,
				&entries,
			)
			.assert_ok();
			destination.validate(PageNo::new(37)).assert_ok();
			let mut expected = vec![0_u8; PAGE_SIZE];
			// Format-v1 header, next/count and entry offsets are a wire
			// contract.
			expected[..8].copy_from_slice(&[b'F', b'A', b'O', 1, 6, 0, 4, 0]);
			expected[8..16].copy_from_slice(&11_u64.to_le_bytes());
			expected[24..32].copy_from_slice(&37_u64.to_le_bytes());
			expected[32..40].copy_from_slice(&next.map_or(u64::MAX, PageNo::get).to_le_bytes());
			expected[40..44].copy_from_slice(&u32::try_from(count).assert_ok().to_le_bytes());
			for (index, entry) in entries.iter().enumerate() {
				expected[48 + index * 8..56 + index * 8]
					.copy_from_slice(&entry.raw().to_le_bytes());
			}
			let mut actual = destination.as_bytes().to_vec();
			// Validation above checks the complete-page checksum separately.
			actual[16..24].fill(0);
			assert_eq!(actual, expected);
			assert_eq!(
				decode_adjacency_overflow(&destination, PageNo::new(37)).assert_ok(),
				(next, entries.clone())
			);
			let returned =
				encode_adjacency_overflow(PageNo::new(37), PageLsn::new(11), next, &entries)
					.assert_ok();
			assert_eq!(returned.as_bytes(), destination.as_bytes());
		}
	}
}

/// Admission failure must leave even an uninitialized output frame untouched.
#[test]
fn adjacency_into_rejects_over_capacity_without_changing_destination() {
	let entries =
		vec![DiskAtomRef::from_parts(AtomKind::Edge, 0).assert_ok(); OVERFLOW_REFS_PER_PAGE + 1];
	let mut destination = Box::new(AlignedPage::zeroed());
	destination.as_bytes_mut().fill(0x5a);
	encode_adjacency_overflow_into(
		&mut destination,
		PageNo::new(37),
		PageLsn::new(11),
		None,
		&entries,
	)
	.assert_err();
	assert!(destination.as_bytes().iter().all(|&byte| byte == 0x5a));
}
