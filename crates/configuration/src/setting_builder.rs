use crate::SettingValueParser;
use crate::constraints::SettingConstraint;
use crate::setting::Setting;
use crate::{Error, Result};
use std::rc::Rc;

pub struct SettingBuilder<'a, T, P>
where
	P: SettingValueParser<T> + 'static,
{
	name: &'a str,
	parser: P,
	default_value: T,
	constraints: Vec<Box<dyn SettingConstraint<T>>>,
	is_dynamic: bool,
	is_immutable: bool,
	is_internal: bool,
	dependency: Option<Setting<'a, T>>,
}

impl<'a, T, P> SettingBuilder<'a, T, P>
where
	T: 'static,
	P: SettingValueParser<T> + 'static,
{
	pub fn new(name: &'a str, parser: P, default_value: T) -> Self {
		Self {
			name,
			parser,
			default_value,
			constraints: Vec::new(),
			is_dynamic: false,
			is_immutable: false,
			is_internal: false,
			dependency: None,
		}
	}

	pub fn dynamic(mut self) -> Self {
		self.is_dynamic = true;
		self
	}

	pub fn immutable(mut self) -> Self {
		self.is_immutable = true;
		self
	}

	pub fn internal(mut self) -> Self {
		self.is_internal = true;
		self
	}

	pub fn add_constraint<C>(mut self, constraint: C) -> Self
	where
		C: SettingConstraint<T> + 'static,
	{
		self.constraints.push(Box::new(constraint));
		self
	}

	pub fn set_dependency(mut self, dependency: Setting<'a, T>) -> Self {
		self.dependency = Some(dependency);
		self
	}

	pub fn build(self) -> Result<Setting<'a, T>> {
		if self.is_immutable && self.is_dynamic {
			return Err(Error::DynamicAndImmutable);
		}

		if let Some(dep) = &self.dependency {
			if !dep.is_immutable {
				return Err(Error::DependencyNotImmutable);
			}
		}

		Ok(Setting {
			name: self.name,
			default_value: self.default_value,
			constraints: self.constraints,
			parser: Rc::new(self.parser),
			is_dynamic: self.is_dynamic,
			is_immutable: self.is_immutable,
			is_internal: self.is_internal,
			description: None,
			deprecated: false,
			dependency: self.dependency.map(|s| Box::new(s)),
			source_location: "".to_string(),
		})
	}
}
