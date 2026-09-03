#[non_exhaustive]
pub struct GQLConfig {
	pub max_object_parsing_depth: u32,
	pub max_query_parsing_depth: u32,
}

impl Default for GQLConfig {
	fn default() -> Self {
		Self {
			max_object_parsing_depth: 100,
			max_query_parsing_depth: 20,
		}
	}
}
