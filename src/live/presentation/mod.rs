//! Bounded native presentation over exact captured result images.

use std::{mem::size_of, ops::Bound};

use super::{LiveBindingOptions, LiveEvent, LiveOutputMode};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Geometry, Position, RecordId, RecordIdPart, Text, Value},
};

/// Explicit bounded JSON export; native values are never silently coerced.
mod json;

/// An owned changed field; an empty path denotes the whole result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveFieldChange {
	/// Literal path segments, before JSON Pointer escaping.
	path: Vec<Text>,
	/// Previous value; absence differs from Value::None.
	before: Option<Value>,
	/// New value; absence denotes removal.
	after: Option<Value>,
}

impl LiveFieldChange {
	/// Returns literal changed path segments.
	pub fn path(&self) -> &[Text] {
		&self.path
	}

	/// Returns the exact optional previous value.
	pub const fn before(&self) -> Option<&Value> {
		self.before.as_ref()
	}

	/// Returns the exact optional new value.
	pub const fn after(&self) -> Option<&Value> {
		self.after.as_ref()
	}
}

/// Native add/remove/replace semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LivePatchKind {
	/// Creates an absent result or property.
	Add,
	/// Removes an existing result or property.
	Remove,
	/// Replaces an existing value atomically.
	Replace,
}

/// An owned operation with JSON Pointer path and exact native value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePatchOperation {
	/// Checked operation shape.
	kind: LivePatchKind,
	/// Empty root pointer or escaped property segments.
	path: Text,
	/// Add/replace payload; remove has none.
	value: Option<Value>,
}

impl LivePatchOperation {
	/// Returns the operation kind.
	pub const fn kind(&self) -> LivePatchKind {
		self.kind
	}

	/// Returns the JSON Pointer, including the empty root pointer.
	pub fn path(&self) -> &str {
		self.path.as_str()
	}

	/// Returns the exact add/replace payload.
	pub const fn value(&self) -> Option<&Value> {
		self.value.as_ref()
	}
}

/// Owned presentation, independent of authoritative raw before/after images.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveEventOutput {
	/// Complete optional native images.
	Full {
		/// Previous result; absence differs from an explicit native None.
		before: Option<Value>,
		/// New result, absent on exit.
		after: Option<Value>,
	},
	/// Deterministic object leaf changes; arrays and other values are atomic.
	Diff(
		/// Deterministic object leaf changes; arrays and other values are
		/// atomic.
		Vec<LiveFieldChange>,
	),
	/// Sequential native operations; JSON export is separately checked.
	Patch(
		/// Sequential native operations; JSON export is separately checked.
		Vec<LivePatchOperation>,
	),
}

impl LiveEvent {
	/// Formats captured images without primary reads or ACK changes.
	///
	/// # Errors
	///
	/// Returns checked scratch/work/depth or allocation errors; never
	/// truncates.
	pub fn presented(
		&self,
		mode: LiveOutputMode,
		options: &LiveBindingOptions,
	) -> FerrosGrynnResult<LiveEventOutput> {
		let mut budget = Budget::new(options);
		render_event(self, mode, &mut budget)
	}
}

/// Prepared immutable presentation; its reserve is accounting metadata only.
#[derive(Debug, Clone)]
pub struct PreparedOutput {
	/// Exact native presentation of the captured images.
	pub output: LiveEventOutput,
	/// Conservative retained reserve admitted before preparing the delivery.
	pub reserved_bytes: usize,
}

impl PartialEq for PreparedOutput {
	/// Queue accounting does not change the meaning of a result image.
	fn eq(&self, other: &Self) -> bool {
		self.output == other.output
	}
}

impl Eq for PreparedOutput {}

/// Prepares one complete commit with shared scratch/work limits before issue.
///
/// # Errors
///
/// Returns complete-batch resource or value errors; callers discard private
/// partial preparation and never publish a prefix.
pub fn prepare_batch_presentation(
	batch: &mut super::LiveBatch,
	mode: LiveOutputMode,
	options: &LiveBindingOptions,
) -> FerrosGrynnResult<(usize, u64)> {
	if mode == LiveOutputMode::Full || batch.events.is_empty() {
		return Ok((0, 0));
	}
	let mut budget = Budget::new(options);
	for event in &mut batch.events {
		budget.take(size_of::<PreparedOutput>(), 1)?;
		let output = render_event(event, mode, &mut budget)?;
		let reserved_bytes = retained_bytes(&output, &mut budget)?;
		event.output = Some(Box::new(PreparedOutput {
			output,
			reserved_bytes,
		}));
	}
	Ok((options.scratch_bytes() - budget.bytes, options.work() - budget.work))
}

/// Counts only the owned presentation; input inspection and temporary buffers
/// remain scratch charges. The existing native-value preflight also bounds
/// this allocation-free inspection of retained copies.
fn retained_bytes(output: &LiveEventOutput, scratch: &mut Budget) -> FerrosGrynnResult<usize> {
	let mut retained = Budget {
		bytes: usize::MAX,
		work: scratch.work,
		maximum_bytes: usize::MAX,
		maximum_work: scratch.maximum_work,
	};
	retained.take(size_of::<PreparedOutput>(), 1)?;
	match output {
		LiveEventOutput::Diff(changes) => {
			retained.take(
				changes
					.capacity()
					.checked_mul(size_of::<LiveFieldChange>())
					.ok_or_else(overflow)?,
				changes.len() as u64,
			)?;
			for change in changes {
				retained.take(
					change.path.capacity().checked_mul(size_of::<Text>()).ok_or_else(overflow)?,
					change.path.len() as u64,
				)?;
				for part in &change.path {
					retained.take(part.len(), part.len() as u64)?;
				}
				for value in change.before.iter().chain(&change.after) {
					retained.value(value, 0)?;
				}
			}
		}
		LiveEventOutput::Patch(operations) => {
			retained.take(
				operations
					.capacity()
					.checked_mul(size_of::<LivePatchOperation>())
					.ok_or_else(overflow)?,
				operations.len() as u64,
			)?;
			for operation in operations {
				retained.take(operation.path.len(), operation.path.len() as u64)?;
				if let Some(value) = &operation.value {
					retained.value(value, 0)?;
				}
			}
		}
		LiveEventOutput::Full {
			before,
			after,
		} => {
			for value in before.iter().chain(after) {
				retained.value(value, 0)?;
			}
		}
	}
	scratch.work = retained.work;
	usize::MAX.checked_sub(retained.bytes).ok_or_else(overflow)
}

/// Shared renderer keeps whole-batch admission cumulative.
fn render_event(
	event: &LiveEvent,
	mode: LiveOutputMode,
	budget: &mut Budget,
) -> FerrosGrynnResult<LiveEventOutput> {
	for value in event.before().into_iter().chain(event.after()) {
		budget.value(value, 0)?;
	}
	if mode == LiveOutputMode::Full {
		return Ok(LiveEventOutput::Full {
			before: event.before.clone(),
			after: event.after.clone(),
		});
	}
	budget.take(64 * size_of::<&Text>(), 1)?;
	let mut path = Vec::with_capacity(64);
	let mut changes = Vec::new();
	diff(event.before(), event.after(), &mut path, &mut changes, budget)?;
	if mode == LiveOutputMode::Diff {
		return Ok(LiveEventOutput::Diff(changes));
	}
	budget.take(
		changes.len().checked_mul(size_of::<LivePatchOperation>()).ok_or_else(overflow)?,
		changes.len() as u64,
	)?;
	let mut operations = Vec::with_capacity(changes.len());
	for change in changes {
		let length = change.path.iter().try_fold(0_usize, |sum, part| {
			sum.checked_add(
				part.len()
					.checked_mul(2)
					.and_then(|length| length.checked_add(1))
					.ok_or_else(overflow)?,
			)
			.ok_or_else(overflow)
		})?;
		budget.take(length, u64::try_from(length).map_err(|_error| overflow())?)?;
		let mut pointer = String::with_capacity(length);
		for part in &change.path {
			pointer.push('/');
			for character in part.as_str().chars() {
				match character {
					'~' => pointer.push_str("~0"),
					'/' => pointer.push_str("~1"),
					other => pointer.push(other),
				}
			}
		}
		let kind = match (&change.before, &change.after) {
			(None, Some(_)) => LivePatchKind::Add,
			(Some(_), None) => LivePatchKind::Remove,
			(Some(_), Some(_)) => LivePatchKind::Replace,
			(None, None) => {
				return Err(FerrosGrynnError::invalid_storage_file("LIVE change has no images"));
			}
		};
		operations.push(LivePatchOperation {
			kind,
			path: pointer.into(),
			value: change.after,
		});
	}
	Ok(LiveEventOutput::Patch(operations))
}

/// Walks borrowed object members before copying changed native fields.
fn diff<'image>(
	before: Option<&'image Value>,
	after: Option<&'image Value>,
	path: &mut Vec<&'image Text>,
	changes: &mut Vec<LiveFieldChange>,
	budget: &mut Budget,
) -> FerrosGrynnResult<()> {
	budget.take(0, 1)?;
	if let (Some(Value::Object(before)), Some(Value::Object(after))) = (before, after) {
		for (key, value) in before {
			path.push(key);
			diff(Some(value), after.get(key.as_str()), path, changes, budget)?;
			path.pop();
		}
		for (key, value) in after {
			budget.take(0, 1)?;
			if !before.contains_key(key.as_str()) {
				path.push(key);
				diff(None, Some(value), path, changes, budget)?;
				path.pop();
			}
		}
		return Ok(());
	}
	if matches!((before, after), (None, None))
		|| matches!((before, after), (Some(before), Some(after)) if crate::values::runtime::same_value(before, after))
	{
		return Ok(());
	}
	let bytes = path.iter().try_fold(4 * size_of::<LiveFieldChange>(), |sum, part| {
		sum.checked_add(size_of::<Text>())
			.and_then(|sum| sum.checked_add(part.len()))
			.ok_or_else(overflow)
	})?;
	budget.take(bytes, 1)?;
	changes.try_reserve_exact(1).map_err(|error| {
		resource("LIVE presentation entries", changes.len() as u64).with_technical_source(error)
	})?;
	changes.push(LiveFieldChange {
		path: path.iter().map(|part| (*part).clone()).collect(),
		before: before.cloned(),
		after: after.cloned(),
	});
	Ok(())
}

/// One invocation admits inspection and owned copies cumulatively.
struct Budget {
	/// Remaining owned scratch allowance.
	bytes: usize,
	/// Remaining inspection/transformation operations.
	work: u64,
	/// Original scratch cap used in diagnostics.
	maximum_bytes: usize,
	/// Original work cap used in diagnostics.
	maximum_work: u64,
}

impl Budget {
	/// Copies validated SDK limits.
	const fn new(options: &LiveBindingOptions) -> Self {
		Self {
			bytes: options.scratch_bytes(),
			work: options.work(),
			maximum_bytes: options.scratch_bytes(),
			maximum_work: options.work(),
		}
	}

	/// Rejects growth before allocation and work before inspection.
	fn take(&mut self, bytes: usize, work: u64) -> FerrosGrynnResult<()> {
		self.bytes = self
			.bytes
			.checked_sub(bytes)
			.ok_or_else(|| resource("LIVE presentation scratch", self.maximum_bytes as u64))?;
		self.work = self
			.work
			.checked_sub(work)
			.ok_or_else(|| resource("LIVE presentation work", self.maximum_work))?;
		Ok(())
	}

	/// Admits complete immutable images before equality or cloning.
	fn value(&mut self, value: &Value, depth: usize) -> FerrosGrynnResult<()> {
		if depth > 64 {
			return Err(resource("LIVE presentation depth", 64));
		}
		self.take(size_of::<Value>(), 4)?;
		match value {
			Value::Array(values) => self.array(values.iter(), depth)?,
			Value::Set(values) => {
				self.take(
					crate::memory::tree_allocation_bytes(values.len(), size_of::<Value>())?,
					values.len() as u64,
				)?;
				for value in values.iter() {
					self.value(value, depth + 1)?;
				}
			}
			Value::Object(values) => self.object(values, depth)?,
			Value::Range(range) => {
				for bound in [range.start(), range.end()] {
					if let Bound::Included(value) | Bound::Excluded(value) = bound {
						self.value(value, depth + 1)?;
					}
				}
			}
			Value::Record(record) => self.record(record, depth)?,
			Value::Atom(atom) => {
				self.take(
					size_of::<crate::graph::AtomRef>()
						.checked_sub(size_of::<RecordId>())
						.ok_or_else(overflow)?,
					1,
				)?;
				self.record(atom.id(), depth)?;
			}
			Value::AtomOccurrence(atom) => {
				self.take(
					size_of::<crate::graph::AtomOccurrenceRef>()
						.checked_sub(size_of::<RecordId>())
						.ok_or_else(overflow)?,
					1,
				)?;
				self.occurrence(atom, depth)?;
			}
			Value::String(text) => self.take(text.len(), text.len() as u64)?,
			Value::Bytes(bytes) => self.take(bytes.len(), bytes.len() as u64)?,
			Value::Vector(vector) => {
				self.take(0, vector.len() as u64)?;
				self.take(value.estimated_heap_bytes(), 0)?;
			}
			Value::Geometry(geometry) => self.geometry(geometry, depth)?,
			Value::Path(path) => {
				self.take(size_of::<crate::graph::Path>(), 1)?;
				self.take(
					path.steps()
						.len()
						.checked_mul(size_of::<crate::graph::PathStep>())
						.ok_or_else(overflow)?,
					path.steps().len() as u64,
				)?;
				for step in path.steps() {
					// Step layouts include inline references; record admission
					// is conservatively shared with boxed
					// reference values.
					self.occurrence(step.atom(), depth)?;
					if let Some(edge) = step.via_edge() {
						self.occurrence(edge, depth)?;
					}
				}
			}
			Value::Graph(graph) => {
				self.graph(graph.as_ref(), depth)?;
				self.take(value.estimated_heap_bytes(), 0)?;
			}
			_ => {
				let bytes = value.estimated_heap_bytes();
				self.take(bytes, u64::try_from(bytes).map_err(|_error| overflow())?)?;
			}
		}
		Ok(())
	}

	/// Shares array-member accounting with validated composite record keys.
	fn array<'value>(
		&mut self,
		values: impl Iterator<Item = &'value Value>,
		depth: usize,
	) -> FerrosGrynnResult<()> {
		for value in values {
			self.value(value, depth + 1)?;
		}
		Ok(())
	}

	/// Covers partially filled B-tree leaves before cloning any member.
	fn object(&mut self, values: &crate::values::Object, depth: usize) -> FerrosGrynnResult<()> {
		self.take(
			crate::memory::tree_allocation_bytes(values.len(), size_of::<(Text, Value)>())?,
			values.len() as u64,
		)?;
		for (name, value) in values {
			self.take(name.len(), name.len() as u64 + 1)?;
			self.value(value, depth + 1)?;
		}
		Ok(())
	}

	/// Qualified references may own arbitrarily wide checked array/object keys.
	fn record(&mut self, record: &RecordId, depth: usize) -> FerrosGrynnResult<()> {
		self.take(
			size_of::<RecordId>().checked_add(record.class().len()).ok_or_else(overflow)?,
			record.class().len() as u64 + 1,
		)?;
		match record.id() {
			RecordIdPart::String(text) => {
				self.take(text.len(), text.len() as u64)?;
			}
			RecordIdPart::Array(values) => self.array(values.iter(), depth)?,
			RecordIdPart::Object(values) => self.object(values, depth)?,
			RecordIdPart::Int(_) | RecordIdPart::Uuid(_) | RecordIdPart::Ulid(_) => {}
		}
		Ok(())
	}

	/// Shared graph descriptors are inspected without pretending to clone Arc
	/// data.
	fn occurrence(
		&mut self,
		occurrence: &crate::graph::AtomOccurrenceRef,
		depth: usize,
	) -> FerrosGrynnResult<()> {
		self.take(0, occurrence.graph().name().len() as u64 + 1)?;
		self.record(occurrence.atom().id(), depth)
	}

	/// Geometry shape work precedes every recursive comparison or copy.
	fn geometry(&mut self, geometry: &Geometry, depth: usize) -> FerrosGrynnResult<()> {
		if depth > 64 {
			return Err(resource("LIVE presentation depth", 64));
		}
		self.take(size_of::<Geometry>(), 1)?;
		match geometry {
			Geometry::Point(position) => self.positions(std::slice::from_ref(position))?,
			Geometry::MultiPoint(positions) | Geometry::LineString(positions) => {
				self.positions(positions)?;
			}
			Geometry::MultiLineString(lines) | Geometry::Polygon(lines) => {
				self.take(
					lines.len().checked_mul(size_of::<Vec<Position>>()).ok_or_else(overflow)?,
					lines.len() as u64,
				)?;
				for line in lines {
					self.positions(line)?;
				}
			}
			Geometry::MultiPolygon(polygons) => {
				self.take(
					polygons
						.len()
						.checked_mul(size_of::<Vec<Vec<Position>>>())
						.ok_or_else(overflow)?,
					polygons.len() as u64,
				)?;
				for polygon in polygons {
					self.take(
						polygon
							.len()
							.checked_mul(size_of::<Vec<Position>>())
							.ok_or_else(overflow)?,
						polygon.len() as u64,
					)?;
					for ring in polygon {
						self.positions(ring)?;
					}
				}
			}
			Geometry::Collection(geometries) => {
				for geometry in geometries {
					self.geometry(geometry, depth + 1)?;
				}
			}
		}
		Ok(())
	}

	/// Admits owned coordinate vectors using their checked lengths.
	fn positions(&mut self, positions: &[Position]) -> FerrosGrynnResult<()> {
		self.take(
			positions.len().checked_mul(size_of::<Position>()).ok_or_else(overflow)?,
			positions.len() as u64,
		)?;
		for position in positions {
			self.take(
				position.as_slice().len().checked_mul(size_of::<f64>()).ok_or_else(overflow)?,
				position.as_slice().len() as u64,
			)?;
		}
		Ok(())
	}

	/// Counts owned runtime graph contents before their recursive estimator.
	fn graph<Graph: crate::graph::GraphRead>(
		&mut self,
		graph: &Graph,
		depth: usize,
	) -> FerrosGrynnResult<()> {
		use crate::graph::{EdgeRead, NodeRead};
		if depth > 64 {
			return Err(resource("LIVE presentation graph depth", 64));
		}
		// Materialized graph readers box their borrowed ID iterators.
		self.take(256, 1)?;
		self.properties(graph.properties()?.as_ref(), depth)?;
		for id in graph.node_ids()? {
			self.take(0, 1)?;
			let node = graph.node(id?)?.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("LIVE graph node missing during preflight")
			})?;
			self.record(node.record_id()?.as_ref(), depth)?;
			if let Some(label) = node.label_text()? {
				self.take(label.len(), label.len() as u64)?;
			}
			self.properties(node.properties()?.as_ref(), depth)?;
			if let Some(child) = node.inner_graph()? {
				self.graph(child.as_ref(), depth + 1)?;
			}
		}
		for id in graph.edge_ids()? {
			self.take(0, 1)?;
			let edge = graph.edge(id?)?.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("LIVE graph edge missing during preflight")
			})?;
			self.record(edge.record_id()?.as_ref(), depth)?;
			if let Some(label) = edge.label_text()? {
				self.take(label.len(), label.len() as u64)?;
			}
			self.properties(edge.properties()?.as_ref(), depth)?;
			let topology = edge.topology()?;
			self.take(0, topology.endpoint_count() as u64)?;
			for endpoint in topology.endpoints() {
				if let crate::graph::EdgeEndpoint::Nested(qualified) = endpoint {
					self.take(0, qualified.scope().len() as u64)?;
				}
			}
			if let Some(child) = edge.transition_graph()? {
				self.graph(child.as_ref(), depth + 1)?;
			}
		}
		Ok(())
	}

	/// Reuses ordinary native-value admission for graph metadata/property
	/// cells.
	fn properties(
		&mut self,
		properties: &crate::values::PropertyMap,
		depth: usize,
	) -> FerrosGrynnResult<()> {
		for (_, value) in properties {
			self.value(value, depth + 1)?;
		}
		Ok(())
	}
}

/// Shared typed status for a rejected complete presentation.
fn resource(name: &str, limit: u64) -> FerrosGrynnError {
	FerrosGrynnError::traversal_resource_limit_exceeded(name, limit)
}

/// Checked count/length overflow, never partial output.
fn overflow() -> FerrosGrynnError {
	FerrosGrynnError::operation_overflow("LIVE presentation")
}

#[cfg(test)]
/// Regression tests for presentation behavior.
mod tests {
	use super::*;
	use crate::{
		live::LiveIdentity,
		values::{Number, Object},
	};

	/// One source identity independent of presentation or result type.
	fn event(before: Option<Value>, after: Option<Value>) -> FerrosGrynnResult<LiveEvent> {
		Ok(LiveEvent {
			output: None,
			ordinal: 0,
			identity: LiveIdentity::Record("item:a".parse()?),
			before,
			after,
		})
	}

	/// Pointer escaping, native types and result presence remain exact.
	#[test]
	fn live_presentation_native_paths_and_presence() {
		use crate::test_assertions::AssertResult as _;

		let before = Value::Object(Object::from_iter([
			("a/b", Value::Object(Object::from_iter([("~", Value::from(1)), ("", Value::Null)]))),
			("gone", Value::None),
		]));
		let after = Value::Object(Object::from_iter([
			(
				"a/b",
				Value::Object(Object::from_iter([("~", Value::from(1.0_f64)), ("", Value::Null)])),
			),
			("new", Value::from("owned")),
		]));
		let event = event(Some(before.clone()), Some(after.clone())).assert_ok();
		let options = LiveBindingOptions::default();
		let LiveEventOutput::Patch(operations) =
			event.presented(LiveOutputMode::Patch, &options).assert_ok()
		else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("PATCH test output"));
				test_result.assert_ok();
			};
		};
		assert_eq!(operations.iter().map(LivePatchOperation::path).collect::<Vec<_>>(), vec![
			"/a~1b/~0", "/gone", "/new"
		]);
		assert_eq!(operations.iter().map(LivePatchOperation::kind).collect::<Vec<_>>(), vec![
			LivePatchKind::Replace,
			LivePatchKind::Remove,
			LivePatchKind::Add
		]);
		assert!(
			matches!(operations[0].value(), Some(Value::Number(Number::Float(value))) if value.partial_cmp(&1.0) == Some(std::cmp::Ordering::Equal))
		);
		assert_eq!(event.before(), Some(&before));
		assert_eq!(event.after(), Some(&after));
		let LiveEventOutput::Diff(changes) =
			event.presented(LiveOutputMode::Diff, &options).assert_ok()
		else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("DIFF test output"));
				test_result.assert_ok();
			};
		};
		assert_eq!(changes[1].before(), Some(&Value::None));
		assert!(changes[1].after().is_none());
		for (before, after, kind) in [
			(None, Some(Value::None), LivePatchKind::Add),
			(Some(Value::Null), None, LivePatchKind::Remove),
		] {
			let LiveEventOutput::Patch(operations) = super::tests::event(before, after)
				.assert_ok()
				.presented(LiveOutputMode::Patch, &options)
				.assert_ok()
			else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument("root PATCH test output"));
					test_result.assert_ok();
				};
			};
			assert_eq!(operations.len(), 1);
			assert_eq!(operations[0].path(), "");
			assert_eq!(operations[0].kind(), kind);
		}
	}

	/// Retained output never counts unchanged input images or scratch buffers.
	#[test]
	fn live_presentation_retained_excludes_unchanged_images() {
		use crate::test_assertions::AssertResult as _;

		let image = |age| {
			Value::Object(Object::from_iter([
				("large", Value::from("x".repeat(16 * 1024))),
				("age", Value::from(age)),
			]))
		};
		let mut batch = crate::live::LiveBatch {
			id: crate::live::LiveId::from_uuid(uuid::Uuid::now_v7().into()).assert_ok(),
			version: crate::live::LiveVersion::default(),
			stamp: 1.into(),
			committed_at: crate::values::DateTime::from(
				jiff::Timestamp::UNIX_EPOCH.to_zoned(jiff::tz::TimeZone::UTC),
			),
			events: vec![event(Some(image(18)), Some(image(21))).assert_ok()],
			bindings: Vec::new(),
		};
		let (scratch, _) = prepare_batch_presentation(
			&mut batch,
			LiveOutputMode::Diff,
			&LiveBindingOptions::default(),
		)
		.assert_ok();
		let output = batch.events[0]
			.output
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("prepared DIFF missing"))
			.assert_ok();
		assert!(scratch > 32 * 1024);
		assert!(output.reserved_bytes < 4096, "unchanged images are not retained output");
	}

	/// A complete renderer rejects limits before returning any prefix.
	#[test]
	fn live_presentation_native_limits_and_array_replacement() {
		use crate::test_assertions::AssertResult as _;

		let before = Value::from(vec![Value::from(1), Value::from(2)]);
		let after = Value::from(vec![Value::from(2), Value::from(1)]);
		let event = event(Some(before), Some(after.clone())).assert_ok();
		let LiveEventOutput::Patch(operations) =
			event.presented(LiveOutputMode::Patch, &LiveBindingOptions::default()).assert_ok()
		else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("array PATCH test output"));
				test_result.assert_ok();
			};
		};
		assert_eq!(operations.len(), 1);
		assert_eq!(operations[0].kind(), LivePatchKind::Replace);
		assert_eq!(operations[0].value(), Some(&after));
		for options in [
			LiveBindingOptions::default().with_budgets(1, 1_000_000).assert_ok(),
			LiveBindingOptions::default().with_budgets(256 * 1024 * 1024, 1).assert_ok(),
		] {
			assert!(event.presented(LiveOutputMode::Patch, &options).is_err());
		}
	}

	/// Semantic equality cannot erase a captured temporal representation
	/// change.
	#[test]
	fn live_presentation_native_temporal_representation() {
		use crate::test_assertions::AssertResult as _;

		let parse_error = |error: jiff::Error| {
			FerrosGrynnError::invalid_argument("temporal presentation fixture")
				.with_technical_source(error)
		};
		for (before, after) in [
			(
				Value::DateTime(
					"2025-01-01T00:00:00+00:00[UTC]".parse().map_err(parse_error).assert_ok(),
				),
				Value::DateTime(
					"2024-12-31T19:00:00-05:00[America/New_York]"
						.parse()
						.map_err(parse_error)
						.assert_ok(),
				),
			),
			(
				Value::Duration("P1Y".parse().map_err(parse_error).assert_ok()),
				Value::Duration("P12M".parse().map_err(parse_error).assert_ok()),
			),
		] {
			assert_eq!(before, after);
			let expected = after.to_string();
			let event = event(Some(before), Some(after)).assert_ok();
			let LiveEventOutput::Patch(operations) =
				event.presented(LiveOutputMode::Patch, &LiveBindingOptions::default()).assert_ok()
			else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument("temporal PATCH output"));
					test_result.assert_ok();
				};
			};
			assert_eq!(operations.len(), 1);
			assert_eq!(operations[0].kind(), LivePatchKind::Replace);
			assert_eq!(operations[0].value().map(ToString::to_string), Some(expected));
		}
	}

	/// Composite references and native buffers cannot bypass inspection limits.
	#[test]
	fn live_presentation_native_reference_and_buffer_limits() {
		use crate::{
			test_assertions::AssertResult as _,
			values::{Set, Vector},
		};
		let key = RecordId::new(
			"item",
			RecordIdPart::Object(Object::from_iter([(
				"key",
				Value::from((0..100).map(Value::from).collect::<Vec<_>>()),
			)])),
		)
		.assert_ok();
		let line = Geometry::LineString(
			(0..100)
				.map(|index| Position::new(vec![f64::from(index % 10), 0.0]))
				.collect::<FerrosGrynnResult<Vec<_>>>()
				.assert_ok(),
		);
		let options = LiveBindingOptions::default().with_budgets(256 * 1024 * 1024, 16).assert_ok();
		for value in [
			Value::Record(Box::new(key)),
			Value::Vector(Vector::F64(vec![1.0; 100])),
			Value::from("x".repeat(100)),
			Value::Geometry(line),
		] {
			assert!(
				event(Some(value.clone()), Some(value))
					.assert_ok()
					.presented(LiveOutputMode::Full, &options)
					.is_err()
			);
		}
		let mut set = Set::new();
		set.insert(Value::from(1));
		let value = Value::Set(set);
		let options = LiveBindingOptions::default()
			.with_budgets(4 * size_of::<Value>(), 1_000_000)
			.assert_ok();
		assert!(
			event(Some(value.clone()), Some(value))
				.assert_ok()
				.presented(LiveOutputMode::Full, &options)
				.is_err()
		);
	}

	/// JSON numbers remain exact and unsupported native values fail explicitly.
	#[test]
	fn live_presentation_json_exact_numbers_and_compatibility() {
		use crate::test_assertions::AssertResult as _;

		let options = LiveBindingOptions::default();
		let output = event(Some(Value::from(1)), Some(Value::from(9_007_199_254_740_993_i64)))
			.assert_ok()
			.presented(LiveOutputMode::Patch, &options)
			.assert_ok();
		assert_eq!(
			output.to_json(&options).assert_ok(),
			"[{\"op\":\"replace\",\"path\":\"\",\"value\":9007199254740993}]"
		);
		for value in [
			Value::None,
			Value::Number(Number::NAN),
			Value::Record(Box::new("item:a".parse().assert_ok())),
		] {
			assert!(
				LiveEventOutput::Full {
					before: None,
					after: Some(value)
				}
				.to_json(&options)
				.is_err()
			);
		}
		assert!(output.to_json(&options.clone().with_budgets(1, 1_000_000).assert_ok()).is_err());
		assert!(output.to_json(&options.with_budgets(256 * 1024 * 1024, 1).assert_ok()).is_err());
	}
}
