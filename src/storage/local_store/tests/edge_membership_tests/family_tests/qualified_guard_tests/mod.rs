//! Qualified publication retains source bounds, old versions and atomic
//! recovery.

use super::*;
use crate::{
	datastore::OccurrenceState,
	graph::{EdgeEndpoint, QualifiedAtomId},
};

/// Two full paths to one deep child preserve NODE/EDGE terminal identities.
mod deep_tests;
/// Multi-page roles retain a real old physical owner through advancing
/// backfill.
mod pin_tests;
/// Admission and publication failures preserve the accepted source atomically.
mod recovery_tests;
/// Checksummed malformed source chains cannot silently request full fallback.
mod source_tests;

/// The accepted containment fixture is cold and uses the requested backend.
fn qualified_fixture(mode: IoMode) -> Fixture {
	family_fixture(mode, GraphKind::Metagraph).0
}

/// Resolves full SDK identities; physical assertions decode persisted bytes
/// separately.
fn nested_node(fixture: &Fixture) -> EdgeEndpoint {
	let root = fixture.store.inner.current_root();
	let child = root.schema.named_graph("child_node").assert_some().reference().id();
	let mut read = fixture.store.begin();
	let owner = read
		.membership(fixture.graph.id(), &"node:container".parse().assert_ok())
		.assert_ok()
		.assert_some();
	let terminal =
		read.membership(child, &"node:child_node".parse().assert_ok()).assert_ok().assert_some();
	EdgeEndpoint::Nested(QualifiedAtomId::new([owner], terminal).assert_ok())
}

/// Mixed scope order and multiplicity cross three physical qualified pages.
pub(super) fn wide_topology(fixture: &Fixture) -> EdgeTopology {
	let nested = nested_node(fixture);
	let endpoints = (0..2050)
		.map(|index| {
			if index % 2 == 0 {
				nested.clone()
			} else {
				EdgeEndpoint::Local(fixture.nodes[0])
			}
		})
		.collect::<Vec<_>>();
	EdgeTopology::directed(endpoints, [EdgeEndpoint::Local(fixture.nodes[1])])
}

/// The sparse inline control still carries a genuine nonzero qualified scope.
pub(super) fn short_topology(fixture: &Fixture) -> EdgeTopology {
	EdgeTopology::directed([nested_node(fixture)], [EdgeEndpoint::Local(fixture.nodes[1])])
}

/// Keeps the provisional local handle private until the commit is accepted.
fn attachment(fixture: &Fixture, topology: EdgeTopology) -> (Transaction, AtomId) {
	let mut write = fixture.store.begin();
	let edge = write
		.attach_edge(fixture.graph.id(), &fixture.records[2], None, topology, None, None)
		.assert_ok();
	(write, edge)
}

/// Replaces the accepted connector without changing its Base or local
/// generation.
fn rewrite(fixture: &Fixture, topology: EdgeTopology) -> Transaction {
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(
			fixture.graph.id(),
			&fixture.records[2],
			OccurrenceState::edge(topology, None, None),
		)
		.assert_ok();
	write
}
