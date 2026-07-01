//! Abstract syntax tree for the Ferrix source language.
//!
//! Parser output is intentionally close to the user-written program: every
//! statement and expression carries a source span so later compiler stages can
//! report diagnostics that point back to the original file.

use ferrix_core::diagnostics::SourceSpan;

/// Root node for one parsed source file or linked module set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramAst {
    /// Top-level statements in source order.
    pub statements: Vec<Stmt>,
}

/// Ferrix statement forms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stmt {
    /// `import name;` declaration used by the CLI module loader.
    Import { module: String, span: SourceSpan },
    /// `let name = expr;` local binding.
    Let {
        name: String,
        initializer: Expr,
        span: SourceSpan,
    },
    /// Top-level function declaration.
    Function {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
        span: SourceSpan,
    },
    /// Assignment to an existing local variable.
    Assign {
        name: String,
        value: Expr,
        span: SourceSpan,
    },
    /// Assignment through an array/map index expression.
    IndexAssign {
        target: Expr,
        index: Expr,
        value: Expr,
        span: SourceSpan,
    },
    /// `return expr;` from the current function or main chunk.
    Return { value: Expr, span: SourceSpan },
    /// Conditional statement with optional else branch.
    If {
        condition: Expr,
        then_branch: Vec<Stmt>,
        else_branch: Vec<Stmt>,
        span: SourceSpan,
    },
    /// Loop statement with a boolean condition.
    While {
        condition: Expr,
        body: Vec<Stmt>,
        span: SourceSpan,
    },
    /// Braced statement sequence.
    Block {
        statements: Vec<Stmt>,
        span: SourceSpan,
    },
    /// Expression used only for its side effects.
    Expr { value: Expr, span: SourceSpan },
}

/// Ferrix expression forms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    /// Integer, boolean, string, or nil literal.
    Literal { value: Literal, span: SourceSpan },
    /// Local variable or namespaced function alias.
    Variable { name: String, span: SourceSpan },
    /// Binary operator expression.
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: SourceSpan,
    },
    /// Function call with positional arguments.
    Call {
        callee: String,
        args: Vec<Expr>,
        span: SourceSpan,
    },
    /// Anonymous function expression that can capture outer locals.
    Function {
        params: Vec<String>,
        body: Vec<Stmt>,
        span: SourceSpan,
    },
    /// Array/map indexing expression.
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
        span: SourceSpan,
    },
    /// Array literal.
    Array {
        elements: Vec<Expr>,
        span: SourceSpan,
    },
    /// Map literal represented as key/value expression pairs.
    Map {
        entries: Vec<(Expr, Expr)>,
        span: SourceSpan,
    },
    /// Parenthesized expression used to preserve source span.
    Grouping { expr: Box<Expr>, span: SourceSpan },
}

/// Literal values accepted by the parser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Literal {
    Int(i64),
    Bool(bool),
    String(String),
    Nil,
}

/// Binary operators ordered by parser precedence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

impl Expr {
    /// Returns the source span covered by this expression.
    pub fn span(&self) -> SourceSpan {
        match self {
            Self::Literal { span, .. }
            | Self::Variable { span, .. }
            | Self::Binary { span, .. }
            | Self::Call { span, .. }
            | Self::Function { span, .. }
            | Self::Index { span, .. }
            | Self::Array { span, .. }
            | Self::Map { span, .. }
            | Self::Grouping { span, .. } => *span,
        }
    }
}
