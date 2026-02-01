use crate::values::AnyValue;

/**
 * Values that represent sequences of values.
 * Thus, we can get an equality check that is based on the values.
 */
pub trait SequenceValue: IntoIterator<Item=AnyValue> {
    /// The number of elements of the collection.
	fn size(&self) -> usize;
	fn is_empty(&self) -> bool {
		self.size() == 0
	}

	fn value_at(&self, offset: usize) -> Option<AnyValue>;

	fn equals(&self, other: &Self) -> bool
	where
		Self: Sized,
	{
		let mut ai = self.into_iter();
		let mut bi = other.into_iter();
		loop {
			match (ai.next(), bi.next()) {
				(Some(x), Some(y)) if x == y => continue,
				(Some(_), Some(_)) => return false,
				(None, None) => return true,
				_ => return false,
			}
		}
	}
}
