//! Bounded predicates for overlapping collections without quantizing their
//! coordinates.

use std::cmp::Ordering;

use geo::{
	Coord,
	CoordinatePosition,
	CoordsIter,
	Geometry,
	Line,
	LineString,
	Polygon,
	Relate,
	Winding,
	coordinate_position::CoordPos,
	dimensions::Dimensions,
	kernels::{Kernel, Orientation, RobustKernel},
	line_intersection::{LineIntersection, line_intersection},
	winding_order::WindingOrder,
};

use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Maximum primitive comparisons admitted before constructing an arrangement.
const MAX_WORK: usize = 16_000_000;

/// Borrowed primitive members and their original boundary segments.
#[derive(Default)]
struct Parts<'a> {
	/// Polygon members retained without an allocating union.
	polygons: Vec<&'a Polygon>,
	/// Original linear members, including their endpoint multiplicity.
	lines: Vec<&'a LineString>,
	/// Zero-dimensional members.
	points: Vec<Coord>,
	/// Nonzero segments used to split arrangement intervals.
	edges: Vec<Line>,
}

impl<'a> Parts<'a> {
	/// Flattens validated members without constructing a polygon union.
	fn add(&mut self, geometry: &'a Geometry) {
		match geometry {
			Geometry::Point(point) => self.points.push(point.0),
			Geometry::MultiPoint(points) => self.points.extend(points.iter().map(|point| point.0)),
			Geometry::LineString(line) => {
				self.edges.extend(line.lines().filter(|edge| edge.start != edge.end));
				self.lines.push(line);
			}
			Geometry::MultiLineString(lines) => {
				for line in lines {
					self.add_line(line);
				}
			}
			Geometry::Polygon(polygon) => self.add_polygon(polygon),
			Geometry::MultiPolygon(polygons) => {
				for polygon in polygons {
					self.add_polygon(polygon);
				}
			}
			Geometry::GeometryCollection(children) => {
				for child in children {
					self.add(child);
				}
			}
			_ => {}
		}
	}

	/// Records an existing line's nonzero segments.
	fn add_line(&mut self, line: &'a LineString) {
		self.edges.extend(line.lines().filter(|edge| edge.start != edge.end));
		self.lines.push(line);
	}

	/// Records an existing polygon's nonzero boundary segments.
	fn add_polygon(&mut self, polygon: &'a Polygon) {
		self.edges.extend(
			rings(polygon).flat_map(LineString::lines).filter(|edge| edge.start != edge.end),
		);
		self.polygons.push(polygon);
	}

	/// Counts the retained arrangement input and relation work.
	fn complexity(&self) -> usize {
		self.polygons
			.iter()
			.map(|polygon| polygon.coords_count())
			.sum::<usize>()
			.saturating_add(self.lines.iter().map(|line| line.coords_count()).sum::<usize>())
			.saturating_add(self.points.len())
			.saturating_add(self.polygons.len())
			.saturating_add(self.lines.len())
			.saturating_add(1)
	}

	/// Classifies a point in the set union, including shared polygon seams.
	fn position(&self, point: Coord) -> CoordPos {
		let mut boundary = Vec::new();
		for polygon in &self.polygons {
			match polygon.coordinate_position(&point) {
				CoordPos::Inside => return CoordPos::Inside,
				CoordPos::OnBoundary => boundary.push(*polygon),
				CoordPos::Outside => {}
			}
		}
		if !boundary.is_empty() {
			return if area_neighbourhood(point, &boundary) {
				CoordPos::Inside
			} else {
				CoordPos::OnBoundary
			};
		}
		let mut covered = false;
		let mut endpoints = 0_usize;
		for line in &self.lines {
			covered |= line.lines().any(|edge| contains(edge, point));
			endpoints += usize::from(line.0.first() == Some(&point));
			endpoints += usize::from(line.0.last() == Some(&point));
		}
		if covered {
			return if endpoints % 2 == 0 {
				CoordPos::Inside
			} else {
				CoordPos::OnBoundary
			};
		}
		if self.points.contains(&point) {
			CoordPos::Inside
		} else {
			CoordPos::Outside
		}
	}

	/// Classifies an open segment whose crossings have already been split.
	fn interval(&self, edge: Line) -> CoordPos {
		let line = LineString(vec![edge.start, edge.end]);
		let mut sides = 0_u8;
		for polygon in &self.polygons {
			let relation = line.relate(*polygon);
			if relation.get(CoordPos::Inside, CoordPos::Inside) == Dimensions::OneDimensional {
				return CoordPos::Inside;
			}
			if relation.get(CoordPos::Inside, CoordPos::OnBoundary) != Dimensions::OneDimensional {
				continue;
			}
			for (ring_id, ring) in rings(polygon).enumerate() {
				for boundary in ring.lines() {
					if let Some(LineIntersection::Collinear {
						intersection,
					}) = line_intersection(edge, boundary)
						&& intersection.start != intersection.end
					{
						let same_direction = if edge.start.x != edge.end.x {
							(edge.end.x > edge.start.x) == (boundary.end.x > boundary.start.x)
						} else {
							(edge.end.y > edge.start.y) == (boundary.end.y > boundary.start.y)
						};
						let left = (ring.winding_order() == Some(WindingOrder::CounterClockwise))
							^ (ring_id != 0)
							^ !same_direction;
						sides |= if left {
							1
						} else {
							2
						};
					}
				}
			}
		}
		if sides != 0 {
			return if sides == 3 {
				CoordPos::Inside
			} else {
				CoordPos::OnBoundary
			};
		}
		if self.lines.iter().any(|other| {
			line.relate(*other).get(CoordPos::Inside, CoordPos::Inside)
				== Dimensions::OneDimensional
		}) {
			CoordPos::Inside
		} else {
			CoordPos::Outside
		}
	}
}

/// Implements Within by coverage of the original arrangement and an interior
/// intersection.
pub(super) fn within(left: &Geometry, right: &Geometry) -> FerrosGrynnResult<bool> {
	let input = left.coords_count().checked_add(right.coords_count());
	if input.and_then(|count| count.checked_mul(128)).is_none_or(|bytes| bytes > 128 * 1024 * 1024)
	{
		return Err(FerrosGrynnError::control_limit_exceeded(
			"geometry collection memory",
			128 * 1024 * 1024,
		));
	}
	let mut a = Parts::default();
	a.add(left);
	let mut b = Parts::default();
	b.add(right);
	// ponytail: bounded pairwise arrangement; use a sweep if collection-heavy
	// workloads exceed this cap.
	let work = a
		.complexity()
		.checked_mul(b.complexity())
		.and_then(|work| work.checked_mul(b.complexity()))
		.and_then(|work| work.checked_mul(b.complexity()))
		.and_then(|work| {
			work.checked_add(if a.polygons.is_empty() {
				0
			} else {
				b.complexity()
					.saturating_mul(b.complexity())
					.saturating_mul(a.complexity().saturating_add(b.complexity()))
			})
		});
	if work.is_none_or(|work| work > MAX_WORK) {
		return Err(FerrosGrynnError::control_limit_exceeded(
			"geometry collection comparisons",
			MAX_WORK as u64,
		));
	}
	let mut interior = false;
	for point in &a.points {
		match b.position(*point) {
			CoordPos::Outside => return Ok(false),
			CoordPos::Inside => interior = true,
			CoordPos::OnBoundary => {}
		}
	}
	for line in &a.lines {
		for edge in line.lines() {
			if edge.start == edge.end {
				if b.position(edge.start) == CoordPos::Outside {
					return Ok(false);
				}
				continue;
			}
			let (covered, inside) = covered(edge, &b);
			if !covered {
				return Ok(false);
			}
			interior |= inside;
		}
	}
	for polygon in &a.polygons {
		if polygon.exterior().0.is_empty() {
			continue;
		}
		if !b.polygons.iter().any(|other| {
			polygon.relate(*other).get(CoordPos::Inside, CoordPos::Inside)
				== Dimensions::TwoDimensional
		}) {
			return Ok(false);
		}
		interior = true;
		for edge in rings(polygon).flat_map(LineString::lines).filter(|edge| edge.start != edge.end)
		{
			if !covered(edge, &b).0 {
				return Ok(false);
			}
		}
		// An uncovered area inside A has a boundary in B; internal union seams
		// have both sides covered.
		for edge in b
			.polygons
			.iter()
			.flat_map(|polygon| rings(polygon).flat_map(LineString::lines))
			.filter(|edge| edge.start != edge.end)
		{
			let cuts = split(edge, a.edges.iter().chain(&b.edges), &[]);
			for pair in cuts.windows(2) {
				let part = Line::new(pair[0], pair[1]);
				if LineString(vec![pair[0], pair[1]])
					.relate(*polygon)
					.get(CoordPos::Inside, CoordPos::Inside)
					== Dimensions::OneDimensional
					&& b.interval(part) != CoordPos::Inside
				{
					return Ok(false);
				}
			}
		}
	}
	Ok(interior)
}

/// Iterates exterior and hole rings in their original winding order.
fn rings(polygon: &Polygon) -> impl Iterator<Item = &LineString> {
	std::iter::once(polygon.exterior()).chain(polygon.interiors())
}

/// Tests segment closure with robust orientation and inclusive coordinate
/// bounds.
fn contains(edge: Line, point: Coord) -> bool {
	RobustKernel::orient2d(edge.start, edge.end, point) == Orientation::Collinear
		&& point.x >= edge.start.x.min(edge.end.x)
		&& point.x <= edge.start.x.max(edge.end.x)
		&& point.y >= edge.start.y.min(edge.end.y)
		&& point.y <= edge.start.y.max(edge.end.y)
}

/// Splits at original vertices and robust intersections, without sampling
/// midpoints.
fn split<'a>(
	edge: Line,
	boundaries: impl Iterator<Item = &'a Line>,
	points: &[Coord],
) -> Vec<Coord> {
	let mut cuts = vec![edge.start, edge.end];
	for boundary in boundaries {
		match line_intersection(edge, *boundary) {
			Some(LineIntersection::SinglePoint {
				intersection,
				..
			}) => cuts.push(intersection),
			Some(LineIntersection::Collinear {
				intersection,
			}) => cuts.extend([intersection.start, intersection.end]),
			None => {}
		}
	}
	cuts.extend(points.iter().copied().filter(|point| contains(edge, *point)));
	cuts.sort_unstable_by(|a, b| {
		if edge.start.x != edge.end.x {
			a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y))
		} else {
			a.y.total_cmp(&b.y)
		}
	});
	cuts.dedup();
	cuts
}

/// Checks closure coverage and whether an open part meets the right interior.
fn covered(edge: Line, right: &Parts<'_>) -> (bool, bool) {
	let cuts = split(edge, right.edges.iter(), &right.points);
	if cuts.iter().any(|point| right.position(*point) == CoordPos::Outside) {
		return (false, false);
	}
	let mut interior = false;
	for pair in cuts.windows(2) {
		match right.interval(Line::new(pair[0], pair[1])) {
			CoordPos::Outside => return (false, false),
			CoordPos::Inside => interior = true,
			CoordPos::OnBoundary => {}
		}
	}
	(true, interior)
}

/// Orders rays robustly around an original boundary point.
fn compare_rays(origin: Coord, a: Coord, b: Coord) -> Ordering {
	let half = |point: Coord| point.y > origin.y || (point.y == origin.y && point.x >= origin.x);
	match half(b).cmp(&half(a)) {
		Ordering::Equal => match RobustKernel::orient2d(origin, a, b) {
			Orientation::CounterClockwise => Ordering::Less,
			Orientation::Clockwise => Ordering::Greater,
			Orientation::Collinear => Ordering::Equal,
		},
		order => order,
	}
}

/// Removes repeated rays independently of segment length.
fn sort_rays(origin: Coord, rays: &mut Vec<Coord>) {
	rays.sort_unstable_by(|a, b| compare_rays(origin, *a, *b));
	rays.dedup_by(|a, b| compare_rays(origin, *a, *b) == Ordering::Equal);
}

/// Finds whether all open angular sectors around a shared polygon boundary are
/// covered.
fn area_neighbourhood(point: Coord, polygons: &[&Polygon]) -> bool {
	let mut constraints = Vec::<Vec<(Coord, Coord, bool)>>::new();
	let mut rays = Vec::new();
	for polygon in polygons {
		let mut polygon_constraints = Vec::new();
		for (ring_id, ring) in rings(polygon).enumerate() {
			let mut before = None;
			let mut after = None;
			for edge in ring.lines().filter(|edge| edge.start != edge.end) {
				if !contains(edge, point) {
					continue;
				}
				if edge.start != point {
					before = Some(edge.start);
				}
				if edge.end != point {
					after = Some(edge.end);
				}
			}
			if let (Some(before), Some(after)) = (before, after) {
				let (start, end) = if ring.winding_order() == Some(WindingOrder::CounterClockwise) {
					(after, before)
				} else {
					(before, after)
				};
				rays.extend([start, end]);
				polygon_constraints.push((start, end, ring_id == 0));
			}
		}
		constraints.push(polygon_constraints);
	}
	sort_rays(point, &mut rays);
	!rays.is_empty()
		&& (0..rays.len()).all(|sector| {
			constraints.iter().any(|polygon| {
				polygon.iter().all(|(start, end, exterior)| {
					let start = rays
						.iter()
						.position(|ray| compare_rays(point, *ray, *start) == Ordering::Equal);
					let end = rays
						.iter()
						.position(|ray| compare_rays(point, *ray, *end) == Ordering::Equal);
					match (start, end) {
						(Some(start), Some(end)) => {
							(if start < end {
								sector >= start && sector < end
							} else {
								sector >= start || sector < end
							}) == *exterior
						}
						_ => false,
					}
				})
			})
		})
}
