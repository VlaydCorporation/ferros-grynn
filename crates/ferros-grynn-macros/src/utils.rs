use syn::Type;

pub fn is_option(ty: &Type) -> bool {
	match ty {
		Type::Path(path) => path
			.path
			.segments
			.last()
			.map(|seg| seg.ident.to_string().contains("Option"))
			.unwrap_or(false),
		_ => false,
	}
}
