use super::error::Result;
use super::{ArrayType, ElementId, Id};
use chrono::Datelike;
use chrono::Timelike;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use crate::values::storable::NumberValue;

/**
 * Writer of values.
 * <p>
 * Has functionality to write all supported primitives.
 */
pub trait ValueWriter {
	fn write_null(&mut self) -> Result<()>;
	fn write_bool(&mut self, _b: bool) -> Result<()>;
	fn write_number(&mut self, _n: &NumberValue) -> Result<()>;
	fn write_str(&mut self, _s: &str) -> Result<()>;

	fn begin_array(&mut self, _size: usize, _array_type: ArrayType) -> Result<()>;
	fn end_array(&mut self) -> Result<()>;
	fn write_bytes(&mut self, _bs: &[u8], offset: u32, len: u32) -> Result<()>;

	fn write_duration(
		&mut self,
		_month: u64,
		_days: u64,
		_seconds: u64,
		_nanos: u64,
	) -> Result<()>;

	fn write_date(&mut self, _date: NaiveDate) -> Result<()>;
	fn write_local_time(&mut self, _time: NaiveTime) -> Result<()>;
	fn write_time(&mut self, _time: DateTime<FixedOffset>) -> Result<()>;
	fn write_local_datetime(&mut self, _dt: NaiveDateTime) -> Result<()>;
	fn write_zoned_datetime(&mut self, _dt: DateTime<FixedOffset>) -> Result<()>;
}

#[derive(Eq, PartialEq)]
pub enum EntityMode {
	Reference,
	Full,
}

/// Writer of any values.
pub trait AnyValueWriter: ValueWriter {
	/**
	 * Returns the wanted `EntityMode` of this AnyValueWriter.
	 *
	 * A returned `EntityMode::Reference` signals to all entity-values that they should callback using `writeNodeReference(i64)` or
	 * `writeRelationshipReference(i64)` even if the whole entity is available.
	 *
	 * A returned `EntityMode::Full` signals to all entity-values that they can callback using either
	 *      `writeNodeReference(i64)`,
	 *      `writeNode(&str, i64, TextArray, MapValue, bool)`,
	 *      `writeRelationshipReference(i64)`
	 *   or `writeRelationship(&str, i64, &str, i64, &str, i64, TextValue, MapValue, bool)`
	 * depending on how much information is available to the value instance.
	 */
	fn entity_mode(&mut self) -> EntityMode;

	fn write_node_reference(&mut self, _node_id: Id) -> Result<()>;
	fn write_node(
		&mut self,
		_element_id: ElementId,
		_node_id: Id,
		_labels: TextArray,
		_properties: MapValue,
		_is_deleted: bool,
	) -> Result<()>;

	fn write_relationship_reference(&mut self, _rel_id: Id) -> Result<()>;
	fn write_relationship(
		&mut self,
		_element_id: ElementId,
		_rel_id: Id,
		_start_node_element_id: ElementId,
		_start_node_id: Id,
		_end_node_element_id: ElementId,
		_end_node_id: Id,
		_type: TextValue,
		_properties: MapValue,
		_is_deleted: bool,
	) -> Result<()>;

	fn begin_map(&mut self, _size: usize) -> Result<()>;
	fn end_map(&mut self) -> Result<()>;

	fn begin_list(&mut self, _size: usize) -> Result<()>;
	fn end_list(&mut self) -> Result<()>;

	fn write_path_ref(&mut self, nodes: &[Id], relationships: &[Id]) -> Result<()>;
	fn write_path_ref_v(&mut self, nodes: Vec<VirtualNodeValue>, relationships: Vec<VirtualRelationshipValue>) -> Result<()>;
	fn write_path(&mut self, nodes: &[NodeValue], relationships: &[RelationshipValue]) -> Result<()>;

	fn write_virtual_node_hack<T>(&mut self, node: T) -> Result<()>;
	fn write_relationship_node_hack<T>(&mut self, relationship: T) -> Result<()>;
}

pub trait TemporalValueWriterAdapter: ValueWriter {
	fn write_date_epoch(&mut self, _epoch_day: i64) -> Result<()> {
		Ok(())
	}
	fn write_local_time_nanos(&mut self, _nano_of_day: i64) -> Result<()> {
		Ok(())
	}
	fn write_time_utc(&mut self, _nanos_of_day_utc: i64, _offset_seconds: i32) -> Result<()> {
		Ok(())
	}
	fn write_local_datetime_epoch(&mut self, _epoch_second: i64, _nano: i32) -> Result<()> {
		Ok(())
	}
	fn write_zoned_datetime_offset(
		&mut self,
		_epoch_second_utc: i64,
		_nano: i32,
		_offset_seconds: i32,
	) -> Result<()> {
		Ok(())
	}
	fn write_zoned_datetime_zone(
		&mut self,
		_epoch_second_utc: i64,
		_nano: i32,
		_zone_id: &str,
	) -> Result<()> {
		Ok(())
	}
}

impl<T> ValueWriter for T
where
	T: TemporalValueWriterAdapter,
{
	fn write_null(&mut self) -> Result<()> {
		Ok(())
	}

	fn write_bool(&mut self, _b: bool) -> Result<()> {
		Ok(())
	}

	fn write_number(&mut self, _n: &NumberValue) -> Result<()> {
		Ok(())
	}

	fn write_str(&mut self, _s: &str) -> Result<()> {
		Ok(())
	}

	fn begin_array(&mut self, _size: usize, _array_type: ArrayType) -> Result<()> {
		Ok(())
	}

	fn end_array(&mut self) -> Result<()> {
		Ok(())
	}

	fn write_bytes(&mut self, _bs: &[u8], offset: u32, len: u32) -> Result<()> {
		Ok(())
	}

	fn write_duration(&mut self, _month: u64, _days: u64, _seconds: u64, _nanos: u64) -> Result<()> {
		Ok(())
	}

	fn write_date(&mut self, date: NaiveDate) -> Result<()> {
		self.write_date_epoch(date.num_days_from_ce().into())
	}

	fn write_local_time(&mut self, time: NaiveTime) -> Result<()> {
		self.write_local_time_nanos(time.num_seconds_from_midnight() as i64 * 1_000_000_000)
	}

	fn write_time(&mut self, time: DateTime<FixedOffset>) -> Result<()> {
		let nanos_utc = time.time().num_seconds_from_midnight() as i64 * 1_000_000_000;
		self.write_time_utc(nanos_utc, time.offset().local_minus_utc())
	}

	fn write_local_datetime(&mut self, dt: NaiveDateTime) -> Result<()> {
		let epoch = dt.and_utc().timestamp();
		self.write_local_datetime_epoch(epoch, dt.and_utc().timestamp_subsec_nanos() as i32)
	}

	fn write_zoned_datetime(&mut self, dt: DateTime<FixedOffset>) -> Result<()> {
		self.write_zoned_datetime_offset(
			dt.timestamp(),
			dt.timestamp_subsec_nanos() as i32,
			dt.offset().local_minus_utc(),
		)
	}
}
