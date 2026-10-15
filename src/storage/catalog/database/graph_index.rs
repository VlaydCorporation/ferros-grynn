//! Checked graph index definitions in the development catalog layout.

use std::collections::BTreeSet;

use super::codec::{Decoder, invalid, read_optional_string, write_optional_string};
use crate::{
	datastore::{
		ClassIndexState,
		FunctionId,
		GraphIndex,
		GraphIndexCost,
		GraphIndexDefinition,
		IndexId,
		IndexMaintenanceMode,
		NeighbourhoodAlgorithm,
		PathIndexAlgorithm,
		ReachabilityAlgorithm,
		SchemaVersion,
		TensorCacheFormat,
	},
	gql_status::FerrosGrynnResult,
	graph::{GraphId, TraversalDirection, TraversalSemantics, tensor::TensorScope},
	values::Text,
};

/// Writes availability separately from the canonical semantic descriptor.
pub(super) fn encode(output: &mut Vec<u8>, index: &GraphIndex) -> FerrosGrynnResult<()> {
	let descriptor = index.descriptor()?;
	output.push(match index.state {
		ClassIndexState::Ready => 0,
		ClassIndexState::Stale => 1,
		ClassIndexState::PreparedForRemoval => 2,
	});
	output.extend_from_slice(&[0; 7]);
	output.extend_from_slice(&index.build_revision.to_le_bytes());
	output.extend_from_slice(&(descriptor.len() as u32).to_le_bytes());
	output.extend_from_slice(&descriptor);
	output.extend_from_slice(&rapidhash::v3::rapidhash_v3(&descriptor).to_le_bytes());
	write_optional_string(output, index.comment.as_ref().map(Text::as_str), "graph index comment")
}

/// Rejects oversized, truncated, noncanonical, or semantically invalid
/// descriptors.
pub(super) fn decode(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<GraphIndex> {
	let state = match decoder.read_u8()? {
		0 => ClassIndexState::Ready,
		1 => ClassIndexState::Stale,
		2 => ClassIndexState::PreparedForRemoval,
		_ => return Err(invalid("graph index state is invalid")),
	};
	decoder.expect_zero(7, "graph index control reserved bytes")?;
	let build_revision = decoder.read_u64()?;
	let length = bounded_count(decoder, 8 * 1024 * 1024, "graph index descriptor")?;
	let bytes = decoder.read_exact(length)?;
	if decoder.read_u64()? != rapidhash::v3::rapidhash_v3(bytes) {
		return Err(invalid("graph index definition fingerprint disagrees"));
	}
	let comment = read_optional_string(decoder, "graph index comment")?.map(Into::into);
	let mut descriptor = Decoder::new(bytes);
	let id = IndexId::from_storage(descriptor.read_u32()?)?;
	let version = SchemaVersion::from_storage(descriptor.read_u32()?)?;
	let graph =
		GraphId::new(descriptor.read_u64()?).ok_or_else(|| invalid("graph index owner is zero"))?;
	let mode = match descriptor.read_u8()? {
		0 => TraversalSemantics::Node,
		1 => TraversalSemantics::Atom,
		_ => return Err(invalid("graph index mode is invalid")),
	};
	let direction = match descriptor.read_u8()? {
		0 => TraversalDirection::Forward,
		1 => TraversalDirection::Backward,
		2 => TraversalDirection::Both,
		_ => return Err(invalid("graph index direction is invalid")),
	};
	let maintenance = match descriptor.read_u8()? {
		0 => IndexMaintenanceMode::Immediate,
		1 => IndexMaintenanceMode::Deferred,
		_ => return Err(invalid("graph index maintenance mode is invalid")),
	};
	let kind = descriptor.read_u8()?;
	let algorithm = descriptor.read_u8()?;
	descriptor.expect_zero(3, "graph index descriptor reserved bytes")?;
	let parameter = descriptor.read_u32()?;
	let radius = descriptor.read_u32()?;
	let max_bytes = match descriptor.read_u64()? {
		0 => None,
		value => Some(value),
	};
	let cost_tag = descriptor.read_u8()?;
	descriptor.expect_zero(3, "graph index cost reserved bytes")?;
	let function = descriptor.read_u32()?;
	let function_version = descriptor.read_u32()?;
	let cost = match (cost_tag, function, function_version) {
		(0, 0, 0) => GraphIndexCost::Hop,
		(1, 0, 0) => GraphIndexCost::Weighted,
		(2, function, version) => GraphIndexCost::Function {
			function: FunctionId::from_storage(function)?,
			version: SchemaVersion::from_storage(version)?,
		},
		_ => return Err(invalid("graph index cost metadata is invalid")),
	};
	let name = text(&mut descriptor, 4096)?;
	let mut edge_labels = BTreeSet::new();
	for _ in 0..bounded_count(&mut descriptor, 1024, "graph index labels")? {
		if !edge_labels.insert(text(&mut descriptor, 4096)?) {
			return Err(invalid("graph index labels are duplicated"));
		}
	}
	let mut dependencies = BTreeSet::new();
	for _ in 0..bounded_count(&mut descriptor, 1024, "graph index dependencies")? {
		let entry = (
			FunctionId::from_storage(descriptor.read_u32()?)?,
			SchemaVersion::from_storage(descriptor.read_u32()?)?,
		);
		if !dependencies.insert(entry) {
			return Err(invalid("graph index dependencies are duplicated"));
		}
	}
	let mut patterns = Vec::new();
	let mut pattern_bytes = 0_usize;
	for _ in 0..bounded_count(&mut descriptor, 64, "graph index patterns")? {
		let pattern = text(&mut descriptor, 1024 * 1024 - pattern_bytes)?;
		pattern_bytes += pattern.len();
		patterns.push(pattern);
	}
	descriptor.finish()?;
	let narrow_u8 = || {
		u8::try_from(parameter).map_err(|_error| invalid("graph algorithm parameter exceeds u8"))
	};
	let narrow_u16 = || {
		u16::try_from(parameter).map_err(|_error| invalid("graph algorithm parameter exceeds u16"))
	};
	let definition = match kind {
		0 => GraphIndexDefinition::Reachability {
			algorithm: match algorithm {
				0 if parameter == 0 => ReachabilityAlgorithm::Auto,
				1 if parameter == 0 => ReachabilityAlgorithm::TreeCover,
				2 => ReachabilityAlgorithm::Grail(narrow_u8()?),
				3 => ReachabilityAlgorithm::Ferrari(narrow_u8()?),
				4 => ReachabilityAlgorithm::Bfl(narrow_u16()?),
				5 if parameter == 0 => ReachabilityAlgorithm::ChainCover,
				6 if parameter == 0 => ReachabilityAlgorithm::OnDemand,
				_ => return Err(invalid("reachability algorithm is invalid")),
			},
		},
		1 => GraphIndexDefinition::Neighbourhood {
			radius,
			algorithm: match algorithm {
				0 if parameter == 0 => NeighbourhoodAlgorithm::Auto,
				1 if parameter == 0 => NeighbourhoodAlgorithm::Exact,
				2 => NeighbourhoodAlgorithm::Sketch(narrow_u8()?),
				3 => NeighbourhoodAlgorithm::Landmark(narrow_u16()?),
				4 if parameter == 0 => NeighbourhoodAlgorithm::OnDemand,
				_ => return Err(invalid("neighbourhood algorithm is invalid")),
			},
		},
		2 => GraphIndexDefinition::Path {
			cost,
			patterns,
			algorithm: match algorithm {
				0 => PathIndexAlgorithm::LandmarkSssp(narrow_u16()?),
				1 if parameter == 0 => PathIndexAlgorithm::PatternCache,
				2 if parameter == 0 => PathIndexAlgorithm::OnDemand,
				_ => return Err(invalid("path algorithm is invalid")),
			},
		},
		3 if radius == 0 && cost == GraphIndexCost::Hop && patterns.is_empty() => {
			GraphIndexDefinition::Tensor {
				scope: match parameter {
					0 => TensorScope::Local,
					1 => TensorScope::Rooted,
					_ => return Err(invalid("tensor scope is invalid")),
				},
				format: match algorithm {
					0 => TensorCacheFormat::Auto,
					1 => TensorCacheFormat::Csr,
					2 => TensorCacheFormat::Dense,
					_ => return Err(invalid("tensor format is invalid")),
				},
			}
		}
		_ => return Err(invalid("graph index kind is invalid")),
	};
	let index = GraphIndex {
		id: Some(id),
		version,
		graph: Some(graph),
		name,
		definition,
		mode,
		direction,
		edge_labels,
		max_bytes,
		state,
		maintenance,
		build_revision,
		dependencies,
		comment,
	};
	index
		.validate()
		.map_err(|cause| invalid("graph index definition is invalid").with_cause(cause))?;
	if index.descriptor()? != bytes {
		return Err(invalid("graph index descriptor is not canonical"));
	}
	Ok(index)
}

/// Checks a declared length before allocation or UTF-8 conversion.
fn bounded_count(decoder: &mut Decoder<'_>, limit: usize, field: &str) -> FerrosGrynnResult<usize> {
	let count = decoder.read_count(field)?;
	if count > limit {
		return Err(invalid(format!("{field} exceeds its bound")));
	}
	Ok(count)
}

/// Reads owned UTF-8 only after checking the byte count and input availability.
fn text(decoder: &mut Decoder<'_>, limit: usize) -> FerrosGrynnResult<Text> {
	let length = bounded_count(decoder, limit, "graph index text")?;
	let bytes = decoder.read_exact(length)?;
	std::str::from_utf8(bytes)
		.map(Into::into)
		.map_err(|_error| invalid("graph index text is not UTF-8"))
}

#[cfg(test)]
/// Regression tests for graph index behavior.
mod tests {
	use super::*;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies tensor definition round trip requires full projection and
	/// explicit budget.
	#[test]
	fn tensor_definition_round_trip_requires_full_projection_and_explicit_budget() {
		for scope in [TensorScope::Local, TensorScope::Rooted] {
			for format in
				[TensorCacheFormat::Auto, TensorCacheFormat::Csr, TensorCacheFormat::Dense]
			{
				let mut index =
					GraphIndex::new_tensor("tensor", scope, format, 1 << 20).assert_ok();
				index.id = IndexId::new(9);
				index.graph = GraphId::new(7);
				let mut bytes = Vec::new();
				encode(&mut bytes, &index).assert_ok();
				assert_eq!(entry(&bytes).assert_ok(), index);
				assert!(index.clone().with_mode(TraversalSemantics::Node).validate().is_err());
				assert!(
					index.clone().with_direction(TraversalDirection::Forward).validate().is_err()
				);
				assert!(index.with_edge_labels(["edge"]).assert_ok().validate().is_err());
			}
		}
		assert!(
			GraphIndex::new_tensor("tensor", TensorScope::Local, TensorCacheFormat::Auto, 0)
				.is_err()
		);
		assert!(
			GraphIndex::new(
				"tensor",
				GraphIndexDefinition::Tensor {
					scope: TensorScope::Local,
					format: TensorCacheFormat::Csr
				},
				None
			)
			.is_err()
		);
	}

	/// Decodes one complete catalog entry, including trailing-byte rejection.
	fn entry(bytes: &[u8]) -> FerrosGrynnResult<GraphIndex> {
		let mut reader = Decoder::new(bytes);
		let index = decode(&mut reader)?;
		reader.finish()?;
		Ok(index)
	}

	/// Verifies graph index descriptor round trips and checks canonical bytes.
	#[test]
	fn graph_index_descriptor_round_trips_and_checks_canonical_bytes() {
		let mut index = GraphIndex::new(
			"reach",
			GraphIndexDefinition::Reachability {
				algorithm: ReachabilityAlgorithm::Grail(2),
			},
			Some(1_000_000),
		)
		.assert_ok()
		.with_mode(TraversalSemantics::Atom)
		.with_direction(TraversalDirection::Both)
		.with_edge_labels(["b", "a"])
		.assert_ok();
		index.id = IndexId::new(9);
		index.graph = GraphId::new(7);
		index.build_revision = 44;
		index.comment = Some("snapshot".into());
		let mut bytes = Vec::new();
		encode(&mut bytes, &index).assert_ok();
		assert_eq!(entry(&bytes).assert_ok(), index);
		assert_eq!(&bytes[..8], &[1, 0, 0, 0, 0, 0, 0, 0]);
		assert_eq!(&bytes[20..44], &[
			9, 0, 0, 0, 1, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 2, 0, 0, 2, 0, 0, 0
		]);
		for length in 0..bytes.len() {
			assert!(entry(&bytes[..length]).is_err(), "prefix {length}");
		}
		let length = index.descriptor().assert_ok().len();
		for (offset, value) in [
			(0, 9),
			(1, 1),
			(16, 255),
			(20 + 16, 3),
			(20 + 21, 1),
			(20 + 24, 33),
			(20 + 28, 1),
			(20 + 52, 255),
		] {
			let mut malformed = bytes.clone();
			malformed[offset] = value;
			let fingerprint = rapidhash::v3::rapidhash_v3(&malformed[20..20 + length]);
			malformed[20 + length..28 + length].copy_from_slice(&fingerprint.to_le_bytes());
			assert!(entry(&malformed).is_err(), "offset {offset}");
		}
		let labels = 20 + 56 + index.name.len() + 4;
		let mut reordered = bytes.clone();
		reordered[labels + 4] = b'b';
		reordered[labels + 9] = b'a';
		let fingerprint = rapidhash::v3::rapidhash_v3(&reordered[20..20 + length]);
		reordered[20 + length..28 + length].copy_from_slice(&fingerprint.to_le_bytes());
		assert!(entry(&reordered).is_err());
		let mut wrong_hash = bytes;
		wrong_hash[20 + length] ^= 1;
		assert!(entry(&wrong_hash).is_err());
	}
}
