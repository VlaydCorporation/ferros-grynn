//! Exact page projection, streaming order and malformed metadata checks.

use super::*;

/// Checked shape with multiple pages in every applicable section.
fn header(algorithm: NeighbourhoodTag) -> NeighbourhoodHeader {
	let vertices = 2500;
	let centers = match algorithm {
		NeighbourhoodTag::Exact => vertices,
		NeighbourhoodTag::Sketch => 0,
		NeighbourhoodTag::Landmark => 10,
	};
	NeighbourhoodHeader {
		identity: GraphArtifactIdentity {
			index: 7,
			version: 2,
			graph: 9,
			revision: 11,
			fingerprint: 13,
		},
		node_capacity: 2600,
		edge_capacity: 12,
		vertices,
		edges: 2600,
		centers,
		members: centers * 2 + u64::from(centers != 0) * 3000,
		register_bytes: if algorithm == NeighbourhoodTag::Sketch {
			vertices * 2 * 16
		} else {
			0
		},
		max_bytes: 8 << 20,
		descriptor_bytes: 17,
		parameter: match algorithm {
			NeighbourhoodTag::Exact => 0,
			NeighbourhoodTag::Sketch => 4,
			NeighbourhoodTag::Landmark => 10,
		},
		radius: 2,
		algorithm,
		direction: 0,
		mode: 0,
		has_cross_level: false,
	}
}

/// Encodes reproducible bytes independently of graph algorithms.
fn element(kind: SectionKind, ordinal: u64, bytes: &mut [u8]) -> FerrosGrynnResult<()> {
	if matches!(kind, SectionKind::DefinitionDescriptor | SectionKind::NeighbourhoodRegisters) {
		bytes[0] = (ordinal % 31) as u8;
	} else {
		bytes.copy_from_slice(&ordinal.to_le_bytes());
	}
	Ok(())
}

#[test]
fn neighbourhood_layout_streams_exact_pages_for_all_algorithms() {
	for algorithm in [NeighbourhoodTag::Exact, NeighbourhoodTag::Sketch, NeighbourhoodTag::Landmark]
	{
		let layout = NeighbourhoodLayout::new(header(algorithm)).unwrap();
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
		assert_eq!(
			NeighbourhoodLayout::decode_header(
				&pages[0],
				layout.header.identity,
				layout.page_count
			)
			.unwrap(),
			layout
		);
		assert!(
			layout
				.load_sections(
					1,
					|_, _| panic!("scratch admission must precede reads"),
					&mut || Ok(())
				)
				.is_err()
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
		assert_eq!(data[6].len() as u64, layout.header.register_bytes);
		assert_eq!(data[7].len() as u64, layout.header.descriptor_bytes);
		for section in layout.sections {
			let mut count = 0;
			section
				.visit(
					layout.page_count,
					|at, page| {
						page.as_bytes_mut().copy_from_slice(pages[at.get() as usize].as_bytes());
						Ok(())
					},
					|ordinal, bytes| {
						let mut expected = vec![0; bytes.len()];
						element(section.kind, ordinal, &mut expected)?;
						assert_eq!(bytes, expected);
						count += 1;
						Ok(())
					},
				)
				.unwrap();
			assert_eq!(count, section.count);
		}
		let bytes = layout.page_count * 16_384;
		assert!(
			NeighbourhoodLayout::new(NeighbourhoodHeader {
				max_bytes: bytes,
				..layout.header
			})
			.is_ok()
		);
		assert!(
			NeighbourhoodLayout::new(NeighbourhoodHeader {
				max_bytes: bytes - 1,
				..layout.header
			})
			.is_err()
		);
	}
}

#[test]
fn neighbourhood_header_and_forged_sections_fail_before_output() {
	let layout = NeighbourhoodLayout::new(header(NeighbourhoodTag::Exact)).unwrap();
	let mut page = Box::new(AlignedPage::zeroed());
	layout.encode_header(&mut page).unwrap();
	assert_eq!(&page.as_bytes()[0..8], &[0x46, 0x49, 0x4e, 1, 0x2c, 0, 0, 0]);
	let valid = page.as_bytes().to_vec();
	for (at, value) in [
		(32, 0),
		(36, 0),
		(40, 0),
		(48, 0),
		(79, 255),
		(88, 0),
		(120, 0),
		(136, 0),
		(144, 0),
		(152, 0),
		(208, 1),
		(212, 5),
		(213, 255),
		(214, 3),
		(215, 2),
		(216, 2),
		(217, 1),
	] {
		page.as_bytes_mut().copy_from_slice(&valid);
		page.as_bytes_mut()[at] = value;
		page.seal().unwrap();
		assert!(
			NeighbourhoodLayout::decode_header(&page, layout.header.identity, layout.page_count)
				.is_err(),
			"at={at}"
		);
	}
	assert!(
		NeighbourhoodLayout::new(NeighbourhoodHeader {
			register_bytes: u64::MAX,
			..header(NeighbourhoodTag::Sketch)
		})
		.is_err()
	);
	assert!(
		NeighbourhoodLayout::new(NeighbourhoodHeader {
			parameter: 19,
			..header(NeighbourhoodTag::Sketch)
		})
		.is_err()
	);
	assert!(
		NeighbourhoodLayout::new(NeighbourhoodHeader {
			centers: 11,
			..header(NeighbourhoodTag::Landmark)
		})
		.is_err()
	);
	let mut writes = 0;
	assert!(
		layout
			.stream(
				element,
				|_, _| {
					writes += 1;
					Ok(())
				},
				&mut || Err(FerrosGrynnError::query_cancelled())
			)
			.is_err()
	);
	assert_eq!(writes, 0);
	let mut forged = layout.clone();
	forged.sections[0].width = 0;
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
	assert!(
		forged.sections[0]
			.visit(
				layout.page_count,
				|_, _| panic!("invalid width must fail before read"),
				|_, _| Ok(())
			)
			.is_err()
	);
	let mut section = layout.sections[0];
	section.root = PageNo::new(u64::MAX - 1);
	assert!(
		section
			.visit(
				layout.page_count,
				|_, _| panic!("invalid extent must fail before read"),
				|_, _| Ok(())
			)
			.is_err()
	);
}
