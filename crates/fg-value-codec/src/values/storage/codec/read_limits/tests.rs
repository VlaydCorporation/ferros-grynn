use super::*;
use crate::{
	test_assertions::AssertResult as _,
	values::storage::{StorageAtomRef, StorageRange, codec::CanonicalCodec},
};

/// Uses the existing byte decoder as the independent resource oracle.
fn agrees(value: &StorageValue, limits: DecodeLimits) -> bool {
	let encoded = CanonicalCodec::encode(value.as_ref()).assert_ok();
	let actual = CanonicalCodec::check_decode_limits(value.as_ref(), limits);
	let expected = CanonicalCodec::decode_with_allocation(&encoded, limits);
	let accepted = actual.is_ok();
	assert_eq!(accepted, expected.is_ok(), "reader admission diverged: {actual:?}, {expected:?}");
	if accepted {
		assert_eq!(actual.assert_ok(), expected.assert_ok().1);
	} else {
		let actual = actual.assert_err();
		let expected = expected.assert_err();
		assert_eq!(
			format!("{:?}", actual.status().status()),
			format!("{:?}", expected.status().status())
		);
	}
	accepted
}

/// Small custom limits exercise boundary failures without large allocations.
fn limits(allocation: usize) -> DecodeLimits {
	DecodeLimits::new(64, 128, 1024, 1024, allocation)
}

/// Places a value at a known enclosing decoder depth.
fn nested(mut value: StorageValue, depth: usize) -> StorageValue {
	for _ in 0..depth {
		value = StorageValue::Array(vec![value]);
	}
	value
}

/// Creates a valid WGS-84 position without relying on unsafe fixtures.
fn point(longitude: f64, latitude: f64) -> Position {
	Position::new([longitude, latitude]).assert_ok()
}

/// Returns one minimal nondegenerate closed ring.
fn ring() -> Vec<Position> {
	vec![point(0.0, 0.0), point(2.0, 0.0), point(0.0, 2.0), point(0.0, 0.0)]
}

/// All allocated scalar shapes use exactly the byte reader's logical charges.
#[test]
fn reader_preflight_scalar_and_geometry_charges_match_decoder() {
	let values = [
		StorageValue::Int(7),
		StorageValue::String(Text::from("inline")),
		StorageValue::String(Text::from("é".repeat(17))),
		StorageValue::Bytes(vec![1, 2, 3].into()),
		StorageValue::Vector(Vector::F64(vec![1.0, 2.0])),
		StorageValue::Vector(Vector::F32(vec![1.0, 2.0])),
		StorageValue::Vector(Vector::F16(vec![half::f16::ONE; 2])),
		StorageValue::Vector(Vector::I64(vec![1, 2])),
		StorageValue::Vector(Vector::I32(vec![1, 2])),
		StorageValue::Vector(Vector::I16(vec![1, 2])),
		StorageValue::Vector(Vector::I8(vec![1, 2])),
		StorageValue::Vector(Vector::U8(vec![1, 2])),
		StorageValue::Geometry(Geometry::Point(point(0.0, 0.0))),
		StorageValue::Geometry(Geometry::MultiPoint(vec![point(0.0, 0.0), point(1.0, 1.0)])),
		StorageValue::Geometry(Geometry::LineString(vec![point(0.0, 0.0), point(1.0, 1.0)])),
		StorageValue::Geometry(Geometry::Polygon(vec![ring()])),
		StorageValue::Geometry(Geometry::MultiLineString(vec![vec![
			point(0.0, 0.0),
			point(1.0, 1.0),
		]])),
		StorageValue::Geometry(Geometry::MultiPolygon(vec![vec![ring()]])),
		StorageValue::Geometry(Geometry::Collection(vec![
			Geometry::Polygon(vec![ring()]),
			Geometry::Point(point(1.0, 1.0)),
		])),
	];
	for value in &values {
		for maximum in [0, 1, 3, 7, 15, 16, 17, 23, 24, 31, 48, 80, 256, 1024, 16384] {
			agrees(value, limits(maximum));
		}
		assert!(agrees(value, limits(16384)));
	}
	assert_eq!(
		CanonicalCodec::check_decode_limits(
			StorageValue::String(Text::from("abc")).as_ref(),
			limits(3)
		)
		.assert_ok(),
		3
	);
}

/// Empty helpers must not invent a child frame that the reader never enters.
#[test]
fn reader_preflight_preserves_empty_and_record_depth_rules() {
	for maximum in [0, 1, 2, 3, 63, 64] {
		let mut policy = limits(1 << 20);
		policy.max_depth = maximum;
		for empty in [
			StorageValue::Array(Vec::new()),
			StorageValue::Set(Vec::new()),
			StorageValue::Object(Vec::new()),
			StorageValue::Range(StorageRange {
				start: StorageBound::Unbounded,
				end: StorageBound::Unbounded,
			}),
		] {
			assert!(agrees(&nested(empty, maximum), policy));
		}
		assert!(agrees(&nested(StorageValue::Int(0), maximum), policy));
		assert!(!agrees(&nested(StorageValue::Int(0), maximum + 1), policy));
	}
	for (part, accepted) in [
		(StorageRecordIdPart::Int(1), true),
		(StorageRecordIdPart::Array(Vec::new()), true),
		(StorageRecordIdPart::Object(Vec::new()), false),
	] {
		let record = StorageRecordId {
			class_id: 1,
			part,
		};
		assert_eq!(
			agrees(&nested(StorageValue::Record(record.clone()), 63), limits(1 << 20)),
			accepted
		);
		let atom = StorageAtomRef {
			kind: crate::graph::AtomKind::Node,
			record_id: record.clone(),
		};
		assert_eq!(agrees(&nested(StorageValue::AtomRef(atom), 63), limits(1 << 20)), accepted);
		assert!(!agrees(&nested(StorageValue::Record(record), 64), limits(1 << 20)));
	}
}

/// Record keys use text payloads while ordinary Object keys use dictionary IDs.
#[test]
fn reader_preflight_record_containers_and_ranges_match_decoder() {
	let record = StorageRecordId {
		class_id: 1,
		part: StorageRecordIdPart::Object(vec![(
			Text::from("key"),
			StorageRecordValue::Array(vec![
				StorageRecordValue::String(Text::from("text")),
				StorageRecordValue::Object(vec![(Text::from("x"), StorageRecordValue::Int(1))]),
			]),
		)]),
	};
	let values = [
		StorageValue::Record(record),
		StorageValue::Range(StorageRange {
			start: StorageBound::Included(Box::new(StorageValue::String(Text::from("a")))),
			end: StorageBound::Excluded(Box::new(StorageValue::String(Text::from("z")))),
		}),
		StorageValue::Object(vec![(
			PropertyKeyId::new(1),
			StorageValue::String(Text::from("text")),
		)]),
	];
	for value in &values {
		for maximum in [0, 1, 3, 16, 80, 256, 1024] {
			agrees(value, limits(maximum));
		}
		for depth in 0..6 {
			let mut policy = limits(1 << 20);
			policy.max_depth = depth;
			agrees(value, policy);
		}
	}
}

/// String, blob and collection caps are independent of the aggregate charge.
#[test]
fn reader_preflight_checks_lengths_and_typed_vector_policy() {
	let policy = DecodeLimits::new(64, 3, 3, 3, 16384);
	for length in 0..=4 {
		assert_eq!(
			agrees(&StorageValue::String(Text::from("x".repeat(length))), policy),
			length <= 3
		);
		assert_eq!(agrees(&StorageValue::Bytes(vec![1; length].into()), policy), length <= 3);
		assert_eq!(
			agrees(&StorageValue::Array(vec![StorageValue::Int(1); length]), policy),
			length <= 3
		);
		let values = (0..length)
			.map(|index| {
				(PropertyKeyId::new(u32::try_from(index).assert_ok()), StorageValue::Int(1))
			})
			.collect();
		assert_eq!(agrees(&StorageValue::Object(values), policy), length <= 3);
		let record = StorageValue::Record(StorageRecordId {
			class_id: 1,
			part: StorageRecordIdPart::String(Text::from("x".repeat(length))),
		});
		assert_eq!(agrees(&record, policy), length <= 3);
	}
	let policy = DecodeLimits::new(64, 8, 3, 0, 16384);
	assert!(agrees(&StorageValue::Vector(Vector::U8(vec![1; 4])), policy));
}

/// Geometry scratch is temporary but overlaps all earlier canonical siblings.
#[test]
fn reader_preflight_geometry_peak_follows_final_dto_order() {
	let geometry = Geometry::MultiPoint(vec![point(0.0, 0.0), point(1.0, 1.0)]);
	let retained = 2 * size_of::<Position>() + 4 * size_of::<f64>();
	let scratch = geometry.validation_scratch_bytes().assert_ok();
	let text_bytes = 64;
	let text = StorageValue::String(Text::from("x".repeat(text_bytes)));
	let geo = StorageValue::Geometry(geometry);
	let object_bytes = 2 * size_of::<(PropertyKeyId, StorageValue)>();
	let geometry_first = StorageValue::Object(vec![
		(PropertyKeyId::new(1), geo.clone()),
		(PropertyKeyId::new(2), text.clone()),
	]);
	let text_first = StorageValue::Object(vec![
		(PropertyKeyId::new(1), text.clone()),
		(PropertyKeyId::new(2), geo.clone()),
	]);
	let final_bytes = object_bytes + retained + text_bytes;
	// Validation includes fixed scratch beyond its coordinate buffers.
	let geometry_first_peak = object_bytes + retained + scratch.max(text_bytes);
	let text_first_peak = final_bytes + scratch;
	assert!(geometry_first_peak < text_first_peak);
	assert!(!agrees(&geometry_first, limits(geometry_first_peak - 1)));
	assert!(agrees(&geometry_first, limits(geometry_first_peak)));
	assert!(!agrees(&text_first, limits(geometry_first_peak)));
	assert!(!agrees(&text_first, limits(text_first_peak - 1)));
	assert!(agrees(&text_first, limits(text_first_peak)));
	assert_eq!(
		CanonicalCodec::check_decode_limits(text_first.as_ref(), limits(text_first_peak))
			.assert_ok(),
		final_bytes
	);
	let set = StorageValue::Set(vec![text, geo]);
	let final_bytes = 2 * size_of::<StorageValue>() + retained + text_bytes;
	assert!(!agrees(
		&set,
		limits(2 * size_of::<StorageValue>() + retained + scratch.max(text_bytes))
	));
	assert!(!agrees(&set, limits(final_bytes + scratch - 1)));
	assert!(agrees(&set, limits(final_bytes + scratch)));
}

/// Geometry collection depth includes its enclosing ordinary value frame.
#[test]
fn reader_preflight_geometry_uses_enclosing_depth() {
	let collection = Geometry::Collection(vec![Geometry::Point(point(0.0, 0.0))]);
	for maximum in [0, 1, 63, 64] {
		let mut policy = limits(1 << 20);
		policy.max_depth = maximum;
		let value = nested(StorageValue::Geometry(collection.clone()), maximum);
		assert!(!agrees(&value, policy));
		assert!(agrees(
			&nested(StorageValue::Geometry(Geometry::Collection(Vec::new())), maximum),
			policy
		));
		if maximum != 0 {
			assert!(agrees(
				&nested(StorageValue::Geometry(collection.clone()), maximum - 1),
				policy
			));
		}
	}
}

/// Literal limits catch shared diagnostic mistakes that differential tests
/// miss.
#[test]
fn reader_limit_diagnostics_label_actual_and_maximum() {
	use crate::gql_status::Status;

	let cases = [
		(
			StorageValue::Array(vec![StorageValue::Null]),
			DecodeLimits {
				max_depth: 0,
				..DecodeLimits::default()
			},
			(1, 0),
		),
		(
			StorageValue::Array(vec![StorageValue::Null, StorageValue::Null]),
			DecodeLimits {
				max_collection_len: 1,
				..DecodeLimits::default()
			},
			(2, 1),
		),
		(
			StorageValue::String(Text::from("é")),
			DecodeLimits {
				max_string_bytes: 1,
				..DecodeLimits::default()
			},
			(2, 1),
		),
		(
			StorageValue::Bytes(vec![1_u8, 2, 3].into()),
			DecodeLimits {
				max_blob_bytes: 2,
				..DecodeLimits::default()
			},
			(3, 2),
		),
		(
			StorageValue::String(Text::from("é")),
			DecodeLimits {
				max_total_allocation: 1,
				..DecodeLimits::default()
			},
			(2, 1),
		),
	];
	for (value, policy, expected) in cases {
		let encoded = CanonicalCodec::encode(value.as_ref()).assert_ok();
		let errors = [
			CanonicalCodec::decode(&encoded, policy).assert_err(),
			CanonicalCodec::check_decode_limits(value.as_ref(), policy).assert_err(),
		];
		for error in errors {
			assert!(
				matches!(error.status().status(),
					Status::StorageDecodeLimitExceeded { actual, maximum, .. }
					if (*actual, *maximum) == expected
				),
				"expected actual/maximum {expected:?}, received {error:?}"
			);
		}
	}
}
