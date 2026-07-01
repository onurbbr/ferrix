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
                if locals.contains(name) {
                    return Err(CompileError::new(
                        CompileErrorKind::DuplicateVariable { name: name.clone() },
                        Some(*span),
                    ));
                }
                let mut initializer_locals = locals.clone();
                if matches!(initializer, Expr::Function { .. }) {
                    initializer_locals.insert(name.clone());
                }
                check_expr(initializer, &initializer_locals, &functions)?;
                locals.insert(name.clone());
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
    let outer = HashSet::new();
    check_statements_with_outer(stmts, locals, functions, &outer)
}

fn check_statements_with_outer(
    stmts: &[Stmt],
    locals: &mut HashSet<String>,
    functions: &HashMap<String, usize>,
    outer: &HashSet<String>,
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
                if locals.contains(name) {
                    return Err(CompileError::new(
                        CompileErrorKind::DuplicateVariable { name: name.clone() },
                        Some(*span),
                    ));
                }
                let mut initializer_locals = locals.clone();
                if matches!(initializer, Expr::Function { .. }) {
                    initializer_locals.insert(name.clone());
                }
                check_expr_with_outer(initializer, &initializer_locals, functions, outer)?;
                locals.insert(name.clone());
            }
            Stmt::Assign { name, value, span } => {
                if !locals.contains(name) && !outer.contains(name) {
                    return Err(CompileError::new(
                        CompileErrorKind::UndefinedVariable { name: name.clone() },
                        Some(*span),
                    ));
                }
                check_expr_with_outer(value, locals, functions, outer)?;
            }
            Stmt::IndexAssign {
                target,
                index,
                value,
                ..
            } => {
                check_expr_with_outer(target, locals, functions, outer)?;
                check_expr_with_outer(index, locals, functions, outer)?;
                check_expr_with_outer(value, locals, functions, outer)?;
            }
            Stmt::Return { value, .. } | Stmt::Expr { value, .. } => {
                check_expr_with_outer(value, locals, functions, outer)?;
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                check_expr_with_outer(condition, locals, functions, outer)?;
                check_statements_with_outer(then_branch, locals, functions, outer)?;
                check_statements_with_outer(else_branch, locals, functions, outer)?;
            }
            Stmt::While {
                condition, body, ..
            } => {
                check_expr_with_outer(condition, locals, functions, outer)?;
                check_statements_with_outer(body, locals, functions, outer)?;
            }
            Stmt::Block { statements, .. } => {
                check_statements_with_outer(statements, locals, functions, outer)?;
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
    let outer = HashSet::new();
    check_expr_with_outer(expr, locals, functions, &outer)
}

fn check_expr_with_outer(
    expr: &Expr,
    locals: &HashSet<String>,
    functions: &HashMap<String, usize>,
    outer: &HashSet<String>,
) -> Result<(), CompileError> {
    match expr {
        Expr::Literal { .. } => Ok(()),
        Expr::Variable { name, span } => {
            if locals.contains(name) || outer.contains(name) {
                Ok(())
            } else {
                Err(CompileError::new(
                    CompileErrorKind::UndefinedVariable { name: name.clone() },
                    Some(*span),
                ))
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            check_expr_with_outer(lhs, locals, functions, outer)?;
            check_expr_with_outer(rhs, locals, functions, outer)
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
                check_expr_with_outer(element, locals, functions, outer)?;
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
                check_expr_with_outer(key, locals, functions, outer)?;
                check_expr_with_outer(value, locals, functions, outer)?;
            }
            Ok(())
        }
        Expr::Index { target, index, .. } => {
            check_expr_with_outer(target, locals, functions, outer)?;
            check_expr_with_outer(index, locals, functions, outer)
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
            } else if !locals.contains(callee) && !outer.contains(callee) {
                return Err(CompileError::new(
                    CompileErrorKind::UndefinedFunction {
                        name: callee.clone(),
                    },
                    Some(*span),
                ));
            }
            for arg in args {
                check_expr_with_outer(arg, locals, functions, outer)?;
            }
            Ok(())
        }
        Expr::Function { params, body, span } => {
            let mut function_locals = HashSet::new();
            let mut closure_outer = outer.clone();
            closure_outer.extend(locals.iter().cloned());
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
            check_statements_with_outer(body, &mut function_locals, functions, &closure_outer)
        }
        Expr::Grouping { expr, .. } => check_expr_with_outer(expr, locals, functions, outer),
    }
}
