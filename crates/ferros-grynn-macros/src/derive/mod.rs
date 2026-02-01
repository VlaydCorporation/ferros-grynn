use syn::{Attribute, Field, Token};
use syn::parse::Parse;
use syn::punctuated::Punctuated;

pub mod measurable;
pub mod structure_accumulator;

pub fn field_has_attribute<'a>(ident: &str, field: &Field) -> bool {
    has_attribute(ident, &field.attrs)
}

pub fn has_attribute<'a>(ident: &str, it: impl IntoIterator<Item=&'a Attribute>) -> bool {
    it.into_iter().any(|attr| attr.path().is_ident(ident))
}

pub fn get_metadata_inner<'a, T: Parse>(
    ident: &str,
    it: impl IntoIterator<Item=&'a Attribute>,
) -> syn::Result<Vec<T>> {
    it.into_iter().filter(|attr| attr.path().is_ident(ident)).try_fold(
        Vec::new(),
        |mut vec, attr| {
            vec.extend(attr.parse_args_with(Punctuated::<T, Token![,]>::parse_terminated)?);
            Ok(vec)
        },
    )
}