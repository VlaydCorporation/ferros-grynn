//! Shared, bounded WGS-84 validation and planar OGC predicates.

use geo::{BoundingRect, Distance, Geodesic, Intersects, Relate, Validation};

use super::{GeoJsonValue, Geometry, Position, topology};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Maximum collection nesting accepted at every geometry boundary.
pub(super) const MAX_DEPTH: usize = 64;
/// Aggregate positions and container members, checked before kernel allocation.
pub(super) const MAX_ELEMENTS: usize = 1_000_000;

impl Geometry {
	/// Checks coordinate, structure, topology and resource limits.
	///
	/// # Errors
	/// Returns a typed value error for malformed or oversized geometries.
	pub fn validate(&self) -> FerrosGrynnResult<()> {
		self.checked_geo().map(drop)
	}

	/// Tests OGC Within using linear longitude/latitude segments.
	///
	/// # Errors
	/// Returns a typed value error if either geometry is invalid.
	pub fn within(&self, other: &Self) -> FerrosGrynnResult<bool> {
		let left = self.checked_geo()?;
		let right = other.checked_geo()?;
		if matches!(left, geo::Geometry::GeometryCollection(_))
			|| matches!(right, geo::Geometry::GeometryCollection(_))
		{
			super::collection::within(&left, &right)
		} else {
			Ok(left.relate(&right).is_within())
		}
	}

	/// Tests intersection, including boundary contact, in the lon/lat plane.
	///
	/// # Errors
	/// Returns a typed value error if either geometry is invalid.
	pub fn intersects(&self, other: &Self) -> FerrosGrynnResult<bool> {
		let left = self.checked_geo()?;
		let right = other.checked_geo()?;
		Ok(left.intersects(&right))
	}

	/// Returns WGS-84 geodesic distance between two points in metres.
	///
	/// # Errors
	/// Rejects invalid geometries and non-point arguments.
	pub fn distance(&self, other: &Self) -> FerrosGrynnResult<f64> {
		self.validate()?;
		other.validate()?;
		let (Self::Point(left), Self::Point(right)) = (self, other) else {
			return Err(invalid("geodesic distance requires two points"));
		};
		Ok(Geodesic.distance(geo::Point(coordinate(left)), geo::Point(coordinate(right))))
	}

	/// Returns the planar MBR; valid empty geometries have no rectangle.
	pub(crate) fn mbr(&self) -> FerrosGrynnResult<Option<[f64; 4]>> {
		Ok(self.checked_geo()?.bounding_rect().map(|rectangle| {
			[rectangle.min().x, rectangle.min().y, rectangle.max().x, rectangle.max().y]
		}))
	}

	/// Validates before converting to private foreign kernel types.
	fn checked_geo(&self) -> FerrosGrynnResult<geo::Geometry<f64>> {
		self.validation_scratch_bytes()?;
		let geometry = to_geo(self);
		geometry.check_validation().map_err(|cause| {
			invalid("OGC topology validation failed").with_technical_source(cause)
		})?;
		topology::check_connected_interiors(self)?;
		Ok(geometry)
	}

	/// Bounds family-specific validation scratch before entering the foreign
	/// kernel.
	pub(crate) fn validation_scratch_bytes(&self) -> FerrosGrynnResult<usize> {
		check_structure(self, 0, &mut 0)?;
		fn sum(
			mut values: impl Iterator<Item = FerrosGrynnResult<usize>>,
		) -> FerrosGrynnResult<usize> {
			values.try_fold(0_usize, |total, value| {
				total.checked_add(value?).ok_or_else(|| invalid("geometry scratch size overflows"))
			})
		}
		fn rings(rings: &[Vec<Position>]) -> FerrosGrynnResult<usize> {
			sum(rings.iter().map(|ring| {
				ring.len()
					.checked_mul(512)
					.and_then(|bytes| bytes.checked_add(24))
					.ok_or_else(|| invalid("geometry ring scratch overflows"))
			}))
		}
		fn size(geometry: &Geometry) -> FerrosGrynnResult<usize> {
			match geometry {
				Geometry::Point(_) => Ok(0),
				Geometry::MultiPoint(positions) | Geometry::LineString(positions) => positions
					.len()
					.checked_mul(16)
					.ok_or_else(|| invalid("geometry scratch size overflows")),
				Geometry::MultiLineString(lines) => sum(lines.iter().map(|line| {
					line.len()
						.checked_mul(16)
						.and_then(|bytes| bytes.checked_add(24))
						.ok_or_else(|| invalid("geometry scratch size overflows"))
				})),
				Geometry::Polygon(value) => rings(value),
				Geometry::MultiPolygon(polygons) => sum(polygons.iter().map(|polygon| {
					rings(polygon).and_then(|bytes| {
						bytes
							.checked_add(48)
							.ok_or_else(|| invalid("geometry scratch size overflows"))
					})
				})),
				Geometry::Collection(values) => sum(values.iter().map(|value| {
					size(value).and_then(|bytes| {
						bytes
							.checked_add(std::mem::size_of::<geo::Geometry<f64>>())
							.ok_or_else(|| invalid("geometry scratch size overflows"))
					})
				})),
			}
		}
		let bytes = size(self)?
			.checked_add(1024)
			.ok_or_else(|| invalid("geometry scratch size overflows"))?;
		let retained = super::super::value::estimated_geometry_heap_bytes(self);
		if bytes.checked_add(retained).is_none_or(|total| total > 128 * 1024 * 1024) {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"geometry validation memory",
				128 * 1024 * 1024,
				bytes.saturating_add(retained) as u64,
			));
		}
		Ok(bytes)
	}
}

/// Charges an aggregate count before copying coordinates into kernel storage.
pub(super) fn charge(elements: &mut usize, count: usize) -> FerrosGrynnResult<()> {
	*elements = elements
		.checked_add(count)
		.filter(|count| *count <= MAX_ELEMENTS)
		.ok_or_else(|| invalid("geometry exceeds the element limit"))?;
	Ok(())
}

/// Checks closure before Polygon::new can silently close a malformed ring.
fn check_rings(rings: &[Vec<Position>], elements: &mut usize) -> FerrosGrynnResult<()> {
	charge(elements, rings.len())?;
	for ring in rings {
		charge(elements, ring.len())?;
		if ring.is_empty() && rings.len() == 1 {
			continue;
		}
		if ring.len() < 4 || ring.first() != ring.last() {
			return Err(invalid(
				"polygon rings must be explicitly closed with at least four positions",
			));
		}
		topology::check_ring(ring)?;
	}
	Ok(())
}

/// Checks the owned representation before recursively allocating foreign
/// geometries.
fn check_structure(value: &Geometry, depth: usize, elements: &mut usize) -> FerrosGrynnResult<()> {
	if depth > MAX_DEPTH {
		return Err(invalid("geometry exceeds the nesting limit"));
	}
	match value {
		Geometry::Point(_) => charge(elements, 1),
		Geometry::MultiPoint(positions) | Geometry::LineString(positions) => {
			charge(elements, positions.len())
		}
		Geometry::MultiLineString(lines) => {
			charge(elements, lines.len())?;
			for line in lines {
				charge(elements, line.len())?;
			}
			Ok(())
		}
		Geometry::Polygon(rings) => check_rings(rings, elements),
		Geometry::MultiPolygon(polygons) => {
			charge(elements, polygons.len())?;
			for rings in polygons {
				check_rings(rings, elements)?;
			}
			Ok(())
		}
		Geometry::Collection(geometries) => {
			charge(elements, geometries.len())?;
			for child in geometries {
				check_structure(child, depth + 1, elements)?;
			}
			Ok(())
		}
	}
}

/// Checks caller-constructed GeoJSON before the conversion recursion or
/// allocation.
pub(super) fn check_geojson_bounds(value: &GeoJsonValue) -> FerrosGrynnResult<()> {
	fn visit(value: &GeoJsonValue, depth: usize, elements: &mut usize) -> FerrosGrynnResult<()> {
		if depth > MAX_DEPTH {
			return Err(invalid("geometry exceeds the nesting limit"));
		}
		match value {
			GeoJsonValue::Point {
				..
			} => charge(elements, 1),
			GeoJsonValue::MultiPoint {
				coordinates,
			}
			| GeoJsonValue::LineString {
				coordinates,
			} => charge(elements, coordinates.len()),
			GeoJsonValue::MultiLineString {
				coordinates,
			}
			| GeoJsonValue::Polygon {
				coordinates,
			} => {
				charge(elements, coordinates.len())?;
				for positions in coordinates {
					charge(elements, positions.len())?;
				}
				Ok(())
			}
			GeoJsonValue::MultiPolygon {
				coordinates,
			} => {
				charge(elements, coordinates.len())?;
				for rings in coordinates {
					charge(elements, rings.len())?;
					for positions in rings {
						charge(elements, positions.len())?;
					}
				}
				Ok(())
			}
			GeoJsonValue::GeometryCollection {
				geometries,
			} => {
				charge(elements, geometries.len())?;
				for geometry in geometries {
					visit(&geometry.value, depth + 1, elements)?;
				}
				Ok(())
			}
		}
	}
	visit(value, 0, &mut 0)
}

/// Position construction guarantees two finite, bounded coordinates.
pub(super) fn coordinate(position: &Position) -> geo::Coord<f64> {
	geo::Coord {
		x: position.as_slice()[0],
		y: position.as_slice()[1],
	}
}

/// Copies one validated linear coordinate sequence.
fn line(positions: &[Position]) -> geo::LineString<f64> {
	geo::LineString(positions.iter().map(coordinate).collect())
}

/// Converts rings only after explicit closure and bounds checks.
fn polygon(rings: &[Vec<Position>]) -> geo::Polygon<f64> {
	match rings.split_first() {
		Some((exterior, holes)) => {
			geo::Polygon::new(line(exterior), holes.iter().map(|ring| line(ring)).collect())
		}
		None => geo::Polygon::empty(),
	}
}

/// Keeps all foreign types behind the owned runtime API.
fn to_geo(value: &Geometry) -> geo::Geometry<f64> {
	match value {
		Geometry::Point(value) => geo::Point(coordinate(value)).into(),
		Geometry::MultiPoint(values) => {
			geo::MultiPoint(values.iter().map(|value| geo::Point(coordinate(value))).collect())
				.into()
		}
		Geometry::LineString(values) => line(values).into(),
		Geometry::MultiLineString(values) => {
			geo::MultiLineString(values.iter().map(|values| line(values)).collect()).into()
		}
		Geometry::Polygon(rings) => polygon(rings).into(),
		Geometry::MultiPolygon(polygons) => {
			geo::MultiPolygon(polygons.iter().map(|rings| polygon(rings)).collect()).into()
		}
		Geometry::Collection(values) => geo::Geometry::GeometryCollection(geo::GeometryCollection(
			values.iter().map(to_geo).collect(),
		)),
	}
}

/// Builds the generated Value-domain diagnostic without exposing coordinates.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_geometry_structure(reason)
}
