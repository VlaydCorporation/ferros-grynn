use crate::helpers::DurationRange;
use crate::{Error, Result};
use humantime::{format_duration, parse_duration};
use itertools::Itertools;
use num::traits::Num;
use std::borrow::Cow;
use std::collections::{BTreeSet, BinaryHeap, HashSet, LinkedList, VecDeque};
use std::hash::Hash;
use std::str::FromStr;
use std::time::Duration;

/// Handling the values associated with a `Setting` object
pub trait SettingValueParser<T> {
	/// Parse a textual representation of a value into a typed object.
	fn parse(&self, value: &str) -> Result<T>;

	fn validate(&self, _value: &T) -> Result<()> {
		Ok(())
	}

	fn description(&self) -> Cow<'static, str>;

	/// String acting as a conjunction when joining the parser description with a constraint description clause.
	fn constraint_conjunction(&self) -> &'static str {
		" that "
	}

	// /// Solving a value against the default value.
	// fn solve_default(&self, value: Option<T>, default_value: T) -> T {
	// 	value.unwrap_or(default_value)
	// }

	/// Callback for settings with a dependency, also referred to as a parent setting.
	/// The default behavior is that when the value for the setting is {@code null}, the final value is taken from the parent.
	/// If the default behavior is changed, the {@link #getSolverDescription()} must be changed as well.
	fn solve_dependency(&self, value: Option<T>, dependency_value: T) -> Result<T> {
		Ok(value.unwrap_or(dependency_value))
	}

	/// Description of the behavior in solve_dependency() fn.
	fn solver_description(&self) -> &'static str {
		"If unset, the value is inherited"
	}

	fn value_to_string(&self, value: &T) -> String;
}

pub(super) mod setting_value_parsers {
	use super::*;
	use crate::helpers;
	use crate::helpers::{DisplayableHashMap, DisplayableHashSet, DisplayableVec};
	use std::collections::HashMap;
	use std::fmt::{Debug, Display};
	use std::marker::PhantomData;
	use std::path::PathBuf;

	const LIST_SEPARATOR: &str = ",";
	const LIST_SEPARATOR_DESCRIPTION: &str = "comma";
	const VALID_TIME_DESCRIPTION: &str =
		"Valid units are: `ns`, `μs`, `ms`, `s`, `m`, `h` and `d`; default unit is `s`";

	//#region --- Simple parsers ---
	#[derive(Debug, Clone, Copy)]
	pub struct StringParser;
	impl SettingValueParser<String> for StringParser {
		fn parse(&self, value: &str) -> Result<String> {
			Ok(value.trim().to_string())
		}
		fn description(&self) -> Cow<'static, str> {
			"a string".into()
		}

		fn value_to_string(&self, value: &String) -> String {
			value.to_string()
		}
	}

	#[derive(Debug, Clone, Copy)]
	pub struct BoolParser;
	impl SettingValueParser<bool> for BoolParser {
		fn parse(&self, value: &str) -> Result<bool> {
			match value.trim().to_lowercase().as_str() {
				"true" => Ok(true),
				"false" => Ok(false),
				_ => Err(Error::InvalidFormat(format!(
					"{} is not a valid boolean value, must be 'true' or 'false'",
					value
				))),
			}
		}
		fn description(&self) -> Cow<'static, str> {
			"a bool".into()
		}

		fn value_to_string(&self, value: &bool) -> String {
			value.to_string()
		}
	}

	#[derive(Debug, Clone, Copy)]
	pub struct DurationParser;
	impl SettingValueParser<Duration> for DurationParser {
		fn parse(&self, value: &str) -> Result<Duration> {
			Ok(parse_duration(&value)?)
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("a duration ({VALID_TIME_DESCRIPTION})"))
		}

		fn value_to_string(&self, value: &Duration) -> String {
			if value.is_zero() {
				"0s".to_string()
			} else {
				format_duration(*value).to_string()
			}
		}
	}

	#[derive(Debug, Clone, Copy)]
	pub struct DurationRangeParser;
	impl SettingValueParser<DurationRange> for DurationRangeParser {
		fn parse(&self, value: &str) -> Result<DurationRange> {
			DurationRange::parse(value)
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("a duration-range <min-max> ({VALID_TIME_DESCRIPTION})"))
		}

		fn value_to_string(&self, value: &DurationRange) -> String {
			value.value_to_string()
		}
	}

	#[derive(Debug, Clone, Copy)]
	pub struct BytesUnitParser;
	impl SettingValueParser<i64> for BytesUnitParser {
		fn parse(&self, value: &str) -> Result<i64> {
			let bytes = io::bytes::parse(value).map_err(|ex| Error::InvalidFormat(ex))?;
			self.validate(&bytes)?;
			Ok(bytes)
		}

		fn validate(&self, value: &i64) -> Result<()> {
			if *value < 0 {
				Err(Error::InvalidFormat(format!(
					"{} is not a valid number of bytes. Must be positive or zero.",
					value
				)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!(
				"a byte size (valid multipliers are {})",
				io::bytes::VALID_MULTIPLIERS
			))
		}

		fn value_to_string(&self, value: &i64) -> String {
			io::bytes::bytes_to_string_without_scientific_notation(*value)
		}
	}

	#[derive(Debug, Clone, Copy)]
	pub struct PathParser;
	impl SettingValueParser<PathBuf> for PathParser {
		fn parse(&self, value: &str) -> Result<PathBuf> {
			Ok(path_clean::clean(value))
		}

		fn validate(&self, value: &PathBuf) -> Result<()> {
			if !value.is_absolute() {
				return Err(Error::Validation(format!(
					"{} is not absolute path.",
					value.display()
				)));
			}
			if *value != path_clean::clean(&value) {
				return Err(Error::Validation(format!(
					"{} is not a normalized path.",
					value.display()
				)));
			}
			Ok(())
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Borrowed("a path")
		}

		fn solve_dependency(
			&self,
			value: Option<PathBuf>,
			dependency_value: PathBuf,
		) -> Result<PathBuf> {
			if !dependency_value.is_absolute() {
				return Err(Error::DependencySolvation(
					"Dependency must be absolute path".to_string(),
				));
			}
			if let Some(value) = value {
				if value.is_absolute() {
					Ok(value)
				} else {
					Ok(dependency_value.join(value))
				}
			} else {
				Ok(dependency_value)
			}
		}

		fn solver_description(&self) -> &'static str {
			"If relative, it is resolved"
		}

		fn value_to_string(&self, value: &PathBuf) -> String {
			value.display().to_string()
		}
	}

	#[derive(Debug, Clone, Copy)]
	pub struct DatabaseNameParser;
	impl SettingValueParser<String> for DatabaseNameParser {
		fn parse(&self, value: &str) -> Result<String> {
			self.validate(&value.to_string())?;
			Ok(value.to_string())
		}

		fn validate(&self, value: &String) -> Result<()> {
			Ok(helpers::validate_external_database_name(&value)?)
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!(
				"a valid database name containing only alphabetic characters, numbers, dots, and dashes with a length between {} and {} characters, starting with an alphabetic character but not with the name `system`",
				helpers::MINIMUM_DATABASE_NAME_LENGTH,
				helpers::MAXIMUM_DATABASE_NAME_LENGTH
			))
		}

		fn value_to_string(&self, value: &String) -> String {
			value.to_string()
		}
	}
	//#endregion --- Simple parsers ---

	//#region --- Numeric parser ---
	#[derive(Debug, Clone, Copy)]
	struct NumericParser<T> {
		_phantom: PhantomData<T>,
	}

	impl<T> NumericParser<T> {
		const fn new() -> Self {
			Self {
				_phantom: PhantomData,
			}
		}
	}

	impl<T> SettingValueParser<T> for NumericParser<T>
	where
		T: FromStr + Default + Num + Display,
		<T as FromStr>::Err: Debug + Display + std::error::Error + 'static,
	{
		fn parse(&self, value: &str) -> Result<T> {
			Ok(value.trim().parse::<T>().map_err(|ex| {
				Error::InvalidFormat(format!("'{}' is not a valid value: {}", value, ex))
			})?)
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("a(n) {}", std::any::type_name::<T>()))
		}

		fn value_to_string(&self, value: &T) -> String {
			value.to_string()
		}
	}
	//#endregion --- Numeric parser ---

	//#region --- Collections parser ---
	#[derive(Debug, Clone, Copy)]
	pub struct CollectionParser<P, T, C>
	where
		P: SettingValueParser<T>,
	{
		parser: P,
		_phantom: PhantomData<(P, T, C)>,
	}

	impl<P, T, C> CollectionParser<P, T, C>
	where
		P: SettingValueParser<T>,
	{
		pub fn new(parser: P) -> Self {
			Self {
				parser,
				_phantom: PhantomData,
			}
		}
	}

	#[doc(hidden)]
	macro_rules! __impl_collections_internal {
    ($t:ident, $($bounds:tt)*) => {
            impl<P, T> SettingValueParser<$t<T>> for CollectionParser<P, T, $t<T>>
            where
                P: SettingValueParser<T>,
                T: $($bounds +)*
            {
                fn parse(&self, value: &str) -> Result<$t<T>> {
                    if value.trim().is_empty() {
                        return Ok($t::new());
                    }
                    value
                        .split(LIST_SEPARATOR)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|s| self.parser.parse(s))
                        .collect()
                }

                fn validate(&self, value: &$t<T>) -> Result<()> {
                    for item in value {
                        self.parser.validate(item)?;
                    }
                    Ok(())
                }

                fn description(&self) -> Cow<'static, str> {
                    Cow::Owned(format!(
                        "a {}-separated {} where each element is {}",
                        LIST_SEPARATOR_DESCRIPTION,
                        stringify!($t),
                        self.parser.description()
                    ))
                }

                fn constraint_conjunction(&self) -> &'static str {
                    ", which "
                }

                fn value_to_string(&self, value: &$t<T>) -> String {
                    value
                        .into_iter()
                        .map(|v| self.parser.value_to_string(v))
                        .join(LIST_SEPARATOR)
                }
            }
        };
    }

	macro_rules! impl_collections {
        // Принимает один или несколько токенов, разделенных запятыми
        ($($entry:tt),+) => {
            // Для каждого токена вызываем внутренний парсер
            $(
                impl_collections!(@parse $entry);
            )*
        };

        // Внутреннее правило для парсинга простого типа (напр., `Vec`)
        (@parse $t:ident) => {
            // Вызываем реализацию без дополнительных ограничений
            __impl_collections_internal!($t,);
        };

        // Внутреннее правило для парсинга типа с ограничениями (напр., `<HashSet: Hash, Eq>`)
        (@parse ($t:ident: $($bounds:tt),+)) => {
            // Вызываем реализацию с указанными ограничениями
            __impl_collections_internal!($t, $($bounds)+);
        };
    }

	impl_collections!(Vec, VecDeque, LinkedList, (HashSet: Hash, Eq), (BTreeSet: Ord), (BinaryHeap: Ord));

	#[derive(Debug, Clone)]
	pub struct MapPattern {
		required_keys: HashSet<String>,
		valid_keys: HashSet<String>,
	}

	impl MapPattern {
		pub fn new(
			required_keys: Option<HashSet<String>>,
			valid_keys: Option<HashSet<String>>,
		) -> Self {
			Self {
				required_keys: required_keys.unwrap_or_default(),
				valid_keys: valid_keys.unwrap_or_default(),
			}
		}
	}

	impl SettingValueParser<HashMap<String, String>> for MapPattern {
		fn parse(&self, value: &str) -> Result<HashMap<String, String>> {
			let setting_map = value
				.split(";")
				.map(|e| {
					let kv = e.split("=").collect_vec();
					if kv.len() != 2 {
						return Err(Error::InvalidFormat(format!(
							"{e} map element does not follow k1=v1 format."
						)));
					}
					let key = kv[0];
					let value = kv[1];

					if !self.valid_keys.is_empty() && !self.valid_keys.contains(key) {
						return Err(Error::InvalidFormat(format!(
							"map element with key {key} is not one of the accepted elements {}.",
							DisplayableHashSet(&self.valid_keys)
						)));
					}

					Ok((key.to_string(), value.to_string()))
				})
				.collect::<Result<HashMap<_, _>>>()?;

			if !self.required_keys.is_empty()
				&& !self.required_keys.iter().all(|key| setting_map.contains_key(key))
			{
				return Err(Error::InvalidFormat(format!(
					"'{value}' map does not contain all of the required keys: {}",
					DisplayableHashSet(&self.required_keys)
				)));
			}

			Ok(setting_map)
		}

		fn description(&self) -> Cow<'static, str> {
			let mut description = "A simple key value map pattern `k1=v1;k2=v2`.".to_string();
			if !self.required_keys.is_empty() {
				description += format!(
					" Required key options are: `{}`",
					DisplayableHashSet(&self.required_keys)
				)
				.as_str();
			}
			if !self.valid_keys.is_empty() {
				description +=
					format!(" Valid key options are: `{}`", DisplayableHashSet(&self.valid_keys))
						.as_str();
			}
			Cow::Owned(description)
		}

		fn value_to_string(&self, value: &HashMap<String, String>) -> String {
			DisplayableHashMap(value).to_string()
		}
	}
	//#endregion --- Collections parser ---

	//#region --- Enum parser ---
	#[derive(Debug, Clone)]
	pub struct EnumParser<T> {
		valid_values: Vec<T>,
	}

	impl<T> EnumParser<T> {
		pub fn new(valid_values: Vec<T>) -> Self {
			Self {
				valid_values,
			}
		}
	}

	impl<T> SettingValueParser<T> for EnumParser<T>
	where
		T: Display + Clone + PartialEq,
	{
		fn parse(&self, value: &str) -> Result<T> {
			let trimmed_value = value.trim();
			for v in &self.valid_values {
				if v.to_string().eq_ignore_ascii_case(trimmed_value) {
					return Ok(v.clone());
				}
			}
			Err(Error::InvalidFormat(format!(
				"'{}' is not one of the valid values: {}",
				value,
				DisplayableVec(&self.valid_values)
			)))
		}

		fn validate(&self, value: &T) -> Result<()> {
			if !self.valid_values.contains(value) {
				Err(Error::Validation(format!(
					"'{}' is not one of the valid values: {}",
					value,
					DisplayableVec(&self.valid_values)
				)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("one of {}", DisplayableVec(&self.valid_values)))
		}

		fn value_to_string(&self, value: &T) -> String {
			value.to_string()
		}
	}
	//#endregion --- Enum parser ---

	//#region --- PARSERS ---
	macro_rules! num_parsers {
        ($(($n:ident, $t:ident)),*) => {
            $(
                pub const $n: NumericParser<$t> = NumericParser::new();
            )*
        };
    }

	pub const STRING: StringParser = StringParser;
	pub const BOOL: BoolParser = BoolParser;
	pub const DURATION: DurationParser = DurationParser;
	pub const DURATION_RANGE: DurationRangeParser = DurationRangeParser;
	pub const BYTES_UNIT: BytesUnitParser = BytesUnitParser;
	pub const PATH: PathParser = PathParser;
	pub const DATABASE_NAME: DatabaseNameParser = DatabaseNameParser;
	num_parsers!(
		(I8, i8),
		(I16, i16),
		(I32, i32),
		(I64, i64),
		(I128, i128),
		(ISIZE, isize),
		(U8, u8),
		(U16, u16),
		(U32, u32),
		(U64, u64),
		(U128, u128),
		(USIZE, usize),
		(F32, f32),
		(F64, f64)
	);
	pub const MAP_PATTERN: once_cell::sync::Lazy<MapPattern> =
		once_cell::sync::Lazy::new(|| MapPattern::new(None, None));

	pub fn vec_of<T, P: SettingValueParser<T>>(parser: P) -> CollectionParser<P, T, Vec<T>> {
		CollectionParser::new(parser)
	}

	pub fn deque_of<T, P: SettingValueParser<T>>(parser: P) -> CollectionParser<P, T, VecDeque<T>> {
		CollectionParser::new(parser)
	}

	pub fn linked_list_of<T, P: SettingValueParser<T>>(
		parser: P,
	) -> CollectionParser<P, T, LinkedList<T>> {
		CollectionParser::new(parser)
	}

	pub fn hash_set_of<T, P: SettingValueParser<T>>(
		parser: P,
	) -> CollectionParser<P, T, HashSet<T>> {
		CollectionParser::new(parser)
	}

	pub fn btree_set_of<T, P: SettingValueParser<T>>(
		parser: P,
	) -> CollectionParser<P, T, BTreeSet<T>> {
		CollectionParser::new(parser)
	}

	pub fn binary_heap_of<T, P: SettingValueParser<T>>(
		parser: P,
	) -> CollectionParser<P, T, BinaryHeap<T>> {
		CollectionParser::new(parser)
	}

	pub trait AllVariants: Sized {
		/// Возвращает срез со всеми вариантами перечисления.
		fn all_variants() -> &'static [Self];
	}

	pub fn of_enum<T>() -> EnumParser<T>
	where
		T: AllVariants + Display + PartialEq + Clone + 'static,
	{
		EnumParser::new(T::all_variants().to_vec())
	}

	pub fn of_partial_enum<T>(values: &[T]) -> EnumParser<T>
	where
		T: Display + PartialEq + Clone,
	{
		EnumParser::new(values.to_vec())
	}

	pub fn set_of_enums<T>() -> CollectionParser<EnumParser<T>, T, HashSet<T>>
	where
		T: AllVariants + Display + Eq + Hash + Clone + 'static,
		EnumParser<T>: SettingValueParser<T>,
	{
		let delegate = of_enum::<T>();
		CollectionParser::new(delegate)
	}

	/// Returns the index of the first non-digit character in `text`.
	/// If all characters are digits, the length of the string is returned.
	fn find_first_non_digit(text: &str) -> Option<usize> {
		text.find(|c: char| !c.is_ascii_digit())
	}

	fn get_byte_unit(
		unit_str: &str,
		num_with_potential_unit: &str,
	) -> Result<io::bytes::ByteUnitKind> {
		let lowercased_unit = unit_str.to_ascii_lowercase();
		match lowercased_unit.as_str() {
			"" | "b" => Ok(io::bytes::ByteUnitKind::Byte),
			"k" => Ok(io::bytes::ByteUnitKind::KibiByte),
			"m" => Ok(io::bytes::ByteUnitKind::MebiByte),
			"g" => Ok(io::bytes::ByteUnitKind::GibiByte),
			_ => Err(Error::InvalidFormat(format!(
				"Illegal unit '{lowercased_unit}' for number '{num_with_potential_unit}'"
			))),
		}
	}

	pub fn parse_u64_with_unit(num_with_potential_unit: String) -> Result<i64> {
		let first_non_digit_index = find_first_non_digit(&num_with_potential_unit);

		let (number_str, unit_str) = if let Some(index) = first_non_digit_index {
			num_with_potential_unit.split_at(index)
		} else {
			(num_with_potential_unit.as_str(), "") // All digits, no unit specified
		};

		let number: i64 = number_str.parse().map_err(|_| {
			Error::InvalidFormat(format!(
				"Invalid number format in '{num_with_potential_unit}', it is not represents a bytes"
			))
		})?;

		let byte_unit_kind = get_byte_unit(unit_str, &num_with_potential_unit)?;
		let byte_unit: &io::bytes::ByteUnit = byte_unit_kind.into();

		Ok(byte_unit.to_bytes(number))
	}
	//#endregion --- PARSERS ---
}
