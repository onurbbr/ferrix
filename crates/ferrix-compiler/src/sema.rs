//! Semantic analysis for Ferrix ASTs.
//!
//! This pass validates names and arity before bytecode generation: variables
//! must be declared before use, functions must be unique, parameters cannot be
//! duplicated, and call sites must match the known function signatures.

use std::collections::{HashMap, HashSet};

use crate::{
    ast::{Expr, ProgramAst, Stmt},
    error::{CompileError, CompileErrorKind},
};

const BUILTIN_FUNCTIONS: &[(&str, usize)] = &[("print", 1), ("len", 1), ("type_of", 1)];

/// Runs semantic checks for a program without module aliases.
pub fn analyze(program: &ProgramAst) -> Result<(), CompileError> {
    analyze_with_function_aliases(program, &[])
}

/// Runs semantic checks while allowing namespaced aliases to point at functions.
pub fn analyze_with_function_aliases(
    program: &ProgramAst,
    aliases: &[(String, String)],
) -> Result<(), CompileError> {
    let mut locals = HashSet::new();
    let functions = collect_functions(program, aliases)?;

    for stmt in &program.statements {
        match stmt {
            Stmt::Import { .. } => {}
            Stmt::Function { .. } => {}
            Stmt::Let {
                name,
                initializer,
                span,
            } => {
                check_expr(initializer, &locals, &functions)?;
                if !locals.insert(name.clone()) {
                    return Err(CompileError::new(
                        CompileErrorKind::DuplicateVariable { name: name.clone() },
                        Some(*span),
                    ));
                }
            }
            Stmt::Assign { name, value, span } => {
                if !locals.contains(name) {
                    return Err(CompileError::new(
                        CompileErrorKind::UndefinedVariable { name: name.clone() },
                        Some(*span),
                    ));
                }
                check_expr(value, &locals, &functions)?;
            }
            Stmt::IndexAssign {
                target,
                index,
                value,
                ..
            } => {
                check_expr(target, &locals, &functions)?;
                check_expr(index, &locals, &functions)?;
                check_expr(value, &locals, &functions)?;
            }
            Stmt::Return { value, .. } | Stmt::Expr { value, .. } => {
                check_expr(value, &locals, &functions)?;
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                check_expr(condition, &locals, &functions)?;
                check_statements(then_branch, &mut locals, &functions)?;
                check_statements(else_branch, &mut locals, &functions)?;
            }
            Stmt::While {
                condition, body, ..
            } => {
                check_expr(condition, &locals, &functions)?;
                check_statements(body, &mut locals, &functions)?;
            }
            Stmt::Block { statements, .. } => {
                check_statements(statements, &mut locals, &functions)?;
            }
        }
    }

    for stmt in &program.statements {
        if let Stmt::Function {
            params, body, span, ..
        } = stmt
        {
            let mut function_locals = HashSet::new();
            if params.len() > u8::MAX as usize {
                return Err(CompileError::new(
                    CompileErrorKind::TooManyParameters {
                        max: u8::MAX as usize,
                    },
                    Some(*span),
                ));
            }
            for param in params {
                if !function_locals.insert(param.clone()) {
                    return Err(CompileError::new(
                        CompileErrorKind::DuplicateParameter {
                            name: param.clone(),
                        },
                        Some(*span),
                    ));
                }
            }
            check_statements(body, &mut function_locals, &functions)?;
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

fn check_statements(
    stmts: &[Stmt],
    locals: &mut HashSet<String>,
    functions: &HashMap<String, usize>,
) -> Result<(), CompileError> {
    for stmt in stmts {
        match stmt {
            Stmt::Import { .. } => {}
            Stmt::Function { .. } => {}
            Stmt::Let {
                name,
                initializer,
                span,
            } => {
                check_expr(initializer, locals, functions)?;
                if !locals.insert(name.clone()) {
                    return Err(CompileError::new(
                        CompileErrorKind::DuplicateVariable { name: name.clone() },
                        Some(*span),
                    ));
                }
            }
            Stmt::Assign { name, value, span } => {
                if !locals.contains(name) {
                    return Err(CompileError::new(
                        CompileErrorKind::UndefinedVariable { name: name.clone() },
                        Some(*span),
                    ));
                }
                check_expr(value, locals, functions)?;
            }
            Stmt::IndexAssign {
                target,
                index,
                value,
                ..
            } => {
                check_expr(target, locals, functions)?;
                check_expr(index, locals, functions)?;
                check_expr(value, locals, functions)?;
            }
            Stmt::Return { value, .. } | Stmt::Expr { value, .. } => {
                check_expr(value, locals, functions)?;
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                check_expr(condition, locals, functions)?;
                check_statements(then_branch, locals, functions)?;
                check_statements(else_branch, locals, functions)?;
            }
            Stmt::While {
                condition, body, ..
            } => {
                check_expr(condition, locals, functions)?;
                check_statements(body, locals, functions)?;
            }
            Stmt::Block { statements, .. } => {
                check_statements(statements, locals, functions)?;
            }
        }
    }

    Ok(())
}

fn check_expr(
    expr: &Expr,
    locals: &HashSet<String>,
    functions: &HashMap<String, usize>,
) -> Result<(), CompileError> {
    match expr {
        Expr::Literal { .. } => Ok(()),
        Expr::Variable { name, span } => {
            if locals.contains(name) {
                Ok(())
            } else {
                Err(CompileError::new(
                    CompileErrorKind::UndefinedVariable { name: name.clone() },
                    Some(*span),
                ))
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            check_expr(lhs, locals, functions)?;
            check_expr(rhs, locals, functions)
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
                check_expr(element, locals, functions)?;
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
                check_expr(key, locals, functions)?;
                check_expr(value, locals, functions)?;
            }
            Ok(())
        }
        Expr::Index { target, index, .. } => {
            check_expr(target, locals, functions)?;
            check_expr(index, locals, functions)
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
            let expected = functions.get(callee).copied().ok_or_else(|| {
                CompileError::new(
                    CompileErrorKind::UndefinedFunction {
                        name: callee.clone(),
                    },
                    Some(*span),
                )
            })?;
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
            for arg in args {
                check_expr(arg, locals, functions)?;
            }
            Ok(())
        }
        Expr::Grouping { expr, .. } => check_expr(expr, locals, functions),
    }
}
