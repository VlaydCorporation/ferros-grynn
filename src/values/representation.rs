pub mod reprs {
	use crate::values::representation::{ValueGroup, ValueKind, ValueRepresentation};

	// Array

	pub const UNKNOWN_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Unknown,
		group: ValueGroup::Unknown,
		can_create_array: false,
	};

	pub const ANYTHING_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Anything,
		group: ValueGroup::Anything,
		can_create_array: false,
	};

	pub const ZONED_DATE_TIME_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::ZonedDateTimeArray,
		group: ValueGroup::ZonedDateTimeArray,
		can_create_array: false,
	};

	pub const LOCAL_DATE_TIME_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::LocalDateTimeArray,
		group: ValueGroup::LocalDateTimeArray,
		can_create_array: false,
	};

	pub const DATE_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::DateArray,
		group: ValueGroup::DateArray,
		can_create_array: false,
	};

	pub const ZONED_TIME_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::ZonedTimeArray,
		group: ValueGroup::ZonedTimeArray,
		can_create_array: false,
	};

	pub const LOCAL_TIME_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::LocalTimeArray,
		group: ValueGroup::LocalTimeArray,
		can_create_array: false,
	};

	pub const DURATION_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::DurationArray,
		group: ValueGroup::DurationArray,
		can_create_array: false,
	};

	pub const TEXT_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::TextArray,
		group: ValueGroup::TextArray,
		can_create_array: false,
	};

	pub const BOOL_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::BoolArray,
		group: ValueGroup::BoolArray,
		can_create_array: false,
	};

	pub const INT64_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Int64Array,
		group: ValueGroup::NumberArray,
		can_create_array: false,
	};

	pub const INT32_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Int32Array,
		group: ValueGroup::NumberArray,
		can_create_array: false,
	};

	pub const INT16_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Int16Array,
		group: ValueGroup::NumberArray,
		can_create_array: false,
	};

	pub const INT8_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Int8Array,
		group: ValueGroup::NumberArray,
		can_create_array: false,
	};

	pub const UINT64_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Uint64Array,
		group: ValueGroup::NumberArray,
		can_create_array: false,
	};

	pub const UINT32_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Uint32Array,
		group: ValueGroup::NumberArray,
		can_create_array: false,
	};

	pub const UINT16_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Uint16Array,
		group: ValueGroup::NumberArray,
		can_create_array: false,
	};

	pub const UINT8_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Uint8Array,
		group: ValueGroup::NumberArray,
		can_create_array: false,
	};

	pub const FLOAT64_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Float64Array,
		group: ValueGroup::NumberArray,
		can_create_array: false,
	};

	pub const FLOAT16_ARRAY_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Float64Array,
		group: ValueGroup::NumberArray,
		can_create_array: false,
	};

	// Scalar

	pub const ZONED_DATE_TIME_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::ZonedDateTime,
		group: ValueGroup::ZonedDateTime,
		can_create_array: true,
	};

	pub const LOCAL_DATE_TIME_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::LocalDateTime,
		group: ValueGroup::LocalDateTime,
		can_create_array: true,
	};

	pub const DATE_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Date,
		group: ValueGroup::Date,
		can_create_array: true,
	};

	pub const ZONED_TIME_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::ZonedTime,
		group: ValueGroup::ZonedTime,
		can_create_array: true,
	};

	pub const LOCAL_TIME_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::LocalTime,
		group: ValueGroup::LocalTime,
		can_create_array: true,
	};

	pub const DURATION_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Duration,
		group: ValueGroup::Duration,
		can_create_array: true,
	};

	pub const UTF16_TEXT_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Utf16Text,
		group: ValueGroup::Text,
		can_create_array: true,
	};

	pub const UTF8_TEXT_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Utf8Text,
		group: ValueGroup::Text,
		can_create_array: true,
	};

	pub const BOOL_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Bool,
		group: ValueGroup::Bool,
		can_create_array: true,
	};

	pub const INT64_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Int64,
		group: ValueGroup::Number,
		can_create_array: true,
	};

	pub const INT32_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Int32,
		group: ValueGroup::Number,
		can_create_array: true,
	};

	pub const INT16_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Int16,
		group: ValueGroup::Number,
		can_create_array: true,
	};

	pub const INT8_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Int8,
		group: ValueGroup::Number,
		can_create_array: true,
	};

	pub const UINT64_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Uint64,
		group: ValueGroup::Number,
		can_create_array: true,
	};

	pub const UINT32_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Uint32,
		group: ValueGroup::Number,
		can_create_array: true,
	};

	pub const UINT16_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Uint16,
		group: ValueGroup::Number,
		can_create_array: true,
	};

	pub const UINT8_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Uint8,
		group: ValueGroup::Number,
		can_create_array: true,
	};

	pub const FLOAT64_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Float64,
		group: ValueGroup::Number,
		can_create_array: true,
	};

	pub const FLOAT32_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Float32,
		group: ValueGroup::Number,
		can_create_array: true,
	};

	pub const NULL_REPR: ValueRepresentation = ValueRepresentation {
		kind: ValueKind::Null,
		group: ValueGroup::Null,
		can_create_array: false,
	};
}

use reprs::*;
use crate::values::sequence::SequenceValue;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ValueRepresentation {
	kind: ValueKind,
	group: ValueGroup,
	can_create_array: bool,
}

impl ValueRepresentation {
	pub fn can_create_array(&self) -> bool {
		self.can_create_array
	}

	pub fn group(&self) -> ValueGroup {
		self.group
	}

	pub fn coerce(&self, other: ValueRepresentation) -> ValueRepresentation {
		match (self.kind, other.kind) {
			(ValueKind::Unknown, _) => UNKNOWN_REPR,
			(ValueKind::Anything, _) => other,

			(ValueKind::Utf16Text, other) => match other {
				ValueKind::Utf8Text | ValueKind::Utf16Text | ValueKind::Anything => UTF16_TEXT_REPR,
				_ => UNKNOWN_REPR,
			},
			(ValueKind::Utf8Text, other) => match other {
				ValueKind::Utf8Text | ValueKind::Anything => UTF8_TEXT_REPR,
				ValueKind::Utf16Text => UTF16_TEXT_REPR,
				_ => UNKNOWN_REPR,
			},
			(ValueKind::Int64, other) => match other {
				ValueKind::Int8
				| ValueKind::Int16
				| ValueKind::Int32
				| ValueKind::Int64
				| ValueKind::Anything => *self,
				ValueKind::Float32 | ValueKind::Float64 => FLOAT64_REPR,
				_ => UNKNOWN_REPR,
			},
			(ValueKind::Int32, other) => match other {
				ValueKind::Int8 | ValueKind::Int16 | ValueKind::Int32 | ValueKind::Anything => {
					*self
				}
				ValueKind::Int64 => INT64_REPR,
				ValueKind::Float32 | ValueKind::Float64 => FLOAT64_REPR,
				_ => UNKNOWN_REPR,
			},
			(ValueKind::Int16, other) => match other {
				ValueKind::Int8 | ValueKind::Int16 | ValueKind::Anything => *self,
				ValueKind::Int32 => INT32_REPR,
				ValueKind::Int64 => INT64_REPR,
				ValueKind::Float32 => FLOAT32_REPR,
				ValueKind::Float64 => FLOAT64_REPR,
				_ => UNKNOWN_REPR,
			},
			(ValueKind::Int8, other) => match other {
				ValueKind::Int8 | ValueKind::Anything => *self,
				ValueKind::Int16 => INT16_REPR,
				ValueKind::Int32 => INT32_REPR,
				ValueKind::Int64 => INT64_REPR,
				ValueKind::Float32 => FLOAT32_REPR,
				ValueKind::Float64 => FLOAT64_REPR,
				_ => UNKNOWN_REPR,
			},
			(ValueKind::Uint64, other) => match other {
				ValueKind::Uint8
				| ValueKind::Uint16
				| ValueKind::Uint32
				| ValueKind::Uint64
				| ValueKind::Anything => *self,
				ValueKind::Float32 | ValueKind::Float64 => FLOAT64_REPR,
				_ => UNKNOWN_REPR,
			},
			(ValueKind::Uint32, other) => match other {
				ValueKind::Uint8 | ValueKind::Uint16 | ValueKind::Uint32 | ValueKind::Anything => {
					*self
				}
				ValueKind::Int64 => INT64_REPR,
				ValueKind::Float32 | ValueKind::Float64 => FLOAT64_REPR,
				_ => UNKNOWN_REPR,
			},
			(ValueKind::Uint16, other) => match other {
				ValueKind::Uint8 | ValueKind::Uint16 | ValueKind::Anything => *self,
				ValueKind::Uint32 => INT32_REPR,
				ValueKind::Uint64 => INT64_REPR,
				ValueKind::Float32 => FLOAT32_REPR,
				ValueKind::Float64 => FLOAT64_REPR,
				_ => UNKNOWN_REPR,
			},
			(ValueKind::Uint8, other) => match other {
				ValueKind::Uint8 | ValueKind::Anything => *self,
				ValueKind::Uint16 => INT16_REPR,
				ValueKind::Uint32 => INT32_REPR,
				ValueKind::Uint64 => INT64_REPR,
				ValueKind::Float32 => FLOAT32_REPR,
				ValueKind::Float64 => FLOAT64_REPR,
				_ => UNKNOWN_REPR,
			},
			(ValueKind::Float64, other) => match other {
				ValueKind::Int8
				| ValueKind::Int16
				| ValueKind::Int32
				| ValueKind::Int64
				| ValueKind::Float32
				| ValueKind::Float64
				| ValueKind::Anything => *self,
				_ => UNKNOWN_REPR,
			},
			(ValueKind::Float32, other) => match other {
				ValueKind::Int8
				| ValueKind::Int16
				| ValueKind::Float32
				| ValueKind::Anything => *self,
				ValueKind::Int32
				| ValueKind::Int64
				| ValueKind::Float64 => FLOAT64_REPR,
				_ => UNKNOWN_REPR,
			},
			(_, _) => {
				if self.group == other.group || other.group == ValueGroup::Anything {
					*self
				} else if self.group == ValueGroup::Anything {
					other
				} else {
					UNKNOWN_REPR
				}
			}
		}
	}

	pub fn array_of(&self, values: &impl SequenceValue) -> Result<ArrayValue> {
		match self.kind {
			ValueKind::ZonedDateTime => {

			}
		}
	}
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ValueKind {
	Unknown,
	Anything,
	Null,

	ZonedDateTime,
	ZonedDateTimeArray,
	LocalDateTime,
	LocalDateTimeArray,
	Date,
	DateArray,
	ZonedTime,
	ZonedTimeArray,
	LocalTime,
	LocalTimeArray,
	Duration,
	DurationArray,
	Utf16Text,
	Utf8Text,
	TextArray,
	Bool,
	BoolArray,

	Int64,
	Int32,
	Int16,
	Int8,
	Uint64,
	Uint32,
	Uint16,
	Uint8,

	Float64,
	Float32,

	Int64Array,
	Int32Array,
	Int16Array,
	Int8Array,
	Uint64Array,
	Uint32Array,
	Uint16Array,
	Uint8Array,

	Float64Array,
	Float32Array,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ValueCategory {
	Number,
	NumberArray,
	Text,
	TextArray,
	Temporal,
	TemporalArray,
	Bool,
	BoolArray,

	Unknown,
	NoCategory,
	Anything,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ValueGroup {
	Unknown,
	Anything,
	Null,

	ZonedDateTimeArray,
	LocalDateTimeArray,
	DateArray,
	ZonedTimeArray,
	LocalTimeArray,
	DurationArray,
	TextArray,
	BoolArray,
	NumberArray,

	ZonedDateTime,
	LocalDateTime,
	Date,
	ZonedTime,
	LocalTime,
	Duration,
	Text,
	Bool,
	Number,
}

impl ValueGroup {
	pub fn category(&self) -> ValueCategory {
		match self {
			ValueGroup::Unknown => ValueCategory::Unknown,
			ValueGroup::Anything => ValueCategory::Anything,
			ValueGroup::Null => ValueCategory::NoCategory,
			ValueGroup::ZonedDateTimeArray => ValueCategory::TemporalArray,
			ValueGroup::LocalDateTimeArray => ValueCategory::TemporalArray,
			ValueGroup::DateArray => ValueCategory::TemporalArray,
			ValueGroup::ZonedTimeArray => ValueCategory::TemporalArray,
			ValueGroup::LocalTimeArray => ValueCategory::TemporalArray,
			ValueGroup::DurationArray => ValueCategory::TemporalArray,
			ValueGroup::TextArray => ValueCategory::TextArray,
			ValueGroup::BoolArray => ValueCategory::BoolArray,
			ValueGroup::NumberArray => ValueCategory::NumberArray,
			ValueGroup::ZonedDateTime => ValueCategory::Temporal,
			ValueGroup::LocalDateTime => ValueCategory::Temporal,
			ValueGroup::Date => ValueCategory::Temporal,
			ValueGroup::ZonedTime => ValueCategory::Temporal,
			ValueGroup::LocalTime => ValueCategory::Temporal,
			ValueGroup::Duration => ValueCategory::Temporal,
			ValueGroup::Text => ValueCategory::Text,
			ValueGroup::Bool => ValueCategory::Bool,
			ValueGroup::Number => ValueCategory::Number,
		}
	}
}
