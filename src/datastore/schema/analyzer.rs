use std::collections::{BTreeMap, BTreeSet};

use super::{FunctionId, SchemaId, SchemaVersion};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Text,
};

/// Stable identity of one durable text analyzer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct AnalyzerId(
	/// Schema-namespace identity assigned to this analyzer definition.
	SchemaId,
);

impl AnalyzerId {
	/// Creates an analyzer identity from its durable representation.
	pub const fn new(value: u32) -> Option<Self> {
		match SchemaId::new(value) {
			Some(value) => Some(Self(value)),
			None => None,
		}
	}

	/// Returns the durable integer representation.
	pub const fn get(self) -> u32 {
		self.0.get()
	}

	/// Creates from schema id.
	pub(crate) const fn from_schema_id(value: SchemaId) -> Self {
		Self(value)
	}

	/// Creates from storage.
	pub(crate) fn from_storage(value: u32) -> FerrosGrynnResult<Self> {
		Self::new(value)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("analyzer identity is reserved"))
	}
}

impl From<AnalyzerId> for SchemaId {
	/// Converts `AnalyzerId` into `SchemaId`.
	fn from(value: AnalyzerId) -> Self {
		value.0
	}
}

impl std::fmt::Display for AnalyzerId {
	/// Formats `AnalyzerId` using the requested formatter.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		std::fmt::Display::fmt(&self.get(), f)
	}
}

/// Built-in boundary detectors combined by one analyzer tokenization pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum AnalyzerTokenizer {
	/// Splits at Unicode whitespace.
	Blank,
	/// Splits at lower-to-upper camel-case transitions.
	Camel,
	/// Splits when the Unicode scalar class changes.
	Class,
	/// Separates punctuation from adjacent text.
	Punct,
}

/// Snowball language supported by the durable analyzer contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum AnalyzerLanguage {
	/// Arabic stemming.
	Arabic,
	/// Danish stemming.
	Danish,
	/// Dutch stemming.
	Dutch,
	/// English stemming.
	English,
	/// Finnish stemming.
	Finnish,
	/// French stemming.
	French,
	/// German stemming.
	German,
	/// Greek stemming.
	Greek,
	/// Hungarian stemming.
	Hungarian,
	/// Italian stemming.
	Italian,
	/// Norwegian stemming.
	Norwegian,
	/// Portuguese stemming.
	Portuguese,
	/// Romanian stemming.
	Romanian,
	/// Russian stemming.
	Russian,
	/// Spanish stemming.
	Spanish,
	/// Swedish stemming.
	Swedish,
	/// Tamil stemming.
	Tamil,
	/// Turkish stemming.
	Turkish,
}

/// One ordered transformation in a durable analyzer pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnalyzerFilter {
	/// Canonical Unicode composition.
	Nfc,
	/// Compatibility Unicode composition.
	Nfkc,
	/// Transliteration to ASCII.
	Ascii,
	/// Unicode lowercase conversion.
	Lowercase,
	/// Unicode uppercase conversion.
	Uppercase,
	/// Prefix scalar n-grams in the inclusive length range.
	EdgeNgram {
		/// Minimum scalar count.
		min: u16,
		/// Maximum scalar count.
		max: u16,
	},
	/// All scalar n-grams in the inclusive length range.
	Ngram {
		/// Minimum scalar count.
		min: u16,
		/// Maximum scalar count.
		max: u16,
	},
	/// Snowball stemming for one fixed language.
	Snowball(
		/// Snowball stemming for one fixed language.
		AnalyzerLanguage,
	),
	/// Inline exact term replacements; `None` removes a term.
	Mapper(
		/// Inline exact term replacements; `None` removes a term.
		BTreeMap<Text, Option<Text>>,
	),
}

/// Immutable durable definition of one named analyzer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaAnalyzer {
	/// Identifier used to locate this value within its owning subsystem.
	pub(crate) id: Option<AnalyzerId>,
	/// Version associated with this schema analyzer.
	pub(crate) version: SchemaVersion,
	/// Published at associated with this schema analyzer.
	pub(crate) published_at: u64,
	/// Name associated with this schema analyzer.
	pub(crate) name: Text,
	/// Tokenizers associated with this schema analyzer.
	pub(crate) tokenizers: BTreeSet<AnalyzerTokenizer>,
	/// Filters associated with this schema analyzer.
	pub(crate) filters: Vec<AnalyzerFilter>,
	/// Function associated with this schema analyzer.
	pub(crate) function: Option<(FunctionId, SchemaVersion)>,
	/// Comment associated with this schema analyzer.
	pub(crate) comment: Option<Text>,
}

impl SchemaAnalyzer {
	/// Creates an unpublished analyzer using built-in tokenizers and ordered
	/// filters. An empty tokenizer set makes the whole non-empty input one
	/// token.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error for an invalid name, n-gram range, or
	/// mapper entry.
	pub fn new<IntoText, Tokenizers, Filters>(
		name: IntoText,
		tokenizers: Tokenizers,
		filters: Filters,
	) -> FerrosGrynnResult<Self>
	where
		IntoText: Into<Text>,
		Tokenizers: IntoIterator<Item = AnalyzerTokenizer>,
		Filters: IntoIterator<Item = AnalyzerFilter>,
	{
		let name = name.into();
		validate_name(&name)?;
		let filters = filters.into_iter().collect::<Vec<_>>();
		for filter in &filters {
			validate_filter(filter)?;
		}
		Ok(Self {
			id: None,
			version: SchemaVersion::INITIAL,
			published_at: 0,
			name,
			tokenizers: tokenizers.into_iter().collect(),
			filters,
			function: None,
			comment: None,
		})
	}

	/// Selects a versioned PURE analyzer function instead of built-in
	/// tokenization.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when built-in tokenizers are already
	/// configured.
	pub fn with_function(
		mut self,
		id: FunctionId,
		version: SchemaVersion,
	) -> FerrosGrynnResult<Self> {
		if !self.tokenizers.is_empty() {
			return Err(FerrosGrynnError::invalid_argument(
				"an analyzer FUNCTION and TOKENIZERS are mutually exclusive",
			));
		}
		self.function = Some((id, version));
		Ok(self)
	}

	/// Attaches an optional durable catalog comment.
	#[must_use]
	pub fn with_comment<IntoText: Into<Text>>(mut self, comment: IntoText) -> Self {
		self.comment = Some(comment.into());
		self
	}

	/// Returns the stable identity after publication.
	pub const fn id(&self) -> Option<AnalyzerId> {
		self.id
	}

	/// Returns the semantic definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the case-sensitive analyzer name.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Iterates built-in tokenizers in canonical order.
	pub fn tokenizers(&self) -> impl ExactSizeIterator<Item = AnalyzerTokenizer> + '_ {
		self.tokenizers.iter().copied()
	}

	/// Iterates filters in execution order.
	pub fn filters(&self) -> impl ExactSizeIterator<Item = &AnalyzerFilter> {
		self.filters.iter()
	}

	/// Returns the exact analyzer-function identity and version, if configured.
	pub const fn function(&self) -> Option<(FunctionId, SchemaVersion)> {
		self.function
	}

	/// Returns the optional durable comment.
	pub const fn comment(&self) -> Option<&Text> {
		self.comment.as_ref()
	}
}

/// Validates name.
fn validate_name(name: &Text) -> FerrosGrynnResult<()> {
	if name.is_empty() || name.as_str().contains('\0') {
		return Err(FerrosGrynnError::invalid_argument(
			"analyzer name must be non-empty and must not contain NUL",
		));
	}
	Ok(())
}

/// Validates filter.
fn validate_filter(filter: &AnalyzerFilter) -> FerrosGrynnResult<()> {
	match filter {
		AnalyzerFilter::EdgeNgram {
			min,
			max,
		}
		| AnalyzerFilter::Ngram {
			min,
			max,
		} if *min == 0 || min > max => Err(FerrosGrynnError::invalid_argument(
			"analyzer n-gram bounds must satisfy 1 <= min <= max",
		)),
		AnalyzerFilter::Mapper(entries)
			if entries.iter().any(|(source, target)| {
				source.is_empty()
					|| source.as_str().contains('\0')
					|| target
						.as_ref()
						.is_some_and(|target| target.is_empty() || target.as_str().contains('\0'))
			}) =>
		{
			Err(FerrosGrynnError::invalid_argument(
				"analyzer mapper terms must be non-empty and must not contain NUL",
			))
		}
		_ => Ok(()),
	}
}

#[cfg(test)]
/// Regression tests for analyzer behavior.
mod tests {
	use std::collections::BTreeMap;

	use super::{AnalyzerFilter, AnalyzerTokenizer, SchemaAnalyzer};
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::values::Text;

	/// Verifies canonicalizes tokenizer set but preserves filter order.
	#[test]
	fn canonicalizes_tokenizer_set_but_preserves_filter_order() {
		let analyzer = SchemaAnalyzer::new(
			"english",
			[AnalyzerTokenizer::Punct, AnalyzerTokenizer::Blank, AnalyzerTokenizer::Punct],
			[AnalyzerFilter::Lowercase, AnalyzerFilter::Nfc],
		)
		.assert_ok();
		assert_eq!(analyzer.tokenizers().collect::<Vec<_>>(), vec![
			AnalyzerTokenizer::Blank,
			AnalyzerTokenizer::Punct
		]);
		assert_eq!(analyzer.filters().collect::<Vec<_>>(), vec![
			&AnalyzerFilter::Lowercase,
			&AnalyzerFilter::Nfc
		]);
	}

	/// Verifies rejects invalid ngrams and mapper terms.
	#[test]
	fn rejects_invalid_ngrams_and_mapper_terms() {
		assert!(
			SchemaAnalyzer::new("bad", [], [AnalyzerFilter::Ngram {
				min: 0,
				max: 3,
			}],)
			.is_err()
		);
		let mut mapper = BTreeMap::new();
		mapper.insert(Text::from(""), Some(Text::from("term")));
		assert!(SchemaAnalyzer::new("bad", [], [AnalyzerFilter::Mapper(mapper)]).is_err());
	}
}
