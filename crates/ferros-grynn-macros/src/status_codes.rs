use std::collections::HashSet;

use proc_macro2::{Literal, TokenStream};
use quote::{ToTokens, format_ident, quote};
use syn::{
	Ident,
	LitStr,
	Path,
	Token,
	Type,
	parse::{Parse, ParseStream},
};

mod kw {
	syn::custom_keyword!(category);
	syn::custom_keyword!(classification);
	syn::custom_keyword!(condition);
	syn::custom_keyword!(constructor);
	syn::custom_keyword!(domain);
	syn::custom_keyword!(hint);
	syn::custom_keyword!(kind);
	syn::custom_keyword!(message);
	syn::custom_keyword!(params);
	syn::custom_keyword!(severity);
	syn::custom_keyword!(subcondition);
}

struct StatusInput {
	definitions: Vec<StatusDefinition>,
}

impl Parse for StatusInput {
	fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
		let definitions =
			syn::punctuated::Punctuated::<StatusDefinition, Token![,]>::parse_terminated(input)?
				.into_iter()
				.collect::<Vec<_>>();
		validate_definitions(&definitions)?;
		Ok(Self {
			definitions,
		})
	}
}

impl ToTokens for StatusInput {
	fn to_tokens(&self, tokens: &mut TokenStream) {
		let variants = self.definitions.iter().map(StatusDefinition::variant_tokens);
		let definition_statics = self.definitions.iter().map(StatusDefinition::definition_static);
		let definitions = self.definitions.iter().map(StatusDefinition::definition_arm);
		let messages = self.definitions.iter().map(StatusDefinition::message_arm);
		let parameter_arms = self.definitions.iter().map(StatusDefinition::parameters_arm);
		let status_constructors = self.definitions.iter().map(StatusDefinition::status_constructor);
		let error_constructors = self
			.definitions
			.iter()
			.filter(|definition| matches!(definition.body.kind, StatusKindInput::Error { .. }))
			.map(StatusDefinition::error_constructor);

		tokens.extend(quote! {
			#(#definition_statics)*

			#[derive(Debug, Clone, PartialEq)]
			pub enum Status {
				#(#variants,)*
			}

			impl Status {
				pub const fn definition(&self) -> &'static StatusDefinition {
					match self {
						#(#definitions,)*
					}
				}

				pub(crate) fn fmt_message(
					&self,
					formatter: &mut ::std::fmt::Formatter<'_>,
				) -> ::std::fmt::Result {
					match self {
						#(#messages,)*
					}
				}

				pub fn message(&self) -> Option<String> {
					if self.definition().has_message {
						Some(self.to_string())
					} else {
						None
					}
				}

				pub fn parameters(
					&self,
				) -> ::std::collections::BTreeMap<
					&'static str,
					crate::gql_status::formatting::DiagnosticValue,
				> {
					match self {
						#(#parameter_arms,)*
					}
				}

				#(#status_constructors)*
			}

			impl crate::gql_status::FerrosGrynnError {
				#(#error_constructors)*
			}
		});
	}
}

struct StatusDefinition {
	name: Ident,
	code: LitStr,
	body: StatusBody,
}

impl Parse for StatusDefinition {
	fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
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

impl StatusDefinition {
	fn variant_tokens(&self) -> TokenStream {
		let name = &self.name;
		if self.body.params.is_empty() {
			quote!(#name)
		} else {
			let fields = self.body.params.iter().map(|parameter| {
				let name = &parameter.name;
				let ty = &parameter.ty;
				quote!(#name: #ty)
			});
			quote!(#name { #(#fields),* })
		}
	}

	fn match_pattern(&self, bind: bool) -> TokenStream {
		let name = &self.name;
		if self.body.params.is_empty() {
			quote!(Status::#name)
		} else if bind {
			let fields = self.body.params.iter().map(|parameter| &parameter.name);
			quote!(Status::#name { #(#fields),* })
		} else {
			quote!(Status::#name { .. })
		}
	}

	fn definition_ident(&self) -> Ident {
		format_ident!("__FG_STATUS_DEFINITION_{}", self.name.to_string().to_uppercase())
	}

	fn definition_static(&self) -> TokenStream {
		let definition = self.definition_ident();
		let name = self.name.to_string();
		let body = self.code.value();
		let body = body.strip_prefix("FG-").expect("validated status code");
		let code = Literal::byte_string(body.as_bytes());
		let condition = &self.body.condition;
		let domain = &self.body.domain;
		let kind = self.body.kind.tokens();
		let subcondition = option_lit(&self.body.subcondition);
		let hint = option_lit(&self.body.hint);
		let has_message = self.body.message.is_some();

		quote! {
			static #definition: StatusDefinition = StatusDefinition {
				name: #name,
				code: StatusCode::new(*#code),
				condition: #condition,
				subcondition: #subcondition,
				domain: #domain,
				kind: #kind,
				hint: #hint,
				has_message: #has_message,
			};
		}
	}

	fn definition_arm(&self) -> TokenStream {
		let pattern = self.match_pattern(false);
		let definition = self.definition_ident();
		quote!(#pattern => &#definition)
	}

	fn message_arm(&self) -> TokenStream {
		let pattern = self.match_pattern(true);
		let Some(message) = &self.body.message else {
			return quote!(#pattern => Ok(()));
		};
		let arguments = self.body.params.iter().map(|parameter| {
			let name = &parameter.name;
			let formatter = &parameter.formatter;
			quote!(
				#name = crate::gql_status::formatting::Formatted::<#formatter, _>::new(#name)
			)
		});
		quote! {
			#pattern => write!(formatter, #message, #(#arguments),*)
		}
	}

	fn parameters_arm(&self) -> TokenStream {
		let pattern = self.match_pattern(true);
		let inserts = self.body.params.iter().map(|parameter| {
			let name = &parameter.name;
			let key = name.to_string();
			quote! {
				values.insert(
					#key,
					crate::gql_status::formatting::StatusParameter::to_diagnostic_value(#name),
				);
			}
		});
		quote! {
			#pattern => {
				let mut values = ::std::collections::BTreeMap::new();
				#(#inserts)*
				values
			}
		}
	}

	fn constructor_name(&self) -> Ident {
		self.body
			.constructor
			.clone()
			.unwrap_or_else(|| format_ident!("{}", to_snake_case(&self.name.to_string())))
	}

	fn constructor_parts(&self) -> (Vec<TokenStream>, Vec<TokenStream>) {
		self.body.params.iter().map(StatusParameterDefinition::constructor_part).unzip()
	}

	fn status_constructor(&self) -> TokenStream {
		let method = self.constructor_name();
		let variant = &self.name;
		let (arguments, values) = self.constructor_parts();
		if values.is_empty() {
			quote! {
				pub fn #method() -> Self {
					Self::#variant
				}
			}
		} else {
			let fields = self.body.params.iter().map(|parameter| &parameter.name);
			quote! {
				pub fn #method(#(#arguments),*) -> Self {
					Self::#variant {
						#(#fields: #values),*
					}
				}
			}
		}
	}

	fn error_constructor(&self) -> TokenStream {
		let method = self.constructor_name();
		let (arguments, values) = self.constructor_parts();
		quote! {
			pub fn #method(#(#arguments),*) -> Self {
				Self::from_error_status(Status::#method(#(#values),*))
			}
		}
	}
}

struct StatusBody {
	kind: StatusKindInput,
	condition: Path,
	domain: Path,
	subcondition: Option<LitStr>,
	message: Option<LitStr>,
	params: Vec<StatusParameterDefinition>,
	hint: Option<LitStr>,
	constructor: Option<Ident>,
}

impl Parse for StatusBody {
	fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
		let content;
		syn::braced!(content in input);

		let mut kind = None;
		let mut condition = None;
		let mut domain = None;
		let mut subcondition = None;
		let mut message = None;
		let mut params = Vec::new();
		let mut hint = None;
		let mut constructor = None;

		while !content.is_empty() {
			if content.peek(kw::kind) {
				content.parse::<kw::kind>()?;
				content.parse::<Token![:]>()?;
				kind = Some(content.parse()?);
			} else if content.peek(kw::condition) {
				content.parse::<kw::condition>()?;
				content.parse::<Token![:]>()?;
				condition = Some(content.parse()?);
			} else if content.peek(kw::domain) {
				content.parse::<kw::domain>()?;
				content.parse::<Token![:]>()?;
				domain = Some(content.parse()?);
			} else if content.peek(kw::subcondition) {
				content.parse::<kw::subcondition>()?;
				content.parse::<Token![:]>()?;
				subcondition = Some(content.parse()?);
			} else if content.peek(kw::message) {
				content.parse::<kw::message>()?;
				content.parse::<Token![:]>()?;
				message = Some(content.parse()?);
			} else if content.peek(kw::params) {
				content.parse::<kw::params>()?;
				content.parse::<Token![:]>()?;
				let parameters;
				syn::braced!(parameters in content);
				params = syn::punctuated::Punctuated::<StatusParameterDefinition, Token![,]>::
					parse_terminated(&parameters)?
					.into_iter()
					.collect();
			} else if content.peek(kw::hint) {
				content.parse::<kw::hint>()?;
				content.parse::<Token![:]>()?;
				hint = Some(content.parse()?);
			} else if content.peek(kw::constructor) {
				content.parse::<kw::constructor>()?;
				content.parse::<Token![:]>()?;
				constructor = Some(content.parse()?);
			} else {
				return Err(content.error("unknown field in status definition"));
			}
			let _ = content.parse::<Token![,]>();
		}

		let kind = kind.ok_or_else(|| input.error("kind field is required"))?;
		let condition = condition.ok_or_else(|| input.error("condition field is required"))?;
		let domain = domain.ok_or_else(|| input.error("domain field is required"))?;
		if message.is_none() && !params.is_empty() {
			return Err(syn::Error::new_spanned(&params[0].name, "params require a message"));
		}

		let mut names = HashSet::new();
		for parameter in &params {
			if !names.insert(parameter.name.to_string()) {
				return Err(syn::Error::new_spanned(&parameter.name, "duplicate parameter name"));
			}
		}

		Ok(Self {
			kind,
			condition,
			domain,
			subcondition,
			message,
			params,
			hint,
			constructor,
		})
	}
}

enum StatusKindInput {
	Completion,
	Notification {
		classification: Path,
		severity: Path,
	},
	Error {
		classification: Path,
		category: Path,
		severity: Path,
	},
}

impl StatusKindInput {
	fn tokens(&self) -> TokenStream {
		match self {
			Self::Completion => quote!(StatusKind::Completion),
			Self::Notification {
				classification,
				severity,
			} => {
				quote!(StatusKind::Notification {
					classification: #classification,
					severity: #severity,
				})
			}
			Self::Error {
				classification,
				category,
				severity,
			} => {
				quote!(StatusKind::Error {
					classification: #classification,
					category: #category,
					severity: #severity,
				})
			}
		}
	}
}

impl Parse for StatusKindInput {
	fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
		let name: Ident = input.parse()?;
		match name.to_string().as_str() {
			"Completion" => Ok(Self::Completion),
			"Notification" => {
				let content;
				syn::braced!(content in input);
				let mut classification = None;
				let mut severity = None;
				while !content.is_empty() {
					if content.peek(kw::classification) {
						content.parse::<kw::classification>()?;
						content.parse::<Token![:]>()?;
						classification = Some(content.parse()?);
					} else if content.peek(kw::severity) {
						content.parse::<kw::severity>()?;
						content.parse::<Token![:]>()?;
						severity = Some(content.parse()?);
					} else {
						return Err(content.error("unknown notification kind field"));
					}
					let _ = content.parse::<Token![,]>();
				}
				Ok(Self::Notification {
					classification: classification
						.ok_or_else(|| input.error("classification is required"))?,
					severity: severity.ok_or_else(|| input.error("severity is required"))?,
				})
			}
			"Error" => {
				let content;
				syn::braced!(content in input);
				let mut classification = None;
				let mut category = None;
				let mut severity = None;
				while !content.is_empty() {
					if content.peek(kw::classification) {
						content.parse::<kw::classification>()?;
						content.parse::<Token![:]>()?;
						classification = Some(content.parse()?);
					} else if content.peek(kw::category) {
						content.parse::<kw::category>()?;
						content.parse::<Token![:]>()?;
						category = Some(content.parse()?);
					} else if content.peek(kw::severity) {
						content.parse::<kw::severity>()?;
						content.parse::<Token![:]>()?;
						severity = Some(content.parse()?);
					} else {
						return Err(content.error("unknown error kind field"));
					}
					let _ = content.parse::<Token![,]>();
				}
				Ok(Self::Error {
					classification: classification
						.ok_or_else(|| input.error("classification is required"))?,
					category: category.ok_or_else(|| input.error("category is required"))?,
					severity: severity.ok_or_else(|| input.error("severity is required"))?,
				})
			}
			_ => Err(syn::Error::new_spanned(name, "expected Completion, Notification, or Error")),
		}
	}
}

struct StatusParameterDefinition {
	name: Ident,
	ty: Type,
	formatter: Type,
}

impl Parse for StatusParameterDefinition {
	fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
		let name = input.parse()?;
		input.parse::<Token![:]>()?;
		let ty = input.parse()?;
		input.parse::<Token![=>]>()?;
		let formatter = input.parse()?;
		Ok(Self {
			name,
			ty,
			formatter,
		})
	}
}

impl StatusParameterDefinition {
	fn constructor_part(&self) -> (TokenStream, TokenStream) {
		let name = &self.name;
		if is_string(&self.ty) {
			(quote!(#name: impl Into<String>), quote!(#name.into()))
		} else if is_vec_string(&self.ty) {
			(
				quote!(#name: impl IntoIterator<Item = impl Into<String>>),
				quote!(#name.into_iter().map(Into::into).collect()),
			)
		} else {
			let ty = &self.ty;
			(quote!(#name: #ty), quote!(#name))
		}
	}
}

fn validate_definitions(definitions: &[StatusDefinition]) -> syn::Result<()> {
	let mut names = HashSet::new();
	let mut codes = HashSet::new();
	let mut constructors = HashSet::new();

	for definition in definitions {
		if !names.insert(definition.name.to_string()) {
			return Err(syn::Error::new_spanned(&definition.name, "duplicate status variant"));
		}
		let code = definition.code.value();
		validate_code(&definition.code, &code)?;
		if !codes.insert(code.clone()) {
			return Err(syn::Error::new_spanned(&definition.code, "duplicate status code"));
		}
		let constructor = definition.constructor_name();
		if !constructors.insert(constructor.to_string()) {
			return Err(syn::Error::new_spanned(constructor, "duplicate generated constructor"));
		}
		validate_condition(definition, &code)?;
		validate_kind(definition)?;
		validate_message(definition)?;
	}
	Ok(())
}

fn validate_code(code_literal: &LitStr, code: &str) -> syn::Result<()> {
	let Some(body) = code.strip_prefix("FG-") else {
		return Err(syn::Error::new_spanned(code_literal, "status code must start with FG-"));
	};
	if body.len() != 5
		|| !body.bytes().all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
	{
		return Err(syn::Error::new_spanned(
			code_literal,
			"status code body must contain exactly five ASCII digits or uppercase letters",
		));
	}
	Ok(())
}

fn validate_condition(definition: &StatusDefinition, code: &str) -> syn::Result<()> {
	let body = &code[3..];
	let class = &body[..2];
	let expected = match class {
		"00" => "SuccessfulCompletion",
		"01" => "Warning",
		"02" => "NoData",
		"03" => "Informational",
		"22" => "DataException",
		"25" => "InvalidTransactionState",
		"40" => "TransactionRollback",
		"42" => "SyntaxErrorOrAccessRuleViolation",
		"50" => "GeneralProcessingException",
		"51" => "SystemConfigurationOrOperationException",
		"52" => "ProcedureException",
		"54" => "ProgramLimitExceeded",
		"57" => "InvalidTransactionTermination",
		"G1" => "DependentObjectError",
		"G2" => "GraphTypeViolation",
		_ => {
			return Err(syn::Error::new_spanned(&definition.code, "unsupported status code class"));
		}
	};
	let actual = last_path_ident(&definition.body.condition);
	if actual != expected {
		return Err(syn::Error::new_spanned(
			&definition.body.condition,
			format!("status code class {class} requires Condition::{expected}"),
		));
	}
	Ok(())
}

fn validate_kind(definition: &StatusDefinition) -> syn::Result<()> {
	let condition = last_path_ident(&definition.body.condition);
	match &definition.body.kind {
		StatusKindInput::Completion => {
			if !matches!(condition.as_str(), "SuccessfulCompletion" | "NoData") {
				return Err(syn::Error::new_spanned(
					&definition.body.condition,
					"completion status requires SuccessfulCompletion or NoData",
				));
			}
		}
		StatusKindInput::Notification {
			severity,
			..
		} => {
			if !matches!(condition.as_str(), "Warning" | "Informational" | "SuccessfulCompletion") {
				return Err(syn::Error::new_spanned(
					&definition.body.condition,
					"notification requires Warning, Informational, or SuccessfulCompletion",
				));
			}
			if !matches!(last_path_ident(severity).as_str(), "Information" | "Warning") {
				return Err(syn::Error::new_spanned(
					severity,
					"notification severity must be Information or Warning",
				));
			}
		}
		StatusKindInput::Error {
			severity,
			..
		} => {
			if matches!(
				condition.as_str(),
				"SuccessfulCompletion" | "NoData" | "Warning" | "Informational"
			) {
				return Err(syn::Error::new_spanned(
					&definition.body.condition,
					"error status requires an exception condition",
				));
			}
			if !matches!(last_path_ident(severity).as_str(), "Error" | "Critical") {
				return Err(syn::Error::new_spanned(
					severity,
					"error severity must be Error or Critical",
				));
			}
		}
	}
	Ok(())
}

fn validate_message(definition: &StatusDefinition) -> syn::Result<()> {
	let Some(message) = &definition.body.message else {
		return Ok(());
	};
	let template = message.value();
	let bytes = template.as_bytes();
	let declared = definition
		.body
		.params
		.iter()
		.map(|parameter| parameter.name.to_string())
		.collect::<HashSet<_>>();
	let mut used = HashSet::new();
	let mut index = 0;

	while index < bytes.len() {
		match bytes[index] {
			b'{' if bytes.get(index + 1) == Some(&b'{') => index += 2,
			b'}' if bytes.get(index + 1) == Some(&b'}') => index += 2,
			b'{' => {
				let tail = &template[index + 1..];
				let Some(relative_end) = tail.find('}') else {
					return Err(syn::Error::new_spanned(message, "unclosed message placeholder"));
				};
				let placeholder = &tail[..relative_end];
				let name = placeholder.split(':').next().unwrap_or_default().trim();
				if name.is_empty() || name.chars().all(|character| character.is_ascii_digit()) {
					return Err(syn::Error::new_spanned(
						message,
						"status messages require named placeholders",
					));
				}
				if !declared.contains(name) {
					return Err(syn::Error::new_spanned(
						message,
						format!("unknown message parameter `{name}`"),
					));
				}
				used.insert(name.to_owned());
				index += relative_end + 2;
			}
			b'}' => {
				return Err(syn::Error::new_spanned(message, "unmatched closing brace in message"));
			}
			_ => index += 1,
		}
	}

	if let Some(unused) = declared.difference(&used).next() {
		return Err(syn::Error::new_spanned(
			message,
			format!("unused message parameter `{unused}`"),
		));
	}
	Ok(())
}
fn option_lit(value: &Option<LitStr>) -> TokenStream {
	value.as_ref().map(|literal| quote!(Some(#literal))).unwrap_or_else(|| quote!(None))
}

fn last_path_ident(path: &Path) -> String {
	path.segments.last().expect("syn paths are non-empty").ident.to_string()
}

fn is_string(ty: &Type) -> bool {
	let Type::Path(path) = ty else {
		return false;
	};
	path.path.segments.last().is_some_and(|segment| segment.ident == "String")
}

fn is_vec_string(ty: &Type) -> bool {
	let Type::Path(path) = ty else {
		return false;
	};
	let Some(segment) = path.path.segments.last() else {
		return false;
	};
	if segment.ident != "Vec" {
		return false;
	}
	let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
		return false;
	};
	arguments.args.len() == 1
		&& arguments.args.first().is_some_and(
			|argument| matches!(argument, syn::GenericArgument::Type(ty) if is_string(ty)),
		)
}

fn to_snake_case(value: &str) -> String {
	let characters = value.chars().collect::<Vec<_>>();
	let mut output = String::new();
	for (index, character) in characters.iter().copied().enumerate() {
		if character.is_uppercase() {
			let previous_is_lower = index > 0 && characters[index - 1].is_lowercase();
			let next_is_lower =
				index + 1 < characters.len() && characters[index + 1].is_lowercase();
			if index > 0 && (previous_is_lower || next_is_lower) {
				output.push('_');
			}
			for lowercase in character.to_lowercase() {
				output.push(lowercase);
			}
		} else {
			output.push(character);
		}
	}
	output
}

pub fn define_status_codes(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
	let parsed = syn::parse_macro_input!(item as StatusInput);
	quote!(#parsed).into()
}

#[cfg(test)]
mod tests {
	use quote::quote;

	use super::*;

	#[test]
	fn accepts_named_parameter_used_more_than_once() {
		let input = quote! {
			Test, "FG-22F01" => {
				kind: Error {
					classification: ErrorClassification::ClientError,
					category: ErrorCategory::Runtime,
					severity: Severity::Error,
				},
				condition: Condition::DataException,
				domain: Domain::Query,
				message: "{value} and {value}",
				params: {
					value: String => DisplayValue,
				},
			}
		};
		assert!(syn::parse2::<StatusInput>(input).is_ok());
	}

	#[test]
	fn rejects_duplicate_codes() {
		let input = quote! {
			One, "FG-22F01" => {
				kind: Error {
					classification: ErrorClassification::ClientError,
					category: ErrorCategory::Runtime,
					severity: Severity::Error,
				},
				condition: Condition::DataException,
				domain: Domain::Query,
			},
			Two, "FG-22F01" => {
				kind: Error {
					classification: ErrorClassification::ClientError,
					category: ErrorCategory::Runtime,
					severity: Severity::Error,
				},
				condition: Condition::DataException,
				domain: Domain::Query,
			}
		};
		assert!(syn::parse2::<StatusInput>(input).is_err());
	}

	#[test]
	fn rejects_condition_mismatched_with_code_class() {
		let input = quote! {
			Test, "FG-42F01" => {
				kind: Error {
					classification: ErrorClassification::ClientError,
					category: ErrorCategory::Syntax,
					severity: Severity::Error,
				},
				condition: Condition::DataException,
				domain: Domain::Query,
			}
		};
		assert!(syn::parse2::<StatusInput>(input).is_err());
	}

	#[test]
	fn rejects_unknown_message_parameter() {
		let input = quote! {
			Test, "FG-22F01" => {
				kind: Error {
					classification: ErrorClassification::ClientError,
					category: ErrorCategory::Runtime,
					severity: Severity::Error,
				},
				condition: Condition::DataException,
				domain: Domain::Query,
				message: "{missing}",
				params: {
					value: String => DisplayValue,
				},
			}
		};
		assert!(syn::parse2::<StatusInput>(input).is_err());
	}

	#[test]
	fn rejects_unused_message_parameter() {
		let input = quote! {
			Test, "FG-22F01" => {
				kind: Error {
					classification: ErrorClassification::ClientError,
					category: ErrorCategory::Runtime,
					severity: Severity::Error,
				},
				condition: Condition::DataException,
				domain: Domain::Query,
				message: "no placeholders",
				params: {
					value: String => DisplayValue,
				},
			}
		};
		assert!(syn::parse2::<StatusInput>(input).is_err());
	}
}
