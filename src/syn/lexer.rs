use crate::gql_status::{ExecutionPhase, FerrosGrynnError, FerrosGrynnResult, Location, TextSpan};

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Token {
	pub kind: TokenKind,
	pub span: TextSpan,
	pub gap_before: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum TokenKind {
	Word {
		value: Box<str>,
		quoted: bool,
	},
	Parameter(Box<str>),
	Number(Box<str>),
	String(Box<str>),
	Symbol(Symbol),
	FullText(Box<str>),
	Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Symbol {
	LeftParen,
	RightParen,
	LeftBracket,
	RightBracket,
	LeftBrace,
	RightBrace,
	Comma,
	Semicolon,
	Colon,
	DoubleColon,
	Dot,
	Range,
	Question,
	QuestionDot,
	Coalesce,
	Elvis,
	Plus,
	Minus,
	Star,
	Power,
	Slash,
	Percent,
	Bang,
	DoubleBang,
	Equal,
	NotEqual,
	Less,
	LessOrEqual,
	Greater,
	GreaterOrEqual,
	QuestionEqual,
	StarEqual,
	And,
	Or,
	Pipe,
	Pipeline,
	ReversePipeline,
	ArrowRight,
	ArrowLeft,
	ArrowBoth,
	Tilde,
	TildeLeftBracket,
	LeftTildeLeftBracket,
	RightBracketTilde,
	RightBracketTildeRight,
	TildeRight,
	DoubleTilde,
}

pub(super) fn lex(source: &str) -> FerrosGrynnResult<Vec<Token>> {
	Lexer {
		source,
		cursor: 0,
	}
	.run()
}

struct Lexer<'a> {
	source: &'a str,
	cursor: usize,
}

impl Lexer<'_> {
	fn run(mut self) -> FerrosGrynnResult<Vec<Token>> {
		let mut tokens = Vec::new();
		loop {
			let gap_before = self.skip_gap()?;
			let start = self.cursor;
			let Some(character) = self.peek() else {
				tokens.push(Token {
					kind: TokenKind::Eof,
					span: span(start, start),
					gap_before,
				});
				return Ok(tokens);
			};

			let kind = if is_ident_start(character) {
				self.word(false)
			} else if character == '`' {
				TokenKind::Word {
					value: self.quoted_identifier()?,
					quoted: true,
				}
			} else if character == '$' {
				self.parameter()?
			} else if character.is_ascii_digit() {
				self.number()
			} else if matches!(character, '\'' | '"') {
				TokenKind::String(self.string(character)?)
			} else if character == '@' {
				self.full_text_or_symbol()?
			} else {
				TokenKind::Symbol(self.symbol()?)
			};

			tokens.push(Token {
				kind,
				span: span(start, self.cursor),
				gap_before,
			});
		}
	}

	fn skip_gap(&mut self) -> FerrosGrynnResult<bool> {
		let start = self.cursor;
		loop {
			while self.peek().is_some_and(char::is_whitespace) {
				self.bump();
			}
			if self.rest().starts_with("--") || self.rest().starts_with("//") {
				while self.peek().is_some_and(|value| value != '\n') {
					self.bump();
				}
				continue;
			}
			if self.rest().starts_with("/*") {
				let comment_start = self.cursor;
				self.cursor += 2;
				if let Some(end) = self.rest().find("*/") {
					self.cursor += end + 2;
					continue;
				}
				return Err(parse_error(
					self.source,
					span(comment_start, self.source.len()),
					"Unterminated block comment.",
				));
			}
			break;
		}
		Ok(self.cursor != start)
	}

	fn word(&mut self, quoted: bool) -> TokenKind {
		let start = self.cursor;
		self.bump();
		while self.peek().is_some_and(is_ident_continue) {
			self.bump();
		}
		TokenKind::Word {
			value: self.source[start..self.cursor].into(),
			quoted,
		}
	}

	fn quoted_identifier(&mut self) -> FerrosGrynnResult<Box<str>> {
		let start = self.cursor;
		self.bump();
		let mut value = String::new();
		loop {
			match self.bump() {
				Some('`') if self.peek() == Some('`') => {
					self.bump();
					value.push('`');
				}
				Some('`') => return Ok(value.into_boxed_str()),
				Some(character) => value.push(character),
				None => {
					return Err(parse_error(
						self.source,
						span(start, self.source.len()),
						"Unterminated quoted identifier.",
					));
				}
			}
		}
	}

	fn parameter(&mut self) -> FerrosGrynnResult<TokenKind> {
		let start = self.cursor;
		self.bump();
		let Some(next) = self.peek() else {
			return Err(parse_error(
				self.source,
				span(start, self.cursor),
				"Expected parameter name.",
			));
		};
		let value = if next == '`' {
			self.quoted_identifier()?
		} else if is_ident_start(next) {
			let name_start = self.cursor;
			self.bump();
			while self.peek().is_some_and(is_ident_continue) {
				self.bump();
			}
			self.source[name_start..self.cursor].into()
		} else {
			return Err(parse_error(
				self.source,
				span(start, self.cursor),
				"Expected parameter name.",
			));
		};
		Ok(TokenKind::Parameter(value))
	}

	fn number(&mut self) -> TokenKind {
		let start = self.cursor;
		while self.peek().is_some_and(|value| value.is_ascii_digit() || value == '_') {
			self.bump();
		}
		if self.peek() == Some('.') && !self.rest().starts_with("..") {
			self.bump();
			while self.peek().is_some_and(|value| value.is_ascii_digit() || value == '_') {
				self.bump();
			}
		}
		if self.peek().is_some_and(|value| matches!(value, 'e' | 'E')) {
			self.bump();
			if self.peek().is_some_and(|value| matches!(value, '+' | '-')) {
				self.bump();
			}
			while self.peek().is_some_and(|value| value.is_ascii_digit() || value == '_') {
				self.bump();
			}
		}
		if self.rest().starts_with("dec") {
			self.cursor += 3;
		}
		TokenKind::Number(self.source[start..self.cursor].into())
	}

	fn string(&mut self, quote: char) -> FerrosGrynnResult<Box<str>> {
		let start = self.cursor;
		self.bump();
		let mut value = String::new();
		loop {
			match self.bump() {
				Some(character) if character == quote => return Ok(value.into_boxed_str()),
				Some('\\') => value.push(self.escape(start)?),
				Some(character) => value.push(character),
				None => {
					return Err(parse_error(
						self.source,
						span(start, self.source.len()),
						"Unterminated string literal.",
					));
				}
			}
		}
	}

	fn escape(&mut self, start: usize) -> FerrosGrynnResult<char> {
		match self.bump() {
			Some('n') => Ok('\n'),
			Some('r') => Ok('\r'),
			Some('t') => Ok('\t'),
			Some('0') => Ok('\0'),
			Some('\\') => Ok('\\'),
			Some('\'') => Ok('\''),
			Some('"') => Ok('"'),
			Some('u') => {
				let digits_start = self.cursor;
				for _ in 0..4 {
					if self.bump().is_none() {
						return Err(parse_error(
							self.source,
							span(start, self.cursor),
							"Incomplete Unicode escape.",
						));
					}
				}
				let digits = &self.source[digits_start..self.cursor];
				u32::from_str_radix(digits, 16).ok().and_then(char::from_u32).ok_or_else(|| {
					parse_error(
						self.source,
						span(digits_start, self.cursor),
						"Invalid Unicode escape.",
					)
				})
			}
			Some(_) | None => {
				Err(parse_error(self.source, span(start, self.cursor), "Invalid string escape."))
			}
		}
	}

	fn full_text_or_symbol(&mut self) -> FerrosGrynnResult<TokenKind> {
		let start = self.cursor;
		self.bump();
		if self.peek() == Some('@') {
			self.bump();
			return Ok(TokenKind::FullText("@@".into()));
		}
		while self.peek().is_some_and(|value| value.is_ascii_alphanumeric()) {
			self.bump();
		}
		if self.peek() == Some('@') {
			self.bump();
			return Ok(TokenKind::FullText(self.source[start..self.cursor].into()));
		}
		Err(parse_error(self.source, span(start, self.cursor), "Invalid full-text operator."))
	}

	fn symbol(&mut self) -> FerrosGrynnResult<Symbol> {
		for (text, symbol) in [
			("<->", Symbol::ArrowBoth),
			("<~[", Symbol::LeftTildeLeftBracket),
			("]~>", Symbol::RightBracketTildeRight),
			("?.", Symbol::QuestionDot),
			("??", Symbol::Coalesce),
			("?:", Symbol::Elvis),
			("**", Symbol::Power),
			("!!", Symbol::DoubleBang),
			("!=", Symbol::NotEqual),
			("<=", Symbol::LessOrEqual),
			(">=", Symbol::GreaterOrEqual),
			("?=", Symbol::QuestionEqual),
			("*=", Symbol::StarEqual),
			("&&", Symbol::And),
			("||", Symbol::Or),
			("|>", Symbol::Pipeline),
			("<|", Symbol::ReversePipeline),
			("->", Symbol::ArrowRight),
			("<-", Symbol::ArrowLeft),
			("~>", Symbol::TildeRight),
			("~~", Symbol::DoubleTilde),
			("~[", Symbol::TildeLeftBracket),
			("]~", Symbol::RightBracketTilde),
			("::", Symbol::DoubleColon),
			("..", Symbol::Range),
		] {
			if self.rest().starts_with(text) {
				self.cursor += text.len();
				return Ok(symbol);
			}
		}
		let start = self.cursor;
		let symbol = match self.bump() {
			Some('(') => Symbol::LeftParen,
			Some(')') => Symbol::RightParen,
			Some('[') => Symbol::LeftBracket,
			Some(']') => Symbol::RightBracket,
			Some('{') => Symbol::LeftBrace,
			Some('}') => Symbol::RightBrace,
			Some(',') => Symbol::Comma,
			Some(';') => Symbol::Semicolon,
			Some(':') => Symbol::Colon,
			Some('.') => Symbol::Dot,
			Some('?') => Symbol::Question,
			Some('+') => Symbol::Plus,
			Some('-') => Symbol::Minus,
			Some('*') => Symbol::Star,
			Some('/') => Symbol::Slash,
			Some('%') => Symbol::Percent,
			Some('!') => Symbol::Bang,
			Some('=') => Symbol::Equal,
			Some('<') => Symbol::Less,
			Some('>') => Symbol::Greater,
			Some('|') => Symbol::Pipe,
			Some('~') => Symbol::Tilde,
			Some(_) | None => {
				return Err(parse_error(
					self.source,
					span(start, self.cursor),
					"Unexpected character.",
				));
			}
		};
		Ok(symbol)
	}

	fn rest(&self) -> &str {
		&self.source[self.cursor..]
	}

	fn peek(&self) -> Option<char> {
		self.rest().chars().next()
	}

	fn bump(&mut self) -> Option<char> {
		let character = self.peek()?;
		self.cursor += character.len_utf8();
		Some(character)
	}
}

fn is_ident_start(value: char) -> bool {
	value == '_' || value.is_alphabetic()
}

fn is_ident_continue(value: char) -> bool {
	is_ident_start(value) || value.is_numeric()
}

fn span(start: usize, end: usize) -> TextSpan {
	TextSpan::new(start as u32, (end - start) as u32)
}

fn parse_error(source: &str, span: TextSpan, message: &str) -> FerrosGrynnError {
	let error = FerrosGrynnError::query_parse_error(message).with_phase(ExecutionPhase::Parsing);
	Location::from_source(None, source, span)
		.map_or(error.clone(), |location| error.with_location(location))
}

#[cfg(test)]
mod tests {
	use super::{Symbol, TokenKind, lex};

	#[test]
	fn keeps_utf8_byte_spans_and_greedy_operators() {
		let tokens =
			lex("α /* gap */ |> <| -> <- <-> ~> ~~ ~[ ]~ <~[ ]~> @@ @12@").expect("valid tokens");
		assert_eq!(tokens[0].span.len, 2);
		assert!(tokens[1].gap_before);
		assert_eq!(tokens[1].kind, TokenKind::Symbol(Symbol::Pipeline));
		assert_eq!(tokens[5].kind, TokenKind::Symbol(Symbol::ArrowBoth));
		assert_eq!(tokens[8].kind, TokenKind::Symbol(Symbol::TildeLeftBracket));
		assert_eq!(tokens[9].kind, TokenKind::Symbol(Symbol::RightBracketTilde));
		assert_eq!(tokens[10].kind, TokenKind::Symbol(Symbol::LeftTildeLeftBracket));
		assert_eq!(tokens[11].kind, TokenKind::Symbol(Symbol::RightBracketTildeRight));
		assert_eq!(tokens[12].kind, TokenKind::FullText("@@".into()));
		assert_eq!(tokens[13].kind, TokenKind::FullText("@12@".into()));
	}

	#[test]
	fn decodes_quoted_names_parameters_and_string_escapes() {
		let tokens = lex("`им``я` $параметр 'line\\n\\u03b1'").expect("valid tokens");
		assert_eq!(tokens[0].kind, TokenKind::Word {
			value: "им`я".into(),
			quoted: true,
		});
		assert_eq!(tokens[1].kind, TokenKind::Parameter("параметр".into()));
		assert_eq!(tokens[2].kind, TokenKind::String("line\nα".into()));
	}
}
