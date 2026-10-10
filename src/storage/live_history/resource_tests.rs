//! Resource boundaries that must agree in the encoder and decoder.

use super::{
	tests::{batch, bytes},
	*,
};
use crate::{
	gql_status::{
		FerrosGrynnError,
		LabeledLocation,
		LineColumn,
		Location,
		TextSpan,
		formatting::DiagnosticValue,
	},
	values::{Array, Object},
};

/// Exercises the complete control envelope rather than just a nested codec.
fn fault_bytes(error: FerrosGrynnError) -> FerrosGrynnResult<Vec<u8>> {
	let batch = batch()?;
	let payload = LivePayload::Fault {
		diagnostic: error,
		gap: LiveGap {
			id: batch.id(),
			from: None,
			through: Some(batch.versionstamp()),
			reason: LiveGapReason::EvaluationFault,
		},
	};
	encode(&payload, batch.id(), batch.version(), batch.versionstamp().get(), 100, 8 * 1024 * 1024)
}

/// Creates the required typed outer fault, without datastore evaluation.
fn fault() -> FerrosGrynnError {
	FerrosGrynnError::live_evaluation_fault("fixture", "FG-G1F09", "fixture evaluation")
}

/// A key one byte over the canonical text cap must fail before output growth.
#[test]
fn live_history_object_name_cap_is_symmetric() -> FerrosGrynnResult<()> {
	let mut original = batch()?;
	let mut object = Object::new();
	object.insert("x".repeat(16 * 1024 * 1024 + 1), Value::None);
	original.events[0].after = Some(Value::Object(object));
	let payload = LivePayload::Batch(original);
	let LivePayload::Batch(batch) = &payload else {
		return Err(invalid("fixture batch"));
	};
	assert!(
		encode(
			&payload,
			batch.id(),
			batch.version(),
			batch.versionstamp().get(),
			100,
			256 * 1024 * 1024
		)
		.is_err()
	);
	Ok(())
}

/// Encoded logical depth is checked even when the runtime value is
/// constructible.
#[test]
fn live_history_value_depth_is_bounded() -> FerrosGrynnResult<()> {
	let mut original = batch()?;
	let mut value = Value::None;
	for _ in 0..65 {
		value = Value::Array(Array::from(vec![value]));
	}
	original.events[0].after = Some(value);
	assert!(bytes(&original).is_err());
	Ok(())
}

/// Even an empty map adds a depth level; both directions use the same bound.
#[test]
fn live_history_diagnostic_map_depth_is_symmetric() -> FerrosGrynnResult<()> {
	for depth in [14, 15] {
		let mut value = DiagnosticValue::Map(std::collections::BTreeMap::new());
		for _ in 0..depth {
			value = DiagnosticValue::List(vec![value]);
		}
		let encoded = fault_bytes(fault().with_extension("nested", value));
		if depth == 15 {
			assert!(encoded.is_err(), "the empty map still exceeds depth 16");
		} else {
			let encoded = encoded?;
			assert!(matches!(decode(&encoded, 8 * 1024 * 1024)?, LivePayload::Fault { .. }));
		}
	}
	Ok(())
}

/// Publicly constructible invalid coordinates must fail before publication.
#[test]
fn live_history_invalid_diagnostic_location_is_rejected() -> FerrosGrynnResult<()> {
	let one = std::num::NonZeroU32::MIN;
	let two = std::num::NonZeroU32::new(2).ok_or_else(|| invalid("fixture coordinate"))?;
	let start = LineColumn::new(one, one);
	let end = LineColumn::new(two, one);
	for location in [
		Location::from_parts(None, TextSpan::new(u32::MAX, 1), start, end),
		Location::from_parts(None, TextSpan::new(0, 0), end, start),
	] {
		assert!(fault_bytes(fault().with_location(location)).is_err());
	}
	Ok(())
}

/// Large extension trees move into the status; constrained readers reject the
/// whole control without a partial diagnostic.
#[test]
fn live_history_diagnostic_extensions_and_related_locations_are_bounded() -> FerrosGrynnResult<()> {
	let one = std::num::NonZeroU32::MIN;
	let point = LineColumn::new(one, one);
	let location = Location::from_parts(None, TextSpan::new(0, 0), point, point);
	let mut error = fault();
	for _ in 0..300 {
		error = error.with_related_location(LabeledLocation::new(location.clone(), Some("result")));
	}
	for index in 0..8 {
		let items = (0..128).map(|_| DiagnosticValue::String("x".repeat(256))).collect();
		error = error.with_extension(format!("list{index}"), DiagnosticValue::List(items));
	}
	let expected = error.to_wire();
	let encoded = fault_bytes(error)?;
	let LivePayload::Fault {
		diagnostic,
		..
	} = decode(&encoded, 8 * 1024 * 1024)?
	else {
		return Err(invalid("fixture fault kind"));
	};
	assert_eq!(diagnostic.to_wire(), expected);
	assert!(decode(&encoded, encoded.len() + 1024).is_err());
	assert!(
		decode_delivery(&encoded, 8 * 1024 * 1024).is_err(),
		"a valid oversized diagnostic cannot use prepaid consumer storage"
	);
	Ok(())
}

/// The binary contract preserves pre-epoch time independently of JSON display.
#[test]
fn live_history_diagnostic_pre_epoch_time_is_exact() -> FerrosGrynnResult<()> {
	let timestamp = std::time::UNIX_EPOCH - std::time::Duration::new(3, 1);
	let encoded = fault_bytes(fault().with_timestamp(timestamp))?;
	let LivePayload::Fault {
		diagnostic,
		..
	} = decode(&encoded, 8 * 1024 * 1024)?
	else {
		return Err(invalid("fixture fault kind"));
	};
	assert_eq!(diagnostic.status().diagnostic().timestamp(), Some(timestamp));
	Ok(())
}
