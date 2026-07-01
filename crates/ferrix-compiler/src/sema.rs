//! Semantic analysis for Ferrix ASTs.
//!
//! This pass validates names and arity before bytecode generation. Variables
//! are resolved through lexical scopes so block-local bindings can shadow outer
//! names without leaking after the block exits.

use std::collections::HashMap;

use crate::{
    ast::{BinaryOp, Expr, Literal, ProgramAst, Stmt},
    error::{CompileError, CompileErrorKind},
};

const BUILTIN_FUNCTIONS: &[(&str, usize)] = &[("print", 1), ("len", 1), ("type_of", 1)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceType {
    Unknown,
    Nil,
    Bool,
    Int,
    String,
    Array,
    Map,
    Function,
}

impl SourceType {
    fn name(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Nil => "nil",
            Self::Bool => "bool",
            Self::Int => "int",
            Self::String => "string",
            Self::Array => "array",
            Self::Map => "map",
            Self::Function => "function",
        }
    }

    fn is_known(self) -> bool {
        !matches!(self, Self::Unknown | Self::Nil)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FunctionSignature {
    arity: usize,
    return_type: SourceType,
}

#[derive(Clone, Debug)]
struct ScopeStack {
    scopes: Vec<HashMap<String, SourceType>>,
}

impl ScopeStack {
    fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes
            .pop()
            .expect("semantic analyzer always keeps one active scope");
    }

    fn contains(&self, name: &str) -> bool {
        self.resolve(name).is_some()
    }

    fn contains_current(&self, name: &str) -> bool {
        self.scopes
            .last()
            .expect("semantic analyzer always has a current scope")
            .contains_key(name)
    }

    fn declare(&mut self, name: String, source_type: SourceType) {
        self.scopes
            .last_mut()
            .expect("semantic analyzer always has a current scope")
            .insert(name, source_type);
    }

    fn assign(&mut self, name: &str, source_type: SourceType) {
        if let Some((_, existing_type)) = self
            .scopes
            .iter_mut()
            .rev()
            .find_map(|scope| scope.get_mut(name).map(|source_type| ((), source_type)))
        {
            *existing_type = merge_assignment_type(*existing_type, source_type);
        }
    }

    fn resolve(&self, name: &str) -> Option<SourceType> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }
}

/// Runs semantic checks for a program without module aliases.
pub fn analyze(program: &ProgramAst) -> Result<(), CompileError> {
    analyze_with_function_aliases(program, &[])
}

/// Runs semantic checks while allowing namespaced aliases to point at functions.
pub fn analyze_with_function_aliases(
    program: &ProgramAst,
    aliases: &[(String, String)],
) -> Result<(), CompileError> {
    let functions = collect_functions(program, aliases)?;
    let mut scopes = ScopeStack::new();

    for stmt in &program.statements {
        if !matches!(stmt, Stmt::Function { .. }) {
            check_stmt(stmt, &mut scopes, &functions)?;
        }
    }

    for stmt in &program.statements {
        if let Stmt::Function {
            params, body, span, ..
        } = stmt
        {
            let mut function_scopes = ScopeStack::new();
            declare_parameters(params, *span, &mut function_scopes)?;
            check_scoped_statements(body, &mut function_scopes, &functions)?;
        }
    }

    Ok(())
}

fn collect_functions(
    program: &ProgramAst,
    aliases: &[(String, String)],
) -> Result<HashMap<String, FunctionSignature>, CompileError> {
    let mut functions = BUILTIN_FUNCTIONS
        .iter()
        .map(|(name, arity)| {
            (
                (*name).to_string(),
                FunctionSignature {
                    arity: *arity,
                    return_type: builtin_return_type(name),
                },
            )
        })
        .collect::<HashMap<_, _>>();

    for stmt in &program.statements {
        if let Stmt::Function {
            name, params, span, ..
        } = stmt
            && functions
                .insert(
                    name.clone(),
                    FunctionSignature {
                        arity: params.len(),
                        return_type: SourceType::Unknown,
                    },
                )
                .is_some()
        {
            return Err(CompileError::new(
                CompileErrorKind::DuplicateFunction { name: name.clone() },
                Some(*span),
            ));
        }
    }

    for (alias, target) in aliases {
        if let Some(signature) = functions.get(target).copied() {
            functions.entry(alias.clone()).or_insert(signature);
        }
    }

    Ok(functions)
}

fn builtin_return_type(name: &str) -> SourceType {
    match name {
        "len" => SourceType::Int,
        "type_of" => SourceType::String,
        "print" => SourceType::Nil,
        _ => SourceType::Unknown,
    }
}

fn check_scoped_statements(
    stmts: &[Stmt],
    scopes: &mut ScopeStack,
    functions: &HashMap<String, FunctionSignature>,
) -> Result<(), CompileError> {
    scopes.push_scope();
    let result = check_statements(stmts, scopes, functions);
    scopes.pop_scope();
    result
}

fn check_statements(
    stmts: &[Stmt],
    scopes: &mut ScopeStack,
    functions: &HashMap<String, FunctionSignature>,
) -> Result<(), CompileError> {
    for stmt in stmts {
        check_stmt(stmt, scopes, functions)?;
    }

    Ok(())
}

fn check_stmt(
    stmt: &Stmt,
    scopes: &mut ScopeStack,
    functions: &HashMap<String, FunctionSignature>,
) -> Result<(), CompileError> {
    match stmt {
        Stmt::Import { .. } | Stmt::Function { .. } => Ok(()),
        Stmt::Let {
            name,
            initializer,
            span,
            ..
        } => check_let(name, initializer, *span, scopes, functions),
        Stmt::Assign { name, value, span } => {
            if !scopes.contains(name) {
                return Err(undefined_name_error(name, *span, NameUse::Variable));
            }
            let expected = scopes.resolve(name).unwrap_or(SourceType::Unknown);
            let found = check_expr(value, scopes, functions)?;
            expect_assignable(expected, found, value.span())?;
            scopes.assign(name, found);
            Ok(())
        }
        Stmt::IndexAssign {
            target,
            index,
            value,
            ..
        } => {
            let target_type = check_expr(target, scopes, functions)?;
            let index_type = check_expr(index, scopes, functions)?;
            check_index_access(target_type, index_type, target.span(), index.span())?;
            check_expr(value, scopes, functions)?;
            Ok(())
        }
        Stmt::Return { value, .. } | Stmt::Throw { value, .. } | Stmt::Expr { value, .. } => {
            check_expr(value, scopes, functions).map(|_| ())
        }
        Stmt::TryCatch {
            try_branch,
            catch_name,
            catch_branch,
            ..
        } => {
            check_scoped_statements(try_branch, scopes, functions)?;
            scopes.push_scope();
            scopes.declare(catch_name.clone(), SourceType::Unknown);
            let result = check_statements(catch_branch, scopes, functions);
            scopes.pop_scope();
            result
        }
        Stmt::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            let condition_type = check_expr(condition, scopes, functions)?;
            expect_type(SourceType::Bool, condition_type, condition.span())?;
            check_scoped_statements(then_branch, scopes, functions)?;
            check_scoped_statements(else_branch, scopes, functions)
        }
        Stmt::While {
            condition, body, ..
        } => {
            let condition_type = check_expr(condition, scopes, functions)?;
            expect_type(SourceType::Bool, condition_type, condition.span())?;
            check_scoped_statements(body, scopes, functions)
        }
        Stmt::Block { statements, .. } => check_scoped_statements(statements, scopes, functions),
    }
}

fn check_let(
    name: &str,
    initializer: &Expr,
    span: ferrix_core::diagnostics::SourceSpan,
    scopes: &mut ScopeStack,
    functions: &HashMap<String, FunctionSignature>,
) -> Result<(), CompileError> {
    if scopes.contains_current(name) {
        return Err(CompileError::new(
            CompileErrorKind::DuplicateVariable {
                name: name.to_string(),
            },
            Some(span),
        ));
    }

    if matches!(initializer, Expr::Function { .. }) {
        scopes.declare(name.to_string(), SourceType::Function);
        check_expr(initializer, scopes, functions).map(|_| ())
    } else {
        let source_type = check_expr(initializer, scopes, functions)?;
        scopes.declare(name.to_string(), source_type);
        Ok(())
    }
}

fn declare_parameters(
    params: &[String],
    span: ferrix_core::diagnostics::SourceSpan,
    scopes: &mut ScopeStack,
) -> Result<(), CompileError> {
    if params.len() > u8::MAX as usize {
        return Err(CompileError::new(
            CompileErrorKind::TooManyParameters {
                max: u8::MAX as usize,
            },
            Some(span),
        ));
    }

    for param in params {
        if scopes.contains_current(param) {
            return Err(CompileError::new(
                CompileErrorKind::DuplicateParameter {
                    name: param.clone(),
                },
                Some(span),
            ));
        }
        scopes.declare(param.clone(), SourceType::Unknown);
    }

    Ok(())
}

fn check_expr(
    expr: &Expr,
    scopes: &mut ScopeStack,
    functions: &HashMap<String, FunctionSignature>,
) -> Result<SourceType, CompileError> {
    match expr {
        Expr::Literal { value, .. } => Ok(literal_type(value)),
        Expr::Variable { name, span } => scopes
            .resolve(name)
            .ok_or_else(|| undefined_name_error(name, *span, NameUse::Variable)),
        Expr::Binary {
            op,
            lhs,
            rhs,
            span: _,
        } => {
            let lhs_type = check_expr(lhs, scopes, functions)?;
            let rhs_type = check_expr(rhs, scopes, functions)?;
            check_binary(*op, lhs_type, rhs_type, lhs.span(), rhs.span())
        }
        Expr::Array { elements, .. } => {
            if elements.len() > u8::MAX as usize {
                return Err(CompileError::new(
                    CompileErrorKind::TooManyArrayElements {
                        max: u8::MAX as usize,
                    },
                    Some(expr.span()),
                ));
            }

            for element in elements {
                check_expr(element, scopes, functions)?;
            }
            Ok(SourceType::Array)
        }
        Expr::Map { entries, .. } => {
            if entries.len() > u8::MAX as usize {
                return Err(CompileError::new(
                    CompileErrorKind::TooManyMapEntries {
                        max: u8::MAX as usize,
                    },
                    Some(expr.span()),
                ));
            }

            for (key, value) in entries {
                check_expr(key, scopes, functions)?;
                check_expr(value, scopes, functions)?;
            }
            Ok(SourceType::Map)
        }
        Expr::Index { target, index, .. } => {
            let target_type = check_expr(target, scopes, functions)?;
            let index_type = check_expr(index, scopes, functions)?;
            check_index_access(target_type, index_type, target.span(), index.span())?;
            Ok(SourceType::Unknown)
        }
        Expr::Call { callee, args, span } => {
            if args.len() > u8::MAX as usize {
                return Err(CompileError::new(
                    CompileErrorKind::TooManyArguments {
                        max: u8::MAX as usize,
                    },
                    Some(*span),
                ));
            }

            if let Some(callee_type) = scopes.resolve(callee) {
                expect_type(SourceType::Function, callee_type, *span)?;
                for arg in args {
                    check_expr(arg, scopes, functions)?;
                }
                return Ok(SourceType::Unknown);
            }

            let signature = functions
                .get(callee)
                .copied()
                .ok_or_else(|| undefined_name_error(callee, *span, NameUse::Function))?;
            if signature.arity != args.len() {
                return Err(CompileError::new(
                    CompileErrorKind::WrongCallArity {
                        name: callee.clone(),
                        expected: signature.arity,
                        actual: args.len(),
                    },
                    Some(*span),
                ));
            }

            for arg in args {
                check_expr(arg, scopes, functions)?;
            }
            Ok(signature.return_type)
        }
        Expr::Function { params, body, span } => {
            let mut function_scopes = scopes.clone();
            function_scopes.push_scope();
            declare_parameters(params, *span, &mut function_scopes)?;
            check_scoped_statements(body, &mut function_scopes, functions)?;
            Ok(SourceType::Function)
        }
        Expr::Grouping { expr, .. } => check_expr(expr, scopes, functions),
    }
}

fn literal_type(value: &Literal) -> SourceType {
    match value {
        Literal::Int(_) => SourceType::Int,
        Literal::Bool(_) => SourceType::Bool,
        Literal::String(_) => SourceType::String,
        Literal::Nil => SourceType::Nil,
    }
}

fn check_binary(
    op: BinaryOp,
    lhs_type: SourceType,
    rhs_type: SourceType,
    lhs_span: ferrix_core::diagnostics::SourceSpan,
    rhs_span: ferrix_core::diagnostics::SourceSpan,
) -> Result<SourceType, CompileError> {
    match op {
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => {
            expect_type(SourceType::Int, lhs_type, lhs_span)?;
            expect_type(SourceType::Int, rhs_type, rhs_span)?;
            Ok(SourceType::Int)
        }
        BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
            expect_type(SourceType::Int, lhs_type, lhs_span)?;
            expect_type(SourceType::Int, rhs_type, rhs_span)?;
            Ok(SourceType::Bool)
        }
        BinaryOp::Equal | BinaryOp::NotEqual => Ok(SourceType::Bool),
    }
}

fn check_index_access(
    target_type: SourceType,
    index_type: SourceType,
    target_span: ferrix_core::diagnostics::SourceSpan,
    index_span: ferrix_core::diagnostics::SourceSpan,
) -> Result<(), CompileError> {
    match target_type {
        SourceType::Array => expect_type(SourceType::Int, index_type, index_span),
        SourceType::Map | SourceType::Unknown | SourceType::Nil => Ok(()),
        found => Err(type_error("array or map", found, target_span)),
    }
}

fn expect_assignable(
    expected: SourceType,
    found: SourceType,
    span: ferrix_core::diagnostics::SourceSpan,
) -> Result<(), CompileError> {
    if expected.is_known() && found.is_known() && expected != found {
        Err(type_error(expected.name(), found, span))
    } else {
        Ok(())
    }
}

fn expect_type(
    expected: SourceType,
    found: SourceType,
    span: ferrix_core::diagnostics::SourceSpan,
) -> Result<(), CompileError> {
    if found.is_known() && found != expected {
        Err(type_error(expected.name(), found, span))
    } else {
        Ok(())
    }
}

fn merge_assignment_type(previous: SourceType, assigned: SourceType) -> SourceType {
    if previous.is_known() && assigned == SourceType::Nil {
        previous
    } else {
        assigned
    }
}

fn type_error(
    expected: impl Into<String>,
    found: SourceType,
    span: ferrix_core::diagnostics::SourceSpan,
) -> CompileError {
    CompileError::new(
        CompileErrorKind::TypeMismatch {
            expected: expected.into(),
            found: found.name().to_string(),
        },
        Some(span),
    )
}

enum NameUse {
    Variable,
    Function,
}

fn undefined_name_error(
    name: &str,
    span: ferrix_core::diagnostics::SourceSpan,
    name_use: NameUse,
) -> CompileError {
    if let Some((module, export)) = name.split_once('.') {
        CompileError::new(
            CompileErrorKind::UndefinedModuleExport {
                module: module.to_string(),
                name: export.to_string(),
            },
            Some(span),
        )
    } else {
        let kind = match name_use {
            NameUse::Variable => CompileErrorKind::UndefinedVariable {
                name: name.to_string(),
            },
            NameUse::Function => CompileErrorKind::UndefinedFunction {
                name: name.to_string(),
            },
        };
        CompileError::new(kind, Some(span))
    }
}
