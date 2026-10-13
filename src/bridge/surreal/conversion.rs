//! Direct typed conversions: unsupported representations fail before receiving
//! IO.

use std::ops::Bound;

use surrealdb::types as sdk;

use crate::{
	bridge::BridgeContext,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{
		Array,
		DateTime,
		Duration,
		Geometry,
		Number,
		Object,
		Position,
		Range,
		RecordId,
		RecordIdPart,
		Regex,
		Set,
		Text,
		Value,
	},
};

/// Rejects values without a lossless representation in the receiving system.
fn invalid() -> FerrosGrynnError {
	FerrosGrynnError::bridge_operation_error("value is not losslessly representable by the bridge")
}
/// Bounds recursive conversion and retained allocations before copying.
fn admit(context: &BridgeContext, depth: usize, bytes: usize) -> FerrosGrynnResult<()> {
	if depth > 64 {
		return Err(FerrosGrynnError::bridge_operation_error("bridge value nesting exceeds 64"));
	}
	context.admit_bytes(bytes.saturating_add(128))
}
/// Converts owned runtime values directly into native typed SDK values.
pub(super) fn outgoing(
	value: Value,
	context: &BridgeContext,
	depth: usize,
) -> FerrosGrynnResult<sdk::Value> {
	admit(context, depth, 0)?;
	Ok(match value {
		Value::None => sdk::Value::None,
		Value::Null => sdk::Value::Null,
		Value::Bool(value) => sdk::Value::Bool(value),
		Value::Number(Number::Int(value)) => sdk::Value::Number(sdk::Number::Int(value)),
		Value::Number(Number::Float(value)) => sdk::Value::Number(sdk::Number::Float(value)),
		Value::Number(Number::Decimal(value)) => sdk::Value::Number(sdk::Number::Decimal(value)),
		Value::String(value) => {
			context.admit_bytes(value.len())?;
			sdk::Value::String(value.into_string())
		}
		Value::Bytes(value) => {
			context.admit_bytes(value.len())?;
			sdk::Value::Bytes(value.into_vec().into())
		}
		Value::Uuid(value) => sdk::Value::Uuid(value.into_inner().into()),
		Value::Array(values) => sdk::Value::Array(
			values
				.into_iter()
				.map(|value| outgoing(value, context, depth + 1))
				.collect::<FerrosGrynnResult<Vec<_>>>()?
				.into(),
		),
		Value::Object(values) => sdk::Value::Object(
			values
				.into_iter()
				.map(|(key, value)| {
					context.admit_bytes(key.len())?;
					Ok((key.into_string(), outgoing(value, context, depth + 1)?))
				})
				.collect::<FerrosGrynnResult<std::collections::BTreeMap<_, _>>>()?
				.into(),
		),
		Value::Set(values) => {
			let count = values.len();
			let values = sdk::Set::from(
				values
					.into_iter()
					.map(|value| outgoing(value, context, depth + 1))
					.collect::<FerrosGrynnResult<Vec<_>>>()?,
			);
			if values.len() != count {
				return Err(invalid());
			}
			sdk::Value::Set(values)
		}
		Value::Record(record) => sdk::Value::RecordId(record_out(*record, context, depth + 1)?),
		Value::DateTime(value) => {
			let nanos = value.as_inner().timestamp().as_nanosecond();
			let seconds = i64::try_from(nanos.div_euclid(1_000_000_000))
				.map_err(|error| invalid().with_technical_source(error))?;
			let nanos = u32::try_from(nanos.rem_euclid(1_000_000_000))
				.map_err(|error| invalid().with_technical_source(error))?;
			sdk::Value::Datetime(sdk::Datetime::from_timestamp(seconds, nanos).ok_or_else(invalid)?)
		}
		Value::Duration(value) => {
			let span = value.as_inner();
			if span.get_years() != 0
				|| span.get_months() != 0
				|| span.get_weeks() != 0
				|| span.get_days() != 0
			{
				return Err(invalid());
			}
			let nanos = value.fixed_nanoseconds().ok_or_else(invalid)?;
			let seconds = u64::try_from(nanos.div_euclid(1_000_000_000))
				.map_err(|error| invalid().with_technical_source(error))?;
			let nanos = u32::try_from(nanos.rem_euclid(1_000_000_000))
				.map_err(|error| invalid().with_technical_source(error))?;
			sdk::Value::Duration(std::time::Duration::new(seconds, nanos).into())
		}
		Value::Regex(value) => {
			context.admit_bytes(value.as_str().len().saturating_mul(1024))?;
			sdk::Value::Regex(
				regex::Regex::new(value.as_str())
					.map_err(|error| invalid().with_technical_source(error))?
					.into(),
			)
		}
		Value::Range(value) => {
			let (start, end) = value.into_bounds();
			sdk::Value::Range(Box::new(sdk::Range::new(
				bound_out(start, context, depth)?,
				bound_out(end, context, depth)?,
			)))
		}
		Value::Geometry(value) => {
			context.admit_bytes(value.validation_scratch_bytes()?)?;
			if !geometry_canonical(&value) {
				return Err(invalid());
			}
			sdk::Value::Geometry(value.checked_geo()?.into())
		}
		_ => return Err(invalid()),
	})
}
/// Preserves inclusive, exclusive and unbounded range endpoints.
fn bound_out(
	bound: Bound<Value>,
	context: &BridgeContext,
	depth: usize,
) -> FerrosGrynnResult<Bound<sdk::Value>> {
	Ok(match bound {
		Bound::Unbounded => Bound::Unbounded,
		Bound::Included(value) => Bound::Included(outgoing(value, context, depth + 1)?),
		Bound::Excluded(value) => Bound::Excluded(outgoing(value, context, depth + 1)?),
	})
}
/// Keeps native record namespaces and supported typed keys without coercion.
pub(super) fn record_out(
	record: RecordId,
	context: &BridgeContext,
	depth: usize,
) -> FerrosGrynnResult<sdk::RecordId> {
	crate::bridge::types::validate_record(&record)?;
	context.admit_bytes(record.estimated_heap_bytes())?;
	let (table, key) = record.into_parts();
	let key = match key {
		RecordIdPart::Int(value) => sdk::RecordIdKey::Number(value),
		RecordIdPart::String(value) => sdk::RecordIdKey::String(value.into_string()),
		RecordIdPart::Uuid(value) => sdk::RecordIdKey::Uuid(value.into_inner().into()),
		RecordIdPart::Array(value) => match outgoing(Value::Array(value), context, depth)? {
			sdk::Value::Array(value) => sdk::RecordIdKey::Array(value),
			_ => return Err(invalid()),
		},
		RecordIdPart::Object(value) => match outgoing(Value::Object(value), context, depth)? {
			sdk::Value::Object(value) => sdk::RecordIdKey::Object(value),
			_ => return Err(invalid()),
		},
		_ => return Err(invalid()),
	};
	Ok(sdk::RecordId {
		table: table.into_string().into(),
		key,
	})
}
/// Admits native value nodes before constructing their runtime representation.
pub(super) fn incoming(
	value: sdk::Value,
	context: &BridgeContext,
	depth: usize,
) -> FerrosGrynnResult<Value> {
	admit(context, depth, 0)?;
	Ok(match value {
		sdk::Value::None => Value::None,
		sdk::Value::Null => Value::Null,
		sdk::Value::Bool(value) => Value::Bool(value),
		sdk::Value::Number(sdk::Number::Int(value)) => Value::Number(Number::Int(value)),
		sdk::Value::Number(sdk::Number::Float(value)) => Value::Number(Number::Float(value)),
		sdk::Value::Number(sdk::Number::Decimal(value)) => Value::Number(Number::Decimal(value)),
		sdk::Value::String(value) => {
			context.admit_bytes(value.len())?;
			Value::String(value.into())
		}
		sdk::Value::Bytes(value) => {
			context.admit_bytes(value.len())?;
			Value::Bytes(value.into_inner().to_vec().into())
		}
		sdk::Value::Uuid(value) => Value::Uuid(value.into_inner().into()),
		sdk::Value::Array(values) => Value::Array(
			values
				.into_iter()
				.map(|value| incoming(value, context, depth + 1))
				.collect::<FerrosGrynnResult<Array>>()?,
		),
		sdk::Value::Object(values) => Value::Object(
			values
				.into_iter()
				.map(|(key, value)| {
					context.admit_bytes(key.len())?;
					Ok((Text::from(key), incoming(value, context, depth + 1)?))
				})
				.collect::<FerrosGrynnResult<Object>>()?,
		),
		sdk::Value::Set(values) => {
			let count = values.len();
			let values = values
				.into_iter()
				.map(|value| incoming(value, context, depth + 1))
				.collect::<FerrosGrynnResult<Set>>()?;
			if values.len() != count {
				return Err(invalid());
			}
			Value::Set(values)
		}
		sdk::Value::RecordId(record) => {
			Value::Record(Box::new(record_in(record, context, depth + 1)?))
		}
		sdk::Value::Datetime(value) => {
			if value.timestamp_subsec_nanos() >= 1_000_000_000 {
				return Err(invalid());
			}
			let nanos = i128::from(value.timestamp()) * 1_000_000_000
				+ i128::from(value.timestamp_subsec_nanos());
			Value::DateTime(DateTime::new(
				jiff::Timestamp::from_nanosecond(nanos)
					.map_err(|error| invalid().with_technical_source(error))?
					.to_zoned(jiff::tz::TimeZone::UTC),
			))
		}
		sdk::Value::Duration(value) => {
			let value = value.into_inner();
			let seconds = i64::try_from(value.as_secs())
				.map_err(|error| invalid().with_technical_source(error))?;
			let span = jiff::Span::new()
				.try_seconds(seconds)
				.and_then(|span| span.try_nanoseconds(value.subsec_nanos()))
				.map_err(|error| invalid().with_technical_source(error))?;
			Value::Duration(Duration::new(span))
		}
		sdk::Value::Regex(value) => {
			context.admit_bytes(value.regex().as_str().len().saturating_mul(1024))?;
			Value::Regex(
				Regex::new(value.regex().as_str())
					.map_err(|error| invalid().with_technical_source(error))?,
			)
		}
		sdk::Value::Range(value) => {
			let bound = |value| match value {
				Bound::Unbounded => Ok(Bound::Unbounded),
				Bound::Included(value) => incoming(value, context, depth + 1).map(Bound::Included),
				Bound::Excluded(value) => incoming(value, context, depth + 1).map(Bound::Excluded),
			};
			Value::Range(Range::new(bound(value.start)?, bound(value.end)?)?)
		}
		sdk::Value::Geometry(value) => {
			let geometry = geometry_in(value, context, depth)?;
			context.admit_bytes(geometry.validation_scratch_bytes()?)?;
			geometry.validate()?;
			Value::Geometry(geometry)
		}
		_ => return Err(invalid()),
	})
}
/// Rejects unsupported native record-key domains before local binding.
fn record_in(
	record: sdk::RecordId,
	context: &BridgeContext,
	depth: usize,
) -> FerrosGrynnResult<RecordId> {
	let value = match record.key {
		sdk::RecordIdKey::Number(value) => Value::from(value),
		sdk::RecordIdKey::String(value) => Value::String(value.into()),
		sdk::RecordIdKey::Uuid(value) => Value::Uuid(value.into_inner().into()),
		sdk::RecordIdKey::Array(value) => incoming(sdk::Value::Array(value), context, depth)?,
		sdk::RecordIdKey::Object(value) => incoming(sdk::Value::Object(value), context, depth)?,
		sdk::RecordIdKey::Range(_) => return Err(invalid()),
	};
	let record = RecordId::from_value(record.table.into_inner(), value)?;
	crate::bridge::types::validate_record(&record)?;
	Ok(record)
}
/// Rejects runtime geometry forms that native polygons would canonicalize away.
fn geometry_canonical(value: &Geometry) -> bool {
	match value {
		Geometry::Polygon(rings) => !rings.is_empty(),
		Geometry::MultiPolygon(polygons) => polygons.iter().all(|rings| !rings.is_empty()),
		Geometry::Collection(values) => values.iter().all(geometry_canonical),
		_ => true,
	}
}
/// Admits native containers before constructing validated WGS-84 coordinates.
fn geometry_in(
	value: sdk::Geometry,
	context: &BridgeContext,
	depth: usize,
) -> FerrosGrynnResult<Geometry> {
	admit(context, depth, 0)?;
	let point = |point: geo::Coord<f64>| {
		context.admit_bytes(64)?;
		Position::new(vec![point.x, point.y])
	};
	let container = |count: usize, bytes: usize| {
		if count > Geometry::MAX_ELEMENTS {
			return Err(invalid());
		}
		context.admit_bytes(count.checked_mul(bytes).ok_or_else(invalid)?)
	};
	let line = |line: geo::LineString<f64>| {
		container(line.0.len(), std::mem::size_of::<Position>())?;
		line.0.into_iter().map(point).collect::<FerrosGrynnResult<Vec<_>>>()
	};
	let polygon = |polygon: geo::Polygon<f64>| {
		let (outer, holes) = polygon.into_inner();
		container(holes.len().saturating_add(1), std::mem::size_of::<Vec<Position>>())?;
		std::iter::once(outer).chain(holes).map(line).collect::<FerrosGrynnResult<Vec<_>>>()
	};
	Ok(match value {
		sdk::Geometry::Point(value) => Geometry::Point(point(value.0)?),
		sdk::Geometry::Line(value) => Geometry::LineString(line(value)?),
		sdk::Geometry::Polygon(value) => Geometry::Polygon(polygon(value)?),
		sdk::Geometry::MultiPoint(value) => {
			container(value.0.len(), std::mem::size_of::<Position>())?;
			Geometry::MultiPoint(
				value
					.0
					.into_iter()
					.map(|value| point(value.0))
					.collect::<FerrosGrynnResult<Vec<_>>>()?,
			)
		}
		sdk::Geometry::MultiLine(value) => {
			container(value.0.len(), std::mem::size_of::<Vec<Position>>())?;
			Geometry::MultiLineString(
				value.0.into_iter().map(line).collect::<FerrosGrynnResult<Vec<_>>>()?,
			)
		}
		sdk::Geometry::MultiPolygon(value) => {
			container(value.0.len(), std::mem::size_of::<Vec<Vec<Position>>>())?;
			Geometry::MultiPolygon(
				value.0.into_iter().map(polygon).collect::<FerrosGrynnResult<Vec<_>>>()?,
			)
		}
		sdk::Geometry::Collection(values) => {
			container(values.len(), std::mem::size_of::<Geometry>())?;
			Geometry::Collection(
				values
					.into_iter()
					.map(|value| geometry_in(value, context, depth + 1))
					.collect::<FerrosGrynnResult<Vec<_>>>()?,
			)
		}
	})
}
