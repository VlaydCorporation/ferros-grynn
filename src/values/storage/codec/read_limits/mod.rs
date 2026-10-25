//! Predicts reader admission without materializing another value tree.

use super::{DecodeLimits, admission::DecodeAdmission};
use crate::{
	gql_status::FerrosGrynnResult,
	values::{
		Geometry,
		Position,
		PropertyKeyId,
		Text,
		Vector,
		storage::{
			StorageBound,
			StorageRecordId,
			StorageRecordIdPart,
			StorageRecordValue,
			StorageValue,
		},
	},
};

/// Uses final DTO order because geometry scratch overlaps earlier siblings.
pub(super) fn check(value: &StorageValue, limits: DecodeLimits) -> FerrosGrynnResult<usize> {
	let mut admission = DecodeAdmission::new(limits);
	value_limits(value, 0, &mut admission)?;
	Ok(admission.allocated)
}

/// Mirrors only resource checks; canonical content remains the codec's job.
fn value_limits(
	value: &StorageValue,
	depth: usize,
	admission: &mut DecodeAdmission,
) -> FerrosGrynnResult<()> {
	admission.check_depth(depth)?;
	match value {
		StorageValue::String(value) => text(value, admission),
		StorageValue::Bytes(value) => {
			DecodeAdmission::check_length(
				"blob bytes",
				admission.limits.max_blob_bytes,
				value.len(),
			)?;
			admission.charge(value.len())
		}
		StorageValue::Array(values) | StorageValue::Set(values) => {
			admission.check_collection_len(values.len())?;
			admission.charge_array::<StorageValue>(values.len())?;
			for value in values {
				value_limits(value, depth + 1, admission)?;
			}
			Ok(())
		}
		StorageValue::Object(values) => {
			admission.check_collection_len(values.len())?;
			admission.charge_array::<(PropertyKeyId, StorageValue)>(values.len())?;
			for (_, value) in values {
				value_limits(value, depth + 1, admission)?;
			}
			Ok(())
		}
		StorageValue::Range(value) => {
			for bound in [&value.start, &value.end] {
				if let StorageBound::Included(value) | StorageBound::Excluded(value) = bound {
					admission.charge_array::<StorageValue>(1)?;
					value_limits(value, depth + 1, admission)?;
				}
			}
			Ok(())
		}
		StorageValue::Record(value) => record(value, depth + 1, admission),
		StorageValue::AtomRef(value) => record(&value.record_id, depth + 1, admission),
		StorageValue::Geometry(value) => {
			geometry(value, depth, admission, &mut 0)?;
			let retained = admission.allocated;
			admission.charge(value.validation_scratch_bytes()?)?;
			admission.allocated = retained;
			Ok(())
		}
		StorageValue::Vector(value) => {
			admission.check_collection_len(value.len())?;
			match value {
				Vector::F64(values) => admission.charge_array::<f64>(values.len()),
				Vector::F32(values) => admission.charge_array::<f32>(values.len()),
				Vector::F16(values) => admission.charge_array::<half::f16>(values.len()),
				Vector::I64(values) => admission.charge_array::<i64>(values.len()),
				Vector::I32(values) => admission.charge_array::<i32>(values.len()),
				Vector::I16(values) => admission.charge_array::<i16>(values.len()),
				Vector::I8(values) => admission.charge_array::<i8>(values.len()),
				Vector::U8(values) => admission.charge_array::<u8>(values.len()),
			}
		}
		StorageValue::None
		| StorageValue::Null
		| StorageValue::Bool(_)
		| StorageValue::Int(_)
		| StorageValue::Float(_)
		| StorageValue::Decimal(_)
		| StorageValue::DateTime(_)
		| StorageValue::Duration(_)
		| StorageValue::Uuid(_)
		| StorageValue::Ulid(_)
		| StorageValue::GraphRef(_)
		| StorageValue::QualifiedAtomRef(_) => Ok(()),
	}
}

/// Inline Text still consumes its full UTF-8 length in the reader's counter.
fn text(value: &Text, admission: &mut DecodeAdmission) -> FerrosGrynnResult<()> {
	DecodeAdmission::check_length("string bytes", admission.limits.max_string_bytes, value.len())?;
	admission.charge(value.len())
}

/// RecordId has its own checked frame even for scalar or empty array keys.
fn record(
	value: &StorageRecordId,
	depth: usize,
	admission: &mut DecodeAdmission,
) -> FerrosGrynnResult<()> {
	admission.check_depth(depth)?;
	match &value.part {
		StorageRecordIdPart::String(value) => text(value, admission),
		StorageRecordIdPart::Array(values) => record_values(values, depth + 1, admission),
		StorageRecordIdPart::Object(values) => record_object(values, depth + 1, admission),
		StorageRecordIdPart::Int(_)
		| StorageRecordIdPart::Uuid(_)
		| StorageRecordIdPart::Ulid(_) => Ok(()),
	}
}

/// Empty record arrays never enter the unused child frame.
fn record_values(
	values: &[StorageRecordValue],
	depth: usize,
	admission: &mut DecodeAdmission,
) -> FerrosGrynnResult<()> {
	admission.check_collection_len(values.len())?;
	admission.charge_array::<StorageRecordValue>(values.len())?;
	for value in values {
		record_value(value, depth, admission)?;
	}
	Ok(())
}

/// The record-object helper checks depth even when there are no members.
fn record_object(
	values: &[(Text, StorageRecordValue)],
	depth: usize,
	admission: &mut DecodeAdmission,
) -> FerrosGrynnResult<()> {
	admission.check_depth(depth)?;
	admission.check_collection_len(values.len())?;
	admission.charge_array::<(Text, StorageRecordValue)>(values.len())?;
	for (name, value) in values {
		text(name, admission)?;
		record_value(value, depth + 1, admission)?;
	}
	Ok(())
}

/// Nested record keys have the reader's restricted scalar/container shape.
fn record_value(
	value: &StorageRecordValue,
	depth: usize,
	admission: &mut DecodeAdmission,
) -> FerrosGrynnResult<()> {
	admission.check_depth(depth)?;
	match value {
		StorageRecordValue::String(value) => text(value, admission),
		StorageRecordValue::Array(values) => record_values(values, depth + 1, admission),
		StorageRecordValue::Object(values) => record_object(values, depth + 1, admission),
		StorageRecordValue::Int(_) | StorageRecordValue::Uuid(_) | StorageRecordValue::Ulid(_) => {
			Ok(())
		}
	}
}

/// Geometry aggregate limits count container members and positions separately.
fn geometry_count(
	count: usize,
	admission: &DecodeAdmission,
	elements: &mut usize,
) -> FerrosGrynnResult<()> {
	admission.check_collection_len(count)?;
	DecodeAdmission::geometry_elements(elements, count)
}

/// Each Position owns a separate two-element coordinate Vec.
fn positions(
	values: &[Position],
	admission: &mut DecodeAdmission,
	elements: &mut usize,
) -> FerrosGrynnResult<()> {
	geometry_count(values.len(), admission, elements)?;
	admission.charge_array::<Position>(values.len())?;
	for _ in values {
		admission.charge_array::<f64>(2)?;
	}
	Ok(())
}

/// Polygon rings retain both the outer vector and each position vector.
fn polygon(
	values: &[Vec<Position>],
	depth: usize,
	admission: &mut DecodeAdmission,
	elements: &mut usize,
) -> FerrosGrynnResult<()> {
	admission.check_depth(depth)?;
	geometry_count(values.len(), admission, elements)?;
	admission.charge_array::<Vec<Position>>(values.len())?;
	for ring in values {
		positions(ring, admission, elements)?;
	}
	Ok(())
}

/// Bounds cloned backing with the common reader's depth and count policy.
pub(in crate::values::storage) fn geometry_allocation(
	value: &Geometry,
	depth: usize,
	limits: DecodeLimits,
) -> FerrosGrynnResult<usize> {
	let mut admission = DecodeAdmission::new(limits);
	geometry(value, depth, &mut admission, &mut 0)?;
	Ok(admission.allocated)
}

/// Visits the actual reader's frames without cloning or entering a geometry
/// kernel.
fn geometry(
	value: &Geometry,
	depth: usize,
	admission: &mut DecodeAdmission,
	elements: &mut usize,
) -> FerrosGrynnResult<()> {
	admission.check_depth(depth)?;
	match value {
		Geometry::Point(_) => {
			DecodeAdmission::geometry_elements(elements, 1)?;
			admission.charge_array::<f64>(2)
		}
		Geometry::MultiPoint(values) | Geometry::LineString(values) => {
			positions(values, admission, elements)
		}
		Geometry::Polygon(values) => polygon(values, depth, admission, elements),
		Geometry::MultiLineString(values) => {
			geometry_count(values.len(), admission, elements)?;
			admission.charge_array::<Vec<Position>>(values.len())?;
			for line in values {
				positions(line, admission, elements)?;
			}
			Ok(())
		}
		Geometry::MultiPolygon(values) => {
			geometry_count(values.len(), admission, elements)?;
			admission.charge_array::<Vec<Vec<Position>>>(values.len())?;
			for value in values {
				polygon(value, depth, admission, elements)?;
			}
			Ok(())
		}
		Geometry::Collection(values) => {
			geometry_count(values.len(), admission, elements)?;
			admission.charge_array::<Geometry>(values.len())?;
			for value in values {
				geometry(value, depth + 1, admission, elements)?;
			}
			Ok(())
		}
	}
}

#[cfg(test)]
/// Differential resource-policy tests against the actual byte decoder.
mod tests;
