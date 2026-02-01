use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use std::cmp::PartialEq;
use std::collections::HashMap;
use syn::parse::{Parse, ParseStream};
use syn::{ExprStruct, Ident, LitStr, Path, Token};

mod kw {
	syn::custom_keyword!(template);
	syn::custom_keyword!(params);
	syn::custom_keyword!(join_styles);
	syn::custom_keyword!(subcondition);
	syn::custom_keyword!(descriptor);
	syn::custom_keyword!(hint);
}

fn quoted_option<T: ToTokens + Clone>(value: &Option<T>) -> TokenStream {
	value.clone().map(|v| quote! { Some(#v) }).unwrap_or(quote! { None })
}

struct StatusInput {
	defs: Vec<StatusDef>,
}

impl Parse for StatusInput {
	fn parse(input: ParseStream) -> syn::Result<Self> {
		let defs = syn::punctuated::Punctuated::<StatusDef, Token![,]>::parse_terminated(input)?
			.into_iter()
			.collect();
		Ok(Self {
			defs,
		})
	}
}

impl ToTokens for StatusInput {
	fn to_tokens(&self, tokens: &mut TokenStream) {
		let variants = self.defs.iter().map(|d| {
			let name = &d.name;
			let fields = d.body.params.iter().map(|p| {
				let n = Ident::new(&p.name.to_string().to_lowercase(), p.name.span());
				let ty = &p.kind.to_ty();
				quote!(#n: #ty)
			});
			let fields = if fields.len() == 0 {
				None
			} else {
				Some(quote! {{ #( #fields ),* }})
			};
			quote!(#name #fields)
		});

		let code_match = self.defs.iter().map(|d| {
			let name = &d.name;
			let code = &d.code;
			quote!(Status::#name { .. } => #code)
		});

		let subcondition_match = self.defs.iter().map(|d| {
			let name = &d.name;
			let subcondition = quoted_option(&d.body.subcondition);
			quote!(Status::#name { .. } => #subcondition)
		});

		let descriptor_match = self.defs.iter().map(|d| {
			let name = &d.name;
			let descriptor = &d.body.descriptor;
			quote!(Status::#name { .. } => #descriptor)
		});

		let message_match = self.defs.iter().map(|d| {
			let name = &d.name;
			let template = &d.body.template;
			let join_styles = &d.body.join_styles;
			let params = d.body.params.iter().map(|p| {
				let kind = &p.kind;
				let pname = &p.name;
				let name = Ident::new(&p.name.to_string().to_lowercase(), p.name.span());
				match kind {
					ParamKind::List => {
						let js = join_styles.styles.get(&p).unwrap(); // OK, checked during parsing
						quote!(#kind::#pname.process_list(#name, #js))
					}
					_ => quote!(#kind::#pname.process(#name)),
				}
			});
			let params_names = d
				.body
				.params
				.iter()
				.map(|p| Ident::new(&p.name.to_string().to_lowercase(), p.name.span()));

			let message =
				quoted_option(&template.clone().map(|t| quote!(format!(#t, #(#params,)*))));

			quote!(Status::#name { #(#params_names,)* } => {
				use crate::gql_status::params::HasJoinStyle;
				#message
			})
		});

		let hint_match = self.defs.iter().map(|d| {
			let name = &d.name;
			let hint = quoted_option(&d.body.hint);
			quote!(Status::#name { .. } => #hint)
		});

		tokens.extend(quote! {
			#[derive(Debug)]
			pub enum Status {
				#( #variants, )*
				InternalError {
					code: &'static str,
					subcondition: Option<&'static str>,
					descriptor: StatusDescriptor,
					message: Option<String>,
					hint: Option<&'static str>,
				},
				UnknownExternalError {
					message: String,
				},
			}

			impl Status {
				pub const fn code(&self) -> &'static str {
					match self {
						#( #code_match, )*
						Status::InternalError { code, .. } => code,
						Status::UnknownExternalError { .. } => "FG-UnknownStatusCode",
					}
				}

				pub const fn subcondition(&self) -> Option<&'static str> {
					match self {
						#( #subcondition_match, )*
						Status::InternalError { subcondition, .. } => *subcondition,
						Status::UnknownExternalError { .. } => Some("unknown external error"),
					}
				}

				pub const fn descriptor(&self) -> StatusDescriptor {
					match self {
						#( #descriptor_match, )*
						Status::InternalError { descriptor, .. } => *descriptor,
						Status::UnknownExternalError { .. } => StatusDescriptor {
							condition: Condition::Unknown,
							category: StatusCategory::Unknown,
							domain: StatusDomain::Unknown,
							properties: StatusProperties::EXTERNAL,
							classification: GqlClassification::ErrorClassification(ErrorClassification::Unknown),
						},
					}
				}

				pub fn message(&self) -> Option<String> {
					match self {
						#( #message_match, )*
						Status::InternalError { message, .. } => message.clone(),
						Status::UnknownExternalError { message } => Some(message.clone()),
					}
				}

				pub const fn hint(&self) -> Option<&'static str> {
					match self {
						#( #hint_match, )*
						Status::InternalError { hint, .. } => *hint,
						Status::UnknownExternalError { .. } => None,
					}
				}
			}
		});
	}
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
enum ParamKind {
	String,
	Number,
	Bool,
	List,
}

impl ParamKind {
	pub fn to_ty(&self) -> TokenStream {
		match self {
			ParamKind::String => quote!(::std::string::String),
			ParamKind::Number => quote!(i64),
			ParamKind::Bool => quote!(bool),
			ParamKind::List => quote!(::std::vec::Vec<::std::string::String>),
		}
	}
}

impl ToTokens for ParamKind {
	fn to_tokens(&self, tokens: &mut TokenStream) {
		let t = match self {
			ParamKind::String => quote!(crate::gql_status::params::StringParam),
			ParamKind::Number => quote!(crate::gql_status::params::NumberParam),
			ParamKind::Bool => quote!(crate::gql_status::params::BoolParam),
			ParamKind::List => quote!(crate::gql_status::params::ListParam),
		};
		tokens.extend([t]);
	}
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct StatusParam {
	kind: ParamKind,
	name: Ident,
}

impl Parse for StatusParam {
	fn parse(input: ParseStream) -> syn::Result<Self> {
		let ty: Ident = input.parse()?;
		input.parse::<Token![::]>()?;
		let name: Ident = input.parse()?;

		let kind = match ty.to_string().as_str() {
			"StringParam" => ParamKind::String,
			"NumberParam" => ParamKind::Number,
			"BoolParam" => ParamKind::Bool,
			"ListParam" => ParamKind::List,
			_ => {
				return Err(syn::Error::new_spanned(
					ty,
					"Unknown parameter type. Expected one of: StringParam, NumberParam, BoolParam, ListParam",
				));
			}
		};

		Ok(Self {
			kind,
			name,
		})
	}
}

struct JoinStyleEntry {
	param: StatusParam,
	style: Path,
}

impl Parse for JoinStyleEntry {
	fn parse(input: ParseStream) -> syn::Result<Self> {
		let param: StatusParam = input.parse()?;
		if param.kind != ParamKind::List {
			return Err(syn::Error::new_spanned(
				param.name,
				"join_styles can only reference ListParam parameters",
			));
		}

		input.parse::<Token![=>]>()?;
		let style: Path = input.parse()?;
		Ok(Self {
			param,
			style,
		})
	}
}

#[derive(Debug)]
struct JoinStyles {
	styles: HashMap<StatusParam, Path>,
}

impl Parse for JoinStyles {
	fn parse(input: ParseStream) -> syn::Result<Self> {
		let inner;
		syn::bracketed!(inner in input);
		let join_styles =
			syn::punctuated::Punctuated::<JoinStyleEntry, Token![,]>::parse_terminated(&inner)?
				.into_iter()
				.map(|e| (e.param, e.style))
				.collect();

		Ok(JoinStyles {
			styles: join_styles,
		})
	}
}

#[derive(Debug)]
struct StatusBody {
	template: Option<LitStr>,
	params: Vec<StatusParam>,
	join_styles: JoinStyles,
	subcondition: Option<LitStr>,
	descriptor: ExprStruct,
	hint: Option<LitStr>,
}

impl Parse for StatusBody {
	fn parse(input: ParseStream) -> syn::Result<Self> {
		let content;
		syn::braced!(content in input);

		let mut template = None;
		let mut params = Vec::new();
		let mut join_styles = JoinStyles {
			styles: HashMap::new(),
		};
		let mut subcondition = None;
		let mut descriptor = None;
		let mut hint = None;

		while !content.is_empty() {
			if content.peek(kw::template) {
				content.parse::<kw::template>()?;
				content.parse::<Token![:]>()?;
				template = Some(content.parse()?);
			} else if content.peek(kw::params) {
				content.parse::<kw::params>()?;
				content.parse::<Token![:]>()?;
				let inner;
				syn::bracketed!(inner in content);
				params = syn::punctuated::Punctuated::<StatusParam, Token![,]>::parse_terminated(
					&inner,
				)?
				.into_iter()
				.collect();
			} else if content.peek(kw::join_styles) {
				content.parse::<kw::join_styles>()?;
				content.parse::<Token![:]>()?;
				join_styles = content.parse()?;
			} else if content.peek(kw::subcondition) {
				content.parse::<kw::subcondition>()?;
				content.parse::<Token![:]>()?;
				subcondition = Some(content.parse()?);
			} else if content.peek(kw::descriptor) {
				content.parse::<kw::descriptor>()?;
				content.parse::<Token![:]>()?;
				descriptor = Some(content.parse()?);
			} else if content.peek(kw::hint) {
				content.parse::<kw::hint>()?;
				content.parse::<Token![:]>()?;
				hint = Some(content.parse()?);
			} else {
				return Err(content.error("Unknown field in status definition"));
			}

			let _ = content.parse::<Token![,]>();
		}

		let descriptor = descriptor
			.ok_or_else(|| syn::Error::new(content.span(), "descriptor field is required"))?;

		for list_param in params.iter().filter(|p| p.kind == ParamKind::List) {
			if !join_styles.styles.contains_key(&list_param) {
				return Err(syn::Error::new_spanned(
					&list_param.name,
					"Every ListParam must have JoinStyle.",
				));
			}
		}

		Ok(Self {
			template,
			params,
			join_styles,
			subcondition,
			descriptor,
			hint,
		})
	}
}

struct StatusDef {
	name: Ident,
	code: LitStr,
	body: StatusBody,
}

impl Parse for StatusDef {
	fn parse(input: ParseStream) -> syn::Result<Self> {
		let name = input.parse()?;
		input.parse::<Token![,]>()?;
		let code = input.parse()?;
		input.parse::<Token![=>]>()?;
		let body = input.parse()?;
		Ok(Self {
			name,
			code,
			body,
		})
	}
}

pub fn define_status_codes(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
	let parsed = syn::parse_macro_input!(item as StatusInput);
	quote!(#parsed).into()
}
