//! Multi-page layout, admission, ownership and corrupt section checks.

use super::*;

/// Complete test shape for one supported directed cost model.
fn header(cost_kind: u8, vertices: u64) -> LandmarkPathHeader {
	LandmarkPathHeader {
		identity: GraphArtifactIdentity {
			index: 7,
			version: 2,
			graph: 9,
			revision: 11,
			fingerprint: 13,
		},
		vertices,
		requested_landmarks: 3,
		descriptor_bytes: 19,
		max_bytes: 2 << 20,
		cost_kind,
		function: if cost_kind == 3 {
			5
		} else {
			0
		},
		function_version: if cost_kind == 3 {
			6
		} else {
			0
		},
		direction: 2,
		mode: 1,
		has_cross_level: true,
	}
}

/// Encodes reproducible values without depending on shortest path kernels.
fn element(kind: SectionKind, ordinal: u64, bytes: &mut [u8]) -> FerrosGrynnResult<()> {
	if kind == SectionKind::DefinitionDescriptor {
		bytes[0] = ordinal as u8;
	} else {
		bytes[..8].copy_from_slice(&ordinal.to_le_bytes());
		if bytes.len() == 16 {
			bytes[8..].copy_from_slice(&ordinal.to_le_bytes());
		}
	}
	Ok(())
}

/// Preallocates final aligned slots before copying any sealed pages.
fn pages(layout: &LandmarkPathLayout) -> Vec<AlignedPage> {
	let mut pages = Vec::with_capacity(layout.page_count as usize);
	pages.resize_with(layout.page_count as usize, AlignedPage::zeroed);
	let mut next = 0;
	layout
		.stream(
			element,
			|at, page| {
				assert_eq!(at.get(), next);
				next += 1;
				pages[at.get() as usize].as_bytes_mut().copy_from_slice(page.as_bytes());
				Ok(())
			},
			&mut || Ok(()),
		)
		.unwrap();
	assert_eq!(next, layout.page_count);
	pages
}

#[test]
fn landmark_path_roundtrip_empty_and_multiple_pages_all_costs() {
	for cost in 1..=3 {
		for vertices in [0, 1, 2500] {
			let layout = LandmarkPathLayout::new(header(cost, vertices)).unwrap();
			let pages = pages(&layout);
			assert_eq!(&pages[0].as_bytes()[..8], &[0x46, 0x49, 0x50, 1, 0x30, 0, 0, 0]);
			assert_eq!(
				LandmarkPathLayout::decode_header(
					&pages[0],
					layout.header.identity,
					layout.page_count
				)
				.unwrap(),
				layout
			);
			let data = layout
				.load_sections(
					8 << 20,
					|at, page| {
						page.as_bytes_mut().copy_from_slice(pages[at.get() as usize].as_bytes());
						Ok(())
					},
					&mut || Ok(()),
				)
				.unwrap();
			for (section, data) in layout.sections.iter().zip(data) {
				assert_eq!(data.len() as u64, section.count * u64::from(section.width));
				for (ordinal, bytes) in data.chunks_exact(usize::from(section.width)).enumerate() {
					let mut expected = vec![0; bytes.len()];
					element(section.kind, ordinal as u64, &mut expected).unwrap();
					assert_eq!(bytes, expected);
				}
			}
			let exact = layout.page_count * 16_384;
			assert!(
				LandmarkPathLayout::new(LandmarkPathHeader {
					max_bytes: exact,
					..layout.header
				})
				.is_ok()
			);
			assert!(
				LandmarkPathLayout::new(LandmarkPathHeader {
					max_bytes: exact - 1,
					..layout.header
				})
				.is_err()
			);
		}
	}
}

#[test]
fn landmark_path_corrupt_header_sections_admission_and_cancellation() {
	let layout = LandmarkPathLayout::new(header(1, 2500)).unwrap();
	let mut pages = pages(&layout);
	let valid = pages[0].as_bytes().to_vec();
	for (at, value) in [
		(32, 0),
		(36, 0),
		(40, 0),
		(48, 0),
		(64, 4),
		(68, 0),
		(72, 0),
		(80, 0),
		(104, 0),
		(112, 0),
		(120, 0),
		(130, 0),
		(136, 1),
		(140, 1),
		(144, 1),
		(145, 3),
		(146, 0),
		(147, 2),
		(148, 2),
		(152, 0),
		(160, 0),
		(168, 0),
		(176, 2),
		(177, 1),
	] {
		pages[0].as_bytes_mut().copy_from_slice(&valid);
		pages[0].as_bytes_mut()[at] = value;
		pages[0].seal().unwrap();
		assert!(
			LandmarkPathLayout::decode_header(&pages[0], layout.header.identity, layout.page_count)
				.is_err(),
			"at={at}"
		);
	}
	assert!(
		LandmarkPathLayout::new(LandmarkPathHeader {
			vertices: u64::MAX,
			..layout.header
		})
		.is_err()
	);
	assert!(
		LandmarkPathLayout::new(LandmarkPathHeader {
			requested_landmarks: 257,
			..layout.header
		})
		.is_err()
	);
	assert!(
		LandmarkPathLayout::new(LandmarkPathHeader {
			cost_kind: 3,
			..layout.header
		})
		.is_err()
	);
	let mut reads = 0;
	assert!(
		layout
			.load_sections(
				1024,
				|_, _| {
					reads += 1;
					Ok(())
				},
				&mut || Ok(())
			)
			.is_err()
	);
	assert_eq!(reads, 0);
	assert!(
		layout
			.load_sections(
				8 << 20,
				|_, _| {
					reads += 1;
					Ok(())
				},
				&mut || Err(FerrosGrynnError::query_cancelled())
			)
			.is_err()
	);
	assert_eq!(reads, 0);
	let mut forged = layout.clone();
	forged.sections[2].width = 0;
	let mut writes = 0;
	assert!(
		forged
			.stream(
				element,
				|_, _| {
					writes += 1;
					Ok(())
				},
				&mut || Ok(())
			)
			.is_err()
	);
	assert_eq!(writes, 0);
	let at = layout.sections[2].root.get() as usize;
	let valid_section = pages[at].as_bytes().to_vec();
	for (offset, byte) in
		[(32, 0), (34, 1), (36, 255), (40, 1), (42, 0), (44, 0), (48, 1), (56, 1), (60, 1)]
	{
		pages[at].as_bytes_mut().copy_from_slice(&valid_section);
		pages[at].as_bytes_mut()[offset] = byte;
		pages[at].seal().unwrap();
		assert!(
			layout
				.load_sections(
					8 << 20,
					|at, page| {
						page.as_bytes_mut().copy_from_slice(pages[at.get() as usize].as_bytes());
						Ok(())
					},
					&mut || Ok(())
				)
				.is_err(),
			"offset={offset}"
		);
	}
}
