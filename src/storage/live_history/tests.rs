//! Independent byte-contract and exact historical result checks.

use std::sync::Arc;

use super::*;
use crate::{
	gql_status::FerrosGrynnError,
	graph::{
		AtomKind,
		AtomOccurrenceRef,
		AtomRef,
		EndpointRole,
		GraphId,
		GraphKind,
		GraphRef,
		GraphStorageMode,
		Path,
		PathDirection,
		PathStep,
		SnapshotId,
	},
	live::LiveBindingId,
	values::{Array, Number, Object},
};

/// Creates a deterministic owned commit with an exact integer above 2^53.
pub(super) fn batch() -> FerrosGrynnResult<LiveBatch> {
	let stamp = (1_u64 << 53) + 17;
	Ok(LiveBatch {
		id: "11111111-1111-1111-1111-111111111111".parse()?,
		version: LiveVersion::new(7)?,
		stamp: stamp.into(),
		committed_at: DateTime::from(
			jiff::Timestamp::from_nanosecond(100)?.to_zoned(jiff::tz::TimeZone::UTC),
		),
		events: vec![LiveEvent {
			output: None,
			ordinal: 0,
			identity: LiveIdentity::Aggregate(Vec::new()),
			before: None,
			after: Some(Value::None),
		}],
		bindings: vec![LiveBindingRevision {
			id: "22222222-2222-2222-2222-222222222222".parse::<LiveBindingId>()?,
			definition: LiveVersion::new(3)?,
			value: stamp.into(),
		}],
	})
}

/// Encodes the logical envelope without a datastore or current primary root.
pub(super) fn bytes(batch: &LiveBatch) -> FerrosGrynnResult<Vec<u8>> {
	encode(
		&LivePayload::Batch(batch.clone()),
		batch.id(),
		batch.version(),
		batch.versionstamp().get(),
		batch.committed_at().as_inner().timestamp().as_nanosecond(),
		8 * 1024 * 1024,
	)
}

/// Uses a full historical graph descriptor; same atom in a second scope
/// differs.
fn occurrence(id: u64, name: &str, kind: AtomKind) -> FerrosGrynnResult<AtomOccurrenceRef> {
	Ok(AtomOccurrenceRef::new(
		AtomRef::new(kind, "item:a".parse()?),
		Arc::new(GraphRef::new(
			GraphId::new(id).ok_or_else(|| invalid("fixture graph id"))?,
			name,
			GraphKind::Metagraph,
			GraphStorageMode::Hybrid,
		)?),
	))
}

/// Independent tags/counts and exact large positions survive checked
/// whole-commit history without consulting current records.
#[test]
fn live_composite_history_golden_roundtrip_and_limits() {
	use crate::test_assertions::AssertResult as _;

	let identity = LiveIdentity::Composite(vec![
		LiveIdentityPart::Record("item:a".parse().assert_ok()),
		LiveIdentityPart::Occurrence(occurrence(1, "original", AtomKind::Edge).assert_ok()),
		LiveIdentityPart::Unmatched,
		LiveIdentityPart::Split(u64::MAX),
	]);
	let mut encoded = Vec::new();
	let mut context = EncodeContext::default();
	encode_identity(
		&mut encoded,
		&identity,
		&mut context,
		&mut CodecBudget::new(1 << 20, 0).assert_ok(),
		&mut Work {
			remaining: MAX_NODES,
		},
	)
	.assert_ok();
	assert_eq!(&encoded[..6], &[3, 4, 0, 0, 0, 0]);
	assert_eq!(&encoded[encoded.len() - 10..encoded.len() - 8], &[2, 3]);
	assert_eq!(&encoded[encoded.len() - 8..], &u64::MAX.to_le_bytes());
	let decode_body = |data: &[u8], maximum, work| -> FerrosGrynnResult<_> {
		decode_identity(
			&mut Decoder::new(data),
			&context,
			&mut CodecBudget::new(maximum, 0)?,
			&mut Work {
				remaining: work,
			},
		)
	};
	assert_eq!(decode_body(&encoded, 1 << 20, MAX_NODES).assert_ok(), identity);
	assert!(decode_body(&encoded, 1 << 20, 3).is_err());
	assert!(decode_body(&encoded, 1, MAX_NODES).is_err());
	for length in 0..encoded.len() {
		assert!(decode_body(&encoded[..length], 1 << 20, MAX_NODES).is_err());
	}
	let mut unknown = encoded.clone();
	let part = unknown.len() - 9;
	unknown[part] = 4;
	assert!(decode_body(&unknown, 1 << 20, MAX_NODES).is_err());
	for count in [0_u32, 1, u32::MAX] {
		let mut malformed = encoded.clone();
		malformed[1..5].copy_from_slice(&count.to_le_bytes());
		assert!(decode_body(&malformed, 1 << 20, MAX_NODES).is_err());
	}
	let mut writer = Vec::new();
	assert!(
		encode_identity(
			&mut writer,
			&identity,
			&mut EncodeContext::default(),
			&mut CodecBudget::new(1, 0).assert_ok(),
			&mut Work {
				remaining: MAX_NODES
			}
		)
		.is_err()
	);
	let mut original = batch().assert_ok();
	original.events[0].identity = identity;
	original.events[0].before = Some(Value::from("same"));
	original.events[0].after = Some(Value::from("changed"));
	let frame = bytes(&original).assert_ok();
	let LivePayload::Batch(restored) = decode(&frame, 8 << 20).assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(invalid("composite result history lost its batch"));
			test_result.assert_ok();
		};
	};
	assert_eq!(restored, original);
	assert_eq!(bytes(&restored).assert_ok(), frame);
}

/// Shape validation is shared by encoding and decoding, before publication.
#[test]
fn live_composite_history_rejects_noncanonical_shapes() {
	use crate::test_assertions::AssertResult as _;

	let record: crate::values::RecordId = "item:a".parse().assert_ok();
	let invalid_parts = [
		vec![],
		vec![LiveIdentityPart::Record(record.clone())],
		vec![LiveIdentityPart::Unmatched, LiveIdentityPart::Split(0)],
		vec![
			LiveIdentityPart::Record(record),
			LiveIdentityPart::Split(0),
			LiveIdentityPart::Unmatched,
		],
	];
	for parts in invalid_parts {
		let mut encoded = Vec::new();
		let mut context = EncodeContext::default();
		assert!(
			encode_identity(
				&mut encoded,
				&LiveIdentity::Composite(parts.clone()),
				&mut context,
				&mut CodecBudget::new(1 << 20, 0).assert_ok(),
				&mut Work {
					remaining: MAX_NODES
				}
			)
			.is_err()
		);
		assert!(encoded.is_empty(), "invalid shape is rejected before writing its identity");
		let mut malformed = vec![3];
		canonical::write_count(
			&mut malformed,
			parts.len(),
			&mut CodecBudget::new(1 << 20, 0).assert_ok(),
		)
		.assert_ok();
		for part in parts {
			match part {
				LiveIdentityPart::Record(record) => encode_record_identity(
					&mut malformed,
					&record,
					&mut context,
					&mut CodecBudget::new(1 << 20, 0).assert_ok(),
				)
				.assert_ok(),
				LiveIdentityPart::Unmatched => malformed.push(2),
				LiveIdentityPart::Split(position) => {
					malformed.push(3);
					malformed.extend_from_slice(&position.to_le_bytes());
				}
				LiveIdentityPart::Occurrence(_) => {
					return {
						let test_result: FerrosGrynnResult<()> =
							Err(invalid("unexpected invalid fixture"));
						test_result.assert_ok();
					};
				}
			}
		}
		assert!(
			decode_identity(
				&mut Decoder::new(&malformed),
				&context,
				&mut CodecBudget::new(1 << 20, 0).assert_ok(),
				&mut Work {
					remaining: MAX_NODES
				}
			)
			.is_err()
		);
	}
}

/// Header bytes are independent golden assertions, not encoder/decoder
/// agreement.
#[test]
fn live_history_header_golden_and_exact_none() {
	use crate::test_assertions::AssertResult as _;

	let original = batch().assert_ok();
	let encoded = bytes(&original).assert_ok();
	assert_eq!(&encoded[..8], b"FGLB\x02\x00\x00\x00");
	assert_eq!(&encoded[8..24], &[0x11; 16]);
	assert_eq!(&encoded[24..32], &7_u64.to_le_bytes());
	assert_eq!(&encoded[32..40], &((1_u64 << 53) + 17).to_le_bytes());
	assert_eq!(&encoded[40..56], &100_i128.to_le_bytes());
	assert_eq!(&encoded[56..64], &[1, 0, 0, 0, 1, 0, 0, 0]);
	assert_eq!(&encoded[80..96], &[0; 16]);
	let LivePayload::Batch(restored) = decode(&encoded, 8 * 1024 * 1024).assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> = Err(invalid("fixture batch kind"));
			test_result.assert_ok();
		};
	};
	assert_eq!(restored, original);
	assert_eq!(restored.events()[0].before(), None);
	assert_eq!(restored.events()[0].after(), Some(&Value::None));
	assert_eq!(
		inspect(&encoded[..96], encoded.len() as u64).assert_ok().stamp,
		original.versionstamp().get()
	);
}

/// The computed envelope uses existing continuation pages and one aggregate
/// decode cap, including page buffers and the restored owned result.
#[test]
fn live_history_shared_continuations_keep_whole_commit_and_budget() {
	use crate::{
		storage::{
			HistoryOwner,
			history_codec::pages::{
				SegmentHeader,
				decode_header,
				decode_pages,
				encode_header,
				encode_pages,
			},
			page::AlignedPage,
		},
		test_assertions::AssertResult as _,
	};
	let mut original = batch().assert_ok();
	original.events[0].after = Some(Value::from("x".repeat(33 * 1024)));
	let encoded = bytes(&original).assert_ok();
	let mut write_budget = CodecBudget::new(8 * 1024 * 1024, encoded.len()).assert_ok();
	let (locator, pages) =
		encode_pages(&encoded, original.versionstamp().get(), 1, &mut write_budget).assert_ok();
	assert!(locator.pages > 1);
	let header = SegmentHeader {
		database: uuid::Uuid::from_u128(1),
		owner: HistoryOwner::Live(original.id()),
		segment: uuid::Uuid::from_u128(2),
		pages: 1 + u64::from(locator.pages),
		commits: 1,
		first_stamp: locator.stamp,
		last_stamp: locator.stamp,
		first_time: 100,
		last_time: 100,
	};
	let mut page = AlignedPage::zeroed();
	encode_header(header, &mut page).assert_ok();
	assert_eq!(decode_header(&page).assert_ok(), header);
	let read = |number: crate::storage::page::PageNo, output: &mut AlignedPage| {
		let index =
			usize::try_from(number.get() - 1).map_err(|_error| invalid("fixture page index"))?;
		let source = pages.get(index).ok_or_else(|| invalid("fixture page absent"))?;
		output.as_bytes_mut().copy_from_slice(source.as_bytes());
		Ok(())
	};
	let mut budget = CodecBudget::new(8 * 1024 * 1024, crate::storage::PAGE_SIZE).assert_ok();
	let restored_bytes = decode_pages(
		locator,
		header.pages,
		&mut |number, output| read(number, output),
		&mut budget,
	)
	.assert_ok();
	let LivePayload::Batch(restored) = decode_bounded(&restored_bytes, &mut budget).assert_ok()
	else {
		return {
			let test_result: FerrosGrynnResult<()> = Err(invalid("fixture batch kind"));
			test_result.assert_ok();
		};
	};
	assert_eq!(restored, original);
	let mut budget = CodecBudget::new(encoded.len() + 2 * crate::storage::PAGE_SIZE, 0).assert_ok();
	let rejection = decode_pages(
		locator,
		header.pages,
		&mut |number, output| read(number, output),
		&mut budget,
	)
	.and_then(|bytes| decode_bounded(&bytes, &mut budget));
	assert!(rejection.is_err(), "payload decode cannot reset the complete page admission");
}

/// Qualified paths and nested rows retain captured descriptors after schema
/// changes.
#[test]
fn live_history_nested_qualified_path_and_numeric_representations() {
	use crate::test_assertions::AssertResult as _;

	let mut original = batch().assert_ok();
	let left = occurrence(1, "historical-before-rename", AtomKind::Node).assert_ok();
	let right = occurrence(2, "child-before-rename", AtomKind::Edge).assert_ok();
	let path = Path::new(
		original.id().as_uuid(),
		SnapshotId::new(44, 3),
		vec![PathStep::new(
			left.clone(),
			Some(right.clone()),
			PathDirection::Reverse,
			EndpointRole::Target,
		)],
		Some(Number::Int(9_007_199_254_740_993)),
	);
	let mut object = Object::new();
	object.insert("path", Value::from(path));
	object.insert("atom", Value::from(left.clone()));
	object.insert("int", Value::Number(Number::Int(1)));
	object.insert("float", Value::Number(Number::Float(1.0)));
	object.insert("scope", Value::from(right.clone()));
	original.events = vec![
		LiveEvent {
			output: None,
			ordinal: 0,
			identity: LiveIdentity::Occurrence(left),
			before: Some(Value::Null),
			after: Some(Value::Object(object)),
		},
		LiveEvent {
			output: None,
			ordinal: 1,
			identity: LiveIdentity::Occurrence(right),
			before: Some(Value::Array(Array::from(vec![Value::None, Value::Null]))),
			after: None,
		},
	];
	let LivePayload::Batch(restored) =
		decode(&bytes(&original).assert_ok(), 8 * 1024 * 1024).assert_ok()
	else {
		return {
			let test_result: FerrosGrynnResult<()> = Err(invalid("fixture batch kind"));
			test_result.assert_ok();
		};
	};
	assert_eq!(restored, original);
	assert!(crate::values::runtime::same_value(
		restored.events[0].after.as_ref().ok_or_else(|| invalid("fixture image")).assert_ok(),
		original.events[0].after.as_ref().ok_or_else(|| invalid("fixture image")).assert_ok()
	));
	let Value::Object(row) =
		restored.events[0].after.as_ref().ok_or_else(|| invalid("fixture row")).assert_ok()
	else {
		return {
			let test_result: FerrosGrynnResult<()> = Err(invalid("fixture object"));
			test_result.assert_ok();
		};
	};
	assert!(matches!(row.get("int"), Some(Value::Number(Number::Int(1)))));
	assert!(matches!(row.get("float"), Some(Value::Number(Number::Float(_)))));
	let Some(Value::Path(path)) = row.get("path") else {
		return {
			let test_result: FerrosGrynnResult<()> = Err(invalid("fixture path"));
			test_result.assert_ok();
		};
	};
	assert_eq!(path.steps()[0].atom().graph().name(), "historical-before-rename");
	assert_eq!(
		path.steps()[0].via_edge().map(|edge| edge.graph().name().as_str()),
		Some("child-before-rename")
	);
	assert_eq!(path.snapshot(), SnapshotId::new(44, 3));
	assert!(matches!(path.cost(), Some(Number::Int(9_007_199_254_740_993))));
}

/// Every truncation and nonzero reserved header byte is rejected before rows
/// escape.
#[test]
fn live_history_malformed_lengths_reserved_and_admission() {
	use crate::test_assertions::AssertResult as _;

	let original = batch().assert_ok();
	let encoded = bytes(&original).assert_ok();
	for length in 0..encoded.len() {
		assert!(decode(&encoded[..length], 8 * 1024 * 1024).is_err(), "prefix {length}");
	}
	for offset in [7, 80, 95] {
		let mut malformed = encoded.clone();
		malformed[offset] = 1;
		assert!(decode(&malformed, 8 * 1024 * 1024).is_err());
	}
	for offset in [56, 60] {
		let mut malformed = encoded.clone();
		malformed[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
		assert!(decode(&malformed, 8 * 1024 * 1024).is_err());
	}
	for offset in [64, 72] {
		let mut malformed = encoded.clone();
		malformed[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
		assert!(decode(&malformed, 8 * 1024 * 1024).is_err());
	}
	assert!(decode(&encoded, encoded.len()).is_err());
	assert!(
		encode(
			&LivePayload::Batch(original.clone()),
			original.id(),
			original.version(),
			original.versionstamp().get(),
			100,
			64
		)
		.is_err()
	);
	let mut malformed = encoded;
	malformed.push(0);
	assert!(decode(&malformed, 8 * 1024 * 1024).is_err());
}

/// Order, clocks and optional images are validated independently of encoding.
#[test]
fn live_history_rejects_invalid_net_order_and_clocks() {
	use crate::test_assertions::AssertResult as _;

	let mut original = batch().assert_ok();
	original.events[0].ordinal = 1;
	assert!(bytes(&original).is_err());
	original.events[0].ordinal = 0;
	original.events.push(original.events[0].clone());
	original.events[1].ordinal = 1;
	assert!(bytes(&original).is_err());
	original.events.pop();
	original.bindings[0].value = (original.versionstamp().get() + 1).into();
	assert!(bytes(&original).is_err());
	original.bindings[0].value = original.versionstamp();
	original.events[0].after = None;
	assert!(bytes(&original).is_err());
}

/// Faults and unknown bounds remain explicit whole-commit control deliveries.
#[test]
fn live_history_control_gap_and_fault_roundtrip() {
	use crate::test_assertions::AssertResult as _;

	let original = batch().assert_ok();
	let gap = LiveGap {
		id: original.id(),
		reason: LiveGapReason::EvaluationFault,
		from: None,
		through: Some(original.versionstamp()),
	};
	let location = crate::gql_status::Location::from_source(
		None,
		"query",
		crate::gql_status::TextSpan::new(0, 5),
	)
	.map_err(|_error| invalid("fixture location"))
	.assert_ok();
	let cause = FerrosGrynnError::storage_decode_limit_exceeded(
		"LIVE result bytes",
		(1_u64 << 53) + 17,
		(1_u64 << 53) + 18,
	)
	.with_phase(crate::gql_status::ExecutionPhase::Execution)
	.with_location(location.clone())
	.with_related_location(crate::gql_status::LabeledLocation::new(location, Some("result")))
	.with_extension("exact", crate::gql_status::formatting::DiagnosticValue::Unsigned(u64::MAX))
	.with_timestamp(std::time::UNIX_EPOCH + std::time::Duration::new(3, 1));
	let payload = LivePayload::Fault {
		diagnostic: FerrosGrynnError::live_evaluation_fault(
			original.id().to_string(),
			cause.status().code().to_string(),
			"fixture evaluation",
		)
		.with_cause(cause),
		gap: gap.clone(),
	};
	let encoded = encode(
		&payload,
		original.id(),
		original.version(),
		original.versionstamp().get(),
		100,
		64 * 1024,
	)
	.assert_ok();
	let LivePayload::Fault {
		diagnostic: original_diagnostic,
		..
	} = &payload
	else {
		return {
			let test_result: FerrosGrynnResult<()> = Err(invalid("fixture fault kind"));
			test_result.assert_ok();
		};
	};
	let LivePayload::Fault {
		diagnostic,
		gap: restored,
	} = decode(&encoded, 64 * 1024).assert_ok()
	else {
		return {
			let test_result: FerrosGrynnResult<()> = Err(invalid("fixture fault"));
			test_result.assert_ok();
		};
	};
	assert_eq!(restored, gap);
	assert_eq!(diagnostic.status().code().to_string(), "FG-G1F09");
	assert_eq!(
		diagnostic.to_wire(),
		original_diagnostic.to_wire(),
		"typed cause parameters and metadata survive replay exactly"
	);
	assert_eq!(inspect(&encoded, encoded.len() as u64).assert_ok().events, 0);
}
