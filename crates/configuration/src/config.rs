use std::collections::{BTreeSet, HashMap};
use crate::Configuration;
use crate::setting::Setting;
use crate::settings_declaration::{SettingsDeclaration, SettingsDeclarationDynRef};
use once_cell::sync::Lazy;
use std::iter::{IntoIterator, Iterator};
use std::path::PathBuf;

pub const DEFAULT_CONFIG_FILE_NAME: &str = "fg.conf";
pub const DEFAULT_CONFIG_DIR_NAME: &str = "conf";
pub const STRICT_FAILURE_MESSAGE: Lazy<String> =
	Lazy::new(|| format!(" Cleanup the config or disable '{}' to continue.",));
pub const SUPPORTED_NAMESPACES: [&'static str; 6] =
	["dbms.", "db.", "internal.", "initial.", "fabric.", "gds."];

// pub static DEFAULT_SETTING_CLASSES: Lazy<&[SettingsDeclarationDynRef]> =
// 	Lazy::new(|| inventory::iter::<SettingsDeclarationDynRef>.into_iter().collect());

pub struct Config {}

impl Configuration for Config {
	fn get<T>(&self, setting: &Setting<T>) -> Option<T> {
		todo!()
	}
}

struct Builder {
	settings: BTreeSet<SettingsDeclarationDynRef>,
	settings_value_string: HashMap<String, String>,
	settings_value_object: HashMap<String, >,
	overridden_defaults: HashMap<String, >,
	config_files: Vec<PathBuf>,
	from_config: Config,
	log: Box<dyn InternalLog>,
	expand_commands: bool,
	strict_duplicate_declaration_warning_message: String
}

impl Builder {
	fn override_setting_value<T>(self, setting: String, value: T, setting_values: &mut HashMap<String, T>, force: bool) {
		if !self.settings_value_string.contains_key(&setting) && !self.settings_value_object.contains_key(&setting) {
			setting_values.insert(setting, value);
		} else if force {
			self.log.warn(format!("The {} setting is overridden. Setting value changed from '{}' to '{}'", setting, ))
		}
	}
}