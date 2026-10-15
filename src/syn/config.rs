/// Depth limits used to bound nested query and object parsing.
#[non_exhaustive]
pub struct GQLConfig {
	/// Maximum nesting depth accepted while parsing object values.
	pub max_object_parsing_depth: u32,
	/// Maximum expression or statement nesting depth accepted by the parser.
	pub max_query_parsing_depth: u32,
}

impl Default for GQLConfig {
	/// Constructs the default initial state of `GQLConfig`.
	fn default() -> Self {
		Self {
			max_object_parsing_depth: 100,
			max_query_parsing_depth: 20,
		}
	}
}
