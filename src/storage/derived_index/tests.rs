//! Golden sections and adversarial metadata at the graph-artifact boundary.

use super::*;

#[test]
fn arrays_round_trip_every_normative_width_and_reject_corruption() -> FerrosGrynnResult<()> {
	let cases = [
		(SectionKind::NodeComponentMap, 8),
		(SectionKind::EdgeComponentMap, 8),
		(SectionKind::SccRepresentativeMap, 8),
		(SectionKind::CondensationCsrOffsets, 8),
		(SectionKind::CondensationCsrTargets, 8),
		(SectionKind::PathVertexMap, 8),
		(SectionKind::LandmarkRefs, 8),
		(SectionKind::LandmarkDistanceFrom, 8),
		(SectionKind::LandmarkDistanceFrom, 16),
		(SectionKind::LandmarkDistanceTo, 8),
		(SectionKind::LandmarkDistanceTo, 16),
		(SectionKind::DefinitionDescriptor, 1),
		(SectionKind::PostorderRanks, 8),
		(SectionKind::TreeCoverIntervals, 16),
		(SectionKind::GrailIntervals, 16),
		(SectionKind::FerrariIntervals, 24),
		(SectionKind::BflBitsets, 16),
		(SectionKind::BflBitsets, 40),
		(SectionKind::BflBitsets, 1024),
		(SectionKind::ChainCoordinates, 16),
		(SectionKind::ChainReachabilityCells, 8),
		(SectionKind::NeighbourhoodCenters, 8),
		(SectionKind::NeighbourhoodOffsets, 8),
		(SectionKind::NeighbourhoodMembers, 8),
		(SectionKind::NeighbourhoodRegisters, 1),
		(SectionKind::NeighbourhoodLandmarkMembers, 8),
		(SectionKind::NeighbourhoodVertexMap, 8),
	];
	let mut page = Box::new(AlignedPage::zeroed());
	for (kind, width) in cases {
		let payload = vec![17; usize::from(width) * 2];
		encode_array(&mut page, PageNo::new(1), PageNo::NULL, kind, width, 4, &payload)?;
		let decoded = decode_array(&page, PageNo::new(1), 2, kind, width, 4)?;
		assert_eq!(decoded.first_ordinal, 4);
		assert_eq!(decoded.next, PageNo::NULL);
		assert_eq!(decoded.elements, payload);
	}
	let kind = SectionKind::NodeComponentMap;
	let payload = 7_u64.to_le_bytes();
	encode_array(&mut page, PageNo::new(1), PageNo::NULL, kind, 8, 0, &payload)?;
	assert_eq!(&page.as_bytes()[0..8], &[0x46, 0x49, 0x44, 1, 0x3e, 0, 0, 0]);
	assert_eq!(&page.as_bytes()[32..64], &[
		255, 255, 255, 255, 255, 255, 255, 255, 1, 0, 8, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8,
		0, 0, 0, 0, 0, 0, 0,
	]);
	let valid = page.as_bytes().to_vec();
	for (offset, value) in [
		(32, 0),
		(40, 0),
		(40, 0xff),
		(42, 16),
		(44, 0),
		(44, 0xff),
		(48, 1),
		(56, 9),
		(60, 1),
		(72, 1),
	] {
		page.as_bytes_mut().copy_from_slice(&valid);
		page.as_bytes_mut()[offset] = value;
		page.seal()?;
		assert!(decode_array(&page, PageNo::new(1), 2, kind, 8, 0).is_err(), "offset {offset}");
	}
	page.as_bytes_mut().copy_from_slice(&valid);
	assert!(decode_array(&page, PageNo::new(1), 2, SectionKind::EdgeComponentMap, 8, 0).is_err());
	assert!(decode_array(&page, PageNo::new(0), 2, kind, 8, 0).is_err());
	assert!(
		encode_array(&mut page, PageNo::new(1), PageNo::NULL, kind, 8, u64::MAX, &payload).is_err()
	);
	assert!(encode_array(&mut page, PageNo::new(1), PageNo::NULL, kind, 0, 0, &payload).is_err());
	assert!(encode_array(&mut page, PageNo::new(1), PageNo::NULL, kind, 8, 0, &[]).is_err());
	assert!(
		encode_array(
			&mut page,
			PageNo::new(1),
			PageNo::NULL,
			SectionKind::BflBitsets,
			17,
			0,
			&payload
		)
		.is_err()
	);
	assert!(check_reference(3, 3, false).is_err());
	check_reference(2, 3, false)?;
	check_reference(u64::MAX, 0, true)?;
	assert!(check_reference(u64::MAX, 3, false).is_err());
	Ok(())
}

#[test]
fn record_offsets_round_trip_empty_rows_and_reject_malformed_lengths() -> FerrosGrynnResult<()> {
	let kinds = [
		SectionKind::PatternDescriptor,
		SectionKind::PatternCanonicalIr,
		SectionKind::PatternBindingSchema,
		SectionKind::PatternDependencies,
		SectionKind::PatternBindingRows,
	];
	let records: [&[u8]; 3] = [b"a", b"", b"bc"];
	let mut page = Box::new(AlignedPage::zeroed());
	for kind in kinds {
		encode_records(&mut page, PageNo::new(1), PageNo::NULL, kind, 5, &records)?;
		let decoded = decode_records(&page, PageNo::new(1), 2, kind, 5)?;
		assert_eq!(decoded.first_ordinal, 5);
		assert_eq!(decoded.next, PageNo::NULL);
		assert_eq!(decoded.records().collect::<Vec<_>>(), records);
	}
	let kind = SectionKind::PatternBindingRows;
	encode_records(&mut page, PageNo::new(1), PageNo::NULL, kind, 0, &records)?;
	assert_eq!(&page.as_bytes()[0..8], &[0x46, 0x49, 0x44, 1, 0x3f, 0, 0, 0]);
	assert_eq!(&page.as_bytes()[40..48], &[0x14, 0, 3, 0, 11, 0, 0, 0]);
	assert_eq!(&page.as_bytes()[56..67], &[0, 0, 1, 0, 1, 0, 3, 0, b'a', b'b', b'c']);
	let valid = page.as_bytes().to_vec();
	for (offset, value) in [
		(32, 1),
		(40, 0),
		(42, 0),
		(43, 0xff),
		(44, 1),
		(45, 0xff),
		(46, 1),
		(48, 1),
		(56, 1),
		(58, 4),
		(60, 0),
		(62, 2),
		(67, 1),
	] {
		page.as_bytes_mut().copy_from_slice(&valid);
		page.as_bytes_mut()[offset] = value;
		page.seal()?;
		assert!(decode_records(&page, PageNo::new(1), 2, kind, 0).is_err(), "offset {offset}");
	}
	assert!(
		encode_records(
			&mut page,
			PageNo::new(1),
			PageNo::NULL,
			SectionKind::NodeComponentMap,
			0,
			&records
		)
		.is_err()
	);
	assert!(
		encode_records(&mut page, PageNo::new(1), PageNo::NULL, kind, u64::MAX - 1, &records)
			.is_err()
	);
	assert!(
		encode_records(&mut page, PageNo::new(1), PageNo::NULL, kind, 0, &[&vec![0; PAGE_SIZE]])
			.is_err()
	);
	Ok(())
}

#[test]
fn chains_require_contiguous_ordinals_exact_totals_and_forward_links() -> FerrosGrynnResult<()> {
	// Fill final aligned slots before sealing; the vector never reallocates.
	let mut pages = (0..4).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
	let kind = SectionKind::CondensationCsrTargets;
	encode_array(&mut pages[1], PageNo::new(1), PageNo::new(3), kind, 8, 0, &4_u64.to_le_bytes())?;
	encode_array(&mut pages[3], PageNo::new(3), PageNo::NULL, kind, 8, 1, &5_u64.to_le_bytes())?;
	let mut values = Vec::new();
	let read = |at: PageNo, out: &mut AlignedPage| {
		out.as_bytes_mut().copy_from_slice(pages[at.get() as usize].as_bytes());
		Ok(())
	};
	visit_array_section(read, PageNo::new(1), 4, kind, 8, 2, |ordinal, bytes| {
		values.push((ordinal, number(bytes, 0)));
		Ok(())
	})?;
	assert_eq!(values, [(0, 4), (1, 5)]);
	assert!(visit_array_section(read, PageNo::new(1), 4, kind, 8, 3, |_, _| Ok(())).is_err());
	assert!(visit_array_section(read, PageNo::new(1), 4, kind, 8, 1, |_, _| Ok(())).is_err());
	visit_array_section(|_, _| unreachable!(), PageNo::NULL, 1, kind, 8, 0, |_, _| unreachable!())?;
	assert!(
		visit_array_section(
			|_, _| unreachable!(),
			PageNo::NULL,
			1,
			kind,
			8,
			1,
			|_, _| unreachable!()
		)
		.is_err()
	);
	assert!(
		encode_array(
			&mut pages[3],
			PageNo::new(3),
			PageNo::new(1),
			kind,
			8,
			1,
			&5_u64.to_le_bytes()
		)
		.is_err()
	);
	encode_array(&mut pages[3], PageNo::new(3), PageNo::NULL, kind, 8, 2, &5_u64.to_le_bytes())?;
	assert!(
		visit_array_section(
			|at, out| {
				out.as_bytes_mut().copy_from_slice(pages[at.get() as usize].as_bytes());
				Ok(())
			},
			PageNo::new(1),
			4,
			kind,
			8,
			2,
			|_, _| Ok(())
		)
		.is_err()
	);
	let kind = SectionKind::PatternBindingRows;
	encode_records(&mut pages[1], PageNo::new(1), PageNo::new(3), kind, 0, &[b"first"])?;
	encode_records(&mut pages[3], PageNo::new(3), PageNo::NULL, kind, 1, &[b"", b"last"])?;
	let mut values = Vec::new();
	visit_record_section(
		|at, out| {
			out.as_bytes_mut().copy_from_slice(pages[at.get() as usize].as_bytes());
			Ok(())
		},
		PageNo::new(1),
		4,
		kind,
		3,
		|ordinal, bytes| {
			values.push((ordinal, bytes.to_vec()));
			Ok(())
		},
	)?;
	assert_eq!(values, [(0, b"first".to_vec()), (1, vec![]), (2, b"last".to_vec())]);
	visit_record_section(|_, _| unreachable!(), PageNo::NULL, 1, kind, 0, |_, _| unreachable!())?;
	Ok(())
}
