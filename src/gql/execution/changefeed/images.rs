//! Owned GQL image projection and conservative allocation admission.

use crate::{
	changefeed::{
		ChangeBatch,
		ChangeEndpoint,
		ChangeGraphState,
		ChangeIdentity,
		ChangeImage,
		ChangeOverlay,
		ChangefeedGap,
		ChangefeedGapReason,
		ChangefeedReadItem,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomOccurrenceRef, EdgeDirection},
	values::{Array, ErrorValue, Geometry, Object, Position, RecordId, RecordIdPart, Value},
};

/// Charges one inspected value/endpoint before recursion or conversion.
fn step(work: &mut u64, depth: usize) -> FerrosGrynnResult<()> {
	*work = work
		.checked_add(1)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("SHOW CHANGES work"))?;
	if depth > 64 {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"SHOW CHANGES depth",
			depth as u64,
			64,
		));
	}
	if *work > 1_000_000 {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"SHOW CHANGES work",
			*work,
			1_000_000,
		));
	}
	Ok(())
}

/// Complete object nodes are charged even for singleton maps.
fn object_bytes(object: &Object, work: &mut u64, depth: usize) -> FerrosGrynnResult<usize> {
	step(work, depth)?;
	let mut bytes = 2048_usize.saturating_mul(object.len());
	for (key, value) in object {
		bytes =
			bytes.saturating_add(key.len()).saturating_add(value_bytes(value, work, depth + 1)?);
	}
	Ok(bytes)
}

/// Admits nested singleton objects and composite record identities without
/// constructing temporary Values or cloning the source images.
pub(super) fn value_bytes(value: &Value, work: &mut u64, depth: usize) -> FerrosGrynnResult<usize> {
	step(work, depth)?;
	Ok(match value {
		Value::Object(object) => object_bytes(object, work, depth + 1)?,
		Value::Array(array) => {
			let mut bytes = array.len().saturating_mul(size_of::<Value>());
			for value in array {
				bytes = bytes.saturating_add(value_bytes(value, work, depth + 1)?);
			}
			bytes
		}
		Value::Set(set) => {
			let mut bytes = set.len().saturating_mul(2048);
			for value in set {
				bytes = bytes.saturating_add(value_bytes(value, work, depth + 1)?);
			}
			bytes
		}
		Value::Range(range) => {
			let mut bytes = 512_usize;
			for bound in [range.start(), range.end()] {
				if let std::ops::Bound::Included(value) | std::ops::Bound::Excluded(value) = bound {
					bytes = bytes.saturating_add(value_bytes(value, work, depth + 1)?);
				}
			}
			bytes
		}
		Value::Record(record) => record_bytes(record, work, depth + 1)?,
		Value::Atom(atom) => record_bytes(atom.id(), work, depth + 1)?,
		Value::AtomOccurrence(atom) => record_bytes(atom.atom().id(), work, depth + 1)?
			.saturating_add(atom.graph().name().len()),
		Value::Path(path) => {
			let mut bytes = 512_usize;
			for entry in path.steps() {
				step(work, depth + 1)?;
				for atom in std::iter::once(entry.atom()).chain(entry.via_edge()) {
					bytes = bytes
						.saturating_add(record_bytes(atom.atom().id(), work, depth + 2)?)
						.saturating_add(atom.graph().name().len());
				}
			}
			bytes
		}
		Value::Geometry(geometry) => geometry_bytes(geometry, work, depth + 1)?,
		Value::Error(error) => error_bytes(error, work, depth + 1)?,
		_ => value.estimated_heap_bytes().saturating_mul(2).saturating_add(256),
	})
}

/// Measures one bounded coordinate sequence without conversion or allocation.
fn positions_bytes(
	positions: &[Position],
	work: &mut u64,
	depth: usize,
) -> FerrosGrynnResult<usize> {
	step(work, depth)?;
	let mut bytes = 256_usize;
	for position in positions {
		step(work, depth + 1)?;
		for _coordinate in position.as_slice() {
			step(work, depth + 2)?;
		}
		bytes = bytes
			.saturating_add(128)
			.saturating_add(position.as_slice().len().saturating_mul(size_of::<f64>()));
	}
	Ok(bytes)
}

/// Checks all container depths and positions before Geometry's recursive Hash.
fn geometry_bytes(geometry: &Geometry, work: &mut u64, depth: usize) -> FerrosGrynnResult<usize> {
	step(work, depth)?;
	let mut bytes = 256_usize;
	match geometry {
		Geometry::Point(point) => {
			bytes = bytes.saturating_add(positions_bytes(
				std::slice::from_ref(point),
				work,
				depth + 1,
			)?);
		}
		Geometry::MultiPoint(points) | Geometry::LineString(points) => {
			bytes = bytes.saturating_add(positions_bytes(points, work, depth + 1)?);
		}
		Geometry::MultiLineString(lines) | Geometry::Polygon(lines) => {
			for line in lines {
				bytes = bytes.saturating_add(positions_bytes(line, work, depth + 1)?);
			}
		}
		Geometry::MultiPolygon(polygons) => {
			for polygon in polygons {
				step(work, depth + 1)?;
				bytes = bytes.saturating_add(256);
				for ring in polygon {
					bytes = bytes.saturating_add(positions_bytes(ring, work, depth + 2)?);
				}
			}
		}
		Geometry::Collection(members) => {
			for member in members {
				bytes = bytes.saturating_add(geometry_bytes(member, work, depth + 1)?);
			}
		}
	}
	Ok(bytes)
}

/// Checks cause depth and includes every diagnostic string before Error Hash.
fn error_bytes(error: &ErrorValue, work: &mut u64, depth: usize) -> FerrosGrynnResult<usize> {
	step(work, depth)?;
	let mut bytes = 512_usize;
	for text in [
		error.code(),
		error.message(),
		error.condition(),
		error.classification(),
		error.category(),
		error.domain(),
	] {
		bytes = bytes.saturating_add(text.len());
	}
	for cause in error.causes() {
		bytes = bytes.saturating_add(error_bytes(cause, work, depth + 1)?);
	}
	Ok(bytes)
}

/// Includes child values of array/object RecordIds in the same checked walk.
fn record_bytes(record: &RecordId, work: &mut u64, depth: usize) -> FerrosGrynnResult<usize> {
	step(work, depth)?;
	let part = match record.id() {
		RecordIdPart::Object(object) => object_bytes(object, work, depth + 1)?,
		RecordIdPart::Array(array) => {
			let mut bytes = array.len().saturating_mul(size_of::<Value>());
			for value in array {
				bytes = bytes.saturating_add(value_bytes(value, work, depth + 1)?);
			}
			bytes
		}
		RecordIdPart::String(text) => text.len(),
		_ => 0,
	};
	Ok(512_usize.saturating_add(record.class().len()).saturating_add(part))
}

/// One qualified occurrence copied into a historical topology object.
fn occurrence_bytes(atom: &AtomOccurrenceRef, work: &mut u64) -> FerrosGrynnResult<usize> {
	record_bytes(atom.atom().id(), work, 0)
		.map(|bytes| bytes.saturating_add(atom.graph().name().len()))
}

/// Complete image, including explicit overlays and qualified endpoint routes.
fn image_bytes(image: &ChangeImage, work: &mut u64) -> FerrosGrynnResult<usize> {
	let mut bytes = 4096_usize.saturating_add(object_bytes(image.properties(), work, 0)?);
	bytes = bytes.saturating_add(image.label().map_or(0, |label| label.len()));
	bytes = bytes.saturating_add(match image.identity() {
		ChangeIdentity::Record(atom) => record_bytes(atom.id(), work, 0)?,
		ChangeIdentity::Occurrence(atom) => occurrence_bytes(atom, work)?,
	});
	for (key, overlay) in image.overlays() {
		bytes = bytes.saturating_add(4096).saturating_add(key.len());
		if let ChangeOverlay::Value(value) = overlay {
			bytes = bytes.saturating_add(value_bytes(value, work, 0)?);
		}
	}
	if let Some(ChangeGraphState::Edge {
		source,
		target,
		..
	}) = image.graph_state()
	{
		for endpoint in source.iter().chain(target) {
			bytes = bytes
				.saturating_add(4096)
				.saturating_add(occurrence_bytes(endpoint.occurrence(), work)?);
			for owner in endpoint.owners() {
				bytes = bytes.saturating_add(occurrence_bytes(owner, work)?);
			}
		}
	}
	Ok(bytes)
}

/// Accounts source batch storage and the complete output while both are live.
pub(super) fn item_bytes(item: &ChangefeedReadItem, work: &mut u64) -> FerrosGrynnResult<usize> {
	let mut bytes = 4096_usize;
	if let ChangefeedReadItem::Commit(batch) = item {
		for entry in batch.entries() {
			step(work, 0)?;
			bytes = bytes.saturating_add(4096);
			for image in entry.before().into_iter().chain(entry.after()) {
				bytes = bytes.saturating_add(image_bytes(image, work)?.saturating_mul(2));
			}
		}
	}
	Ok(bytes)
}

/// Projects qualified endpoint identity and its independent containment route.
fn endpoint_value(endpoint: &ChangeEndpoint) -> Value {
	let mut result = Object::new();
	result.insert("occurrence", endpoint.occurrence().clone());
	result
		.insert("owners", Array::new(endpoint.owners().iter().cloned().map(Value::from).collect()));
	Value::Object(result)
}

/// Preserves exact runtime Numbers and graph references in owned images.
fn image_value(image: &ChangeImage) -> Value {
	let mut result = Object::new();
	result.insert("identity", match image.identity() {
		ChangeIdentity::Record(atom) => Value::from(atom.clone()),
		ChangeIdentity::Occurrence(atom) => Value::from(atom.clone()),
	});
	result.insert("properties", image.properties().clone());
	result.insert("label", image.label().cloned().map_or(Value::None, Value::String));
	let mut overlays = Object::new();
	for (key, decision) in image.overlays() {
		let mut overlay = Object::new();
		match decision {
			ChangeOverlay::Hidden => {
				overlay.insert("kind", "hidden");
			}
			ChangeOverlay::Value(value) => {
				overlay.insert("kind", "value");
				overlay.insert("value", value.clone());
			}
		}
		overlays.insert(key.clone(), overlay);
	}
	result.insert("overlays", overlays);
	let topology = image.graph_state().map_or(Value::None, |state| {
		let mut result = Object::new();
		match state {
			ChangeGraphState::Node {
				nested_graph,
			} => {
				result.insert("kind", "node");
				result
					.insert("nested_graph", nested_graph.clone().map_or(Value::None, Value::from));
			}
			ChangeGraphState::Edge {
				direction,
				source,
				target,
				weight,
				transition_graph,
			} => {
				result.insert("kind", "edge");
				result.insert("direction", match direction {
					EdgeDirection::Directed => "directed",
					EdgeDirection::Bidirectional => "bidirectional",
					EdgeDirection::Undirected => "undirected",
				});
				result.insert("source", Array::new(source.iter().map(endpoint_value).collect()));
				result.insert("target", Array::new(target.iter().map(endpoint_value).collect()));
				result.insert("weight", weight.map_or(Value::None, Value::Number));
				result.insert(
					"transition_graph",
					transition_graph.clone().map_or(Value::None, Value::from),
				);
			}
		}
		Value::Object(result)
	});
	result.insert("topology", topology);
	Value::Object(result)
}

/// Retains operation order and all entries of the same commit.
pub(super) fn entries(batch: &ChangeBatch) -> Value {
	Value::Array(Array::new(
		batch
			.entries()
			.iter()
			.map(|entry| {
				let mut result = Object::new();
				result.insert("ordinal", entry.ordinal());
				result.insert("operation", entry.operation());
				result.insert("before", entry.before().map_or(Value::None, image_value));
				result.insert("after", entry.after().map_or(Value::None, image_value));
				Value::Object(result)
			})
			.collect(),
	))
}

/// Explicit unknown bounds remain missing instead of fabricated stamps.
pub(super) fn gap(gap: &ChangefeedGap) -> Object {
	let mut result = Object::new();
	result.insert("kind", "gap");
	result.insert("source", gap.source().as_uuid());
	result.insert("reason", match gap.reason() {
		ChangefeedGapReason::SourceDropped => "SourceDropped",
		ChangefeedGapReason::ImportSuppressed => "ImportSuppressed",
		ChangefeedGapReason::Retention => "Retention",
		ChangefeedGapReason::SegmentCorrupt => "SegmentCorrupt",
		ChangefeedGapReason::FeedExpiredOrRetired => "FeedExpiredOrRetired",
	});
	result.insert("from", gap.from().map_or(Value::None, |stamp| Value::from(stamp.get())));
	result.insert("through", gap.through().map_or(Value::None, |stamp| Value::from(stamp.get())));
	result
}
