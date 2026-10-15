use syn::{Attribute, Token, parse::Parse, punctuated::Punctuated};

/// Heap-measurement derive parsing and code generation.
pub mod measurable;

/// Generates metadata accessors from the annotated type definition.
pub fn get_metadata_inner<'a, T: Parse, ItArg: IntoIterator<Item = &'a Attribute>>(
	ident: &str,
	it: ItArg,
) -> syn::Result<Vec<T>> {
	it.into_iter().filter(|attr| attr.path().is_ident(ident)).try_fold(
		Vec::new(),
		|mut vec, attr| {
			vec.extend(attr.parse_args_with(Punctuated::<T, Token![,]>::parse_terminated)?);
			Ok(vec)
		},
	)
}
