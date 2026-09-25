use crate::gql_status::TextSpan;

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
	GraphRef(Box<str>),
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
