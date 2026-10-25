use super::{build_graph_image, open_graph_image};
#[cfg(test)]
use crate::test_assertions::AssertOption as _;
use crate::{
	datastore::{
		ClassId,
		ClassSchema,
		DatabaseRoot,
		GraphSchema,
		IndexId,
		SchemaVersion,
		SystemIndex,
		SystemIndexKind,
	},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, EdgeTopology, GraphId, GraphKind, GraphRef, GraphStorageMode},
	storage::page::PageLsn,
	values::{
		PropertyDictionary,
		PropertyKeyId,
		PropertyMap,
		RecordId,
		RecordIdPart,
		Value,
		storage::EncodeContext,
	},
};

/// Verifies occurrence image round trips identity topology labels and overlay.
#[test]
fn occurrence_image_round_trips_identity_topology_labels_and_overlay() {
	use crate::test_assertions::AssertResult as _;

	let person = class(1, "person", AtomKind::Node).assert_ok();
	let relation = class(2, "knows", AtomKind::Edge).assert_ok();
	let graph_id = GraphId::new(7).assert_some();
	let reference =
		GraphRef::new(graph_id, "social", GraphKind::Graph, GraphStorageMode::DiskBacked)
			.assert_ok();
	let mut database = DatabaseRoot::default();
	database
		.install_graph(
			GraphSchema::new(
				SchemaVersion::INITIAL,
				reference,
				SystemIndex::new(IndexId::new(10).assert_some(), SystemIndexKind::Label),
				SystemIndex::new(IndexId::new(11).assert_some(), SystemIndexKind::Adjacency),
				PropertyMap::new(),
				crate::graph::GraphTopologyConstraints::empty(),
				None,
				None,
				0,
			),
			0,
		)
		.assert_ok();
	let left = database
		.insert_atom(
			&person,
			RecordId::new("person", RecordIdPart::String("left".into())).assert_ok(),
			PropertyMap::new(),
		)
		.assert_ok();
	let right = database
		.insert_atom(
			&person,
			RecordId::new("person", RecordIdPart::String("right".into())).assert_ok(),
			PropertyMap::new(),
		)
		.assert_ok();
	let relation = database
		.insert_atom(
			&relation,
			RecordId::new("knows", RecordIdPart::String("edge".into())).assert_ok(),
			PropertyMap::new(),
		)
		.assert_ok();
	let left = database.attach_node(graph_id, left, Some("Person".into()), None).assert_ok();
	let right = database.attach_node(graph_id, right, Some("Person".into()), None).assert_ok();
	let edge = database
		.attach_edge(
			graph_id,
			relation,
			Some("Friend".into()),
			EdgeTopology::directed([left], [right]),
			None,
			None,
		)
		.assert_ok();
	let key = PropertyKeyId::new(9);
	database.set_overlay(graph_id, left, key, Value::from("private")).assert_ok();

	let expected = database.graph(graph_id).assert_some().clone();
	let mut context = EncodeContext::new(PropertyDictionary::new());
	let image = build_graph_image(&database, graph_id, &mut context, PageLsn::new(11)).assert_ok();
	assert_eq!(image.superblock.node_count, 2);
	assert_eq!(image.superblock.edge_count, 1);
	assert!(!image.overlays.is_empty());
	let decoder = context.decoder();
	let opened = open_graph_image(image, &database, &decoder).assert_ok();

	assert_eq!(
		opened.occurrences().collect::<Vec<_>>(),
		expected.occurrences().collect::<Vec<_>>()
	);
	assert_eq!(opened.local_id(expected.occurrence(edge).assert_some().global_id()), Some(edge));
}

/// Returns class for this module.
fn class(id: u32, name: &str, kind: AtomKind) -> FerrosGrynnResult<ClassSchema> {
	let mut class = ClassSchema::new(name, kind, [])?;
	class.id = ClassId::new(id);
	Ok(class)
}

/// Real writers preserve full qualified roles, side tags, order and duplicates.
#[test]
fn hot_side_writers_preserve_local_suffix_and_whole_qualified_roles() {
	use super::{
		builders::{OverflowBuilder, QualifiedOverflowBuilder},
		topology::encode_hot_side,
	};
	use crate::{
		storage::{
			graph_pages::{
				EncodedEndpoint,
				EndpointSide,
				decode_adjacency_overflow,
				decode_cross_level_overflow,
			},
			page::PageNo,
		},
		test_assertions::AssertResult as _,
		values::storage::DiskAtomRef,
	};
	let a = DiskAtomRef::from_parts(AtomKind::Node, 4).assert_ok();
	let b = DiskAtomRef::from_parts(AtomKind::Edge, 9).assert_ok();
	for (scopes, cross) in [([0, 0, 0], false), ([7, 7, 7], true), ([0, 7, 0], true)] {
		for side in [EndpointSide::Source, EndpointSide::Target] {
			let entries = scopes
				.into_iter()
				.zip([a, b, a])
				.map(|(graph_slot, atom)| EncodedEndpoint {
					graph_slot,
					atom,
				})
				.collect::<Vec<_>>();
			let mut local = OverflowBuilder::default();
			let mut qualified = QualifiedOverflowBuilder::default();
			let encoded =
				encode_hot_side(&entries, side, cross, PageLsn::ZERO, &mut local, &mut qualified)
					.assert_ok();
			let local = local.finish();
			let qualified = qualified.finish();
			if cross {
				assert_eq!(encoded.inline, [None; 2]);
				assert_eq!(encoded.graph_slot, None);
				assert_eq!(encoded.local_overflow, None);
				assert_eq!(encoded.graph_overflow, Some(PageNo::new(0)));
				assert!(local.is_empty());
				assert_eq!(qualified.len(), 1);
				let (next, decoded) =
					decode_cross_level_overflow(&qualified[0], PageNo::new(0)).assert_ok();
				assert_eq!(next, None);
				assert_eq!(
					decoded
						.iter()
						.map(|entry| (entry.graph_slot, entry.atom, entry.side))
						.collect::<Vec<_>>(),
					vec![(scopes[0], a, side), (scopes[1], b, side), (scopes[2], a, side)]
				);
			} else {
				assert_eq!(encoded.inline, [Some(a), Some(b)]);
				assert_eq!(encoded.graph_slot, None);
				assert_eq!(encoded.local_overflow, Some(PageNo::new(0)));
				assert_eq!(encoded.graph_overflow, None);
				assert!(qualified.is_empty());
				assert_eq!(local.len(), 1);
				let (next, decoded) =
					decode_adjacency_overflow(&local[0], PageNo::new(0)).assert_ok();
				assert_eq!(next, None);
				assert_eq!(decoded, vec![a]);
			}
		}
	}
}
