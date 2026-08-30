#![allow(
	clippy::pattern_type_mismatch,
	reason = "status classification matching intentionally uses Rust match ergonomics"
)]
#![allow(
	clippy::impl_trait_in_params,
	reason = "using `impl` in the params makes it easier to generate code in a \
	          `define_status_codes` macro."
)]

mod code;
mod definitions;
mod object;
mod taxonomy;

pub use code::{StatusCode, StatusCodeParseError};
pub use definitions::Status;
pub use object::{InvalidSeverity, StatusObject};
pub use taxonomy::{
	Condition,
	Domain,
	ErrorCategory,
	ErrorClassification,
	NotificationClassification,
	StatusDefinition,
	StatusKind,
};

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_and_formats_status_code() {
		let code = "FG-22N28".parse::<StatusCode>().unwrap();
		assert_eq!(code.body(), "22N28");
		assert_eq!(code.to_string(), "FG-22N28");
		assert!("22N28".parse::<StatusCode>().is_err());
	}

	#[test]
	fn named_parameter_may_be_used_twice() {
		let status = Status::conversion_error("42", "integer");
		assert_eq!(
			status.message().unwrap(),
			"Cannot convert '42' to integer; the rejected value was '42'."
		);
		assert_eq!(status.parameters().len(), 2);
	}

	#[test]
	fn status_object_builds_full_description() {
		let status = StatusObject::new(Status::division_by_zero());
		assert_eq!(status.to_string(), "FG-22012: error: data exception - division by zero");
	}

	#[test]
	fn value_and_graph_limits_have_structured_domains() {
		let value = StatusObject::new(Status::invalid_text_boundary(1, 4));
		assert_eq!(value.code().to_string(), "FG-22F05");
		assert_eq!(value.domain(), Domain::Value);
		assert_eq!(
			value.message().as_deref(),
			Some("Byte index 1 is not a UTF-8 character boundary for a string of 4 bytes."),
		);

		let graph = StatusObject::new(Status::graph_capacity_exceeded("node"));
		assert_eq!(graph.code().to_string(), "FG-54F03");
		assert_eq!(graph.condition(), Condition::ProgramLimitExceeded);
		assert_eq!(graph.domain(), Domain::Graph);

		let columnar = StatusObject::new(Status::columnar_type_conflict(7, "integer", "string"));
		assert_eq!(columnar.code().to_string(), "FG-22F16");
		assert_eq!(columnar.domain(), Domain::Value);

		let dictionary = StatusObject::new(Status::property_dictionary_capacity_exceeded());
		assert_eq!(dictionary.code().to_string(), "FG-54F04");
		assert_eq!(dictionary.condition(), Condition::ProgramLimitExceeded);
		assert_eq!(dictionary.domain(), Domain::Value);

		let storage = StatusObject::new(Status::unknown_storage_type_tag(0x14, 9));
		assert_eq!(storage.code().to_string(), "FG-22S01");
		assert_eq!(storage.condition(), Condition::DataException);
		assert_eq!(storage.domain(), Domain::Storage);

		let storage_limit =
			StatusObject::new(Status::storage_decode_limit_exceeded("nesting depth", 64, 65));
		assert_eq!(storage_limit.code().to_string(), "FG-54S01");
		assert_eq!(storage_limit.condition(), Condition::ProgramLimitExceeded);
		assert_eq!(storage_limit.domain(), Domain::Storage);

		let invalid_page = StatusObject::new(Status::invalid_storage_page(7, "bad checksum"));
		assert_eq!(invalid_page.code().to_string(), "FG-22S08");
		assert_eq!(invalid_page.domain(), Domain::Storage);

		let direct_io = StatusObject::new(Status::direct_io_unavailable("unsupported filesystem"));
		assert_eq!(direct_io.code().to_string(), "FG-51I01");
		assert_eq!(direct_io.condition(), Condition::SystemConfigurationOrOperationException);
		assert_eq!(direct_io.domain(), Domain::Io);
	}
}
