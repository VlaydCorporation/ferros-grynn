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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Severity {
	Information,
	Warning,
	Error,
	Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
	Parsing,
	SemanticAnalysis,
	Planning,
	Optimization,
	Execution,
	Commit,
	Persistence,
	Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TextSpan {
	pub offset: u32,
	pub len: u32,
}

impl TextSpan {
	pub const fn new(offset: u32, len: u32) -> Self {
		Self {
			offset,
			len,
		}
	}

	pub const fn end(self) -> Option<u32> {
		self.offset.checked_add(self.len)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LineColumn {
	pub line: NonZeroU32,
	pub column: NonZeroU32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
	source_id: Option<Arc<str>>,
	span: TextSpan,
	start: LineColumn,
	end: LineColumn,
}

impl Location {
	pub fn from_source(
		source_id: Option<Arc<str>>,
		source: &str,
		span: TextSpan,
	) -> Result<Self, LocationError> {
		let end = span.end().ok_or(LocationError::RangeOverflow)?;
		let start_offset = usize::try_from(span.offset).map_err(|_| LocationError::OutOfBounds)?;
		let end_offset = usize::try_from(end).map_err(|_| LocationError::OutOfBounds)?;

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

	pub fn source_id(&self) -> Option<&str> {
		self.source_id.as_deref()
	}

	pub const fn span(&self) -> TextSpan {
		self.span
	}

	pub const fn start(&self) -> LineColumn {
		self.start
	}

	pub const fn end(&self) -> LineColumn {
		self.end
	}
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationError {
	RangeOverflow,
	OutOfBounds,
	NotCharBoundary,
}

impl Display for LocationError {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::RangeOverflow => f.write_str("location range overflows u32"),
			Self::OutOfBounds => f.write_str("location is outside the source"),
			Self::NotCharBoundary => f.write_str("location is not on an UTF-8 character boundary"),
		}
	}
}

impl Error for LocationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabeledLocation {
	location: Location,
	label: Option<Arc<str>>,
}

impl LabeledLocation {
	pub fn new(location: Location, label: Option<impl Into<Arc<str>>>) -> Self {
		Self {
			location,
			label: label.map(Into::into),
		}
	}

	pub const fn location(&self) -> &Location {
		&self.location
	}

	pub fn label(&self) -> Option<&str> {
		self.label.as_deref()
	}
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiagnosticRecord {
	primary_location: Option<Location>,
	related_locations: Vec<LabeledLocation>,
	timestamp: Option<SystemTime>,
	phase: Option<ExecutionPhase>,
	severity_override: Option<Severity>,
	extensions: BTreeMap<String, DiagnosticValue>,
}

impl DiagnosticRecord {
	pub const fn primary_location(&self) -> Option<&Location> {
		self.primary_location.as_ref()
	}

	pub fn related_locations(&self) -> &[LabeledLocation] {
		&self.related_locations
	}

	pub const fn timestamp(&self) -> Option<SystemTime> {
		self.timestamp
	}

	pub const fn phase(&self) -> Option<ExecutionPhase> {
		self.phase
	}

	pub const fn severity_override(&self) -> Option<Severity> {
		self.severity_override
	}

	pub const fn extensions(&self) -> &BTreeMap<String, DiagnosticValue> {
		&self.extensions
	}

	pub fn with_primary_location(mut self, location: Location) -> Self {
		self.primary_location = Some(location);
		self
	}

	pub fn with_related_location(mut self, location: LabeledLocation) -> Self {
		self.related_locations.push(location);
		self
	}

	pub fn with_timestamp(mut self, timestamp: SystemTime) -> Self {
		self.timestamp = Some(timestamp);
		self
	}

	pub fn with_phase(mut self, phase: ExecutionPhase) -> Self {
		self.phase = Some(phase);
		self
	}

	pub(crate) fn set_severity_override(&mut self, severity: Severity) {
		self.severity_override = Some(severity);
	}

	pub fn with_extension(mut self, key: impl Into<String>, value: DiagnosticValue) -> Self {
		self.extensions.insert(key.into(), value);
		self
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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

	#[test]
	fn rejects_non_character_boundary() {
		let error = Location::from_source(None, "α", TextSpan::new(1, 1)).unwrap_err();
		assert_eq!(error, LocationError::NotCharBoundary);
	}
}
