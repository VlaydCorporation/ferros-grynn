//! Root JOIN identities retain logical slots without changing ordinary row
//! keys.

use super::*;

/// One source in declaration order, independent of physical JOIN ordering.
struct JoinedSource {
	/// The immutable source id column in the padded input row.
	offset: usize,
	/// A once-resolved qualified scope; absent means database-global records.
	graph: Option<GraphRef>,
}

/// Root-only provenance follows existing rows through the ordinary executor.
pub(super) struct JoinedSources {
	/// Sources remain in FROM/JOIN order, including unmatched LEFT slots.
	sources: Vec<JoinedSource>,
	/// Exact positions follow the ordinary sequential SPLIT clauses.
	splits: usize,
}

impl JoinedSources {
	/// Freezes independent scopes on an already admitted private plan copy.
	///
	/// # Errors
	/// Rejects unsupported sources, invalid scopes and exhausted query budgets.
	pub(super) fn prepare<Read: QueryRead>(
		read: &Read,
		snapshot: Snapshot,
		plan: &mut LogicalSelect,
		slots: &[Value],
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Self> {
		if plan.outer_columns != 0 {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"correlated LIVE JOIN source identity",
			));
		}
		let fields = read.bounded_fields().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("LIVE JOIN requires bounded evaluation")
		})?;
		let count = plan.joins.len().checked_add(1).ok_or_else(overflow)?;
		fields
			.admit(count.checked_mul(std::mem::size_of::<JoinedSource>()).ok_or_else(overflow)?)?;
		let mut sources = Vec::new();
		sources.try_reserve_exact(count).map_err(|error| {
			FerrosGrynnError::traversal_resource_limit_exceeded("LIVE JOIN sources", count as u64)
				.with_technical_source(error)
		})?;
		let mut offset = 0_usize;
		for source in std::iter::once(&mut plan.source)
			.chain(plan.joins.iter_mut().map(|join| &mut join.source))
		{
			control.check()?;
			fields.work(1)?;
			let graph = match &mut source.kind {
				LogicalSelectSourceKind::Atoms {
					graph,
					..
				}
				| LogicalSelectSourceKind::AtomUnion {
					graph,
					..
				} => graph,
				LogicalSelectSourceKind::Empty {
					graph,
				} if source.schema.columns.is_empty()
					|| source.schema.columns.first().is_some_and(|column| {
						column.name.as_ref() == "id" && column.ty == crate::values::Type::Record
					}) =>
				{
					graph
				}
				_ => {
					return Err(FerrosGrynnError::query_feature_not_supported(
						"LIVE JOIN identity requires atom sources",
					));
				}
			};
			let graph = if let Some(reference) = graph {
				let resolved =
					resolve_graph_reference(read, snapshot, reference, None, slots, "", control)?;
				read.graph(snapshot, &resolved)?;
				fields.admit(
					resolved
						.name()
						.len()
						.checked_add(std::mem::size_of::<GraphRef>())
						.ok_or_else(overflow)?,
				)?;
				*reference = LogicalGraphReference::Static(resolved.clone());
				Some(resolved)
			} else {
				None
			};
			sources.push(JoinedSource {
				offset,
				graph,
			});
			offset = offset.checked_add(source.schema.columns.len()).ok_or_else(overflow)?;
		}
		if offset != plan.input_schema.columns.len() {
			return Err(invalid("LIVE JOIN source layout mismatch"));
		}
		Ok(Self {
			sources,
			splits: plan.split.len(),
		})
	}

	/// Captures identities before projection/OMIT; original sorter keys stay
	/// intact.
	///
	/// # Errors
	/// Rejects malformed input identities/positions and exhausted query
	/// budgets.
	pub(super) fn capture<Read: QueryRead>(
		&self,
		read: &Read,
		row: &Row,
	) -> FerrosGrynnResult<Value> {
		let fields = read.bounded_fields().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("LIVE JOIN requires bounded evaluation")
		})?;
		let count = self.sources.len();
		let values = count
			.checked_mul(3)
			.and_then(|n| n.checked_add(self.splits))
			.and_then(|n| n.checked_add(2))
			.ok_or_else(overflow)?;
		fields.admit(values.checked_mul(2 * std::mem::size_of::<Value>()).ok_or_else(overflow)?)?;
		let mut sources = Vec::new();
		sources.try_reserve_exact(count).map_err(|error| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE JOIN source identity",
				count as u64,
			)
			.with_technical_source(error)
		})?;
		for (index, source) in self.sources.iter().enumerate() {
			fields.work(1)?;
			let record = match row.value(source.offset) {
				Some(Value::Record(record)) => {
					fields.admit(
						record
							.estimated_heap_bytes()
							.checked_add(std::mem::size_of::<RecordId>())
							.ok_or_else(overflow)?,
					)?;
					Value::from((**record).clone())
				}
				Some(Value::None) if index != 0 => Value::None,
				_ => return Err(invalid("LIVE JOIN source has no record identity")),
			};
			let graph = if let Some(graph) = &source.graph {
				fields.admit(
					graph
						.name()
						.len()
						.checked_add(std::mem::size_of::<GraphRef>())
						.ok_or_else(overflow)?,
				)?;
				Value::from(graph.clone())
			} else {
				Value::None
			};
			sources.push(Value::Array(vec![record, graph].into()));
		}
		let start = row
			.stable
			.len()
			.checked_sub(self.splits)
			.ok_or_else(|| invalid("LIVE JOIN SPLIT positions missing"))?;
		let mut positions = Vec::new();
		positions.try_reserve_exact(self.splits).map_err(|error| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE JOIN SPLIT positions",
				self.splits as u64,
			)
			.with_technical_source(error)
		})?;
		for position in &row.stable[start..] {
			fields.work(1)?;
			let Value::Number(number) = position else {
				return Err(invalid("LIVE JOIN SPLIT position is not numeric"));
			};
			positions.push(Value::from(u64::try_from(*number)?));
		}
		Ok(Value::Array(vec![Value::Array(sources.into()), Value::Array(positions.into())].into()))
	}
}

/// Decodes the admitted suffix after the ordinary unique output ordinal.
///
/// # Errors
/// Rejects malformed anchors or exhausted result-identity budgets.
pub(super) fn joined_identity(
	read: &RootRead<'_>,
	stable: &[Value],
	sources: usize,
	splits: usize,
) -> FerrosGrynnResult<crate::live::LiveIdentity> {
	use crate::live::{LiveIdentity, LiveIdentityPart};
	let Some(Value::Array(suffix)) = stable.last() else {
		return Err(invalid("LIVE JOIN output identity suffix missing"));
	};
	let [Value::Array(records), Value::Array(positions)] = suffix.as_slice() else {
		return Err(invalid("LIVE JOIN output identity envelope mismatch"));
	};
	if records.len() != sources || positions.len() != splits || sources < 2 {
		return Err(invalid("LIVE JOIN output identity counts mismatch"));
	}
	let count = sources.checked_add(splits).ok_or_else(overflow)?;
	read.fields()
		.admit(count.checked_mul(std::mem::size_of::<LiveIdentityPart>()).ok_or_else(overflow)?)?;
	let mut parts = Vec::new();
	parts.try_reserve_exact(count).map_err(|error| {
		FerrosGrynnError::traversal_resource_limit_exceeded("LIVE JOIN identity", count as u64)
			.with_technical_source(error)
	})?;
	for (index, tuple) in records.iter().enumerate() {
		read.fields().work(1)?;
		let Value::Array(tuple) = tuple else {
			return Err(invalid("LIVE JOIN source tuple is not an array"));
		};
		let [record, graph] = tuple.as_slice() else {
			return Err(invalid("LIVE JOIN source tuple layout mismatch"));
		};
		let graph = match graph {
			Value::None => None,
			Value::GraphRef(graph) => Some(graph.as_ref()),
			_ => return Err(invalid("LIVE JOIN source scope is invalid")),
		};
		parts.push(match record {
			Value::None if index != 0 => LiveIdentityPart::Unmatched,
			Value::Record(record) => match root_record_identity(read, record, graph)? {
				LiveIdentity::Record(record) => LiveIdentityPart::Record(record),
				LiveIdentity::Occurrence(occurrence) => LiveIdentityPart::Occurrence(occurrence),
				_ => return Err(invalid("LIVE JOIN source identity kind mismatch")),
			},
			_ => return Err(invalid("LIVE JOIN primary/source record is invalid")),
		});
	}
	for position in positions {
		read.fields().work(1)?;
		let Value::Number(number) = position else {
			return Err(invalid("LIVE JOIN position is not numeric"));
		};
		parts.push(LiveIdentityPart::Split(u64::try_from(*number)?));
	}
	Ok(LiveIdentity::Composite(parts))
}

/// Invalid private anchors propagate through the existing corruption status.
fn invalid(message: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(message)
}

/// Layout arithmetic fails before reserving or copying identity buffers.
fn overflow() -> FerrosGrynnError {
	FerrosGrynnError::operation_overflow("LIVE JOIN identity")
}
