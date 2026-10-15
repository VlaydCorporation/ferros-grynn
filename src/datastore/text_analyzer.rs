use icu_normalizer::ComposingNormalizerBorrowed;
use rust_stemmers::{Algorithm, Stemmer};

use super::schema::{AnalyzerFilter, AnalyzerLanguage, AnalyzerTokenizer, SchemaAnalyzer};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Number, Text, Value},
};

/// Configured max analyzer input scalars value.
const MAX_ANALYZER_INPUT_SCALARS: usize = 1_000_000;
/// Configured max analyzer term scalars value.
const MAX_ANALYZER_TERM_SCALARS: usize = 4_096;
/// Configured max analyzer term bytes value.
const MAX_ANALYZER_TERM_BYTES: usize = 8 * 1_024;
/// Configured max analyzer tokens value.
const MAX_ANALYZER_TOKENS: usize = 1_000_000;

/// One final analyzer token and the half-open scalar span of its source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyzedToken {
	/// Term associated with this analyzed token.
	pub(crate) term: Text,
	/// Start associated with this analyzed token.
	pub(crate) start: u32,
	/// End associated with this analyzed token.
	pub(crate) end: u32,
}

/// Executes built-in tokenization followed by the analyzer's ordered filters.
///
/// # Errors
///
/// Returns an error when the analyzer uses a UDF or a configured resource
/// limit is exceeded.
pub fn analyze_builtin(
	analyzer: &SchemaAnalyzer,
	input: &str,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	analyze_builtin_bounded(analyzer, input, &|_bytes, _work| Ok(()))
}

/// Applies the normal analyzer with caller admission before token expansion.
///
/// # Errors
/// Propagates validation and caller resource/cancellation failures.
#[allow(
	clippy::redundant_pub_crate,
	reason = "Bounded analysis is re-exported for the GQL evaluation boundary"
)]
pub(crate) fn analyze_builtin_bounded(
	analyzer: &SchemaAnalyzer,
	input: &str,
	admit: &dyn Fn(usize, u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	if analyzer.function().is_some() {
		return Err(FerrosGrynnError::invalid_argument(
			"a function-backed analyzer requires evaluated UDF tokens",
		));
	}
	admit(
		input.len().checked_mul(256).ok_or_else(|| analyzer_limit("tokenization scratch"))?,
		input.len() as u64,
	)?;
	let tokens = tokenize(input, analyzer.tokenizers())?;
	apply_filters(analyzer, tokens, admit)
}

/// Converts a function result into checked source-spanned tokens, then applies
/// the analyzer's ordered filters.
///
/// # Errors
///
/// Returns an error for an invalid function result, scalar span, term, or
/// resource limit.
pub fn analyze_function_result(
	analyzer: &SchemaAnalyzer,
	input: &str,
	result: &Value,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	analyze_function_result_bounded(analyzer, input, result, &|_bytes, _work| Ok(()))
}

/// Admits function-token copies and ordered filters before allocation.
///
/// # Errors
/// Propagates token validation and caller admission failures.
#[allow(
	clippy::redundant_pub_crate,
	reason = "Bounded token conversion is re-exported for the GQL evaluation boundary"
)]
pub(crate) fn analyze_function_result_bounded(
	analyzer: &SchemaAnalyzer,
	input: &str,
	result: &Value,
	admit: &dyn Fn(usize, u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	if analyzer.function().is_none() {
		return Err(FerrosGrynnError::invalid_argument(
			"built-in analyzer does not accept UDF token output",
		));
	}
	admit(0, input.len() as u64)?;
	let scalar_count = bounded_scalar_count(input)?;
	let values = result.as_array().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("analyzer function must return array<object>")
	})?;
	check_token_count(values.len())?;
	admit(
		values
			.len()
			.checked_mul(size_of::<AnalyzedToken>())
			.ok_or_else(|| analyzer_limit("token array scratch"))?,
		values.len() as u64,
	)?;
	let mut tokens = Vec::with_capacity(values.len());
	for value in values {
		admit(0, 1)?;
		let object = value.as_object().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("each analyzer function token must be an object")
		})?;
		if object.len() != 3
			|| !object.contains_key("term")
			|| !object.contains_key("start")
			|| !object.contains_key("end")
		{
			return Err(FerrosGrynnError::invalid_argument(
				"analyzer token must contain exactly term, start, and end",
			));
		}
		let term = object
			.get("term")
			.and_then(Value::as_text)
			.filter(|term| !term.is_empty())
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("analyzer token term must be a non-empty string")
			})?;
		admit(term.len(), term.len() as u64)?;
		check_term(term.as_str())?;
		let start = token_offset(object.get("start"), "start")?;
		let end = token_offset(object.get("end"), "end")?;
		if start >= end || usize::try_from(end).map_or(true, |end| end > scalar_count) {
			return Err(FerrosGrynnError::invalid_argument(
				"analyzer token span must be a non-empty range inside the source text",
			));
		}
		tokens.push(AnalyzedToken {
			term: term.clone(),
			start,
			end,
		});
	}
	apply_filters(analyzer, tokens, admit)
}

/// Returns the tokenize result.
fn tokenize(
	input: &str,
	tokenizers: impl ExactSizeIterator<Item = AnalyzerTokenizer>,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	let scalar_count = bounded_scalar_count(input)?;
	if input.is_empty() {
		return Ok(Vec::new());
	}
	let tokenizers = tokenizers.collect::<Vec<_>>();
	if tokenizers.is_empty() {
		check_term(input)?;
		return Ok(vec![AnalyzedToken {
			term: Text::from(input),
			start: 0,
			end: u32::try_from(scalar_count)
				.map_err(|_conversion_error| analyzer_limit("source span"))?,
		}]);
	}

	let camel = tokenizers.contains(&AnalyzerTokenizer::Camel);
	let class = tokenizers.contains(&AnalyzerTokenizer::Class);
	let punct = tokenizers.contains(&AnalyzerTokenizer::Punct);
	let blank = tokenizers.contains(&AnalyzerTokenizer::Blank);
	let chars = input.char_indices().collect::<Vec<_>>();
	let mut tokens = Vec::new();
	let mut start = None;
	for (scalar_index, &(byte_index, current)) in chars.iter().enumerate() {
		if blank && current.is_whitespace() {
			flush_token(input, &chars, &mut tokens, &mut start, scalar_index)?;
			continue;
		}
		let previous = scalar_index.checked_sub(1).map(|index| chars[index].1);
		let boundary = previous.is_some_and(|previous| {
			(camel && previous.is_lowercase() && current.is_uppercase())
				|| (class && scalar_class(previous) != scalar_class(current))
				|| (punct && is_punctuation(previous) != is_punctuation(current))
		});
		if boundary {
			flush_token(input, &chars, &mut tokens, &mut start, scalar_index)?;
		}
		if start.is_none() {
			start = Some((scalar_index, byte_index));
		}
	}
	flush_token(input, &chars, &mut tokens, &mut start, chars.len())?;
	Ok(tokens)
}

/// Flushes token.
fn flush_token(
	input: &str,
	chars: &[(usize, char)],
	tokens: &mut Vec<AnalyzedToken>,
	start: &mut Option<(usize, usize)>,
	end_scalar: usize,
) -> FerrosGrynnResult<()> {
	let Some((start_scalar, start_byte)) = start.take() else {
		return Ok(());
	};
	let end_byte = chars.get(end_scalar).map_or(input.len(), |(offset, _)| *offset);
	let term = &input[start_byte..end_byte];
	if term.is_empty() {
		return Ok(());
	}
	check_term(term)?;
	check_token_count(tokens.len().saturating_add(1))?;
	tokens.push(AnalyzedToken {
		term: Text::from(term),
		start: u32::try_from(start_scalar)
			.map_err(|_conversion_error| analyzer_limit("source span"))?,
		end: u32::try_from(end_scalar)
			.map_err(|_conversion_error| analyzer_limit("source span"))?,
	});
	Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
/// Scalar class alternatives.
enum ScalarClass {
	/// Whitespace case of scalar class.
	Whitespace,
	/// Letter case of scalar class.
	Letter,
	/// Number case of scalar class.
	Number,
	/// Punctuation case of scalar class.
	Punctuation,
}

/// Returns the scalar class result.
fn scalar_class(value: char) -> ScalarClass {
	if value.is_whitespace() {
		ScalarClass::Whitespace
	} else if value.is_alphabetic() {
		ScalarClass::Letter
	} else if value.is_numeric() {
		ScalarClass::Number
	} else {
		ScalarClass::Punctuation
	}
}

/// Returns whether this value is punctuation.
fn is_punctuation(value: char) -> bool {
	!value.is_whitespace() && !value.is_alphanumeric()
}

/// Applies filters.
fn apply_filters(
	analyzer: &SchemaAnalyzer,
	mut tokens: Vec<AnalyzedToken>,
	admit: &dyn Fn(usize, u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	for filter in analyzer.filters() {
		admit(
			tokens
				.len()
				.checked_mul(2 * size_of::<AnalyzedToken>())
				.ok_or_else(|| analyzer_limit("filter token array"))?,
			tokens.len() as u64,
		)?;
		tokens = match filter {
			AnalyzerFilter::Nfc => map_terms(tokens, admit, |term| {
				ComposingNormalizerBorrowed::new_nfc().normalize(term).into_owned()
			})?,
			AnalyzerFilter::Nfkc => map_terms(tokens, admit, |term| {
				ComposingNormalizerBorrowed::new_nfkc().normalize(term).into_owned()
			})?,
			AnalyzerFilter::Ascii => map_terms(tokens, admit, deunicode::deunicode)?,
			AnalyzerFilter::Lowercase => map_terms(tokens, admit, str::to_lowercase)?,
			AnalyzerFilter::Uppercase => map_terms(tokens, admit, str::to_uppercase)?,
			AnalyzerFilter::EdgeNgram {
				min,
				max,
			} => ngrams(tokens, usize::from(*min), usize::from(*max), true, admit)?,
			AnalyzerFilter::Ngram {
				min,
				max,
			} => ngrams(tokens, usize::from(*min), usize::from(*max), false, admit)?,
			AnalyzerFilter::Snowball(language) => {
				let stemmer = Stemmer::create(stemmer_algorithm(*language));
				map_terms(tokens, admit, |term| stemmer.stem(&term.to_lowercase()).into_owned())?
			}
			AnalyzerFilter::Mapper(mapper) => {
				let mut output = Vec::with_capacity(tokens.len());
				for mut token in tokens {
					admit(
						0,
						(token.term.len() as u64)
							.checked_mul(u64::from(usize::BITS - mapper.len().leading_zeros()) + 1)
							.ok_or_else(|| analyzer_limit("mapper lookup work"))?,
					)?;
					match mapper.get(&token.term) {
						Some(Some(replacement)) => {
							admit(replacement.len(), replacement.len() as u64)?;
							token.term = replacement.clone();
						}
						Some(None) => continue,
						None => {}
					}
					output.push(token);
				}
				output
			}
		};
		check_token_count(tokens.len())?;
	}
	Ok(tokens)
}

/// Returns the map terms result.
fn map_terms(
	tokens: Vec<AnalyzedToken>,
	admit: &dyn Fn(usize, u64) -> FerrosGrynnResult<()>,
	mut transform: impl FnMut(&str) -> String,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	let mut output = Vec::with_capacity(tokens.len());
	for mut token in tokens {
		admit(
			token
				.term
				.len()
				.checked_mul(128)
				.and_then(|bytes| bytes.checked_add(MAX_ANALYZER_TERM_BYTES))
				.ok_or_else(|| analyzer_limit("term transform scratch"))?,
			(token.term.len() as u64)
				.checked_mul(128)
				.ok_or_else(|| analyzer_limit("term transform work"))?,
		)?;
		let term = transform(token.term.as_str());
		if term.is_empty() {
			continue;
		}
		check_term(&term)?;
		token.term = Text::from(term);
		output.push(token);
	}
	Ok(output)
}

/// Returns the ngrams result.
fn ngrams(
	tokens: Vec<AnalyzedToken>,
	minimum: usize,
	maximum: usize,
	edge_only: bool,
	admit: &dyn Fn(usize, u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	let mut output = Vec::new();
	for token in tokens {
		admit(
			token
				.term
				.len()
				.checked_add(1)
				.and_then(|len| len.checked_mul(2 * size_of::<usize>()))
				.ok_or_else(|| analyzer_limit("ngram boundaries"))?,
			token.term.len() as u64,
		)?;
		let boundaries = token
			.term
			.char_indices()
			.map(|(offset, _)| offset)
			.chain(std::iter::once(token.term.len()))
			.collect::<Vec<_>>();
		let scalar_count = boundaries.len().saturating_sub(1);
		for length in minimum..=maximum.min(scalar_count) {
			admit(0, 1)?;
			let starts = if edge_only {
				0..=0
			} else {
				0..=scalar_count.saturating_sub(length)
			};
			for start in starts {
				let term = &token.term[boundaries[start]..boundaries[start + length]];
				admit(
					term.len()
						.checked_add(4 * size_of::<AnalyzedToken>())
						.ok_or_else(|| analyzer_limit("ngram token scratch"))?,
					term.len() as u64 + 1,
				)?;
				check_token_count(output.len().saturating_add(1))?;
				output.push(AnalyzedToken {
					term: Text::from(term),
					start: token.start,
					end: token.end,
				});
			}
		}
	}
	Ok(output)
}

/// Returns the stemmer algorithm result.
const fn stemmer_algorithm(language: AnalyzerLanguage) -> Algorithm {
	match language {
		AnalyzerLanguage::Arabic => Algorithm::Arabic,
		AnalyzerLanguage::Danish => Algorithm::Danish,
		AnalyzerLanguage::Dutch => Algorithm::Dutch,
		AnalyzerLanguage::English => Algorithm::English,
		AnalyzerLanguage::Finnish => Algorithm::Finnish,
		AnalyzerLanguage::French => Algorithm::French,
		AnalyzerLanguage::German => Algorithm::German,
		AnalyzerLanguage::Greek => Algorithm::Greek,
		AnalyzerLanguage::Hungarian => Algorithm::Hungarian,
		AnalyzerLanguage::Italian => Algorithm::Italian,
		AnalyzerLanguage::Norwegian => Algorithm::Norwegian,
		AnalyzerLanguage::Portuguese => Algorithm::Portuguese,
		AnalyzerLanguage::Romanian => Algorithm::Romanian,
		AnalyzerLanguage::Russian => Algorithm::Russian,
		AnalyzerLanguage::Spanish => Algorithm::Spanish,
		AnalyzerLanguage::Swedish => Algorithm::Swedish,
		AnalyzerLanguage::Tamil => Algorithm::Tamil,
		AnalyzerLanguage::Turkish => Algorithm::Turkish,
	}
}

/// Returns the bounded scalar count result.
fn bounded_scalar_count(input: &str) -> FerrosGrynnResult<usize> {
	let count = input.chars().count();
	if count > MAX_ANALYZER_INPUT_SCALARS {
		return Err(analyzer_limit("input scalar"));
	}
	Ok(count)
}

/// Checks term.
fn check_term(term: &str) -> FerrosGrynnResult<()> {
	if term.len() > MAX_ANALYZER_TERM_BYTES || term.chars().count() > MAX_ANALYZER_TERM_SCALARS {
		return Err(analyzer_limit("term"));
	}
	Ok(())
}

/// Checks token count.
fn check_token_count(count: usize) -> FerrosGrynnResult<()> {
	if count > MAX_ANALYZER_TOKENS {
		return Err(analyzer_limit("token"));
	}
	Ok(())
}

/// Returns the analyzer limit result.
fn analyzer_limit(resource: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("analyzer {resource} limit exceeded"))
}

/// Returns the token offset result.
fn token_offset(value: Option<&Value>, field: &str) -> FerrosGrynnResult<u32> {
	value
		.and_then(Value::as_number)
		.and_then(Number::as_i64)
		.and_then(|value| u32::try_from(value).ok())
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument(format!(
				"analyzer token {field} must be a non-negative integer",
			))
		})
}

#[cfg(test)]
/// Regression tests for text analyzer behavior.
mod tests {
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};

	/// Verifies bounded filters admit mapper and ngram expansion before copy.
	#[test]
	fn bounded_filters_admit_mapper_and_ngram_expansion_before_copy() {
		use std::{cell::Cell, collections::BTreeMap};

		use super::analyze_builtin_bounded;
		use crate::{
			datastore::{AnalyzerFilter, SchemaAnalyzer},
			gql_status::FerrosGrynnError,
			values::Text,
		};
		let bytes = Cell::new(0_usize);
		let work = Cell::new(0_u64);
		let admit = |allocation: usize, operations: u64| {
			let next_bytes =
				bytes.get().checked_add(allocation).filter(|bytes| *bytes <= 1024 * 1024);
			let next_work = work.get().checked_add(operations).filter(|work| *work <= 2000);
			let (Some(next_bytes), Some(next_work)) = (next_bytes, next_work) else {
				return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
					"test analyzer budget",
					2000,
				));
			};
			bytes.set(next_bytes);
			work.set(next_work);
			Ok(())
		};
		let mapper =
			BTreeMap::from([(Text::from("x"), Some(Text::from("y".repeat(4 * 1024 * 1024))))]);
		let mapper =
			SchemaAnalyzer::new("mapper_budget", [], [AnalyzerFilter::Mapper(mapper)]).assert_ok();
		assert_eq!(
			analyze_builtin_bounded(&mapper, "x", &admit).assert_err().status().code().to_string(),
			"FG-54F13"
		);
		bytes.set(0);
		work.set(0);
		let grams = SchemaAnalyzer::new("gram_budget", [], [AnalyzerFilter::Ngram {
			min: 1,
			max: 1000,
		}])
		.assert_ok();
		assert_eq!(
			analyze_builtin_bounded(&grams, &"a".repeat(1000), &admit)
				.assert_err()
				.status()
				.code()
				.to_string(),
			"FG-54F13"
		);
		bytes.set(0);
		work.set(0);
		let small = SchemaAnalyzer::new("small_budget", [], [AnalyzerFilter::Ngram {
			min: 1,
			max: 2,
		}])
		.assert_ok();
		assert_eq!(
			analyze_builtin_bounded(&small, "ab", &admit).assert_ok(),
			super::analyze_builtin(&small, "ab").assert_ok()
		);
	}

	use std::collections::BTreeMap;

	use super::{analyze_builtin, analyze_function_result};
	use crate::{
		datastore::schema::{AnalyzerFilter, AnalyzerLanguage, AnalyzerTokenizer, SchemaAnalyzer},
		values::{Array, Object, Text, Value},
	};

	/// Verifies combines boundaries and preserves source spans through filters.
	#[test]
	fn combines_boundaries_and_preserves_source_spans_through_filters() {
		let analyzer =
			SchemaAnalyzer::new("words", [AnalyzerTokenizer::Camel, AnalyzerTokenizer::Punct], [
				AnalyzerFilter::Lowercase,
			])
			.assert_ok();
		let tokens = analyze_builtin(&analyzer, "RustGraph!").assert_ok();
		assert_eq!(
			tokens
				.iter()
				.map(|token| (token.term.as_str(), token.start, token.end))
				.collect::<Vec<_>>(),
			vec![("rust", 0, 4), ("graph", 4, 9), ("!", 9, 10)],
		);
	}

	/// Verifies blank is the only implicit whitespace boundary.
	#[test]
	fn blank_is_the_only_implicit_whitespace_boundary() {
		let camel = SchemaAnalyzer::new("camel", [AnalyzerTokenizer::Camel], []).assert_ok();
		let blank = SchemaAnalyzer::new("blank", [AnalyzerTokenizer::Blank], []).assert_ok();
		assert_eq!(
			analyze_builtin(&camel, "lower wordsHere")
				.assert_ok()
				.into_iter()
				.map(|token| token.term)
				.collect::<Vec<_>>(),
			vec![Text::from("lower words"), Text::from("Here")],
		);
		assert_eq!(
			analyze_builtin(&blank, "lower wordsHere")
				.assert_ok()
				.into_iter()
				.map(|token| token.term)
				.collect::<Vec<_>>(),
			vec![Text::from("lower"), Text::from("wordsHere")],
		);
	}

	/// Verifies applies ordered unicode ngram and mapper filters.
	#[test]
	fn applies_ordered_unicode_ngram_and_mapper_filters() {
		let mut mapper = BTreeMap::new();
		mapper.insert(Text::from("ffi"), Some(Text::from("ligature")));
		let analyzer = SchemaAnalyzer::new("compat", [], [
			AnalyzerFilter::Nfkc,
			AnalyzerFilter::Lowercase,
			AnalyzerFilter::EdgeNgram {
				min: 3,
				max: 3,
			},
			AnalyzerFilter::Mapper(mapper),
		])
		.assert_ok();
		let tokens = analyze_builtin(&analyzer, "\u{FB03}eld").assert_ok();
		assert_eq!(tokens.len(), 1);
		assert_eq!(tokens[0].term, "ligature");
		assert_eq!((tokens[0].start, tokens[0].end), (0, 4));
	}

	/// Verifies normalizes nfc and runs every snowball language.
	#[test]
	fn normalizes_nfc_and_runs_every_snowball_language() {
		let nfc = SchemaAnalyzer::new("nfc", [], [AnalyzerFilter::Nfc]).assert_ok();
		let tokens = analyze_builtin(&nfc, "e\u{301}").assert_ok();
		assert_eq!(tokens[0].term, "é");
		assert_eq!((tokens[0].start, tokens[0].end), (0, 2));

		for (language, term) in [
			(AnalyzerLanguage::Arabic, "الكتابات"),
			(AnalyzerLanguage::Danish, "huse"),
			(AnalyzerLanguage::Dutch, "huizen"),
			(AnalyzerLanguage::English, "running"),
			(AnalyzerLanguage::Finnish, "taloissa"),
			(AnalyzerLanguage::French, "maisons"),
			(AnalyzerLanguage::German, "häuser"),
			(AnalyzerLanguage::Greek, "σπιτιών"),
			(AnalyzerLanguage::Hungarian, "házak"),
			(AnalyzerLanguage::Italian, "case"),
			(AnalyzerLanguage::Norwegian, "husene"),
			(AnalyzerLanguage::Portuguese, "casas"),
			(AnalyzerLanguage::Romanian, "caselor"),
			(AnalyzerLanguage::Russian, "машины"),
			(AnalyzerLanguage::Spanish, "casas"),
			(AnalyzerLanguage::Swedish, "husen"),
			(AnalyzerLanguage::Tamil, "மரங்கள்"),
			(AnalyzerLanguage::Turkish, "evler"),
		] {
			let analyzer =
				SchemaAnalyzer::new("stem", [], [AnalyzerFilter::Snowball(language)]).assert_ok();
			let tokens = analyze_builtin(&analyzer, term).assert_ok();
			assert_eq!(tokens.len(), 1);
			assert!(!tokens[0].term.is_empty());
			assert_eq!((tokens[0].start, tokens[0].end), (0, term.chars().count() as u32));
		}
	}

	/// Verifies validates function token shape and scalar ranges.
	#[test]
	fn validates_function_token_shape_and_scalar_ranges() {
		let mut analyzer = SchemaAnalyzer::new("udf", [], []).assert_ok();
		analyzer.function = Some((
			crate::datastore::schema::FunctionId::new(7).assert_some(),
			crate::datastore::schema::SchemaVersion::INITIAL,
		));
		let token = Object::from_iter([
			("term", Value::from("граф")),
			("start", Value::from(0_i64)),
			("end", Value::from(4_i64)),
		]);
		let result = Value::Array(Array::new(vec![Value::Object(token)]));
		let tokens = analyze_function_result(&analyzer, "граф!", &result).assert_ok();
		assert_eq!(tokens[0].term, "граф");
		assert_eq!((tokens[0].start, tokens[0].end), (0, 4));

		assert!(analyze_function_result(&analyzer, "граф!", &Value::from("граф")).is_err());
		for token in [
			Object::from_iter([("term", Value::from("граф")), ("start", Value::from(0_i64))]),
			Object::from_iter([
				("term", Value::from("граф")),
				("start", Value::from(0_i64)),
				("end", Value::from(4_i64)),
				("extra", Value::None),
			]),
			Object::from_iter([
				("term", Value::from("граф")),
				("start", Value::from(4_i64)),
				("end", Value::from(6_i64)),
			]),
			Object::from_iter([
				("term", Value::from("граф")),
				("start", Value::from("zero")),
				("end", Value::from(4_i64)),
			]),
		] {
			let result = Value::Array(Array::new(vec![Value::Object(token)]));
			assert!(analyze_function_result(&analyzer, "граф!", &result).is_err());
		}
	}

	/// Verifies covers class ascii uppercase ngrams and mapper drop.
	#[test]
	fn covers_class_ascii_uppercase_ngrams_and_mapper_drop() {
		let class = SchemaAnalyzer::new("class", [AnalyzerTokenizer::Class], []).assert_ok();
		let terms = analyze_builtin(&class, "abc123!Ж")
			.assert_ok()
			.into_iter()
			.map(|token| token.term)
			.collect::<Vec<_>>();
		assert_eq!(terms, ["abc", "123", "!", "Ж"].map(Text::from));

		let ascii_upper = SchemaAnalyzer::new("ascii_upper", [], [
			AnalyzerFilter::Ascii,
			AnalyzerFilter::Uppercase,
		])
		.assert_ok();
		let tokens = analyze_builtin(&ascii_upper, "Crème").assert_ok();
		assert_eq!(tokens[0].term, "CREME");
		assert_eq!((tokens[0].start, tokens[0].end), (0, 5));

		let ngram = SchemaAnalyzer::new("ngram", [], [AnalyzerFilter::Ngram {
			min: 2,
			max: 2,
		}])
		.assert_ok();
		let tokens = analyze_builtin(&ngram, "rust").assert_ok();
		assert_eq!(tokens.iter().map(|token| token.term.as_str()).collect::<Vec<_>>(), vec![
			"ru", "us", "st"
		],);
		assert!(tokens.iter().all(|token| (token.start, token.end) == (0, 4)));

		let mut mapper = BTreeMap::new();
		mapper.insert(Text::from("drop"), None);
		mapper.insert(Text::from("keep"), Some(Text::from("kept")));
		let mapper =
			SchemaAnalyzer::new("mapper", [AnalyzerTokenizer::Blank], [AnalyzerFilter::Mapper(
				mapper,
			)])
			.assert_ok();
		let tokens = analyze_builtin(&mapper, "drop keep").assert_ok();
		assert_eq!(tokens.len(), 1);
		assert_eq!(tokens[0].term, "kept");
		assert_eq!((tokens[0].start, tokens[0].end), (5, 9));
	}
}
