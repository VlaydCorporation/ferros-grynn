use darling::{FromAttributes, FromField};
use itertools::Itertools;
use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;
use syn::{Attribute, Data, DeriveInput, Meta, Token};
use syn::parse::ParseStream;
use syn::punctuated::Punctuated;

#[derive(Default)]
pub struct AccumList {
	pub fields: Vec<Ident>,
}

impl syn::parse::Parse for AccumList {
	fn parse(input: ParseStream) -> syn::Result<Self> {
		let p: Punctuated<Ident, Token![,]> = Punctuated::parse_terminated(input)?;
		Ok(Self {
			fields: p.into_iter().collect(),
		})
	}
}

impl FromAttributes for AccumList {
	fn from_attributes(attrs: &[Attribute]) -> darling::Result<Self> {
		let mut accum_list = AccumList::default();

		for attr in attrs {
			match &attr.meta {
				Meta::List(list) => {
					accum_list = syn::parse2(list.tokens.clone())?;
				}
				_ => {
					return Err(darling::Error::custom("expected identifiers list in #[accum(...)]"));
				}
			}
		}

		Ok(accum_list)
	}
}

#[derive(Default, FromAttributes)]
#[darling(attributes(accum))]
pub struct StructAccumArgs(#[darling(default)] AccumList);

#[derive(Debug, FromField)]
#[darling(attributes(accum))]
pub struct FieldAccumArgs {
	pub ident: Option<Ident>,
}

pub fn structure_accumulator_inner(input: DeriveInput) -> syn::Result<TokenStream> {
	let ident = &input.ident;

	let extra_fields = StructAccumArgs::from_attributes(&input.attrs)?
		.0
		.fields
		.into_iter()
		.map(|f| Some(f));

	let ident_str = ident.to_string();

	match input.data {
		Data::Struct(s) => {
			let fields = s
				.fields
				.iter()
				.filter_map(|f| match FieldAccumArgs::from_field(f) {
					Ok(f) => Some(f.ident),
					_ => None,
				})
				.merge(extra_fields);

			// let fields = s.fields.iter().filter(|f| field_has_attribute("accum", f));
			let name = Ident::new(format!("{ident}Accumulator").as_str(), Span::call_site());

			let field_arms = fields.clone().map(|f| {
				let name = f.clone().unwrap(); // OK, struct's field must have ident
				quote! {
					#name: crate::values::AnyValue
				}
			});

			let add_arms = fields.clone().map(|f| {
				let ident = f.clone().unwrap();
				let name = ident.to_string();
				let self_field = quote! {self.#ident};
				quote! {
					#name => #self_field = value.into()
				}
			});

			Ok(quote! {
				#[derive(Default)]
				pub struct #name {
					#(#field_arms,)*
				}

				impl crate::values::utils::StructureAccumulator for #name {
					type Output = #ident;

					fn add(&mut self, field: &str, value: crate::values::AnyValue) -> crate::gql_status::FerrosGrynnResult<()> {
						match field {
							#(#add_arms,)*
							_ => return Err(crate::gql_status::FerrosGrynnError::accumulator_unknown_field(#ident_str, field))
						}
						Ok(())
					}
				}
			})
		}
		_ => {
			Err(syn::Error::new(Span::call_site(), "StructureAccumulator supports only structures"))
		}
	}
}
