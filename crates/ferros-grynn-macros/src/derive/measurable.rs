use proc_macro2::{Ident, Span, TokenStream};
use quote::{quote, ToTokens};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Attribute, Data, DeriveInput, Fields, Token};
use crate::derive::get_metadata_inner;

mod kw {
    use syn::custom_keyword;

    custom_keyword!(add);
    custom_keyword!(add_m);
}

pub fn occurrence_error<T: ToTokens>(fst: T, snd: T, attr: &str) -> syn::Error {
    let mut e = syn::Error::new_spanned(
        snd,
        format!("Found multiple occurrences of m({})", attr),
    );
    e.combine(syn::Error::new_spanned(fst, "first one here"));
    e
}

enum Attrs {
    Add {
        kw: kw::add,
    },
    AddM {
        kw: kw::add_m,
    },
}

impl Parse for Attrs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let lookahead = input.lookahead1();
        if lookahead.peek(kw::add) {
            let kw = input.parse::<kw::add>()?;
            Ok(Attrs::Add {
                kw,
            })
        } else if lookahead.peek(kw::add_m) {
            let kw = input.parse::<kw::add_m>()?;
            Ok(Attrs::AddM {
                kw,
            })
        } else {
            Err(lookahead.error())
        }
    }
}

pub fn measurable_inner(input: DeriveInput) -> syn::Result<TokenStream> {
    let ident = &input.ident;
    let generics = &input.generics;
    let (gen_impl, gen_tys, gen_where) = generics.split_for_impl();

    match input.data {
        Data::Struct(s) => {
            let mut arms = Vec::new();
            for field in &s.fields {
                let ident = field.ident.clone().unwrap();

                let attrs: Vec<Attrs> = {
                    let res = get_metadata_inner("m", &field.attrs)?;
                    field
                        .attrs
                        .iter()
                        .filter(|attr| attr.meta.path().is_ident("add"))
                        .try_fold(res, |vec, _attr| syn::Result::Ok(vec))?
                };

                let mut add = None;
                let mut add_m = None;
                for attr in attrs {
                    match attr {
                        Attrs::Add { kw } => {
                            if let Some(fst_kw) = add {
                                return Err(occurrence_error(fst_kw, kw, "add"));
                            }
                            add = Some(kw);
                        }
                        Attrs::AddM { kw } => {
                            if let Some(fst_kw) = add_m {
                                return Err(occurrence_error(fst_kw, kw, "add"));
                            }
                            add_m = Some(kw);
                        }
                    }
                }

                if add.is_some() {
                    arms.push(quote! {self.#ident as usize})
                }
                if add_m.is_some() {
                    arms.push(quote! {self.#ident.estimated_heap_usage()})
                }
            }

            Ok(quote! {
				impl #gen_impl crate::common::memory::Measurable for #ident #gen_tys #gen_where {
					fn estimated_heap_usage(&self) -> usize {
						crate::unsafed::shallow_size_of::<#ident>() #(+ #arms)*
					}
				}
			})
        }
        Data::Enum(e) => {
            let variants = &e.variants;
            let vars = variants.iter().map(|v| &v.ident);
            let names = vars
                .clone()
                .map(|v| Ident::new(v.to_string().to_lowercase().as_str(), Span::call_site()));

            let arms = variants.iter().zip(vars).zip(names).map(|((v, var), n)| {
                match v.fields {
                    Fields::Unit => quote! {
						#ident::#var => 0,
					},
                    _ => quote! {
						#ident::#var(#n) => #n.estimated_heap_usage(),
					}
                }
            });

			Ok(quote! {
				impl #gen_impl crate::common::memory::Measurable for #ident #gen_tys #gen_where {
					fn estimated_heap_usage(&self) -> usize {
						match self {
							#(#arms)*
						}
					}
				}
			})
        }
        Data::Union(_) => Err(syn::Error::new(Span::call_site(), "Measurable does not support unions")),
    }
}
