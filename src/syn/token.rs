use crate::gql_status::TextSpan;

#[derive(Debug, Clone, PartialEq)]
/// Token state.
pub(super) struct Token {
	/// Kind associated with this token.
	pub kind: TokenKind,
	/// Source byte range used to locate diagnostics.
	pub span: TextSpan,
	/// Gap before associated with this token.
	pub gap_before: bool,
}

#[derive(Debug, Clone, PartialEq)]
/// Token kind alternatives.
pub(super) enum TokenKind {
	/// Word case of token kind.
	Word {
		/// Value associated with this token kind.
		value: Box<str>,
		/// Quoted associated with this token kind.
		quoted: bool,
	},
	/// Parameter case of token kind.
	Parameter(
		/// Parameter case of token kind.
		Box<str>,
	),
	/// Graph ref case of token kind.
	GraphRef(
		/// Graph ref case of token kind.
		Box<str>,
	),
	/// Number case of token kind.
	Number(
		/// Number case of token kind.
		Box<str>,
	),
	/// String case of token kind.
	String(
		/// String case of token kind.
		Box<str>,
	),
	/// Symbol case of token kind.
	Symbol(
		/// Symbol case of token kind.
		Symbol,
	),
	/// Full text case of token kind.
	FullText(
		/// Full text case of token kind.
		Box<str>,
	),
	/// Eof case of token kind.
	Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Symbol alternatives.
pub(super) enum Symbol {
	/// `LeftParen` operator or delimiter token.
	LeftParen,
	/// `RightParen` operator or delimiter token.
	RightParen,
	/// `LeftBracket` operator or delimiter token.
	LeftBracket,
	/// `RightBracket` operator or delimiter token.
	RightBracket,
	/// `LeftBrace` operator or delimiter token.
	LeftBrace,
	/// `RightBrace` operator or delimiter token.
	RightBrace,
	/// `Comma` operator or delimiter token.
	Comma,
	/// `Semicolon` operator or delimiter token.
	Semicolon,
	/// `Colon` operator or delimiter token.
	Colon,
	/// `DoubleColon` operator or delimiter token.
	DoubleColon,
	/// `Dot` operator or delimiter token.
	Dot,
	/// `Range` operator or delimiter token.
	Range,
	/// `Question` operator or delimiter token.
	Question,
	/// `QuestionDot` operator or delimiter token.
	QuestionDot,
	/// `Coalesce` operator or delimiter token.
	Coalesce,
	/// `Elvis` operator or delimiter token.
	Elvis,
	/// `Plus` operator or delimiter token.
	Plus,
	/// `Minus` operator or delimiter token.
	Minus,
	/// `Star` operator or delimiter token.
	Star,
	/// `Power` operator or delimiter token.
	Power,
	/// `Slash` operator or delimiter token.
	Slash,
	/// `Percent` operator or delimiter token.
	Percent,
	/// `Bang` operator or delimiter token.
	Bang,
	/// `DoubleBang` operator or delimiter token.
	DoubleBang,
	/// `Equal` operator or delimiter token.
	Equal,
	/// `NotEqual` operator or delimiter token.
	NotEqual,
	/// `Less` operator or delimiter token.
	Less,
	/// `LessOrEqual` operator or delimiter token.
	LessOrEqual,
	/// `Greater` operator or delimiter token.
	Greater,
	/// `GreaterOrEqual` operator or delimiter token.
	GreaterOrEqual,
	/// `QuestionEqual` operator or delimiter token.
	QuestionEqual,
	/// `StarEqual` operator or delimiter token.
	StarEqual,
	/// `And` operator or delimiter token.
	And,
	/// `Or` operator or delimiter token.
	Or,
	/// `Pipe` operator or delimiter token.
	Pipe,
	/// `Pipeline` operator or delimiter token.
	Pipeline,
	/// `ReversePipeline` operator or delimiter token.
	ReversePipeline,
	/// `ArrowRight` operator or delimiter token.
	ArrowRight,
	/// `ArrowLeft` operator or delimiter token.
	ArrowLeft,
	/// `ArrowBoth` operator or delimiter token.
	ArrowBoth,
	/// `Tilde` operator or delimiter token.
	Tilde,
	/// `TildeLeftBracket` operator or delimiter token.
	TildeLeftBracket,
	/// `LeftTildeLeftBracket` operator or delimiter token.
	LeftTildeLeftBracket,
	/// `RightBracketTilde` operator or delimiter token.
	RightBracketTilde,
	/// `RightBracketTildeRight` operator or delimiter token.
	RightBracketTildeRight,
	/// `TildeRight` operator or delimiter token.
	TildeRight,
	/// `DoubleTilde` operator or delimiter token.
	DoubleTilde,
}
