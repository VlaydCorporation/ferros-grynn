use std::{
	collections::BTreeMap,
	error::Error,
	fmt::{self, Display, Formatter},
	num::NonZeroU32,
	sync::Arc,
	time::SystemTime,
};

use serde::{Deserialize, Serialize};

use crate::gql_status::formatting::DiagnosticValue;

/// Notification or error severity, separate from the status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum Severity {
	/// An informational message that does not require an error to be corrected.
	Information,
	/// A warning about an undesirable but permitted condition.
	Warning,
	/// An error that prevents the operation from completing successfully.
	Error,
	/// A critical error requiring special attention from the maintainer or
	/// client.
	Critical,
}

/// The processing phase in which the diagnostic event occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionPhase {
	/// Lexical or syntactic analysis of the source text.
	Parsing,
	/// Validation of names, types, and semantic constraints.
	SemanticAnalysis,
	/// Construction of a logical or physical plan.
	Planning,
	/// Transformation and selection of an execution plan.
	Optimization,
	/// Execution of the operation itself.
	Execution,
	/// Validation and commit of transactional changes.
	Commit,
	/// Writing durable data.
	Persistence,
	/// State recovery after opening or failure.
	Recovery,
}

/// A source-text byte range with a starting offset and length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TextSpan {
	/// The starting byte offset in the UTF-8 source text.
	pub offset: u32,
	/// The range length in bytes of UTF-8 source text.
	pub len: u32,
}

impl TextSpan {
	/// Creates a value from the supplied inputs.
	pub const fn new(offset: u32, len: u32) -> Self {
		Self {
			offset,
			len,
		}
	}

	/// Returns the position immediately after the end of the range.
	pub const fn end(self) -> Option<u32> {
		self.offset.checked_add(self.len)
	}
}

/// A source-text position with one-based line and column numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LineColumn {
	/// The one-based starting line number.
	pub line: NonZeroU32,
	/// The one-based starting column number.
	pub column: NonZeroU32,
}

impl LineColumn {
	/// Constructs source coordinates from already validated nonzero components.
	pub const fn new(line: NonZeroU32, column: NonZeroU32) -> Self {
		Self {
			line,
			column,
		}
	}
}

/// A source-text range with a source name and computed boundary
/// positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
	/// The optional logical name of the source text.
	source_id: Option<Arc<str>>,
	/// The byte range associated with this diagnostic location.
	span: TextSpan,
	/// The line and column at the start of the range.
	start: LineColumn,
	/// The line and column immediately after the end of the range.
	end: LineColumn,
}

impl Location {
	/// Computes range positions from the source text without retaining its
	/// buffer.
	pub fn from_source(
		source_id: Option<Arc<str>>,
		source: &str,
		span: TextSpan,
	) -> Result<Self, LocationError> {
		let end = span.end().ok_or(LocationError::RangeOverflow)?;
		let start_offset =
			usize::try_from(span.offset).map_err(|_err| LocationError::OutOfBounds)?;
		let end_offset = usize::try_from(end).map_err(|_err| LocationError::OutOfBounds)?;

		if end_offset > source.len() {
			return Err(LocationError::OutOfBounds);
		}
		if !source.is_char_boundary(start_offset) || !source.is_char_boundary(end_offset) {
			return Err(LocationError::NotCharBoundary);
		}

		Ok(Self {
			source_id,
			span,
			start: line_column_at(source, start_offset),
			end: line_column_at(source, end_offset),
		})
	}

	/// Constructs a location from an existing range and line and column
	/// numbers.
	pub const fn from_parts(
		source_id: Option<Arc<str>>,
		span: TextSpan,
		start: LineColumn,
		end: LineColumn,
	) -> Self {
		Self {
			source_id,
			span,
			start,
			end,
		}
	}

	/// Returns the logical source name of the diagnostic range.
	pub fn source_id(&self) -> Option<&str> {
		self.source_id.as_deref()
	}

	/// Returns the byte range associated with this location.
	pub const fn span(&self) -> TextSpan {
		self.span
	}

	/// Returns the starting position of the diagnostic range.
	pub const fn start(&self) -> LineColumn {
		self.start
	}

	/// Returns the position immediately after the end of the range.
	pub const fn end(&self) -> LineColumn {
		self.end
	}
}

/// Computes the line and column for an offset in UTF-8 text.
#[allow(
	clippy::expect_used,
	clippy::missing_panics_doc,
	reason = "line starts at one & column starts at one"
)]
fn line_column_at(source: &str, byte_offset: usize) -> LineColumn {
	let mut line = 1_u32;
	let mut column = 1_u32;

	for character in source[..byte_offset].chars() {
		if character == '\n' {
			line = line.saturating_add(1);
			column = 1;
		} else {
			column = column.saturating_add(1);
		}
	}

	LineColumn {
		line: NonZeroU32::new(line).expect("line starts at one"),
		column: NonZeroU32::new(column).expect("column starts at one"),
	}
}

/// The reason a range cannot be associated with the UTF-8 source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocationError {
	/// The range offset and length overflow their representation.
	RangeOverflow,
	/// The range extends beyond the supplied source text.
	OutOfBounds,
	/// A range boundary does not lie on a UTF-8 character boundary.
	NotCharBoundary,
}

impl Display for LocationError {
	/// Formats the value using the corresponding text representation.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::RangeOverflow => f.write_str("location range overflows u32"),
			Self::OutOfBounds => f.write_str("location is outside the source"),
			Self::NotCharBoundary => f.write_str("location is not on an UTF-8 character boundary"),
		}
	}
}

impl Error for LocationError {}

/// An additional diagnostic location with an optional explanatory label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabeledLocation {
	/// The source-text range explained by this label.
	location: Location,
	/// The optional explanatory label for the location.
	label: Option<Arc<str>>,
}

impl LabeledLocation {
	/// Creates a value from the supplied inputs.
	pub fn new<ArcStr: Into<Arc<str>>>(location: Location, label: Option<ArcStr>) -> Self {
		Self {
			location,
			label: label.map(Into::into),
		}
	}

	/// Returns the location associated with this label.
	pub const fn location(&self) -> &Location {
		&self.location
	}

	/// Returns the explanatory label for the diagnostic location.
	pub fn label(&self) -> Option<&str> {
		self.label.as_deref()
	}
}

/// Context for an individual diagnostic event without changing the semantics of
/// its status.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiagnosticRecord {
	/// The primary location where the diagnosed condition was detected.
	primary_location: Option<Location>,
	/// Additional locations that explain the primary diagnostic.
	related_locations: Vec<LabeledLocation>,
	/// The optional timestamp of the diagnostic event.
	timestamp: Option<SystemTime>,
	/// The optional processing phase in which the event occurred.
	phase: Option<ExecutionPhase>,
	/// An explicit severity override validated against the status kind.
	severity_override: Option<Severity>,
	/// Additional structured data for the diagnostic event.
	extensions: BTreeMap<String, DiagnosticValue>,
}

impl DiagnosticRecord {
	/// Returns the primary diagnostic location, if present.
	pub const fn primary_location(&self) -> Option<&Location> {
		self.primary_location.as_ref()
	}

	/// Returns additional locations that explain the diagnostic.
	pub fn related_locations(&self) -> &[LabeledLocation] {
		&self.related_locations
	}

	/// Returns the timestamp of the diagnostic event.
	pub const fn timestamp(&self) -> Option<SystemTime> {
		self.timestamp
	}

	/// Returns the execution phase in which the diagnostic was produced.
	pub const fn phase(&self) -> Option<ExecutionPhase> {
		self.phase
	}

	/// Returns the explicitly specified severity override.
	pub const fn severity_override(&self) -> Option<Severity> {
		self.severity_override
	}

	/// Returns the additional structured diagnostic properties.
	pub const fn extensions(&self) -> &BTreeMap<String, DiagnosticValue> {
		&self.extensions
	}

	/// Sets the primary location and returns the updated diagnostic.
	pub fn with_primary_location(mut self, location: Location) -> Self {
		self.primary_location = Some(location);
		self
	}

	/// Adds an explanatory location and returns the updated value.
	pub fn with_related_location(mut self, location: LabeledLocation) -> Self {
		self.related_locations.push(location);
		self
	}

	/// Sets the timestamp and returns the updated value.
	pub const fn with_timestamp(mut self, timestamp: SystemTime) -> Self {
		self.timestamp = Some(timestamp);
		self
	}

	/// Sets the execution phase and returns the updated value.
	pub const fn with_phase(mut self, phase: ExecutionPhase) -> Self {
		self.phase = Some(phase);
		self
	}

	/// Changes the explicit severity override for the diagnostic event.
	pub(crate) const fn set_severity_override(&mut self, severity: Severity) {
		self.severity_override = Some(severity);
	}

	/// Adds a structured extension and returns the updated
	/// value.
	pub fn with_extension<Str: Into<String>>(mut self, key: Str, value: DiagnosticValue) -> Self {
		self.extensions.insert(key.into(), value);
		self
	}
}

#[cfg(test)]
/// Regression tests for diagnostic behavior.
mod tests {
	use super::*;

	/// Tests the `computes_unicode_locations_without_retaining_source` scenario
	/// and its diagnostic outcome.
	#[test]
	fn computes_unicode_locations_without_retaining_source() {
		let source = "αβ\nname";
		let offset = u32::try_from("α".len()).unwrap();
		let len = u32::try_from("β\nna".len()).unwrap();
		let location = Location::from_source(None, source, TextSpan::new(offset, len)).unwrap();

		assert_eq!(location.start().line.get(), 1);
		assert_eq!(location.start().column.get(), 2);
		assert_eq!(location.end().line.get(), 2);
		assert_eq!(location.end().column.get(), 3);
	}

	/// Tests the `rejects_non_character_boundary` scenario and its
	/// diagnostic outcome.
	#[test]
	fn rejects_non_character_boundary() {
		let error = Location::from_source(None, "α", TextSpan::new(1, 1)).unwrap_err();
		assert_eq!(error, LocationError::NotCharBoundary);
	}
}
