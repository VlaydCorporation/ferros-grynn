use std::collections::{HashMap, HashSet};
use std::fmt::Display;

fn formatter<T, C: IntoIterator<Item = T>>(
	f: &mut std::fmt::Formatter<'_>,
	c: C,
	brackets: [&str; 2],
	pred: impl Fn(&T) -> String,
) -> std::fmt::Result {
	let mut p = c.into_iter().peekable();
	write!(f, "{}", brackets[0])?;
	while let Some(item) = p.next() {
		write!(f, "{}", pred(&item))?;

		if p.peek().is_some() {
			write!(f, ", ")?;
		}
	}
	write!(f, "{}", brackets[1])?;
	Ok(())
}

pub struct DisplayableHashSet<'a, T: 'a>(pub &'a HashSet<T>);

impl<'a, T> Display for DisplayableHashSet<'a, T>
where
	T: Display,
{
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter(f, self.0, ["{", "}"], |v| format!("{}", v))
	}
}

pub struct DisplayableHashMap<'a, K: 'a, V: 'a>(pub &'a HashMap<K, V>);

impl<'a, K: 'a, V: 'a> Display for DisplayableHashMap<'a, K, V>
where
	K: Display,
	V: Display,
{
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter(f, self.0, ["{", "}"], |(k, v)| format!("{}: {}", k, v))
	}
}

pub struct DisplayableVec<'a, T: 'a>(pub &'a Vec<T>);

impl<'a, T: 'a> Display for DisplayableVec<'a, T>
where
	T: Display,
{
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter(f, self.0, ["[", "]"], |v| format!("{}", v))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_set() {
		let mut s = HashSet::<String>::new();
		s.insert("a".to_string());
		s.insert("b".to_string());
		s.insert("c".to_string());

		let v1 = DisplayableHashSet(&s);
		println!("{v1}")
	}

	#[test]
	fn test_map() {
		let mut m = HashMap::<String, usize>::new();
		m.insert("a".to_string(), 1);
		m.insert("b".to_string(), 2);
		m.insert("c".to_string(), 3);

		let v1 = DisplayableHashMap(&m);
		println!("{v1}")
	}

	#[test]
	fn test_vec() {
		let v = vec![1, 2, 3];
		let v1 = DisplayableVec(&v);
		assert_eq!(v1.to_string(), "[1, 2, 3]");
	}
}
