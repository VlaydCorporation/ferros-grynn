//! OGC ring checks omitted by geo's current Validation implementation.

use std::collections::{BTreeMap, BTreeSet};

use geo::{
	kernels::{Kernel, Orientation, RobustKernel},
	line_intersection::{LineIntersection, line_intersection},
};

use super::{
	Geometry,
	Position,
	kernels::{charge, coordinate},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Rejects zero-dimensional rings and overlapping adjacent segments robustly.
pub(super) fn check_ring(ring: &[Position]) -> FerrosGrynnResult<()> {
	let Some(first) = ring.first() else {
		return Ok(());
	};
	let Some(second) = ring.iter().find(|position| *position != first) else {
		return Err(invalid());
	};
	if ring.iter().all(|third| {
		RobustKernel::orient2d(coordinate(first), coordinate(second), coordinate(third))
			== Orientation::Collinear
	}) {
		return Err(invalid());
	}
	let mut previous = None;
	let mut initial = None;
	for positions in ring.windows(2) {
		if positions[0] == positions[1] {
			continue;
		}
		let segment = geo::Line::new(coordinate(&positions[0]), coordinate(&positions[1]));
		if previous.is_some_and(|previous| {
			matches!(line_intersection(previous, segment), Some(LineIntersection::Collinear { .. }))
		}) {
			return Err(invalid());
		}
		initial.get_or_insert(segment);
		previous = Some(segment);
	}
	if let (Some(last), Some(first)) = (previous, initial)
		&& matches!(line_intersection(last, first), Some(LineIntersection::Collinear { .. }))
	{
		return Err(invalid());
	}
	Ok(())
}

/// Applies the ring-touch forest criterion after ordinary topology validation.
pub(super) fn check_connected_interiors(geometry: &Geometry) -> FerrosGrynnResult<()> {
	match geometry {
		Geometry::Polygon(rings) => connected(rings),
		Geometry::MultiPolygon(polygons) => {
			for rings in polygons {
				connected(rings)?;
			}
			Ok(())
		}
		Geometry::Collection(children) => {
			for child in children {
				check_connected_interiors(child)?;
			}
			Ok(())
		}
		_ => Ok(()),
	}
}

/// Multiple independent contacts between rings, or contact cycles, split the
/// interior.
fn connected(rings: &[Vec<Position>]) -> FerrosGrynnResult<()> {
	if rings.len() < 2 {
		return Ok(());
	}
	let mut preceding = 0_usize;
	let mut comparisons = 0_usize;
	for ring in rings {
		let segments = ring.len().saturating_sub(1);
		comparisons = comparisons
			.checked_add(preceding.checked_mul(segments).ok_or_else(|| {
				FerrosGrynnError::control_limit_exceeded("geometry ring comparisons", 16_000_000)
			})?)
			.filter(|count| *count <= 16_000_000)
			.ok_or_else(|| {
				FerrosGrynnError::control_limit_exceeded("geometry ring comparisons", 16_000_000)
			})?;
		preceding = preceding.checked_add(segments).ok_or_else(invalid)?;
	}
	let mut contacts = BTreeMap::<[u64; 2], BTreeSet<usize>>::new();
	let mut elements = 0;
	// shortcut: pairwise ring segments; replace with a bounded sweep if
	// hole-heavy workloads dominate.
	for (left_id, left) in rings.iter().enumerate() {
		for (right_id, right) in rings.iter().enumerate().skip(left_id + 1) {
			for a in left.windows(2) {
				for b in right.windows(2) {
					if let Some(LineIntersection::SinglePoint {
						intersection,
						..
					}) = line_intersection(
						geo::Line::new(coordinate(&a[0]), coordinate(&a[1])),
						geo::Line::new(coordinate(&b[0]), coordinate(&b[1])),
					) {
						let bits = |value: f64| {
							if value == 0.0 {
								0
							} else {
								value.to_bits()
							}
						};
						let key = [bits(intersection.x), bits(intersection.y)];
						if !contacts.contains_key(&key) {
							charge(&mut elements, 1)?;
						}
						let rings = contacts.entry(key).or_default();
						for ring in [left_id, right_id] {
							if !rings.contains(&ring) {
								charge(&mut elements, 1)?;
								rings.insert(ring);
							}
						}
					}
				}
			}
		}
	}
	let mut parents = (0..rings.len()).collect::<Vec<_>>();
	for contact in contacts.values() {
		let mut members = contact.iter().copied();
		let Some(first) = members.next() else {
			continue;
		};
		for member in members {
			let left = root(&mut parents, first);
			let right = root(&mut parents, member);
			if left == right {
				return Err(invalid());
			}
			parents[right] = left;
		}
	}
	Ok(())
}

/// Resolves a ring component using iterative path halving.
fn root(parents: &mut [usize], mut ring: usize) -> usize {
	while parents[ring] != ring {
		parents[ring] = parents[parents[ring]];
		ring = parents[ring];
	}
	ring
}

/// Keeps topology diagnostics independent of coordinates and foreign
/// implementations.
fn invalid() -> FerrosGrynnError {
	FerrosGrynnError::invalid_geometry_structure(
		"polygon rings must bound a connected two-dimensional interior",
	)
}
