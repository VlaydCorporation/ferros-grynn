//! Directed distance, interval enclosure, selection and admission oracles.

use super::*;
use crate::graph::{AtomId, AtomKind};

/// Builds checked CSR from row-major optional nonnegative integer costs.
fn relation(costs: &[Option<u64>], vertices: usize) -> (ProjectedRelation, Vec<Number>) {
	let mut offsets = vec![0];
	let mut targets = Vec::new();
	let mut weights = Vec::new();
	for row in costs.chunks(vertices) {
		for (target, weight) in row.iter().enumerate() {
			if let Some(weight) = weight {
				targets.push(target);
				weights.push(Number::Int(i64::try_from(*weight).unwrap()));
			}
		}
		offsets.push(targets.len());
	}
	let atoms = (0..vertices)
		.map(|ordinal| {
			AtomId::from_parts(
				if ordinal.is_multiple_of(2) {
					AtomKind::Node
				} else {
					AtomKind::Edge
				},
				ordinal as u64,
				0,
			)
			.unwrap()
		})
		.collect();
	let relation = ProjectedRelation::from_csr(
		atoms,
		RelationCsr {
			offsets,
			targets,
		},
		false,
		1 << 20,
		&mut || Ok(()),
	)
	.unwrap();
	(relation, weights)
}

/// Gives tiny generated graphs independent Floyd distances in u128.
fn oracle(costs: &[Option<u64>], vertices: usize, hop: bool) -> Vec<u128> {
	let mut result = costs
		.iter()
		.map(|weight| {
			weight.map_or(u128::MAX, |weight| {
				if hop {
					1
				} else {
					u128::from(weight)
				}
			})
		})
		.collect::<Vec<_>>();
	for vertex in 0..vertices {
		result[vertex * vertices + vertex] = 0;
	}
	for middle in 0..vertices {
		for source in 0..vertices {
			for target in 0..vertices {
				if let Some(candidate) = result[source * vertices + middle]
					.checked_add(result[middle * vertices + target])
				{
					result[source * vertices + target] =
						result[source * vertices + target].min(candidate);
				}
			}
		}
	}
	result
}

/// Admits only these small independent fixtures.
fn limits() -> LandmarkBuildLimits {
	LandmarkBuildLimits {
		result_bytes: 1 << 20,
		scratch_bytes: 1 << 20,
		live_scratch_bytes: 0,
	}
}

#[test]
fn directed_landmark_all_pairs_match_integer_oracle_and_alt_is_admissible() {
	let mut random = 0x1234_5678_u64;
	for sample in 0..24 {
		let vertices = 2 + sample % 11;
		let mut costs = Vec::new();
		for _ in 0..vertices * vertices {
			random ^= random << 13;
			random ^= random >> 7;
			random ^= random << 17;
			costs.push((random % 5 == 0).then_some(if sample % 3 == 0 {
				(1_u64 << 53) + random % 7
			} else {
				random % 8
			}));
		}
		let (relation, weights) = relation(&costs, vertices);
		for weighted in [false, true] {
			let oracle = oracle(&costs, vertices, !weighted);
			let built = DirectedLandmarks::build(
				&relation,
				weighted.then_some(weights.as_slice()),
				vertices as u16,
				limits(),
				&mut || Ok(()),
			)
			.unwrap();
			assert_eq!(built.landmarks.len(), vertices);
			for (row, landmark) in built.landmarks.iter().copied().enumerate() {
				for vertex in 0..vertices {
					for (rows, distance) in [
						(&built.from, oracle[landmark * vertices + vertex]),
						(&built.to, oracle[vertex * vertices + landmark]),
					] {
						match rows {
							LandmarkDistanceRows::Hops(rows) => assert_eq!(
								u128::from(rows[row * vertices + vertex]),
								if distance == u128::MAX {
									u128::from(u64::MAX)
								} else {
									distance
								}
							),
							LandmarkDistanceRows::Intervals(rows) => {
								let interval = rows[row * vertices + vertex];
								if distance == u128::MAX {
									assert_eq!(interval, DistanceInterval::INFINITY);
								} else {
									let exact = Number::Int(i64::try_from(distance).unwrap());
									assert!(
										Number::Float(interval.lower) <= exact,
										"{interval:?} {exact:?}"
									);
									assert!(Number::Float(interval.upper) >= exact);
								}
							}
						}
					}
				}
			}
			for source in 0..vertices {
				for target in 0..vertices {
					let distance = oracle[source * vertices + target];
					let lower = built.lower_bound(source, target).unwrap();
					assert!(lower >= Number::Int(0));
					if distance != u128::MAX {
						assert!(
							lower <= Number::Int(i64::try_from(distance).unwrap()),
							"ALT overestimates {source}->{target}: {lower:?}, distance {distance}"
						);
					}
				}
			}
		}
	}
}

#[test]
fn directed_landmark_selection_and_asymmetric_terms_are_deterministic() {
	let mut costs = vec![None; 16];
	costs[1] = Some(1);
	costs[6] = Some(1);
	let (relation, _) = relation(&costs, 4);
	let first = DirectedLandmarks::build(&relation, None, 256, limits(), &mut || Ok(())).unwrap();
	let second = DirectedLandmarks::build(&relation, None, 256, limits(), &mut || Ok(())).unwrap();
	assert_eq!(first.landmarks, [0, 3, 2, 1]);
	assert_eq!(first.landmarks, second.landmarks);
	assert_eq!(first.lower_bound(0, 2).unwrap(), Number::Int(2));
	assert_eq!(first.lower_bound(1, 2).unwrap(), Number::Int(1));
	assert_eq!(first.lower_bound(2, 0).unwrap(), Number::Int(0));
	assert!(first.lower_bound(4, 0).is_err());
}

#[test]
fn landmark_intervals_preserve_large_integer_decimal_float_and_zero_boundaries() {
	use rust_decimal::Decimal;
	for number in [
		Number::Int(0),
		Number::Int((1_i64 << 53) + 1),
		Number::Int(i64::MAX),
		Number::Decimal(Decimal::new(1, 28)),
		Number::Decimal(Decimal::new(1, 1)),
		Number::Float(f64::from_bits(1)),
		Number::Float(f64::MAX),
		Number::Float(-0.0),
	] {
		let interval = DistanceInterval::from_number(number).unwrap();
		assert!(Number::Float(interval.lower) <= number);
		assert!(Number::Float(interval.upper) >= number);
		assert_eq!(interval.add(DistanceInterval::ZERO), interval);
	}
	for number in [Number::Int(-1), Number::Float(f64::NAN), Number::Float(f64::INFINITY)] {
		assert!(DistanceInterval::from_number(number).is_err());
	}
	for (lower, upper) in [(1.0, 0.0), (-0.0, 1.0), (0.0, -0.0), (f64::NAN, 0.0), (0.0, f64::NAN)] {
		assert!(DistanceInterval::new(lower, upper).is_err());
	}
	let large = DistanceInterval::from_number(Number::Float(1.0e100)).unwrap();
	let tiny = DistanceInterval::from_number(Number::Float(f64::from_bits(1))).unwrap();
	assert!(large.add(tiny).lower >= large.lower);
	assert_eq!(DistanceInterval::INFINITY.lower_difference(large), 0.0);
	assert_eq!(large.lower_difference(DistanceInterval::INFINITY), 0.0);
	let overflow = DistanceInterval::from_number(Number::Float(f64::MAX))
		.unwrap()
		.add(DistanceInterval::from_number(Number::Float(f64::MAX)).unwrap());
	assert_eq!(overflow.lower, f64::MAX);
	assert_eq!(overflow.upper, f64::INFINITY);
	let (relation, mut weights) =
		relation(&[None, Some(1), None, None, None, Some(1), None, None, None], 3);
	weights[0] = Number::Float(0.1);
	weights[1] = Number::Float(0.2);
	let built =
		DirectedLandmarks::build(&relation, Some(&weights), 3, limits(), &mut || Ok(())).unwrap();
	let LandmarkDistanceRows::Intervals(rows) = &built.from else {
		panic!("weighted rows expected");
	};
	assert!(rows[2].lower <= 0.1 + 0.2 && rows[2].upper >= 0.1 + 0.2);
}

#[test]
fn directed_landmark_budgets_malformed_relations_and_cancellation_fail_closed() {
	let (mut relation, weights) = relation(&[None, Some(1), None, None], 2);
	for count in [0, 257] {
		assert!(
			DirectedLandmarks::build(&relation, None, count, limits(), &mut || Ok(())).is_err()
		);
	}
	assert!(DirectedLandmarks::build(&relation, Some(&[]), 2, limits(), &mut || Ok(())).is_err());
	for limit in [
		LandmarkBuildLimits {
			result_bytes: 1,
			..limits()
		},
		LandmarkBuildLimits {
			scratch_bytes: 1,
			..limits()
		},
		LandmarkBuildLimits {
			live_scratch_bytes: usize::MAX,
			..limits()
		},
	] {
		assert!(
			DirectedLandmarks::build(&relation, Some(&weights), 2, limit, &mut || Ok(())).is_err()
		);
	}
	assert!(
		DirectedLandmarks::build(&relation, None, 2, limits(), &mut || Err(
			FerrosGrynnError::query_cancelled()
		))
		.is_err()
	);
	relation.reverse.targets.clear();
	relation.reverse.offsets.fill(0);
	assert!(DirectedLandmarks::build(&relation, None, 2, limits(), &mut || Ok(())).is_err());
	let empty = ProjectedRelation::from_csr(
		Vec::new(),
		RelationCsr {
			offsets: vec![0],
			targets: Vec::new(),
		},
		false,
		1024,
		&mut || Ok(()),
	)
	.unwrap();
	assert!(
		DirectedLandmarks::build(&empty, None, 1, limits(), &mut || Ok(()))
			.unwrap()
			.landmarks
			.is_empty()
	);
	assert!(
		DirectedLandmarks::build(&empty, None, 1, limits(), &mut || Err(
			FerrosGrynnError::query_cancelled()
		))
		.is_err()
	);
}
