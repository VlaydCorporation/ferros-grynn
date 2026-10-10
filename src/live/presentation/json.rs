//! JSON export validates compatibility and counts bytes before allocation.

use std::io::{self, Write};

use super::{Budget, LiveEventOutput, LivePatchKind, overflow, resource};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::LiveBindingOptions,
	values::{Number, Value},
};

impl LiveEventOutput {
	/// Exports compatible values as JSON, preserving exact integers and
	/// decimals. PATCH produces an RFC 6902 array; DIFF retains literal
	/// path segments.
	///
	/// # Errors
	///
	/// Rejects native non-JSON values, nonfinite numbers, depth/work/byte
	/// limits and allocation failures before returning any output.
	pub fn to_json(&self, options: &LiveBindingOptions) -> FerrosGrynnResult<String> {
		let mut probe = SizeWriter {
			budget: Budget::new(options),
			length: 0,
			failure: None,
		};
		preflight(self, &mut probe.budget)?;
		let measured = encode(&mut probe, self);
		if let Some(error) = probe.failure {
			return Err(error);
		}
		measured?;
		let mut bytes = Vec::new();
		bytes.try_reserve_exact(probe.length).map_err(|error| {
			resource("LIVE JSON scratch", options.scratch_bytes() as u64)
				.with_technical_source(error)
		})?;
		encode(&mut bytes, self)?;
		String::from_utf8(bytes).map_err(|error| {
			FerrosGrynnError::invalid_storage_file("LIVE JSON writer emitted invalid UTF-8")
				.with_technical_source(error)
		})
	}
}

/// Rejects inspection limits before serializers scan unescaped string runs.
fn preflight(output: &LiveEventOutput, budget: &mut Budget) -> FerrosGrynnResult<()> {
	match output {
		LiveEventOutput::Full {
			before,
			after,
		} => {
			for value in before.iter().chain(after) {
				preflight_value(value, budget, 0)?;
			}
		}
		LiveEventOutput::Diff(changes) => {
			budget.take(0, changes.len() as u64)?;
			for change in changes {
				for part in &change.path {
					budget.take(0, part.len() as u64 + 1)?;
				}
				for value in change.before.iter().chain(&change.after) {
					preflight_value(value, budget, 0)?;
				}
			}
		}
		LiveEventOutput::Patch(operations) => {
			budget.take(0, operations.len() as u64)?;
			for operation in operations {
				budget.take(0, operation.path.len() as u64 + 1)?;
				if let Some(value) = &operation.value {
					preflight_value(value, budget, 0)?;
				}
			}
		}
	}
	Ok(())
}

/// JSON-only borrowed inspection has no native clone-byte reservation.
fn preflight_value(value: &Value, budget: &mut Budget, depth: usize) -> FerrosGrynnResult<()> {
	if depth > 64 {
		return Err(resource("LIVE JSON depth", 64));
	}
	budget.take(0, 1)?;
	match value {
		Value::Null | Value::Bool(_) => {}
		Value::Number(number) => {
			if matches!(number, Number::Float(value) if !value.is_finite()) {
				return Err(incompatible());
			}
		}
		Value::String(text) => budget.take(0, text.len() as u64)?,
		Value::Array(values) => {
			for value in values.iter() {
				preflight_value(value, budget, depth + 1)?;
			}
		}
		Value::Object(values) => {
			for (name, value) in values.iter() {
				budget.take(0, name.len() as u64 + 1)?;
				preflight_value(value, budget, depth + 1)?;
			}
		}
		_ => return Err(incompatible()),
	}
	Ok(())
}

/// Allocation-free first pass; the same encoder writes the admitted buffer.
struct SizeWriter {
	/// Caller limits shared by every emitted byte.
	budget: Budget,
	/// Checked required output capacity.
	length: usize,
	/// Original typed admission error, separate from the I/O adapter error.
	failure: Option<FerrosGrynnError>,
}

impl Write for SizeWriter {
	/// Accounts bytes before accepting a formatter/serializer chunk.
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		let admitted = u64::try_from(buf.len())
			.map_err(|_error| overflow())
			.and_then(|work| self.budget.take(buf.len(), work));
		if let Err(error) = admitted {
			self.failure = Some(error);
			return Err(io::ErrorKind::OutOfMemory.into());
		}
		let Some(length) = self.length.checked_add(buf.len()) else {
			self.failure = Some(overflow());
			return Err(io::ErrorKind::OutOfMemory.into());
		};
		self.length = length;
		Ok(buf.len())
	}

	/// No external I/O exists in the measurement pass.
	fn flush(&mut self) -> io::Result<()> {
		Ok(())
	}
}

/// Emits a single deterministic output envelope without an intermediate DOM.
fn encode(writer: &mut impl Write, output: &LiveEventOutput) -> FerrosGrynnResult<()> {
	match output {
		LiveEventOutput::Full {
			before,
			after,
		} => images(writer, before.as_ref(), after.as_ref()),
		LiveEventOutput::Diff(changes) => {
			bytes(writer, b"[")?;
			for (index, change) in changes.iter().enumerate() {
				if index != 0 {
					bytes(writer, b",")?;
				}
				bytes(writer, b"{\"path\":[")?;
				for (index, part) in change.path.iter().enumerate() {
					if index != 0 {
						bytes(writer, b",")?;
					}
					string(writer, part.as_str())?;
				}
				bytes(writer, b"]")?;
				if let Some(before) = &change.before {
					bytes(writer, b",\"before\":")?;
					value(writer, before, 0)?;
				}
				if let Some(after) = &change.after {
					bytes(writer, b",\"after\":")?;
					value(writer, after, 0)?;
				}
				bytes(writer, b"}")?;
			}
			bytes(writer, b"]")
		}
		LiveEventOutput::Patch(operations) => {
			bytes(writer, b"[")?;
			for (index, operation) in operations.iter().enumerate() {
				if index != 0 {
					bytes(writer, b",")?;
				}
				bytes(writer, b"{\"op\":")?;
				string(writer, match operation.kind {
					LivePatchKind::Add => "add",
					LivePatchKind::Remove => "remove",
					LivePatchKind::Replace => "replace",
				})?;
				bytes(writer, b",\"path\":")?;
				string(writer, operation.path.as_str())?;
				if let Some(payload) = &operation.value {
					bytes(writer, b",\"value\":")?;
					value(writer, payload, 0)?;
				}
				bytes(writer, b"}")?;
			}
			bytes(writer, b"]")
		}
	}
}

/// Absent images omit their keys; an actual native Null remains JSON null.
fn images(
	writer: &mut impl Write,
	before: Option<&Value>,
	after: Option<&Value>,
) -> FerrosGrynnResult<()> {
	bytes(writer, b"{")?;
	if let Some(before) = before {
		bytes(writer, b"\"before\":")?;
		value(writer, before, 0)?;
	}
	if let Some(after) = after {
		if before.is_some() {
			bytes(writer, b",")?;
		}
		bytes(writer, b"\"after\":")?;
		value(writer, after, 0)?;
	}
	bytes(writer, b"}")
}

/// Ordinary JSON types only; exact native numbers never pass through f64.
fn value(writer: &mut impl Write, input: &Value, depth: usize) -> FerrosGrynnResult<()> {
	if depth > 64 {
		return Err(resource("LIVE JSON depth", 64));
	}
	match input {
		Value::Null => bytes(writer, b"null"),
		Value::Bool(value) => bytes(
			writer,
			if *value {
				b"true"
			} else {
				b"false"
			},
		),
		Value::Number(number) => {
			if matches!(number, Number::Float(value) if !value.is_finite()) {
				return Err(incompatible());
			}
			write!(writer, "{number}").map_err(io_error)
		}
		Value::String(value) => string(writer, value.as_str()),
		Value::Array(values) => {
			bytes(writer, b"[")?;
			for (index, input) in values.iter().enumerate() {
				if index != 0 {
					bytes(writer, b",")?;
				}
				value(writer, input, depth + 1)?;
			}
			bytes(writer, b"]")
		}
		Value::Object(values) => {
			bytes(writer, b"{")?;
			for (index, (key, input)) in values.iter().enumerate() {
				if index != 0 {
					bytes(writer, b",")?;
				}
				string(writer, key.as_str())?;
				bytes(writer, b":")?;
				value(writer, input, depth + 1)?;
			}
			bytes(writer, b"}")
		}
		_ => Err(incompatible()),
	}
}

/// Reuses the installed JSON serializer solely for UTF-8 string escaping.
fn string(writer: &mut impl Write, text: &str) -> FerrosGrynnResult<()> {
	serde_json::to_writer(writer, text).map_err(|error| {
		FerrosGrynnError::invalid_argument("LIVE JSON string encoding failed")
			.with_technical_source(error)
	})
}

/// Writes static framing without allocations.
fn bytes(writer: &mut impl Write, bytes: &[u8]) -> FerrosGrynnResult<()> {
	writer.write_all(bytes).map_err(io_error)
}

/// Preserves the shared typed error boundary for writer failures.
fn io_error(error: io::Error) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument("LIVE JSON encoding failed").with_technical_source(error)
}

/// No native type receives an implicit string, null or floating conversion.
fn incompatible() -> FerrosGrynnError {
	FerrosGrynnError::conversion_error("native LIVE value", "JSON-compatible value")
}
