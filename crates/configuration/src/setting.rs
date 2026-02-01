use crate::setting_builder::SettingBuilder;
use crate::setting_constraint::SettingConstraint;
use crate::{Configuration, Result, SettingValueParser};
use itertools::Itertools;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use crate::config::Config;

pub struct Setting<'a, T /*: ToString*/> {
	pub name: &'a str,
	pub dependency: Option<Box<Setting<'a, T>>>,
	pub parser: Rc<dyn SettingValueParser<T>>,
	pub default_value: T,
	pub constraints: Vec<Box<dyn SettingConstraint<T>>>,
	pub is_dynamic: bool,
	pub is_immutable: bool,
	pub is_internal: bool,
	pub description: Option<String>,
	pub deprecated: bool,
	pub source_location: String,
}

impl<'a, T> Setting<'a, T>
where
	T: 'static,
{
	pub fn new_builder<P: SettingValueParser<T>>(
		name: &'a str,
		parser: P,
		default_value: T,
	) -> SettingBuilder<'a, T, P> {
		SettingBuilder::new(name, parser, default_value)
	}

	pub fn default_value(&self) -> &T {
		&self.default_value
	}

	pub fn parse(&self, value: String) -> T {
		self.parser.parse(&value).expect("REASON")
	}

	pub fn value_to_string(&self, value: &T) -> String {
		self.parser.value_to_string(value)
	}

	// fn solve_default(&self, value: Option<T>, default_value: T) -> T {
	// 	self.parser.solve_default(value, default_value)
	// }

	fn solve_dependency(&self, value: Option<T>, dependency_value: T) -> Result<T> {
		self.parser.solve_dependency(value, dependency_value)
	}

	pub fn validate(&self, value: T, config: &Config) -> Result<()> {
		self.parser.validate(&value)?;
		for constraint in &self.constraints {
			constraint.validate(&value, config)?;
		}
		Ok(())
	}

	pub fn valid_values(&self) -> String {
		let mut desc = self.parser.description().to_string();

		if !self.constraints.is_empty() {
			let mut cc = self.parser.constraint_conjunction().to_string();
			let constraint_desc = self.constraints.iter().map(|c| c.description()).join(" and ");
			cc += &constraint_desc;
			desc += &cc;
		}

		if let Some(dependency) = &self.dependency {
			desc =
				format!("{}. {} from {}", desc, self.parser.solver_description(), dependency.name);
		}

		desc += ".";
		desc
	}

	pub fn description(&self) -> String {
		self.description.clone().unwrap_or(self.to_string())
	}
}

impl<'a, T: 'static> Display for Setting<'a, T> {
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}, {}", &self.name, self.valid_values())
	}
}

impl<'a, T> PartialEq<Self> for Setting<'a, T> {
	fn eq(&self, other: &Self) -> bool {
		self.name == other.name
	}
}

impl<'a, T> Hash for Setting<'a, T> {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.name.hash(state);
	}
}
