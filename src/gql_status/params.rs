use itertools::Itertools;
use paste::paste;
use std::fmt::Display;
use std::sync::Arc;

macro_rules! simple_param_type {
    ($name:ident; $($var:ident => $pr:expr),+) => {
		#[derive(magic_utils::IntoStaticStr)]
		pub enum $name {
			$($var,)+
		}
		impl $name {
			pub fn name(&self) -> &'static str {
				self.into()
			}
			pub fn process(&self, s: &dyn Display) -> String {
				// match self {
				// 	$($name::$var => $pr.process(s)),+
				// }
				self.processor().process(s) // does compiler optimize this???
			}
			fn processor(&self) -> Arc<dyn Processor> {
				match self {
					$($name::$var => Arc::new($pr),)+
				}
			}
		}
	};
}

macro_rules! list_param_type {
    ($name:ident; $($var:ident),+) => {
		paste!{
			#[derive(magic_utils::IntoStaticStr)]
			pub enum $name {
				$([<$var List>],)+
			}
			impl $name {
				pub fn name(&self) -> &'static str {
					self.into()
				}
				fn processor(&self) -> Arc<NeList> {
					match self {
						$($name::[<$var List>] => Arc::new(NeList::default().with_inner(StringParam::$var.processor()).to_owned()),)+
					}
				}
			}
			impl HasJoinStyle for $name {
				fn process_list<T: Display>(&self, vec: &Vec<T>, join_style: JoinStyle) -> String {
					self.processor().process_list(vec, join_style)
				}
			}
		}
	};
}

simple_param_type!(
	StringParam;
	AtomType => Ident::default(),
	Component => StrLit::default(),
	Id => Verbatim::default(),
	Ident => Ident::default(),
	Field => Ident::default(),
	Message => Verbatim::default(),
	Operation => StrLit::default(),
	Value => Val::default(),
	ValueType => ValType::default()
	// Action => Verbatim::default(),
	// Alias => Ident::default(),
	// Alias1 => Ident::default(),
	// Alias2 => Ident::default(),
	// Alloc => Ident::default(),
	// AllocType => StrLit::default(),
	// Auth => Ident::default(),
	// BoltServerState => StrLit::default(),
	// Cause => Verbatim::default(),
	// CfgSetting => Verbatim::default(),
	// ChangeIdent => Ident::default(),
	// CharRange => CharRange::default(),
	// Clause => Upper::default().with_inner(Arc::new(Verbatim::default())).to_owned(),
	// Cmd => StrLit::default(),
	// Component => StrLit::default(),
	// Constr => Ident::default(),
	// ConstrDescriptionOrName => StrLit::default(),
	// Context => Verbatim::default(),
	// Coordinates => Coordinates::default(),
	// Crs => Verbatim::default(),
	// Db => Ident::default(),
	// Db1 => Ident::default(),
	// Db2 => Ident::default(),
	// Db3 => Ident::default(),
	// Edition => Verbatim::default(),
	// EntityId1 => StrLit::default(),
	// EntityId2 => StrLit::default(),
	// EntityType => Verbatim::default(),
	// Expr => StrLit::default(),
	// ExprType => Verbatim::default(),
	// Feat => Verbatim::default(),
	// Feat1 => Verbatim::default(),
	// Feat2 => Verbatim::default(),
	// Field => Ident::default(),
	// Format => StrLit::default(),
	// Fun => CallableIdent::default(),
	// Graph => Ident::default(),
	// Hint => Verbatim::default(),
	// Ident => Ident::default(),
	// Idx => Ident::default(),
	// IdxDescription => StrLit::default(),
	// IdxDescriptionOrName => StrLit::default(),
	// IdxOrConstr => Ident::default(),
	// IdxOrConstrPat => StrLit::default(),
	// IdxType => Verbatim::default(),
	// Input => StrLit::default(),
	// Input1 => StrLit::default(),
	// Input2 => StrLit::default(),
	// Item => Verbatim::default(),
	// Keyword => StrLit::default(),
	// Label => Ident::default(),
	// LabelExpr => StrLit::default(),
	// MapKey => StrLit::default(),
	// MatchMode => Verbatim::default(),
	// Msg => Verbatim::default(),
	// MsgTitle => Verbatim::default(),
	// Namespace => Ident::default(),
	// Operation => StrLit::default(),
	// Option => StrLit::default(),
	// Option1 => StrLit::default(),
	// Option2 => StrLit::default(),
	// Param => Param::default(),
	// Param1 => Param::default(),
	// Param2 => Param::default(),
	// Pat => StrLit::default(),
	// Pred => StrLit::default(),
	// PreparserInput => StrLit::default(),
	// PreparserInput1 => StrLit::default(),
	// PreparserInput2 => StrLit::default(),
	// Port => Ident::default(),
	// Proc => CallableIdent::default(),
	// ProcClass => Ident::default(),
	// ProcExeMode => StrLit::default(),
	// ProcField => Ident::default(),
	// ProcFieldType => StrLit::default(),
	// ProcFun => CallableIdent::default(),
	// ProcMethod => Ident::default(),
	// ProcParam => Ident::default(),
	// ProcParamFmt => Verbatim::default(),
	// PropKey => Ident::default(),
	// Query => StrLit::default(),
	// RelType => Ident::default(),
	// Replacement => StrLit::default(),
	// Role => Ident::default(),
	// RoutingPolicy => StrLit::default(),
	// Runtime => StrLit::default(),
	// SchemaDescription => StrLit::default(),
	// Selector => Verbatim::default(),
	// SelectorType => StrLit::default(),
	// SelectorType1 => StrLit::default(),
	// SelectorType2 => StrLit::default(),
	// Server => StrLit::default(),
	// ServerType => StrLit::default(),
	// Sig => Verbatim::default(),
	// Syntax => Ident::default(),
	// Temporal => Temporal::default(),
	// TimeUnit => Ident::default(),
	// Token => StrLit::default(),
	// TokenId => StrLit::default(),
	// TokenType => Verbatim::default(),
	// TransactionId => StrLit::default(),
	// TransactionId1 => StrLit::default(),
	// TransactionId2 => StrLit::default(),
	// Url => Verbatim::default(),
	// User => Ident::default(),
	// Value => Val::default(),
	// ValueType => ValType::default(),
	// Variable => Ident::default()
);

simple_param_type!(
	NumberParam;
	Lower => Num::default(),
	Upper => Num::default()

	// BoltMsgLenLimit => Num::default(),
	// Count => NonNeg::default(),
	// Count1 => NonNeg::default(),
	// Count2 => NonNeg::default(),
	// CountAllocs => Num::default(),
	// CountSeeders => Num::default(),
	// Dim1 => NonNeg::default(),
	// Dim2 => NonNeg::default(),
	// EntityId => StrLit::default(),

	// Pos => Num::default(),
	// TimeAmount => Num::default(),

	// Value => Val::default()
);

// simple_param_type!(
// 	BoolParam;
// 	// Value => Val::default()
// );
//
// list_param_type!(
// 	ListParam;
// 	// CharRange,
// 	// Hint,
// 	// Input,
// 	// Label,
// 	// MapKey,
// 	// Namespace,
// 	// Option,
// 	// Param,
// 	// Port,
// 	// Pred,
// 	// PropKey,
// 	// Server,
// 	// Value,
// 	// ValueType,
// 	// Variable
// );

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum JoinStyle {
	ANDED,
	ORED,
	COMMAD,
}

pub trait HasJoinStyle {
	fn process_list<T: Display>(&self, vec: &Vec<T>, join_style: JoinStyle) -> String;
}

pub trait Processor: Send + Sync {
	fn process(&self, t: &dyn Display) -> String {
		t.to_string()
	}
	fn with_inner(&mut self, inner: Arc<dyn Processor>) -> &mut Self
	where
		Self: Sized,
	{
		self.set_inner(Some(inner));
		self
	}
	fn set_inner(&mut self, _inner: Option<Arc<dyn Processor>>) {}
}

macro_rules! impl_processor {
    ($($st:ident),+) => {$(
		#[derive(Default, Clone)]
		pub struct $st {
			inner: Option<Arc<dyn Processor>>,
		}
		impl Processor for $st {
			fn set_inner(&mut self, inner: Option<Arc<dyn Processor>>) {
				self.inner = inner;
			}
		}
	)+};
	($({$st:ident, $e:expr}),+) => {$(
		#[derive(Default, Clone)]
		pub struct $st {
			inner: Option<Arc<dyn Processor>>,
		}
		impl Processor for $st {
			fn process(&self, t: &dyn Display) -> String {
				($e)(t)
			}
			fn set_inner(&mut self, inner: Option<Arc<dyn Processor>>) {
				self.inner = inner;
			}
		}
	)+};
}

impl_processor!(Verbatim, Val, ValType, Num, NonNeg, Temporal, Coordinates, Bool);

impl_processor!(
	{Ident, |t| format!("`{t}`")},
	{CallableIdent, |t| format!("{t}()")},
	{StrLit, |t| format!("'{t}'")},
	{Param, |t| format!("$`{t}`")},
	{CharRange, |t| format!("`{t}`")}
);

#[derive(Default, Clone)]
pub struct Upper {
	inner: Option<Arc<dyn Processor>>,
}
impl Processor for Upper {
	fn process(&self, t: &dyn Display) -> String {
		let mut val = t.to_string();
		if let Some(inner) = &self.inner {
			val = inner.process(t);
		}
		val.to_uppercase()
	}
	fn set_inner(&mut self, inner: Option<Arc<dyn Processor>>) {
		self.inner = inner;
	}
}

pub trait ListProcessor: HasJoinStyle {
	fn list_process<T: Display>(
		&self,
		vec: &Vec<T>,
		join_style: Option<JoinStyle>,
		inner: Option<&dyn Processor>,
	) -> String {
		if let Some(inner) = inner {
			let processed = vec.iter().map(|v| inner.process(v)).collect_vec();
			Self::format_vec(&processed, join_style)
		} else {
			Self::format_vec(vec, join_style)
		}
	}

	fn format_vec<T: Display>(vec: &Vec<T>, join_style: Option<JoinStyle>) -> String {
		match join_style {
			None => Self::commad_format(vec),
			Some(j) => match j {
				JoinStyle::ANDED => Self::anded_format(vec),
				JoinStyle::ORED => Self::ored_format(vec),
				JoinStyle::COMMAD => Self::commad_format(vec),
			},
		}
	}

	fn ored_format<T: Display>(vec: &Vec<T>) -> String {
		if vec.len() == 0 {
			"".to_string()
		} else if vec.len() == 1 {
			vec[0].to_string()
		} else {
			let init = Self::initial_commas(vec);
			format!("{} or {}", init, vec[vec.len() - 1])
		}
	}

	fn anded_format<T: Display>(vec: &Vec<T>) -> String {
		if vec.len() == 0 {
			"".to_string()
		} else if vec.len() == 1 {
			vec[0].to_string()
		} else {
			let init = Self::initial_commas(vec);
			format!("{} and {}", init, vec[vec.len() - 1])
		}
	}

	fn commad_format<T: Display>(vec: &Vec<T>) -> String {
		if vec.len() == 0 {
			"".to_string()
		} else if vec.len() == 1 {
			vec[0].to_string()
		} else {
			vec.iter().join(", ")
		}
	}

	fn initial_commas<T: Display>(vec: &Vec<T>) -> String {
		vec.iter().take(vec.len() - 2).join(", ")
	}
}

#[derive(Default, Clone)]
pub struct NeList {
	inner: Option<Arc<dyn Processor>>,
}
impl NeList {
	pub fn with_inner(&mut self, inner: Arc<dyn Processor>) -> &mut Self {
		self.set_inner(Some(inner));
		self
	}
	fn set_inner(&mut self, inner: Option<Arc<dyn Processor>>) {
		self.inner = inner;
	}
}
impl HasJoinStyle for NeList {
	fn process_list<T: Display>(&self, vec: &Vec<T>, join_style: JoinStyle) -> String {
		self.list_process(vec, Some(join_style), self.inner.as_deref())
	}
}
impl ListProcessor for NeList {}
