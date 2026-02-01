use super::sequence::SequenceValue;
use super::virt::path::VirtualPathValue;
use crate::values::storable::{DateTimeValue, DateValue, NumberValue, TimeValue};

pub trait ValueMapper<Base> {
	// Virtual
	fn map_path(&self, value: &impl VirtualPathValue) -> Base;
	fn map_node(&self, value: &VirtualNodeValue) -> Base;
	fn map_relationship(&self, value: &VirtualRelationshipValue) -> Base;
	fn map_map(&self, value: &MapValue) -> Base;

	// Storable
	fn map_null(&self) -> Base;
	fn map_sequence(&self, value: &dyn SequenceValue) -> Base;

	fn map_text(&self, value: &TextValue) -> Base;
	fn map_text_array(&self, value: &TextArray) -> Base {
		self.map_sequence(value)
	}

	fn map_bool(&self, value: &BooleanValue) -> Base;
	fn map_bool_array(&self, value: &BooleanArray) -> Base {
		self.map_sequence(value)
	}

	fn map_number(&self, value: &NumberValue) -> Base;
	fn map_number_array(&self, value: &NumberArray) -> Base {
		self.map_sequence(value)
	}

	fn map_bytes(&self, value: &BytesValue) -> Base {
		self.map_sequence(value)
	}

	fn map_datetime(&self, value: &DateTimeValue) -> Base;
	fn map_local_datetime(&self, value: &LocalDateTimeValue) -> Base;
	fn map_date(&self, value: &DateValue) -> Base;
	fn map_time(&self, value: &TimeValue) -> Base;
	fn map_local_time(&self, value: &LocalTimeValue) -> Base;
	fn map_duration(&self, value: &DurationValue) -> Base;

	fn map_datetime_array(&self, value: &DateTimeArray) -> Base {
		self.map_sequence(value)
	}
	fn map_local_datetime_array(&self, value: &LocalDateTimeArray) -> Base {
		self.map_sequence(value)
	}
	fn map_date_array(&self, value: &DateValueArray) -> Base {
		self.map_sequence(value)
	}
	fn map_time_array(&self, value: &TimeArray) -> Base {
		self.map_sequence(value)
	}
	fn map_local_time_array(&self, value: &LocalTimeArray) -> Base {
		self.map_sequence(value)
	}
	fn map_duration_array(&self, value: &DurationArray) -> Base {
		self.map_sequence(value)
	}
}


