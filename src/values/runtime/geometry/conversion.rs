//! Checked conversion from owned GeoJSON-shaped runtime objects.

use super::{
	Geometry,
	Position,
	kernels::{MAX_DEPTH, charge},
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Object, Value},
};

impl Geometry {
	/// Converts a GeoJSON-shaped object while retaining owned runtime types.
	///
	/// # Errors
	/// Rejects missing members, incorrect types, invalid coordinates or
	/// topology, and collection/depth limits before constructing kernel
	/// storage.
	pub fn from_object(object: &Object) -> FerrosGrynnResult<Self> {
		let geometry = from_object(object, 0, &mut 0)?;
		geometry.validate()?;
		Ok(geometry)
	}
}

/// Reads one object with the same aggregate limits as GeoJSON conversion.
fn from_object(object: &Object, depth: usize, elements: &mut usize) -> FerrosGrynnResult<Geometry> {
	if depth > MAX_DEPTH {
		return Err(invalid("geometry exceeds the nesting limit"));
	}
	let name = object
		.get("type")
		.and_then(Value::as_str)
		.ok_or_else(|| invalid("GeoJSON requires a string type"))?;
	if name == "GeometryCollection" {
		return sequence(member(object, "geometries")?, elements, |value, elements| {
			let object = value
				.as_object()
				.ok_or_else(|| invalid("collection members must be geometry objects"))?;
			from_object(object, depth + 1, elements)
		})
		.map(Geometry::Collection);
	}
	let coordinates = member(object, "coordinates")?;
	match name {
		"Point" => position(coordinates).map(Geometry::Point),
		"MultiPoint" => positions(coordinates, elements).map(Geometry::MultiPoint),
		"LineString" => positions(coordinates, elements).map(Geometry::LineString),
		"MultiLineString" => {
			sequence(coordinates, elements, positions).map(Geometry::MultiLineString)
		}
		"Polygon" => sequence(coordinates, elements, positions).map(Geometry::Polygon),
		"MultiPolygon" => {
			sequence(coordinates, elements, |value, elements| sequence(value, elements, positions))
				.map(Geometry::MultiPolygon)
		}
		_ => Err(invalid("unknown GeoJSON geometry type")),
	}
}

/// Resolves required fields without a sentinel or default conversion.
fn member<'a>(object: &'a Object, field: &str) -> FerrosGrynnResult<&'a Value> {
	object.get(field).ok_or_else(|| invalid("required GeoJSON member is absent"))
}

/// Checks each container before allocating its converted elements.
fn sequence<T>(
	value: &Value,
	elements: &mut usize,
	mut convert: impl FnMut(&Value, &mut usize) -> FerrosGrynnResult<T>,
) -> FerrosGrynnResult<Vec<T>> {
	let array = value
		.as_array()
		.ok_or_else(|| invalid("GeoJSON coordinates and members require arrays"))?;
	charge(elements, array.len())?;
	array.iter().map(|value| convert(value, elements)).collect()
}

/// Converts a bounded set of positions.
fn positions(value: &Value, elements: &mut usize) -> FerrosGrynnResult<Vec<Position>> {
	sequence(value, elements, |value, _| position(value))
}

/// Checks dimension before numeric conversion or coordinate allocation.
fn position(value: &Value) -> FerrosGrynnResult<Position> {
	let array = value.as_array().ok_or_else(|| invalid("a position requires an array"))?;
	if array.len() != 2 {
		return Err(FerrosGrynnError::invalid_geometry_coordinate_dimension(array.len() as u64));
	}
	let number = |value: &Value| {
		value
			.as_number()
			.ok_or_else(|| invalid("coordinates require numbers"))
			.and_then(f64::try_from)
	};
	Position::new([number(&array.as_slice()[0])?, number(&array.as_slice()[1])?])
}

/// Creates a redacted generated value error.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_geometry_structure(reason)
}
