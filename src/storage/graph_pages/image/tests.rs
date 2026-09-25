use super::{build_graph_image, open_graph_image};
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

#[test]
fn occurrence_image_round_trips_identity_topology_labels_and_overlay() -> FerrosGrynnResult<()> {
	let person = class(1, "person", AtomKind::Node)?;
	let relation = class(2, "knows", AtomKind::Edge)?;
	let graph_id = GraphId::new(7).unwrap();
	let reference =
		GraphRef::new(graph_id, "social", GraphKind::Graph, GraphStorageMode::DiskBacked)?;
	let mut database = DatabaseRoot::default();
	database.install_graph(
		GraphSchema::new(
			SchemaVersion::INITIAL,
			reference,
			SystemIndex::new(IndexId::new(10).unwrap(), SystemIndexKind::Label),
			SystemIndex::new(IndexId::new(11).unwrap(), SystemIndexKind::Adjacency),
			PropertyMap::new(),
			0,
		),
		0,
	)?;
	let left = database.insert_atom(
		&person,
		RecordId::new("person", RecordIdPart::String("left".into()))?,
		PropertyMap::new(),
	)?;
	let right = database.insert_atom(
		&person,
		RecordId::new("person", RecordIdPart::String("right".into()))?,
		PropertyMap::new(),
	)?;
	let relation = database.insert_atom(
		&relation,
		RecordId::new("knows", RecordIdPart::String("edge".into()))?,
		PropertyMap::new(),
	)?;
	let left = database.attach_node(graph_id, left, Some("Person".into()), None)?;
	let right = database.attach_node(graph_id, right, Some("Person".into()), None)?;
	let edge = database.attach_edge(
		graph_id,
		relation,
		Some("Friend".into()),
		EdgeTopology::directed([left], [right]),
		None,
		None,
	)?;
	let key = PropertyKeyId::new(9);
	database.set_overlay(graph_id, left, key, Value::from("private"))?;

	let expected = database.graph(graph_id).unwrap().clone();
	let mut context = EncodeContext::new(PropertyDictionary::new());
	let image = build_graph_image(&database, graph_id, &mut context, PageLsn::new(11))?;
	assert_eq!(image.superblock.node_count, 2);
	assert_eq!(image.superblock.edge_count, 1);
	assert!(!image.overlays.is_empty());
	let decoder = context.decoder();
	let opened = open_graph_image(image, &database, &decoder)?;

	assert_eq!(
		opened.occurrences().collect::<Vec<_>>(),
		expected.occurrences().collect::<Vec<_>>()
	);
	assert_eq!(opened.local_id(expected.occurrence(edge).unwrap().global_id()), Some(edge));
	Ok(())
}

fn class(id: u32, name: &str, kind: AtomKind) -> FerrosGrynnResult<ClassSchema> {
	let mut class = ClassSchema::new(name, kind, [])?;
	class.id = ClassId::new(id);
	Ok(class)
}
