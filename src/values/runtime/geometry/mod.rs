use std::{
	cmp::Ordering,
	fmt::{self, Display, Formatter},
	hash::{Hash, Hasher},
	str::FromStr,
};

use geojson::{
	GeoJson,
	Geometry as GeoJsonGeometry,
	GeometryValue as GeoJsonValue,
	Position as GeoJsonPosition,
};

use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Bounded collection predicates without coordinate quantization.
mod collection;
/// Public value conversion into GeoJSON geometry.
mod conversion;
/// Shared validation, predicates and geodesic distance.
mod kernels;
/// Polygon contact and connected-interior validation.
mod topology;

/// Geometry family accepted by an optional geometry index TYPE constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum GeometryKind {
	/// One position.
	Point,
	/// A set of positions.
	MultiPoint,
	/// A linear sequence.
	LineString,
	/// A collection of linear sequences.
	MultiLineString,
	/// A shell and optional holes.
	Polygon,
	/// A collection of polygons.
	MultiPolygon,
	/// A collection of arbitrary geometry families.
	Collection,
}

impl GeometryKind {
	/// Returns the canonical GeoJSON type spelling.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Point => "Point",
			Self::MultiPoint => "MultiPoint",
			Self::LineString => "LineString",
			Self::MultiLineString => "MultiLineString",
			Self::Polygon => "Polygon",
			Self::MultiPolygon => "MultiPolygon",
			Self::Collection => "GeometryCollection",
		}
	}
}

impl FromStr for GeometryKind {
	type Err = FerrosGrynnError;

	/// Constructs from str for `GeometryKind`.
	fn from_str(name: &str) -> FerrosGrynnResult<Self> {
		match name.to_ascii_lowercase().as_str() {
			"point" => Ok(Self::Point),
			"multipoint" => Ok(Self::MultiPoint),
			"linestring" => Ok(Self::LineString),
			"multilinestring" => Ok(Self::MultiLineString),
			"polygon" => Ok(Self::Polygon),
			"multipolygon" => Ok(Self::MultiPolygon),
			"geometrycollection" | "collection" => Ok(Self::Collection),
			_ => Err(FerrosGrynnError::invalid_geometry_structure("unknown geometry type")),
		}
	}
}

/// Validated coordinate sequence used by runtime geometry values.
#[derive(Debug, Clone)]
pub struct Position(
	/// Coordinate values in the runtime position representation.
	Vec<f64>,
);

impl Position {
	/// Constructs one finite two-dimensional WGS-84 position.
	///
	/// # Errors
	/// Rejects dimensions other than two, nonfinite coordinates and values
	/// outside lon/lat bounds.
	pub fn new<IntoVec: Into<Vec<f64>>>(coordinates: IntoVec) -> FerrosGrynnResult<Self> {
		let mut coordinates = coordinates.into();
		if coordinates.len() != 2 {
			return Err(FerrosGrynnError::invalid_geometry_coordinate_dimension(
				coordinates.len() as u64
			));
		}
		if let Some(index) = coordinates.iter().position(|coordinate| !coordinate.is_finite()) {
			return Err(FerrosGrynnError::non_finite_geometry_coordinate(index as u64));
		}
		if !(-180.0..=180.0).contains(&coordinates[0]) {
			return Err(FerrosGrynnError::geometry_coordinate_out_of_bounds(
				"longitude",
				-180,
				180,
				coordinates[0],
			));
		}
		if !(-90.0..=90.0).contains(&coordinates[1]) {
			return Err(FerrosGrynnError::geometry_coordinate_out_of_bounds(
				"latitude",
				-90,
				90,
				coordinates[1],
			));
		}
		for coordinate in &mut coordinates {
			if *coordinate == 0.0 {
				*coordinate = 0.0;
			}
		}
		Ok(Self(coordinates))
	}

	/// Borrows the elements as a contiguous slice.
	pub fn as_slice(&self) -> &[f64] {
		&self.0
	}

	/// Counts retained coordinates, including caller-supplied spare capacity.
	pub(super) fn estimated_heap_bytes(&self) -> usize {
		self.0.capacity().saturating_mul(size_of::<f64>())
	}

	/// Consumes the collection and returns its owned element vector.
	pub fn into_vec(self) -> Vec<f64> {
		self.0
	}
}

impl PartialEq for Position {
	/// Compares this `Position` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}

impl Eq for Position {}

impl PartialOrd for Position {
	/// Compares this `Position` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Position {
	/// Compares this `Position` with another value using its ordering contract.
	fn cmp(&self, other: &Self) -> Ordering {
		compare_float_slices(&self.0, &other.0)
	}
}

impl Hash for Position {
	/// Feeds this `Position` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.0.len().hash(state);
		for coordinate in &self.0 {
			coordinate.to_bits().hash(state);
		}
	}
}

/// Runtime geometry value containing coordinates or other geometries.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum Geometry {
	/// One coordinate position.
	Point(
		/// One coordinate position.
		Position,
	),
	/// Several independent coordinate positions.
	MultiPoint(
		/// Several independent coordinate positions.
		Vec<Position>,
	),
	/// One connected coordinate sequence.
	LineString(
		/// One connected coordinate sequence.
		Vec<Position>,
	),
	/// Several independent connected coordinate sequences.
	MultiLineString(
		/// Several independent connected coordinate sequences.
		Vec<Vec<Position>>,
	),
	/// One polygon represented by coordinate rings.
	Polygon(
		/// One polygon represented by coordinate rings.
		Vec<Vec<Position>>,
	),
	/// Several polygon geometries.
	MultiPolygon(
		/// Several polygon geometries.
		Vec<Vec<Vec<Position>>>,
	),
	/// A collection of geometry values.
	Collection(
		/// A collection of geometry values.
		Vec<Geometry>,
	),
}

impl Geometry {
	/// Aggregate decoded positions and container members allowed per geometry.
	pub(crate) const MAX_ELEMENTS: usize = kernels::MAX_ELEMENTS;

	/// Returns the owned geometry's family without conversion or allocation.
	pub const fn kind(&self) -> GeometryKind {
		match self {
			Self::Point(_) => GeometryKind::Point,
			Self::MultiPoint(_) => GeometryKind::MultiPoint,
			Self::LineString(_) => GeometryKind::LineString,
			Self::MultiLineString(_) => GeometryKind::MultiLineString,
			Self::Polygon(_) => GeometryKind::Polygon,
			Self::MultiPolygon(_) => GeometryKind::MultiPolygon,
			Self::Collection(_) => GeometryKind::Collection,
		}
	}

	/// Converts one GeoJSON geometry after structure and topology validation.
	///
	/// # Errors
	/// Returns typed geometry errors for malformed input or exceeded resource
	/// limits.
	pub fn from_geojson(value: GeoJsonGeometry) -> FerrosGrynnResult<Self> {
		value.value.try_into()
	}

	/// Converts this value into geojson for `Geometry`.
	pub fn into_geojson(self) -> GeoJsonGeometry {
		GeoJsonGeometry::new(self.into())
	}

	/// Converts to geojson string for `Geometry`.
	pub fn to_geojson_string(&self) -> String {
		GeoJson::Geometry(self.clone().into_geojson()).to_string()
	}
}

impl TryFrom<GeoJsonValue> for Geometry {
	type Error = FerrosGrynnError;

	/// Converts `GeoJsonValue` into `Geometry` after validating the conversion.
	fn try_from(value: GeoJsonValue) -> Result<Self, Self::Error> {
		kernels::check_geojson_bounds(&value)?;
		let geometry = convert_geojson(value)?;
		geometry.validate()?;
		Ok(geometry)
	}
}

/// Converts preflighted GeoJSON containers into the owned representation.
fn convert_geojson(value: GeoJsonValue) -> FerrosGrynnResult<Geometry> {
	use Geometry as G;
	match value {
		GeoJsonValue::Point {
			coordinates,
		} => Ok(G::Point(position(coordinates)?)),
		GeoJsonValue::MultiPoint {
			coordinates,
		} => Ok(G::MultiPoint(positions(coordinates)?)),
		GeoJsonValue::LineString {
			coordinates,
		} => Ok(G::LineString(positions(coordinates)?)),
		GeoJsonValue::MultiLineString {
			coordinates,
		} => {
			coordinates.into_iter().map(positions).collect::<Result<_, _>>().map(G::MultiLineString)
		}
		GeoJsonValue::Polygon {
			coordinates,
		} => coordinates.into_iter().map(positions).collect::<Result<_, _>>().map(G::Polygon),
		GeoJsonValue::MultiPolygon {
			coordinates,
		} => coordinates
			.into_iter()
			.map(|polygon| polygon.into_iter().map(positions).collect::<Result<_, _>>())
			.collect::<Result<_, _>>()
			.map(G::MultiPolygon),
		GeoJsonValue::GeometryCollection {
			geometries,
		} => geometries
			.into_iter()
			.map(|geometry| convert_geojson(geometry.value))
			.collect::<Result<_, _>>()
			.map(G::Collection),
	}
}

impl From<Geometry> for GeoJsonValue {
	/// Converts `Geometry` into `GeoJsonValue`.
	fn from(value: Geometry) -> Self {
		match value {
			Geometry::Point(value) => Self::Point {
				coordinates: into_position(value),
			},
			Geometry::MultiPoint(value) => Self::MultiPoint {
				coordinates: into_positions(value),
			},
			Geometry::LineString(value) => Self::LineString {
				coordinates: into_positions(value),
			},
			Geometry::MultiLineString(value) => Self::MultiLineString {
				coordinates: value.into_iter().map(into_positions).collect(),
			},
			Geometry::Polygon(value) => Self::Polygon {
				coordinates: value.into_iter().map(into_positions).collect(),
			},
			Geometry::MultiPolygon(value) => Self::MultiPolygon {
				coordinates: value
					.into_iter()
					.map(|polygon| polygon.into_iter().map(into_positions).collect())
					.collect(),
			},
			Geometry::Collection(value) => Self::GeometryCollection {
				geometries: value.into_iter().map(Geometry::into_geojson).collect(),
			},
		}
	}
}

impl FromStr for Geometry {
	type Err = FerrosGrynnError;

	/// Constructs from str for `Geometry`.
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value.parse::<GeoJson>().map_err(|source| {
			FerrosGrynnError::invalid_geo_json_geometry().with_technical_source(source)
		})? {
			GeoJson::Geometry(value) => Self::from_geojson(value),
			GeoJson::Feature(_) => Err(FerrosGrynnError::expected_geo_json_geometry("feature")),
			GeoJson::FeatureCollection(_) => {
				Err(FerrosGrynnError::expected_geo_json_geometry("feature collection"))
			}
		}
	}
}

impl Display for Geometry {
	/// Formats `Geometry` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(&self.to_geojson_string())
	}
}

/// Returns the position result.
fn position(value: GeoJsonPosition) -> FerrosGrynnResult<Position> {
	if value.as_slice().len() != 2 {
		return Err(FerrosGrynnError::invalid_geometry_coordinate_dimension(
			value.as_slice().len() as u64,
		));
	}
	Position::new(value.as_slice().to_vec())
}

/// Returns the positions result.
fn positions(value: Vec<GeoJsonPosition>) -> FerrosGrynnResult<Vec<Position>> {
	value.into_iter().map(position).collect()
}

/// Converts this value into position.
fn into_position(value: Position) -> GeoJsonPosition {
	value.into_vec().into()
}

/// Converts this value into positions.
fn into_positions(value: Vec<Position>) -> Vec<GeoJsonPosition> {
	value.into_iter().map(into_position).collect()
}

/// Compares float slices.
fn compare_float_slices(left: &[f64], right: &[f64]) -> Ordering {
	left.iter()
		.zip(right)
		.find_map(|(left, right)| {
			let ordering = left.total_cmp(right);
			(ordering != Ordering::Equal).then_some(ordering)
		})
		.unwrap_or_else(|| left.len().cmp(&right.len()))
}

#[cfg(test)]
/// Regression tests for geometry behavior.
mod tests {
	use super::{Geometry, Position};
	use crate::gql_status::Domain;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies round trips geojson geometry.
	#[test]
	fn round_trips_geojson_geometry() {
		let source = r#"{"type":"Point","coordinates":[10.0,20.0]}"#;
		let geometry = source.parse::<Geometry>().assert_ok();
		assert_eq!(geometry, Geometry::Point(Position::new(vec![10.0, 20.0]).assert_ok()),);
		assert_eq!(geometry.to_geojson_string().parse::<Geometry>().assert_ok(), geometry,);
	}

	/// Verifies rejects non finite and short positions.
	#[test]
	fn rejects_non_finite_and_short_positions() {
		assert_status(Position::new(vec![1.0]).assert_err(), "FG-22F06");
		assert_status(Position::new(vec![1.0, 2.0, 3.0]).assert_err(), "FG-22F06");
		assert_status(Position::new(vec![1.0, f64::NAN]).assert_err(), "FG-22F07");
		assert_status(Position::new(vec![181.0, 0.0]).assert_err(), "FG-22F08");
		assert_status(Position::new(vec![0.0, 91.0]).assert_err(), "FG-22F08");
	}

	/// Verifies wraps geojson parser errors as technical sources.
	#[test]
	fn wraps_geojson_parser_errors_as_technical_sources() {
		let error = "{invalid".parse::<Geometry>().assert_err();
		assert_status(error.clone(), "FG-22F09");
		assert!(error.technical_source().is_some());
		assert!(!error.to_string().contains("expected ident"));
	}

	/// Verifies rejects non geometry geojson with a value status.
	#[test]
	fn rejects_non_geometry_geojson_with_a_value_status() {
		let error =
			r#"{"type":"Feature","geometry":null,"properties":{}}"#.parse::<Geometry>().assert_err();
		assert_status(error, "FG-22F10");
	}

	/// Returns assert status for this module.
	fn assert_status(error: crate::gql_status::FerrosGrynnError, code: &str) {
		assert_eq!(error.status().code().to_string(), code);
		assert_eq!(error.status().domain(), Domain::Value);
	}

	/// Verifies validates rings holes and ogc boundaries.
	#[test]
	fn validates_rings_holes_and_ogc_boundaries() {
		let polygon: Geometry = r#"{"type":"Polygon","coordinates":[[[0,0],[8,0],[8,8],[0,8],[0,0]],[[2,2],[4,2],[4,4],[2,4],[2,2]]]}"#.parse().assert_ok();
		let point = |x, y| Geometry::Point(Position::new([x, y]).assert_ok());
		assert!(point(1.0, 1.0).within(&polygon).assert_ok());
		assert!(!point(0.0, 1.0).within(&polygon).assert_ok());
		assert!(point(0.0, 1.0).intersects(&polygon).assert_ok());
		assert!(!point(3.0, 3.0).intersects(&polygon).assert_ok());
		assert!(!point(2.0, 3.0).within(&polygon).assert_ok());
		assert!(point(2.0, 3.0).intersects(&polygon).assert_ok());
		for source in [
			r#"{"type":"LineString","coordinates":[[0,0]]}"#,
			r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,1]]]}"#,
			r#"{"type":"Polygon","coordinates":[[[0,0],[1,1],[0,1],[1,0],[0,0]]]}"#,
			r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,1],[0,0]],[[2,2],[3,2],[3,3],[2,2]]]}"#,
		] {
			assert_status(source.parse::<Geometry>().assert_err(), "FG-22F31");
		}
	}

	/// Verifies empty geometries have no mbr and geodesic distance is in
	/// metres.
	#[test]
	fn empty_geometries_have_no_mbr_and_geodesic_distance_is_in_metres() {
		let empty = [
			Geometry::MultiPoint(vec![]),
			Geometry::LineString(vec![]),
			Geometry::MultiLineString(vec![]),
			Geometry::Polygon(vec![]),
			Geometry::MultiPolygon(vec![]),
			Geometry::Collection(vec![]),
		];
		let origin = Geometry::Point(Position::new([0.0, 0.0]).assert_ok());
		for geometry in empty {
			assert_eq!(geometry.mbr().assert_ok(), None);
			assert!(!geometry.within(&origin).assert_ok());
			assert!(!geometry.intersects(&origin).assert_ok());
		}
		let east = Geometry::Point(Position::new([1.0, 0.0]).assert_ok());
		assert!((origin.distance(&east).assert_ok() - 111_319.490_793).abs() < 0.001);
		assert_eq!(
			origin.distance(&origin).assert_ok().partial_cmp(&0.0),
			Some(std::cmp::Ordering::Equal)
		);
		assert!(Geometry::LineString(vec![]).distance(&origin).is_err());
	}

	/// Verifies checks directly constructed values and depth before kernels.
	#[test]
	fn checks_directly_constructed_values_and_depth_before_kernels() {
		let p = Position::new([0.0, 0.0]).assert_ok();
		assert!(Geometry::Polygon(vec![vec![p.clone(); 4]]).validate().is_err());
		let mut geometry = Geometry::Point(p);
		for _ in 0..65 {
			geometry = Geometry::Collection(vec![geometry]);
		}
		assert!(geometry.validate().is_err());
	}

	/// Verifies rejects collinear rings and disconnected polygon interiors.
	#[test]
	fn rejects_collinear_rings_and_disconnected_polygon_interiors() {
		for source in [
			r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[2,0],[0,0]]]}"#,
			r#"{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]],[[0,2],[2,1],[4,2],[2,3],[0,2]]]}"#,
		] {
			assert!(source.parse::<Geometry>().is_err(), "{source}");
		}
	}

	/// Verifies overlapping collection members are a geometric union.
	#[test]
	fn overlapping_collection_members_are_a_geometric_union() {
		let collection: Geometry = r#"{"type":"GeometryCollection","geometries":[{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]]]},{"type":"Polygon","coordinates":[[[2,-1],[6,-1],[6,3],[2,3],[2,-1]]]}]}"#.parse().assert_ok();
		let point = Geometry::Point(Position::new([3.0, 0.0]).assert_ok());
		let line = Geometry::LineString(vec![
			Position::new([1.0, 1.0]).assert_ok(),
			Position::new([5.0, 1.0]).assert_ok(),
		]);
		assert!(point.within(&collection).assert_ok());
		assert!(line.within(&collection).assert_ok());
		assert!(line.within(&Geometry::Collection(vec![collection])).assert_ok());
	}

	/// Verifies geometry collection preserves small coordinates seams and
	/// holes.
	#[test]
	fn geometry_collection_preserves_small_coordinates_seams_and_holes() {
		let point = |x, y| Geometry::Point(Position::new([x, y]).assert_ok());
		let polygon = |x0, y0, x1, y1| {
			Geometry::Polygon(vec![vec![
				Position::new([x0, y0]).assert_ok(),
				Position::new([x1, y0]).assert_ok(),
				Position::new([x1, y1]).assert_ok(),
				Position::new([x0, y1]).assert_ok(),
				Position::new([x0, y0]).assert_ok(),
			]])
		};
		let tiny = Geometry::Collection(vec![
			polygon(0.0, 0.0, 1.0e-12, 1.0e-12),
			polygon(1.0, 1.0, 2.0, 2.0),
		]);
		assert!(point(5.0e-13, 5.0e-13).within(&tiny).assert_ok());
		assert!(!point(0.0, 5.0e-13).within(&tiny).assert_ok());
		let adjacent =
			Geometry::Collection(vec![polygon(0.0, 0.0, 1.0, 2.0), polygon(1.0, 0.0, 2.0, 2.0)]);
		assert!(point(1.0, 1.0).within(&adjacent).assert_ok());
		assert!(!point(1.0, 0.0).within(&adjacent).assert_ok());
		assert!(polygon(0.0, 0.0, 2.0, 2.0).within(&adjacent).assert_ok());
		let mut hole = polygon(0.0, 0.0, 4.0, 4.0);
		if let Geometry::Polygon(rings) = &mut hole
			&& let Geometry::Polygon(mut interior) = polygon(1.0, 1.0, 3.0, 3.0)
		{
			rings.append(&mut interior);
		}
		let unfilled = Geometry::Collection(vec![hole.clone()]);
		assert!(!polygon(0.0, 0.0, 4.0, 4.0).within(&unfilled).assert_ok());
		let filled = Geometry::Collection(vec![hole, polygon(1.0, 1.0, 3.0, 3.0)]);
		assert!(polygon(0.0, 0.0, 4.0, 4.0).within(&filled).assert_ok());
		assert!(point(1.0, 2.0).within(&filled).assert_ok());
	}

	/// Verifies geometry collection line boundary uses endpoints.
	#[test]
	fn geometry_collection_line_boundary_uses_endpoints() {
		let point = Geometry::Point(Position::new([0.0, 0.0]).assert_ok());
		for coordinates in [
			vec![[0.0, 0.0], [1.0, 0.0], [0.0, 0.0]],
			vec![[-1.0, 0.0], [0.0, 0.0], [1.0, 0.0], [0.0, 0.0], [0.0, 1.0]],
			vec![[-1.0, -1.0], [1.0, 1.0], [-1.0, 1.0], [1.0, -1.0]],
		] {
			let line = Geometry::LineString(
				coordinates
					.into_iter()
					.map(|position| Position::new(position).assert_ok())
					.collect(),
			);
			assert!(point.within(&line).assert_ok());
			assert!(point.within(&Geometry::Collection(vec![line.clone()])).assert_ok());
			assert!(
				point
					.within(&Geometry::Collection(vec![Geometry::Collection(vec![line])]))
					.assert_ok()
			);
		}
	}

	/// Verifies geometry collection work budget counts repeated coordinates.
	#[test]
	fn geometry_collection_work_budget_counts_repeated_coordinates() {
		let mut coordinates = vec![Position::new([0.0, 0.0]).assert_ok(); 200_000];
		coordinates.push(Position::new([1.0, 0.0]).assert_ok());
		let collection = Geometry::Collection(vec![Geometry::LineString(coordinates)]);
		let points = Geometry::MultiPoint(vec![Position::new([0.5, 0.0]).assert_ok(); 100]);
		assert!(collection.validate().is_ok());
		assert!(points.within(&collection).is_err());
	}
}
