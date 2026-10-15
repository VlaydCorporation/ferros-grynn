//! Exact ownership/packing and malformed reachability metadata checks.

use super::*;
#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::{
		ClassSchema,
		Datastore,
		GraphIndex,
		GraphIndexDefinition,
		ReachabilityAlgorithm,
		ReachabilityArtifact,
	},
	graph::{AtomKind, EdgeTopology, GraphKind},
	storage::page::PAGE_SIZE,
	values::{PropertyMap, RecordId},
};

/// Verifies snapshot artifacts pack complete definition maps and same
/// condensation.
#[test]
fn snapshot_artifacts_pack_complete_definition_maps_and_same_condensation() {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = tx.define_graph("network", GraphKind::Graph).assert_ok();
	let mut nodes = Vec::new();
	for value in 0..7 {
		let record = format!("person:{value}").parse::<RecordId>().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		nodes.push(tx.attach_node(graph.id(), &record, None, None).assert_ok());
	}
	for (ordinal, (source, target)) in
		[(0, 1), (1, 0), (1, 2), (2, 3), (4, 5)].into_iter().enumerate()
	{
		let record = format!("link:{ordinal}").parse::<RecordId>().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		tx.attach_edge(
			graph.id(),
			&record,
			Some("keep".into()),
			EdgeTopology::directed([nodes[source]], [nodes[target]]),
			None,
			None,
		)
		.assert_ok();
	}
	for (ordinal, algorithm) in [
		ReachabilityAlgorithm::Grail(2),
		ReachabilityAlgorithm::Ferrari(4),
		ReachabilityAlgorithm::Bfl(160),
		ReachabilityAlgorithm::ChainCover,
	]
	.into_iter()
	.enumerate()
	{
		tx.define_graph_index(
			"network",
			GraphIndex::new(
				format!("reach{ordinal}"),
				GraphIndexDefinition::Reachability {
					algorithm,
				},
				Some(8 << 20),
			)
			.assert_ok()
			.with_edge_labels(["keep"])
			.assert_ok(),
		)
		.assert_ok();
	}
	tx.commit().assert_ok();
	let database = store.inner.current_root();
	for index in database.schema.graph(graph.id()).assert_some().indexes() {
		let artifact = ReachabilityArtifact::build(
			&database,
			index,
			database.commit_ts + 1,
			8 << 20,
			&mut || Ok(()),
		)
		.assert_ok()
		.assert_some();
		assert_eq!(artifact.descriptor, index.descriptor().assert_ok());
		assert_eq!(artifact.layout.header.identity.fingerprint, index.fingerprint().assert_ok());
		assert_eq!(
			artifact.node_components[nodes[0].slot() as usize],
			artifact.node_components[nodes[1].slot() as usize]
		);
		assert!(artifact.edge_components.iter().all(|component| *component == u64::MAX));
		let mut pages = Vec::with_capacity(artifact.layout.page_count as usize);
		pages.resize_with(
			artifact.layout.page_count as usize,
			crate::storage::page::AlignedPage::zeroed,
		);
		let mut next_page = 0;
		artifact
			.layout
			.stream(
				|kind, ordinal, bytes| artifact.encode_element(kind, ordinal, bytes),
				|at, page| {
					assert_eq!(at.get(), next_page);
					next_page += 1;
					pages[at.get() as usize].as_bytes_mut().copy_from_slice(page.as_bytes());
					Ok(())
				},
				&mut || Ok(()),
			)
			.assert_ok();
		assert_eq!(next_page, artifact.layout.page_count);
		let decoded = crate::storage::ReachabilityLayout::decode_header(
			&pages[0],
			artifact.layout.header.identity,
			pages.len() as u64,
		)
		.assert_ok();
		for section in decoded.sections {
			section
				.visit(
					decoded.page_count,
					|at, page| {
						page.as_bytes_mut().copy_from_slice(pages[at.get() as usize].as_bytes());
						Ok(())
					},
					|ordinal, bytes| {
						let mut expected = vec![0; bytes.len()];
						artifact.encode_element(section.kind, ordinal, &mut expected)?;
						assert_eq!(bytes, expected);
						Ok(())
					},
				)
				.assert_ok();
		}
		let mut stored_index = index.clone();
		stored_index.build_revision = artifact.layout.header.identity.revision;
		stored_index.state = crate::datastore::ClassIndexState::Ready;
		let data = decoded
			.load_sections(
				8 << 20,
				|at, page| {
					page.as_bytes_mut().copy_from_slice(pages[at.get() as usize].as_bytes());
					Ok(())
				},
				&mut || Ok(()),
			)
			.assert_ok();
		let restored = ReachabilityArtifact::restore(
			database.graph(graph.id()).assert_some(),
			&stored_index,
			decoded.clone(),
			data.clone(),
			8 << 20,
			&mut || Ok(()),
		)
		.assert_ok();
		assert_eq!(restored.node_components, artifact.node_components);
		assert_eq!(restored.condensation.forward, artifact.condensation.forward);
		assert_eq!(restored.condensation.reverse, artifact.condensation.reverse);
		assert_eq!(restored.condensation.representatives, artifact.condensation.representatives);
		for section in restored.layout.sections {
			for ordinal in 0..section.count {
				let mut expected = vec![0; usize::from(section.width)];
				let mut actual = expected.clone();
				artifact.encode_element(section.kind, ordinal, &mut expected).assert_ok();
				restored.encode_element(section.kind, ordinal, &mut actual).assert_ok();
				assert_eq!(actual, expected);
			}
		}
		for section in [0, 2, 3, 7] {
			let mut malformed = data.clone();
			malformed[section][0] = 255;
			assert!(
				ReachabilityArtifact::restore(
					database.graph(graph.id()).assert_some(),
					&stored_index,
					decoded.clone(),
					malformed,
					8 << 20,
					&mut || Ok(())
				)
				.is_err(),
				"section {section}"
			);
		}
		let read_calls = std::cell::Cell::new(0);
		assert!(
			decoded
				.load_sections(
					1,
					|_, _| {
						read_calls.set(read_calls.get() + 1);
						Ok(())
					},
					&mut || Ok(())
				)
				.is_err()
		);
		assert_eq!(read_calls.get(), 0, "allocation must be rejected before reads");
		let mut collision = data;
		collision[7][0] ^= 1;
		assert!(
			ReachabilityArtifact::restore(
				database.graph(graph.id()).assert_some(),
				&stored_index,
				decoded,
				collision,
				8 << 20,
				&mut || Ok(())
			)
			.is_err()
		);
		assert!(
			artifact
				.encode_element(
					crate::storage::DerivedSectionKind::TreeCoverIntervals,
					u64::MAX,
					&mut [0; 16]
				)
				.is_err()
		);
		let mut capped = index.clone();
		capped.max_bytes = Some(artifact.layout.page_count * 16_384 - 1);
		assert!(
			ReachabilityArtifact::build(
				&database,
				&capped,
				database.commit_ts + 1,
				8 << 20,
				&mut || Ok(())
			)
			.is_err()
		);
	}
}

/// A sparse projection whose descriptor crosses a full array page.
fn header(algorithm: ReachabilityTag, parameter: u32, chains: u64) -> ReachabilityHeader {
	ReachabilityHeader {
		identity: GraphArtifactIdentity {
			index: 3,
			version: 4,
			graph: 9,
			revision: 17,
			fingerprint: 23,
		},
		node_capacity: 2041,
		edge_capacity: 3,
		components: 1021,
		edges: 2041,
		algorithm,
		parameter,
		chains,
		direction: 1,
		mode: 1,
		reversed: false,
		has_cross_level: true,
		max_bytes: 64 * 1024 * 1024,
		descriptor_bytes: 16_321,
	}
}

/// Fills final page slots without moving populated aligned pages.
fn image(layout: &ReachabilityLayout) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let mut pages = Vec::with_capacity(layout.page_count as usize);
	pages.resize_with(layout.page_count as usize, AlignedPage::zeroed);
	layout.stream(
		|kind, ordinal, bytes| {
			bytes.fill((ordinal % 251) as u8 ^ kind as u8);
			Ok(())
		},
		|at, page| {
			pages[at.get() as usize].as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		},
		&mut || Ok(()),
	)?;
	Ok(pages)
}

/// Verifies every algorithm round trips exact full page projection and payload.
#[test]
fn every_algorithm_round_trips_exact_full_page_projection_and_payload() {
	use crate::test_assertions::AssertResult as _;

	for (algorithm, parameter, chains) in [
		(ReachabilityTag::TreeCover, 0, 0),
		(ReachabilityTag::Grail, 32, 0),
		(ReachabilityTag::Ferrari, 4, 0),
		(ReachabilityTag::Bfl, 4096, 0),
		(ReachabilityTag::ChainCover, 0, 7),
	] {
		let layout = ReachabilityLayout::new(header(algorithm, parameter, chains)).assert_ok();
		let pages = image(&layout).assert_ok();
		let decoded = ReachabilityLayout::decode_header(
			&pages[0],
			layout.header.identity,
			pages.len() as u64,
		)
		.assert_ok();
		assert_eq!(decoded, layout);
		assert_eq!(&pages[0].as_bytes()[32..40], &[3, 0, 0, 0, 4, 0, 0, 0]);
		assert_eq!(&pages[0].as_bytes()[196..200], &[algorithm as u8, 1, 1, 2]);
		for section in decoded.sections {
			let mut visited = 0;
			section
				.visit(
					decoded.page_count,
					|at, page| {
						page.as_bytes_mut().copy_from_slice(pages[at.get() as usize].as_bytes());
						Ok(())
					},
					|ordinal, bytes| {
						assert_eq!(ordinal, visited);
						assert!(
							bytes
								.iter()
								.all(|byte| *byte == (ordinal % 251) as u8 ^ section.kind as u8)
						);
						visited += 1;
						Ok(())
					},
				)
				.assert_ok();
			assert_eq!(visited, section.count);
		}
		let exact_bytes = layout.page_count * PAGE_SIZE as u64;
		let mut capped = layout.header;
		capped.max_bytes = exact_bytes;
		ReachabilityLayout::new(capped).assert_ok();
		capped.max_bytes -= 1;
		assert!(ReachabilityLayout::new(capped).is_err());
	}
}

/// Verifies header and sections reject adversarial roots tags ordinals and
/// reserved bytes.
#[test]
fn header_and_sections_reject_adversarial_roots_tags_ordinals_and_reserved_bytes() {
	use crate::test_assertions::AssertResult as _;

	let layout = ReachabilityLayout::new(header(ReachabilityTag::Grail, 2, 0)).assert_ok();
	let mut pages = image(&layout).assert_ok();
	let original = pages[0].as_bytes().to_vec();
	for (at, value) in [
		(32, 0),
		(36, 0),
		(40, 0),
		(48, 0),
		(79, 255),
		(88, 0),
		(96, 1),
		(144, 1),
		(160, 0),
		(168, 1),
		(176, 0),
		(184, 0),
		(192, 0),
		(196, 5),
		(197, 3),
		(198, 2),
		(199, 4),
		(200, 1),
	] {
		pages[0].as_bytes_mut().copy_from_slice(&original);
		pages[0].as_bytes_mut()[at] = value;
		pages[0].seal().assert_ok();
		assert!(
			ReachabilityLayout::decode_header(&pages[0], layout.header.identity, layout.page_count)
				.is_err(),
			"header byte {at}"
		);
	}
	pages[0].as_bytes_mut().copy_from_slice(&original);
	assert!(
		ReachabilityLayout::decode_header(&pages[0], layout.header.identity, layout.page_count + 1)
			.is_err()
	);
	let section = layout.sections[0];
	let at = section.root.get() as usize;
	let original = pages[at].as_bytes().to_vec();
	for (offset, value) in [(32, 0), (40, 0), (42, 1), (44, 0), (48, 1), (56, 0), (60, 1)] {
		pages[at].as_bytes_mut().copy_from_slice(&original);
		pages[at].as_bytes_mut()[offset] = value;
		pages[at].seal().assert_ok();
		assert!(
			section
				.visit(
					layout.page_count,
					|number, page| {
						page.as_bytes_mut()
							.copy_from_slice(pages[number.get() as usize].as_bytes());
						Ok(())
					},
					|_, _| Ok(())
				)
				.is_err(),
			"section byte {offset}"
		);
	}
}

/// Verifies empty projection and budget overflow cancellation fail before
/// publication.
#[test]
fn empty_projection_and_budget_overflow_cancellation_fail_before_publication() {
	use crate::test_assertions::AssertResult as _;

	let mut empty = header(ReachabilityTag::ChainCover, 0, 0);
	empty.node_capacity = 0;
	empty.edge_capacity = 0;
	empty.components = 0;
	empty.edges = 0;
	empty.descriptor_bytes = 1;
	let layout = ReachabilityLayout::new(empty).assert_ok();
	assert_eq!(layout.page_count, 3);
	assert!(layout.sections[..3].iter().all(|section| section.root == PageNo::NULL));
	assert_eq!(layout.sections[3].count, 1);
	image(&layout).assert_ok();
	let mut writes = 0;
	assert!(
		layout
			.stream(
				|_, _, _| Ok(()),
				|_, _| {
					writes += 1;
					Ok(())
				},
				&mut || Err(invalid("cancelled"))
			)
			.is_err()
	);
	assert_eq!(writes, 0);
	let mut overflow = layout.header;
	overflow.node_capacity = u64::MAX;
	assert!(ReachabilityLayout::new(overflow).is_err());
	let mut forged = layout;
	forged.page_count += 1;
	assert!(
		forged
			.stream(
				|_, _, _| Ok(()),
				|_, _| {
					writes += 1;
					Ok(())
				},
				&mut || Ok(())
			)
			.is_err()
	);
	assert_eq!(writes, 0);
}
