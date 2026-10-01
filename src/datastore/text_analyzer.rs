use icu_normalizer::ComposingNormalizerBorrowed;
use rust_stemmers::{Algorithm, Stemmer};

use super::schema::{AnalyzerFilter, AnalyzerLanguage, AnalyzerTokenizer, SchemaAnalyzer};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Number, Text, Value},
};

const MAX_ANALYZER_INPUT_SCALARS: usize = 1_000_000;
const MAX_ANALYZER_TERM_SCALARS: usize = 4_096;
const MAX_ANALYZER_TERM_BYTES: usize = 8 * 1_024;
const MAX_ANALYZER_TOKENS: usize = 1_000_000;

/// One final analyzer token and the half-open scalar span of its source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnalyzedToken {
	pub(crate) term: Text,
	pub(crate) start: u32,
	pub(crate) end: u32,
}

/// Executes built-in tokenization followed by the analyzer's ordered filters.
///
/// # Errors
///
/// Returns an error when the analyzer uses a UDF or a configured resource
/// limit is exceeded.
pub(crate) fn analyze_builtin(
	analyzer: &SchemaAnalyzer,
	input: &str,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	if analyzer.function().is_some() {
		return Err(FerrosGrynnError::invalid_argument(
			"a function-backed analyzer requires evaluated UDF tokens",
		));
	}
	let tokens = tokenize(input, analyzer.tokenizers())?;
	apply_filters(analyzer, tokens)
}

/// Converts a function result into checked source-spanned tokens, then applies
/// the analyzer's ordered filters.
///
/// # Errors
///
/// Returns an error for an invalid function result, scalar span, term, or
/// resource limit.
pub(crate) fn analyze_function_result(
	analyzer: &SchemaAnalyzer,
	input: &str,
	result: &Value,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	if analyzer.function().is_none() {
		return Err(FerrosGrynnError::invalid_argument(
			"built-in analyzer does not accept UDF token output",
		));
	}
	let scalar_count = bounded_scalar_count(input)?;
	let values = result.as_array().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("analyzer function must return array<object>")
	})?;
	check_token_count(values.len())?;
	let mut tokens = Vec::with_capacity(values.len());
	for value in values {
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
	apply_filters(analyzer, tokens)
}

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
			end: u32::try_from(scalar_count).map_err(|_| analyzer_limit("source span"))?,
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
		start: u32::try_from(start_scalar).map_err(|_| analyzer_limit("source span"))?,
		end: u32::try_from(end_scalar).map_err(|_| analyzer_limit("source span"))?,
	});
	Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScalarClass {
	Whitespace,
	Letter,
	Number,
	Punctuation,
}

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

fn is_punctuation(value: char) -> bool {
	!value.is_whitespace() && !value.is_alphanumeric()
}

fn apply_filters(
	analyzer: &SchemaAnalyzer,
	mut tokens: Vec<AnalyzedToken>,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	for filter in analyzer.filters() {
		tokens = match filter {
			AnalyzerFilter::Nfc => map_terms(tokens, |term| {
				ComposingNormalizerBorrowed::new_nfc().normalize(term).into_owned()
			})?,
			AnalyzerFilter::Nfkc => map_terms(tokens, |term| {
				ComposingNormalizerBorrowed::new_nfkc().normalize(term).into_owned()
			})?,
			AnalyzerFilter::Ascii => map_terms(tokens, deunicode::deunicode)?,
			AnalyzerFilter::Lowercase => map_terms(tokens, str::to_lowercase)?,
			AnalyzerFilter::Uppercase => map_terms(tokens, str::to_uppercase)?,
			AnalyzerFilter::EdgeNgram {
				min,
				max,
			} => ngrams(tokens, usize::from(*min), usize::from(*max), true)?,
			AnalyzerFilter::Ngram {
				min,
				max,
			} => ngrams(tokens, usize::from(*min), usize::from(*max), false)?,
			AnalyzerFilter::Snowball(language) => {
				let stemmer = Stemmer::create(stemmer_algorithm(*language));
				map_terms(tokens, |term| stemmer.stem(&term.to_lowercase()).into_owned())?
			}
			AnalyzerFilter::Mapper(mapper) => tokens
				.into_iter()
				.filter_map(|mut token| match mapper.get(&token.term) {
					Some(Some(replacement)) => {
						token.term = replacement.clone();
						Some(token)
					}
					Some(None) => None,
					None => Some(token),
				})
				.collect(),
		};
		check_token_count(tokens.len())?;
	}
	Ok(tokens)
}

fn map_terms(
	tokens: Vec<AnalyzedToken>,
	mut transform: impl FnMut(&str) -> String,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	let mut output = Vec::with_capacity(tokens.len());
	for mut token in tokens {
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

fn ngrams(
	tokens: Vec<AnalyzedToken>,
	minimum: usize,
	maximum: usize,
	edge_only: bool,
) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
	let mut output = Vec::new();
	for token in tokens {
		let boundaries = token
			.term
			.char_indices()
			.map(|(offset, _)| offset)
			.chain(std::iter::once(token.term.len()))
			.collect::<Vec<_>>();
		let scalar_count = boundaries.len().saturating_sub(1);
		for length in minimum..=maximum.min(scalar_count) {
			let starts = if edge_only {
				0..=0
			} else {
				0..=scalar_count.saturating_sub(length)
			};
			for start in starts {
				check_token_count(output.len().saturating_add(1))?;
				output.push(AnalyzedToken {
					term: Text::from(&token.term[boundaries[start]..boundaries[start + length]]),
					start: token.start,
					end: token.end,
				});
			}
		}
	}
	Ok(output)
}

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

fn bounded_scalar_count(input: &str) -> FerrosGrynnResult<usize> {
	let count = input.chars().count();
	if count > MAX_ANALYZER_INPUT_SCALARS {
		return Err(analyzer_limit("input scalar"));
	}
	Ok(count)
}

fn check_term(term: &str) -> FerrosGrynnResult<()> {
	if term.len() > MAX_ANALYZER_TERM_BYTES || term.chars().count() > MAX_ANALYZER_TERM_SCALARS {
		return Err(analyzer_limit("term"));
	}
	Ok(())
}

fn check_token_count(count: usize) -> FerrosGrynnResult<()> {
	if count > MAX_ANALYZER_TOKENS {
		return Err(analyzer_limit("token"));
	}
	Ok(())
}

fn analyzer_limit(resource: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("analyzer {resource} limit exceeded"))
}

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
mod tests {
	use std::collections::BTreeMap;

	use super::{analyze_builtin, analyze_function_result};
	use crate::{
		datastore::schema::{AnalyzerFilter, AnalyzerLanguage, AnalyzerTokenizer, SchemaAnalyzer},
		values::{Array, Object, Text, Value},
	};

	#[test]
	fn combines_boundaries_and_preserves_source_spans_through_filters() {
		let analyzer =
			SchemaAnalyzer::new("words", [AnalyzerTokenizer::Camel, AnalyzerTokenizer::Punct], [
				AnalyzerFilter::Lowercase,
			])
			.unwrap();
		let tokens = analyze_builtin(&analyzer, "RustGraph!").unwrap();
		assert_eq!(
			tokens
				.iter()
				.map(|token| (token.term.as_str(), token.start, token.end))
				.collect::<Vec<_>>(),
			vec![("rust", 0, 4), ("graph", 4, 9), ("!", 9, 10)],
		);
	}

	#[test]
	fn blank_is_the_only_implicit_whitespace_boundary() {
		let camel = SchemaAnalyzer::new("camel", [AnalyzerTokenizer::Camel], []).unwrap();
		let blank = SchemaAnalyzer::new("blank", [AnalyzerTokenizer::Blank], []).unwrap();
		assert_eq!(
			analyze_builtin(&camel, "lower wordsHere")
				.unwrap()
				.into_iter()
				.map(|token| token.term)
				.collect::<Vec<_>>(),
			vec![Text::from("lower words"), Text::from("Here")],
		);
		assert_eq!(
			analyze_builtin(&blank, "lower wordsHere")
				.unwrap()
				.into_iter()
				.map(|token| token.term)
				.collect::<Vec<_>>(),
			vec![Text::from("lower"), Text::from("wordsHere")],
		);
	}

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
		.unwrap();
		let tokens = analyze_builtin(&analyzer, "\u{FB03}eld").unwrap();
		assert_eq!(tokens.len(), 1);
		assert_eq!(tokens[0].term, "ligature");
		assert_eq!((tokens[0].start, tokens[0].end), (0, 4));
	}

	#[test]
	fn normalizes_nfc_and_runs_every_snowball_language() {
		let nfc = SchemaAnalyzer::new("nfc", [], [AnalyzerFilter::Nfc]).unwrap();
		let tokens = analyze_builtin(&nfc, "e\u{301}").unwrap();
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
				SchemaAnalyzer::new("stem", [], [AnalyzerFilter::Snowball(language)]).unwrap();
			let tokens = analyze_builtin(&analyzer, term).unwrap();
			assert_eq!(tokens.len(), 1);
			assert!(!tokens[0].term.is_empty());
			assert_eq!((tokens[0].start, tokens[0].end), (0, term.chars().count() as u32));
		}
	}

	#[test]
	fn validates_function_token_shape_and_scalar_ranges() {
		let mut analyzer = SchemaAnalyzer::new("udf", [], []).unwrap();
		analyzer.function = Some((
			crate::datastore::schema::FunctionId::new(7).unwrap(),
			crate::datastore::schema::SchemaVersion::INITIAL,
		));
		let token = Object::from_iter([
			("term", Value::from("граф")),
			("start", Value::from(0_i64)),
			("end", Value::from(4_i64)),
		]);
		let result = Value::Array(Array::new(vec![Value::Object(token)]));
		let tokens = analyze_function_result(&analyzer, "граф!", &result).unwrap();
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

	#[test]
	fn covers_class_ascii_uppercase_ngrams_and_mapper_drop() {
		let class = SchemaAnalyzer::new("class", [AnalyzerTokenizer::Class], []).unwrap();
		let terms = analyze_builtin(&class, "abc123!Ж")
			.unwrap()
			.into_iter()
			.map(|token| token.term)
			.collect::<Vec<_>>();
		assert_eq!(terms, ["abc", "123", "!", "Ж"].map(Text::from));

		let ascii_upper = SchemaAnalyzer::new("ascii_upper", [], [
			AnalyzerFilter::Ascii,
			AnalyzerFilter::Uppercase,
		])
		.unwrap();
		let tokens = analyze_builtin(&ascii_upper, "Crème").unwrap();
		assert_eq!(tokens[0].term, "CREME");
		assert_eq!((tokens[0].start, tokens[0].end), (0, 5));

		let ngram = SchemaAnalyzer::new("ngram", [], [AnalyzerFilter::Ngram {
			min: 2,
			max: 2,
		}])
		.unwrap();
		let tokens = analyze_builtin(&ngram, "rust").unwrap();
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
			.unwrap();
		let tokens = analyze_builtin(&mapper, "drop keep").unwrap();
		assert_eq!(tokens.len(), 1);
		assert_eq!(tokens[0].term, "kept");
		assert_eq!((tokens[0].start, tokens[0].end), (5, 9));
	}
}
