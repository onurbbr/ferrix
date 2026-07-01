//! Semantic analysis for Ferrix ASTs.
//!
//! This pass validates names and arity before bytecode generation. Variables
//! are resolved through lexical scopes so block-local bindings can shadow outer
//! names without leaking after the block exits.

use std::collections::{HashMap, HashSet};

use crate::{
    ast::{Expr, ProgramAst, Stmt},
    error::{CompileError, CompileErrorKind},
};

const BUILTIN_FUNCTIONS: &[(&str, usize)] = &[("print", 1), ("len", 1), ("type_of", 1)];

#[derive(Clone, Debug)]
struct ScopeStack {
    scopes: Vec<HashSet<String>>,
}

impl ScopeStack {
    fn new() -> Self {
        Self {
            scopes: vec![HashSet::new()],
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashSet::new());
    }

    fn pop_scope(&mut self) {
        self.scopes
            .pop()
            .expect("semantic analyzer always keeps one active scope");
    }

    fn contains(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|scope| scope.contains(name))
    }

    fn contains_current(&self, name: &str) -> bool {
        self.scopes
            .last()
            .expect("semantic analyzer always has a current scope")
            .contains(name)
    }

    fn declare(&mut self, name: String) {
        self.scopes
            .last_mut()
            .expect("semantic analyzer always has a current scope")
            .insert(name);
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
) -> Result<HashMap<String, usize>, CompileError> {
    let mut functions = BUILTIN_FUNCTIONS
        .iter()
        .map(|(name, arity)| ((*name).to_string(), *arity))
        .collect::<HashMap<_, _>>();

    for stmt in &program.statements {
        if let Stmt::Function {
            name, params, span, ..
        } = stmt
            && functions.insert(name.clone(), params.len()).is_some()
        {
            return Err(CompileError::new(
                CompileErrorKind::DuplicateFunction { name: name.clone() },
                Some(*span),
            ));
        }
    }

    for (alias, target) in aliases {
        if let Some(arity) = functions.get(target).copied() {
            functions.entry(alias.clone()).or_insert(arity);
        }
    }

    Ok(functions)
}

fn check_scoped_statements(
    stmts: &[Stmt],
    scopes: &mut ScopeStack,
    functions: &HashMap<String, usize>,
) -> Result<(), CompileError> {
    scopes.push_scope();
    let result = check_statements(stmts, scopes, functions);
    scopes.pop_scope();
    result
}

fn check_statements(
    stmts: &[Stmt],
    scopes: &mut ScopeStack,
    functions: &HashMap<String, usize>,
) -> Result<(), CompileError> {
    for stmt in stmts {
        check_stmt(stmt, scopes, functions)?;
    }

    Ok(())
}

fn check_stmt(
    stmt: &Stmt,
    scopes: &mut ScopeStack,
    functions: &HashMap<String, usize>,
) -> Result<(), CompileError> {
    match stmt {
        Stmt::Import { .. } | Stmt::Function { .. } => Ok(()),
        Stmt::Let {
            name,
            initializer,
            span,
        } => check_let(name, initializer, *span, scopes, functions),
        Stmt::Assign { name, value, span } => {
            if !scopes.contains(name) {
                return Err(CompileError::new(
                    CompileErrorKind::UndefinedVariable { name: name.clone() },
                    Some(*span),
                ));
            }
            check_expr(value, scopes, functions)
        }
        Stmt::IndexAssign {
            target,
            index,
            value,
            ..
        } => {
            check_expr(target, scopes, functions)?;
            check_expr(index, scopes, functions)?;
            check_expr(value, scopes, functions)
        }
        Stmt::Return { value, .. } | Stmt::Expr { value, .. } => {
            check_expr(value, scopes, functions)
        }
        Stmt::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            check_expr(condition, scopes, functions)?;
            check_scoped_statements(then_branch, scopes, functions)?;
            check_scoped_statements(else_branch, scopes, functions)
        }
        Stmt::While {
            condition, body, ..
        } => {
            check_expr(condition, scopes, functions)?;
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
    functions: &HashMap<String, usize>,
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
        scopes.declare(name.to_string());
        check_expr(initializer, scopes, functions)
    } else {
        check_expr(initializer, scopes, functions)?;
        scopes.declare(name.to_string());
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
        scopes.declare(param.clone());
    }

    Ok(())
}

fn check_expr(
    expr: &Expr,
    scopes: &mut ScopeStack,
    functions: &HashMap<String, usize>,
) -> Result<(), CompileError> {
    match expr {
        Expr::Literal { .. } => Ok(()),
        Expr::Variable { name, span } => {
            if scopes.contains(name) {
                Ok(())
            } else {
                Err(CompileError::new(
                    CompileErrorKind::UndefinedVariable { name: name.clone() },
                    Some(*span),
                ))
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            check_expr(lhs, scopes, functions)?;
            check_expr(rhs, scopes, functions)
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
            Ok(())
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
            Ok(())
        }
        Expr::Index { target, index, .. } => {
            check_expr(target, scopes, functions)?;
            check_expr(index, scopes, functions)
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

            if scopes.contains(callee) {
                for arg in args {
                    check_expr(arg, scopes, functions)?;
                }
                return Ok(());
            }

            if let Some(expected) = functions.get(callee).copied() {
                if expected != args.len() {
                    return Err(CompileError::new(
                        CompileErrorKind::WrongCallArity {
                            name: callee.clone(),
                            expected,
                            actual: args.len(),
                        },
                        Some(*span),
                    ));
                }
            } else {
                return Err(CompileError::new(
                    CompileErrorKind::UndefinedFunction {
                        name: callee.clone(),
                    },
                    Some(*span),
                ));
            }

            for arg in args {
                check_expr(arg, scopes, functions)?;
            }
            Ok(())
        }
        Expr::Function { params, body, span } => {
            let mut function_scopes = scopes.clone();
            function_scopes.push_scope();
            declare_parameters(params, *span, &mut function_scopes)?;
            check_scoped_statements(body, &mut function_scopes, functions)
        }
        Expr::Grouping { expr, .. } => check_expr(expr, scopes, functions),
    }
}
