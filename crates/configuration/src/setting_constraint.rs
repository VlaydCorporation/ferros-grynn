use crate::{Configuration, Error, Result, SettingValueParser};
use std::borrow::Cow;
use std::fmt::Display;
use std::rc::Rc;
use crate::config::Config;

pub trait SettingConstraint<T> {
	fn validate(&self, value: &T, config: &Config) -> Result<()>;

	fn description(&self) -> Cow<'static, str>;

	fn value_to_string(&self, value: &T) -> String;

	fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<T>>);
}

type ValueFormatter<T> = Rc<dyn Fn(&T) -> String>;

pub(super) mod setting_constraints {
	use super::*;
	use crate::helpers::{ChronoUnit, DisplayableVec, Truncatable, is_blank_string};
	use crate::setting::Setting;
	use humantime::format_duration;
	use itertools::Itertools;
	use num::Num;
	use regex::Regex;
	use std::hash::Hash;
	use std::marker::PhantomData;
	use std::path::PathBuf;
	use std::time::Duration;

	pub struct ExceptConstraint {
		forbidden_values: &'static [&'static str],
		formatter: Option<ValueFormatter<String>>,
	}
	impl SettingConstraint<String> for ExceptConstraint {
		fn validate(&self, value: &String, _config: &Config) -> Result<()> {
			if !is_blank_string(value) {
				if self.forbidden_values.contains(&value.as_str()) {
					return Err(Error::ConstraintError(format!("not allowed value is: {value}")));
				}
			}

			Ok(())
		}

		fn description(&self) -> Cow<'static, str> {
			if self.forbidden_values.len() > 1 {
				Cow::Owned(format!(
					"is none of {}",
					DisplayableVec(&self.forbidden_values.iter().collect_vec())
				))
			} else if self.forbidden_values.len() == 1 {
				Cow::Owned(format!("is not `{}`", self.forbidden_values[0]))
			} else {
				Cow::Borrowed("")
			}
		}

		fn value_to_string(&self, value: &String) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<String>>) {
			self.formatter = Some(Rc::new(move |v: &String| parser.value_to_string(v)));
		}
	}

	pub struct MatchesConstraint {
		regex: Regex,
		description: String,
		formatter: Option<ValueFormatter<String>>,
	}
	impl SettingConstraint<String> for MatchesConstraint {
		fn validate(&self, value: &String, _config: &Config) -> Result<()> {
			if !self.regex.is_match(value) {
				Err(Error::ConstraintError(format!(
					"value does not match expression: `{}`{}",
					self.regex, self.description
				)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("matches the pattern `{}`{}", self.regex, self.description))
		}

		fn value_to_string(&self, value: &String) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<String>>) {
			self.formatter = Some(Rc::new(move |v: &String| parser.value_to_string(v)));
		}
	}

	pub struct MinConstraint<T: PartialOrd> {
		min_value: T,
		formatter: Option<ValueFormatter<T>>,
	}
	impl<T: Num + PartialOrd + Display + 'static> SettingConstraint<T> for MinConstraint<T> {
		fn validate(&self, value: &T, _config: &Config) -> Result<()> {
			if *value < self.min_value {
				Err(Error::ConstraintError(format!(
					"minimum allowed value is {}",
					self.value_to_string(value)
				)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("is minimum `{}`", self.value_to_string(&self.min_value)))
		}

		fn value_to_string(&self, value: &T) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<T>>) {
			self.formatter = Some(Rc::new(move |v: &T| parser.value_to_string(v)));
		}
	}

	pub struct MaxConstraint<T: PartialOrd> {
		max_value: T,
		formatter: Option<ValueFormatter<T>>,
	}
	impl<T: Num + PartialOrd + Display + 'static> SettingConstraint<T> for MaxConstraint<T> {
		fn validate(&self, value: &T, _config: &Config) -> Result<()> {
			if *value > self.max_value {
				Err(Error::ConstraintError(format!(
					"maximum allowed value is {}",
					self.value_to_string(value)
				)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("is maximum `{}`", self.value_to_string(&self.max_value)))
		}

		fn value_to_string(&self, value: &T) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<T>>) {
			self.formatter = Some(Rc::new(move |v: &T| parser.value_to_string(v)));
		}
	}

	pub struct RangeConstraint<T: PartialOrd> {
		min: MinConstraint<T>,
		max: MaxConstraint<T>,
		formatter: Option<ValueFormatter<T>>,
	}
	impl<T: Num + PartialOrd + Display + 'static> SettingConstraint<T> for RangeConstraint<T> {
		fn validate(&self, value: &T, config: &Config) -> Result<()> {
			self.min.validate(value, config)?;
			self.max.validate(value, config)?;
			Ok(())
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!(
				"is in the range `{}` to `{}`",
				self.value_to_string(&self.min.min_value),
				self.value_to_string(&self.max.max_value)
			))
		}

		fn value_to_string(&self, value: &T) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<T>>) {
			self.formatter = Some(Rc::new(move |v: &T| parser.value_to_string(v)));
		}
	}

	pub struct IsConstraint<T: PartialEq> {
		expected_value: T,
		formatter: Option<ValueFormatter<T>>,
	}
	impl<T: PartialEq + Display + 'static> SettingConstraint<T> for IsConstraint<T> {
		fn validate(&self, value: &T, config: &Config) -> Result<()> {
			if !value.eq(&self.expected_value) {
				Err(Error::ConstraintError(format!("is not `{}`", self.value_to_string(value))))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("is `{}`", self.value_to_string(&self.expected_value)))
		}

		fn value_to_string(&self, value: &T) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<T>>) {
			self.formatter = Some(Rc::new(move |v: &T| parser.value_to_string(v)));
		}
	}

	pub struct AnyConstraint<T> {
		constraints: Vec<Box<dyn SettingConstraint<T>>>,
		formatter: Option<ValueFormatter<T>>,
	}
	impl<T: Display + 'static> SettingConstraint<T> for AnyConstraint<T> {
		fn validate(&self, value: &T, config: &Config) -> Result<()> {
			for constraint in &self.constraints {
				if constraint.validate(value, config).is_ok() {
					return Ok(());
				}
			}
			Err(Error::ConstraintError(format!("does not fulfill any of: {}", self.description())))
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(self.constraints.iter().map(|c| c.description()).join(" or "))
		}

		fn value_to_string(&self, value: &T) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<T>>) {
			self.constraints.iter_mut().for_each(|c| c.set_parser(parser.clone()));
			self.formatter = Some(Rc::new(move |v: &T| parser.value_to_string(v)));
		}
	}

	pub struct PowerOfTwoConstraint {
		formatter: Option<ValueFormatter<u64>>,
	}
	impl SettingConstraint<u64> for PowerOfTwoConstraint {
		fn validate(&self, value: &u64, config: &Config) -> Result<()> {
			if !value.is_power_of_two() {
				Err(Error::ConstraintError(format!(
					"only power of 2 values allowed, but value is {}",
					value
				)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Borrowed("is power of 2")
		}

		fn value_to_string(&self, value: &u64) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<u64>>) {
			self.formatter = Some(Rc::new(move |v: &u64| parser.value_to_string(v)));
		}
	}

	pub struct SizeConstraint<T> {
		size: usize,
		formatter: Option<ValueFormatter<Vec<T>>>,
	}
	impl<T: Display + 'static> SettingConstraint<Vec<T>> for SizeConstraint<T> {
		fn validate(&self, value: &Vec<T>, config: &Config) -> Result<()> {
			if value.len() != self.size {
				Err(Error::ConstraintError(format!("needs to be of size {}", self.size)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("is of size `{}`", self.size))
		}

		fn value_to_string(&self, value: &Vec<T>) -> String {
			self.formatter
				.as_ref()
				.map(|f| f(value))
				.unwrap_or_else(|| DisplayableVec(value).to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<Vec<T>>>) {
			self.formatter = Some(Rc::new(move |v: &Vec<T>| parser.value_to_string(v)));
		}
	}

	pub struct MinSizeConstraint<T> {
		min_size: usize,
		formatter: Option<ValueFormatter<Vec<T>>>,
	}
	impl<T: Display + 'static> SettingConstraint<Vec<T>> for MinSizeConstraint<T> {
		fn validate(&self, value: &Vec<T>, config: &Config) -> Result<()> {
			if value.len() <= self.min_size {
				Err(Error::ConstraintError(format!(
					"needs to be greater than size of {}",
					self.min_size
				)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("has minimum size `{}`", self.min_size))
		}

		fn value_to_string(&self, value: &Vec<T>) -> String {
			self.formatter
				.as_ref()
				.map(|f| f(value))
				.unwrap_or_else(|| DisplayableVec(value).to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<Vec<T>>>) {
			self.formatter = Some(Rc::new(move |v: &Vec<T>| parser.value_to_string(v)));
		}
	}

	pub struct NoDuplicatesConstraint<T> {
		formatter: Option<ValueFormatter<Vec<T>>>,
	}
	impl<T: Display + Eq + Hash + 'static> SettingConstraint<Vec<T>> for NoDuplicatesConstraint<T> {
		fn validate(&self, value: &Vec<T>, config: &Config) -> Result<()> {
			if value.iter().duplicates().count() > 0 {
				Err(Error::ConstraintError(format!(
					"items should not have duplicates: {}",
					self.value_to_string(value)
				)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Borrowed("contains no duplicate items")
		}

		fn value_to_string(&self, value: &Vec<T>) -> String {
			self.formatter
				.as_ref()
				.map(|f| f(value))
				.unwrap_or_else(|| DisplayableVec(value).to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<Vec<T>>>) {
			self.formatter = Some(Rc::new(move |v: &Vec<T>| parser.value_to_string(v)));
		}
	}

	pub struct AbsolutePathConstraint {
		formatter: Option<ValueFormatter<PathBuf>>,
	}
	impl SettingConstraint<PathBuf> for AbsolutePathConstraint {
		fn validate(&self, value: &PathBuf, config: &Config) -> Result<()> {
			if !value.is_absolute() {
				Err(Error::ConstraintError(format!(
					"{} is not an absolute path.",
					self.value_to_string(value)
				)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Borrowed("is absolute path")
		}

		fn value_to_string(&self, value: &PathBuf) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.display().to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<PathBuf>>) {
			self.formatter = Some(Rc::new(move |v: &PathBuf| parser.value_to_string(v)));
		}
	}

	/// Firstly, try condition validate. If it's ok, do if_constraint validation,
	/// else do else_constraint validation.
	pub struct DependencyConstraint<'a, T, U: Display> {
		if_constraint: Box<dyn SettingConstraint<T>>,
		else_constraint: Box<dyn SettingConstraint<T>>,
		dependency: Setting<'a, U>,
		condition: Box<dyn SettingConstraint<U>>,
		formatter: Option<ValueFormatter<T>>,
	}
	impl<'a, T: Display + 'static, U: Display> SettingConstraint<T> for DependencyConstraint<'a, T, U> {
		fn validate(&self, value: &T, config: &Config) -> Result<()> {
			let dep_value: U = config.get(&self.dependency)?;
			if self.condition.validate(&dep_value, config).is_ok() {
				self.if_constraint.validate(value, config)
			} else {
				let _ = self.else_constraint.validate(value, config)?;
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!(
				"depends on {}. If {} {} then it {} otherwise it {}",
				self.dependency.name,
				self.dependency.name,
				self.condition.description(),
				self.if_constraint.description(),
				self.else_constraint.description()
			))
		}

		fn value_to_string(&self, value: &T) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<T>>) {
			self.if_constraint.set_parser(parser.clone());
			self.else_constraint.set_parser(parser.clone());
			self.condition.set_parser(self.dependency.parser.clone());
			self.formatter = Some(Rc::new(move |v: &T| parser.value_to_string(v)));
		}
	}

	pub struct UnconstrainedConstraint<T> {
		_phantom: PhantomData<T>,
		formatter: Option<ValueFormatter<T>>,
	}
	impl<T: Display + 'static> SettingConstraint<T> for UnconstrainedConstraint<T> {
		fn validate(&self, _value: &T, _config: &Config) -> Result<()> {
			Ok(())
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Borrowed("is unconstrained")
		}

		fn value_to_string(&self, value: &T) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<T>>) {
			self.formatter = Some(Rc::new(move |v: &T| parser.value_to_string(v)));
		}
	}

	pub struct GTEConstraint<'a, T: Num + PartialOrd + Display> {
		other: Setting<'a, T>,
		formatter: Option<ValueFormatter<T>>,
	}
	impl<'a, T: Num + PartialOrd + Display + 'static> SettingConstraint<T> for GTEConstraint<'a, T> {
		fn validate(&self, value: &T, config: &Config) -> Result<()> {
			let other_value = config.get(&self.other)?;
			if *value < other_value {
				Err(Error::ConstraintError(format!(
					"{}; was {}, which is not more than or equal to {} from {}",
					self.description(),
					value,
					other_value,
					self.other.name
				)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("must be set greater than or equal to value of {}", self.other.name))
		}

		fn value_to_string(&self, value: &T) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<T>>) {
			self.formatter = Some(Rc::new(move |v: &T| parser.value_to_string(v)));
		}
	}

	pub struct LTEConstraint<'a, T: Num + PartialOrd + Display> {
		other: Setting<'a, T>,
		formatter: Option<ValueFormatter<T>>,
	}
	impl<'a, T: Num + PartialOrd + Display + 'static> SettingConstraint<T> for LTEConstraint<'a, T> {
		fn validate(&self, value: &T, config: &Config) -> Result<()> {
			let other_value = config.get(&self.other)?;
			if *value > other_value {
				Err(Error::ConstraintError(format!(
					"{}; was {}, which is not less than or equal to {} from {}",
					self.description(),
					value,
					other_value,
					self.other.name
				)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("must be set less than or equal to value of {}", self.other.name))
		}

		fn value_to_string(&self, value: &T) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| value.to_string())
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<T>>) {
			self.formatter = Some(Rc::new(move |v: &T| parser.value_to_string(v)));
		}
	}

	pub struct ResolutionConstraint {
		resolution: ChronoUnit,
		formatter: Option<ValueFormatter<Duration>>,
	}
	impl SettingConstraint<Duration> for ResolutionConstraint {
		fn validate(&self, value: &Duration, config: &Config) -> Result<()> {
			if value.truncated_to(&self.resolution) != *value {
				Err(Error::ConstraintError(format!(
					"minimum allowed resolution is {} but was {}",
					self.resolution,
					value.resolution()
				)))
			} else {
				Ok(())
			}
		}

		fn description(&self) -> Cow<'static, str> {
			Cow::Owned(format!("has minimum resolution of {}", self.resolution))
		}

		fn value_to_string(&self, value: &Duration) -> String {
			self.formatter.as_ref().map(|f| f(value)).unwrap_or_else(|| {
				if value.is_zero() {
					"0s".to_string()
				} else {
					format_duration(*value).to_string()
				}
			})
		}

		fn set_parser(&mut self, parser: Rc<dyn SettingValueParser<Duration>>) {
			self.formatter = Some(Rc::new(move |v: &Duration| parser.value_to_string(v)));
		}
	}

	pub fn except(forbidden_values: &'static [&'static str]) -> ExceptConstraint {
		ExceptConstraint {
			forbidden_values,
			formatter: None,
		}
	}

	pub fn matches(regex: Regex, description: Option<String>) -> MatchesConstraint {
		MatchesConstraint {
			regex,
			description: description.map(|s| format!(" ({s})")).unwrap_or_default(),
			formatter: None,
		}
	}

	pub fn min<T: PartialOrd>(min_value: T) -> MinConstraint<T> {
		MinConstraint {
			min_value,
			formatter: None,
		}
	}

	pub fn max<T: PartialOrd>(max_value: T) -> MaxConstraint<T> {
		MaxConstraint {
			max_value,
			formatter: None,
		}
	}

	pub fn range<T: PartialOrd>(min_value: T, max_value: T) -> RangeConstraint<T> {
		RangeConstraint {
			min: min(min_value),
			max: max(max_value),
			formatter: None,
		}
	}

	pub fn is<T: PartialEq>(expected_value: T) -> IsConstraint<T> {
		IsConstraint {
			expected_value,
			formatter: None,
		}
	}

	pub fn any<T: Display>(constraints: Vec<Box<dyn SettingConstraint<T>>>) -> AnyConstraint<T> {
		AnyConstraint {
			constraints,
			formatter: None,
		}
	}

	pub const POWER_OF_2: PowerOfTwoConstraint = PowerOfTwoConstraint {
		formatter: None,
	};

	pub fn size<T: Display>(size: usize) -> SizeConstraint<T> {
		SizeConstraint {
			size,
			formatter: None,
		}
	}

	pub fn min_size<T: Display>(min_size: usize) -> MinSizeConstraint<T> {
		MinSizeConstraint {
			min_size,
			formatter: None,
		}
	}

	pub fn no_duplicates<T: Display + Eq + Hash>() -> NoDuplicatesConstraint<T> {
		NoDuplicatesConstraint {
			formatter: None,
		}
	}

	pub const ABSOLUTE_PATH: AbsolutePathConstraint = AbsolutePathConstraint {
		formatter: None,
	};

	pub fn dependency<T, U: Display>(
		if_constraint: Box<dyn SettingConstraint<T>>,
		else_constraint: Box<dyn SettingConstraint<T>>,
		dependency: Setting<U>,
		condition: Box<dyn SettingConstraint<U>>,
	) -> DependencyConstraint<T, U> {
		DependencyConstraint {
			if_constraint,
			else_constraint,
			dependency,
			condition,
			formatter: None,
		}
	}

	pub fn unconstrained<T>() -> UnconstrainedConstraint<T> {
		UnconstrainedConstraint {
			_phantom: PhantomData,
			formatter: None,
		}
	}

	pub fn greater_than_or_equal<T: Num + PartialOrd + Display>(
		other: Setting<T>,
	) -> GTEConstraint<T> {
		GTEConstraint {
			other,
			formatter: None,
		}
	}

	pub fn less_than_or_equal<T: Num + PartialOrd + Display>(
		other: Setting<T>,
	) -> LTEConstraint<T> {
		LTEConstraint {
			other,
			formatter: None,
		}
	}

	pub fn resolution(resolution: ChronoUnit) -> ResolutionConstraint {
		ResolutionConstraint {
			resolution,
			formatter: None
		}
	}
}
