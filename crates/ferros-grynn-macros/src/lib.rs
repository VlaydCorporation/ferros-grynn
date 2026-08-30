mod derive;
mod status_codes;
pub(crate) mod utils;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

/// Implements Measurable trait for structs and enums.
/// Use `#[m(add)]` attribute on structure field that can be cast to `u64`
/// to add its value to result estimated heap usage.
/// Use `#[m(add_m)]` attribute on structure field that also implements
/// Measurable trait to add estimated heap usage of its field to result
/// estimated heap usage.
#[proc_macro_derive(Measurable, attributes(m))]
pub fn measurable(input: TokenStream) -> TokenStream {
	derive::measurable::measurable_inner(parse_macro_input!(input as DeriveInput))
		.unwrap_or_else(|err| err.to_compile_error())
		.into()
}

#[proc_macro]
pub fn define_status_codes(item: TokenStream) -> TokenStream {
	status_codes::define_status_codes(item)
}
