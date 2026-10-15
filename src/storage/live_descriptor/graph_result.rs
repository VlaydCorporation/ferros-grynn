//! Bounded graph terminal selectors in the logical descriptor format.

use super::{
	CodecBudget,
	Decoder,
	EncodeContext,
	FerrosGrynnResult,
	Work,
	append,
	decode_expr,
	encode_expr,
	invalid,
};
use crate::{
	graph::{GraphKind, GraphTopologyConstraints},
	live::LiveGraphResult,
};

/// Encodes the shared logical graph terminal without executable IR.
pub(super) fn encode(
	output: &mut Vec<u8>,
	graph: &LiveGraphResult,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<()> {
	work.take(depth, 1)?;
	if graph.root().is_some() && graph.family() != GraphKind::RootedTree {
		return Err(invalid("LIVE graph ROOT requires RootedTree"));
	}
	let family = match graph.family() {
		GraphKind::Graph => 0,
		GraphKind::Tree => 1,
		GraphKind::RootedTree => 2,
		GraphKind::Forest => 3,
		GraphKind::Dag => 4,
		GraphKind::Hypergraph => 5,
		GraphKind::Metagraph => 6,
		GraphKind::Subgraph => 7,
	};
	append(output, &[family], budget)?;
	append(output, &graph.validators().bits().to_le_bytes(), budget)?;
	append(output, &[u8::from(graph.root().is_some())], budget)?;
	encode_expr(output, graph.nodes(), context, budget, work, depth + 1)?;
	encode_expr(output, graph.edges(), context, budget, work, depth + 1)?;
	if let Some(root) = graph.root() {
		encode_expr(output, root, context, budget, work, depth + 1)?;
	}
	Ok(())
}

/// Restores the checked logical graph terminal through the common descriptor
/// budget.
pub(super) fn decode(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work<'_>,
	depth: usize,
) -> FerrosGrynnResult<LiveGraphResult> {
	work.take(depth, 1)?;
	budget.admit(std::mem::size_of::<LiveGraphResult>())?;
	let family = match decoder.u8()? {
		0 => GraphKind::Graph,
		1 => GraphKind::Tree,
		2 => GraphKind::RootedTree,
		3 => GraphKind::Forest,
		4 => GraphKind::Dag,
		5 => GraphKind::Hypergraph,
		6 => GraphKind::Metagraph,
		7 => GraphKind::Subgraph,
		_ => return Err(invalid("LIVE graph family tag is unknown")),
	};
	let validators =
		GraphTopologyConstraints::from_bits(u16::from_le_bytes([decoder.u8()?, decoder.u8()?]))
			.ok_or_else(|| invalid("LIVE graph validator bits are unknown"))?;
	let has_root = match decoder.u8()? {
		0 => false,
		1 => true,
		_ => return Err(invalid("LIVE graph ROOT flag is invalid")),
	};
	if has_root && family != GraphKind::RootedTree {
		return Err(invalid("LIVE graph ROOT requires RootedTree"));
	}
	let nodes = decode_expr(decoder, context, budget, work, depth + 1)?;
	let edges = decode_expr(decoder, context, budget, work, depth + 1)?;
	let root = if has_root {
		Some(decode_expr(decoder, context, budget, work, depth + 1)?)
	} else {
		None
	};
	Ok(LiveGraphResult {
		family,
		validators,
		root,
		nodes,
		edges,
	})
}

#[cfg(test)]
/// Regression tests for graph result behavior.
mod tests {
	use super::{super::*, *};
	use crate::{
		datastore::ClassId,
		graph::GraphTopologyConstraint,
		live::{LiveBindingId, LiveSource},
	};

	/// Verifies graph recipe roundtrips all families and bounded selectors.
	#[test]
	fn graph_recipe_roundtrips_all_families_and_bounded_selectors() {
		use crate::test_assertions::AssertResult as _;

		let class =
			ClassId::new(1).ok_or_else(|| invalid("fixture class identity is invalid")).assert_ok();
		for family in [
			GraphKind::Graph,
			GraphKind::Tree,
			GraphKind::RootedTree,
			GraphKind::Forest,
			GraphKind::Dag,
			GraphKind::Hypergraph,
			GraphKind::Metagraph,
			GraphKind::Subgraph,
		] {
			let mut graph = LiveGraphResult::new(
				LiveExpr::field("picked").assert_ok(),
				LiveExpr::Array(Vec::new()),
			)
			.with_family(family)
			.with_validators([GraphTopologyConstraint::Simple, GraphTopologyConstraint::Bipartite]);
			if family == GraphKind::RootedTree {
				graph = graph.with_root(LiveExpr::Parameter("root".into()));
			}
			let query = LiveQuery::new(LiveSource::Class(class)).with_graph_result(graph);
			let descriptor = LiveDescriptor::Query(query);
			let bytes = super::super::encode(&descriptor, 1 << 20).assert_ok();
			assert_eq!(&bytes[4..6], &[2, 0]);
			assert_eq!(super::super::decode(&bytes, 1 << 20).assert_ok(), descriptor);
			let mut old = bytes.clone();
			old[4] = 1;
			assert!(super::super::decode(&old, 1 << 20).is_err());
		}
		let binding = LiveBindingId::from_uuid(crate::values::runtime::Uuid::now_v7()).assert_ok();
		let descriptor =
			LiveDescriptor::Query(LiveQuery::new(LiveSource::Class(class)).with_graph_result(
				LiveGraphResult::new(LiveExpr::Binding(binding), LiveExpr::Array(Vec::new())),
			));
		let bytes = super::super::encode(&descriptor, 1 << 20).assert_ok();
		assert!(super::super::decode(&bytes, 128).is_err());
		for length in 0..bytes.len() {
			assert!(super::super::decode(&bytes[..length], 1 << 20).is_err());
		}
	}

	/// Verifies graph recipe rejects unknown family validators and root flags.
	#[test]
	fn graph_recipe_rejects_unknown_family_validators_and_root_flags() {
		use crate::test_assertions::AssertResult as _;

		let graph = LiveGraphResult::new(LiveExpr::Array(Vec::new()), LiveExpr::Array(Vec::new()));
		let mut budget = CodecBudget::new(1 << 20, 0).assert_ok();
		let mut context = EncodeContext::default();
		let mut work = Work {
			observe: None,
			remaining: MAX_NODES,
		};
		let mut body = Vec::new();
		super::encode(&mut body, &graph, &mut context, &mut budget, &mut work, 0).assert_ok();
		for (offset, byte) in [(0, 8), (1, 8), (2, 128), (3, 2), (3, 1)] {
			let mut bad = body.clone();
			bad[offset] = byte;
			let mut decoder = Decoder::new(&bad);
			let mut budget = CodecBudget::new(1 << 20, 0).assert_ok();
			let mut work = Work {
				observe: None,
				remaining: MAX_NODES,
			};
			assert!(super::decode(&mut decoder, &context, &mut budget, &mut work, 0).is_err());
		}
		assert!(
			super::super::encode(
				&LiveDescriptor::Query(
					LiveQuery::new(LiveSource::Class(
						ClassId::new(1)
							.ok_or_else(|| invalid("fixture class identity is invalid"))
							.assert_ok()
					))
					.with_graph_result(graph.with_root(LiveExpr::Literal(Value::None)))
				),
				1 << 20
			)
			.is_err()
		);
	}
}
