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

#[derive(Debug, Clone)]
pub struct Position(Vec<f64>);

impl Position {
	pub fn new(coordinates: impl Into<Vec<f64>>) -> FerrosGrynnResult<Self> {
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

	pub fn as_slice(&self) -> &[f64] {
		&self.0
	}

	pub fn into_vec(self) -> Vec<f64> {
		self.0
	}
}

impl PartialEq for Position {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}

impl Eq for Position {}

impl PartialOrd for Position {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Position {
	fn cmp(&self, other: &Self) -> Ordering {
		compare_float_slices(&self.0, &other.0)
	}
}

impl Hash for Position {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.0.len().hash(state);
		for coordinate in &self.0 {
			coordinate.to_bits().hash(state);
		}
	}
}

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum Geometry {
	Point(Position),
	MultiPoint(Vec<Position>),
	LineString(Vec<Position>),
	MultiLineString(Vec<Vec<Position>>),
	Polygon(Vec<Vec<Position>>),
	MultiPolygon(Vec<Vec<Vec<Position>>>),
	Collection(Vec<Geometry>),
}

impl Geometry {
	pub fn from_geojson(value: GeoJsonGeometry) -> FerrosGrynnResult<Self> {
		value.value.try_into()
	}

	pub fn into_geojson(self) -> GeoJsonGeometry {
		GeoJsonGeometry::new(self.into())
	}

	pub fn to_geojson_string(&self) -> String {
		GeoJson::Geometry(self.clone().into_geojson()).to_string()
	}
}

impl TryFrom<GeoJsonValue> for Geometry {
	type Error = FerrosGrynnError;

	fn try_from(value: GeoJsonValue) -> Result<Self, Self::Error> {
		match value {
			GeoJsonValue::Point {
				coordinates,
			} => Ok(Self::Point(position(coordinates)?)),
			GeoJsonValue::MultiPoint {
				coordinates,
			} => Ok(Self::MultiPoint(positions(coordinates)?)),
			GeoJsonValue::LineString {
				coordinates,
			} => Ok(Self::LineString(positions(coordinates)?)),
			GeoJsonValue::MultiLineString {
				coordinates,
			} => coordinates
				.into_iter()
				.map(positions)
				.collect::<Result<_, _>>()
				.map(Self::MultiLineString),
			GeoJsonValue::Polygon {
				coordinates,
			} => coordinates.into_iter().map(positions).collect::<Result<_, _>>().map(Self::Polygon),
			GeoJsonValue::MultiPolygon {
				coordinates,
			} => coordinates
				.into_iter()
				.map(|polygon| polygon.into_iter().map(positions).collect::<Result<_, _>>())
				.collect::<Result<_, _>>()
				.map(Self::MultiPolygon),
			GeoJsonValue::GeometryCollection {
				geometries,
			} => geometries
				.into_iter()
				.map(Self::from_geojson)
				.collect::<Result<_, _>>()
				.map(Self::Collection),
		}
	}
}

impl From<Geometry> for GeoJsonValue {
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
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(&self.to_geojson_string())
	}
}

fn position(value: GeoJsonPosition) -> FerrosGrynnResult<Position> {
	Position::new(value.as_slice().to_vec())
}

fn positions(value: Vec<GeoJsonPosition>) -> FerrosGrynnResult<Vec<Position>> {
	value.into_iter().map(position).collect()
}

fn into_position(value: Position) -> GeoJsonPosition {
	value.into_vec().into()
}

fn into_positions(value: Vec<Position>) -> Vec<GeoJsonPosition> {
	value.into_iter().map(into_position).collect()
}

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
mod tests {
	use super::{Geometry, Position};
	use crate::gql_status::Domain;

	#[test]
	fn round_trips_geojson_geometry() {
		let source = r#"{"type":"Point","coordinates":[10.0,20.0]}"#;
		let geometry = source.parse::<Geometry>().unwrap();
		assert_eq!(geometry, Geometry::Point(Position::new(vec![10.0, 20.0]).unwrap()),);
		assert_eq!(geometry.to_geojson_string().parse::<Geometry>().unwrap(), geometry,);
	}

	#[test]
	fn rejects_non_finite_and_short_positions() {
		assert_status(Position::new(vec![1.0]).unwrap_err(), "FG-22F06");
		assert_status(Position::new(vec![1.0, 2.0, 3.0]).unwrap_err(), "FG-22F06");
		assert_status(Position::new(vec![1.0, f64::NAN]).unwrap_err(), "FG-22F07");
		assert_status(Position::new(vec![181.0, 0.0]).unwrap_err(), "FG-22F08");
		assert_status(Position::new(vec![0.0, 91.0]).unwrap_err(), "FG-22F08");
	}

	#[test]
	fn wraps_geojson_parser_errors_as_technical_sources() {
		let error = "{invalid".parse::<Geometry>().unwrap_err();
		assert_status(error.clone(), "FG-22F09");
		assert!(error.technical_source().is_some());
		assert!(!error.to_string().contains("expected ident"));
	}

	#[test]
	fn rejects_non_geometry_geojson_with_a_value_status() {
		let error =
			r#"{"type":"Feature","geometry":null,"properties":{}}"#.parse::<Geometry>().unwrap_err();
		assert_status(error, "FG-22F10");
	}

	fn assert_status(error: crate::gql_status::FerrosGrynnError, code: &str) {
		assert_eq!(error.status().code().to_string(), code);
		assert_eq!(error.status().domain(), Domain::Value);
	}
}
