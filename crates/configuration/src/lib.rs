mod config;
mod error;
mod graph_db_settings;
mod helpers;
mod setting;
mod setting_builder;
mod setting_constraint;
mod setting_value_parser;
mod settings_declaration;

use crate::setting::Setting;
pub use error::*;
pub use setting_value_parser::SettingValueParser;

pub mod parsers {
	use super::setting_value_parser::setting_value_parsers;

	pub use super::setting_value_parser::SettingValueParser;

	pub use setting_value_parsers::{
		BOOL, BYTES_UNIT, DATABASE_NAME, DURATION, DURATION_RANGE, F32, F64, I8, I16, I32, I64,
		I128, ISIZE, MAP_PATTERN, PATH, STRING, U8, U16, U32, U64, U128, USIZE,
	};
	pub use setting_value_parsers::{
		binary_heap_of, btree_set_of, deque_of, hash_set_of, linked_list_of, vec_of,
	};

	pub mod enums {
		use super::*;
		pub use setting_value_parsers::{AllVariants, of_enum, of_partial_enum, set_of_enums};
	}

	pub use setting_value_parsers::parse_u64_with_unit;
}

pub mod constraints {
	use super::setting_constraint::setting_constraints;

	pub use super::setting_constraint::SettingConstraint;

	pub use setting_constraints::{
		ABSOLUTE_PATH, POWER_OF_2, any, dependency, except, greater_than_or_equal, is,
		less_than_or_equal, matches, max, min, min_size, no_duplicates, range, resolution, size,
		unconstrained,
	};
}

pub trait Configuration {
	fn get<T>(&self, setting: &Setting<T>) -> Option<T>;
}

struct EmptyConfig;
impl Configuration for EmptyConfig {
	fn get<T>(&self, _setting: &Setting<T>) -> Option<T> {
		None
	}
}

pub const EMPTY_CONFIG: EmptyConfig = EmptyConfig;
