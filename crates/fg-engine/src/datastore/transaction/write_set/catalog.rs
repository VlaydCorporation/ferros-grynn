//! Borrowed retained catalog operands, separate from compiler scratch policy.

use fg_runtime::__internal::{TextInternal as _, TypeInternal as _};

use crate::{
	datastore::{
		AnalyzerFilter,
		AnalyzerTokenizer,
		ClassField,
		ClassIndex,
		ClassSchema,
		SchemaAnalyzer,
		SchemaFunction,
		SchemaFunctionParameter,
	},
	values::Text,
};

/// Includes the separately owned class-name lookup and field/index arrays.
pub(in crate::datastore::transaction) fn class_payload_bytes(class: &ClassSchema) -> usize {
	let mut bytes = class
		.name
		.estimated_heap_bytes()
		.saturating_mul(2)
		.saturating_add(class.fields.capacity().saturating_mul(size_of::<ClassField>()))
		.saturating_add(class.indexes.capacity().saturating_mul(size_of::<ClassIndex>()));
	for field in &class.fields {
		bytes = bytes.saturating_add(field_payload_bytes(field));
	}
	for index in &class.indexes {
		bytes = bytes.saturating_add(index_payload_bytes(index));
	}
	bytes
}

/// The enclosing class vector pays for the field shell.
pub(in crate::datastore::transaction) fn field_payload_bytes(field: &ClassField) -> usize {
	field.name.estimated_heap_bytes().saturating_add(field.value_type.estimated_heap_bytes())
}

/// Index options are Copy; only names and their vector retain heap storage.
pub(in crate::datastore::transaction) fn index_payload_bytes(index: &ClassIndex) -> usize {
	index.fields.iter().fold(
		index
			.name
			.estimated_heap_bytes()
			.saturating_add(index.fields.capacity().saturating_mul(size_of::<Text>())),
		|bytes, name| bytes.saturating_add(name.estimated_heap_bytes()),
	)
}

/// Includes the independently owned signature-map key as well as source text.
pub(in crate::datastore::transaction) fn function_payload_bytes(
	function: &SchemaFunction,
) -> usize {
	let mut bytes = function
		.namespace
		.estimated_heap_bytes()
		.saturating_add(function.name.estimated_heap_bytes())
		.saturating_mul(2)
		.saturating_add(function.body.estimated_heap_bytes())
		.saturating_add(function.comment.as_ref().map_or(0, Text::estimated_heap_bytes))
		.saturating_add(function.result.estimated_heap_bytes())
		.saturating_add(
			function
				.parameters
				.capacity()
				.saturating_mul(size_of::<SchemaFunctionParameter>())
				.saturating_mul(2),
		);
	for parameter in &function.parameters {
		bytes = bytes
			.saturating_add(parameter.name.estimated_heap_bytes())
			.saturating_add(parameter.value_type.estimated_heap_bytes().saturating_mul(2));
	}
	bytes
}

/// Mapper trees keep their complete nodes and owned replacement terms.
pub(in crate::datastore::transaction) fn analyzer_payload_bytes(
	analyzer: &SchemaAnalyzer,
) -> usize {
	let mut bytes = analyzer
		.name
		.estimated_heap_bytes()
		.saturating_mul(2)
		.saturating_add(analyzer.comment.as_ref().map_or(0, Text::estimated_heap_bytes))
		.saturating_add(analyzer.filters.capacity().saturating_mul(size_of::<AnalyzerFilter>()))
		.saturating_add(
			crate::memory::tree_retained_bytes::<AnalyzerTokenizer>(analyzer.tokenizers.len())
				.unwrap_or(usize::MAX),
		);
	for filter in &analyzer.filters {
		if let AnalyzerFilter::Mapper(values) = filter {
			bytes = bytes.saturating_add(
				crate::memory::tree_retained_bytes::<(Text, Option<Text>)>(values.len())
					.unwrap_or(usize::MAX),
			);
			for (name, replacement) in values {
				bytes = bytes
					.saturating_add(name.estimated_heap_bytes())
					.saturating_add(replacement.as_ref().map_or(0, Text::estimated_heap_bytes));
			}
		}
	}
	bytes
}
