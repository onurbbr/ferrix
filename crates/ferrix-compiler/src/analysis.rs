//! Compiler analysis and optimization reporting.
//!
//! The initial analysis layer favors developer insight over heavy compiler
//! theory. It records what the compiler can already infer from the AST and the
//! emitted bytecode: dependencies, feature flags, host capability hints, and
//! optimizer pass metrics.

use std::collections::BTreeSet;

use ferrix_core::bytecode::{
    FunctionKind, OptimizationReport, Program, bytecode_features, infer_program_feature_flags,
};

use crate::ast::{Expr, ProgramAst, Stmt};

/// End-to-end compiler report attached to a successful compile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileReport {
    /// Static analysis facts inferred from source and bytecode.
    pub analysis: CompilerAnalysisReport,
    /// Optimizer pass breakdown for emitted bytecode chunks.
    pub optimization: ProgramOptimizationReport,
}

/// Source/bytecode facts useful to CLI and runtime tooling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerAnalysisReport {
    /// Required bytecode feature flags.
    pub required_feature_flags: u32,
    /// Human-readable bytecode feature names.
    pub required_features: Vec<String>,
    /// Dotted host capability names inferred from known native calls.
    pub required_capabilities: Vec<String>,
    /// Source-level module imports discovered before module linking.
    pub module_dependencies: Vec<String>,
    /// Native function names referenced by the program.
    pub native_dependencies: Vec<String>,
    /// Custom host extension ids referenced by the program.
    pub custom_extension_dependencies: Vec<String>,
    /// Number of bytecode functions emitted.
    pub bytecode_function_count: usize,
    /// Total instruction count across bytecode functions.
    pub bytecode_instruction_count: usize,
}

/// Whole-program optimizer report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramOptimizationReport {
    /// Per-chunk optimizer reports.
    pub chunks: Vec<OptimizationReport>,
}

impl ProgramOptimizationReport {
    /// Returns the total number of optimizer passes that ran.
    pub fn total_passes(&self) -> usize {
        self.chunks.iter().map(|chunk| chunk.passes.len()).sum()
    }

    /// Returns the total number of transformations across all chunks.
    pub fn total_transformations(&self) -> usize {
        self.chunks
            .iter()
            .map(OptimizationReport::total_transformations)
            .sum()
    }

    /// Returns true when any optimizer pass changed bytecode.
    pub fn changed(&self) -> bool {
        self.chunks
            .iter()
            .flat_map(|chunk| &chunk.passes)
            .any(|pass| pass.changed)
    }
}

/// Builds a compile report from source-level imports, emitted bytecode, and optimizer metadata.
pub fn build_compile_report(
    program: &Program,
    module_dependencies: Vec<String>,
    chunks: Vec<OptimizationReport>,
) -> CompileReport {
    let required_feature_flags = infer_program_feature_flags(program);
    let native_dependencies = native_dependencies(program);
    let required_capabilities = required_capabilities_for_natives(&native_dependencies);
    CompileReport {
        analysis: CompilerAnalysisReport {
            required_feature_flags,
            required_features: bytecode_features(required_feature_flags)
                .into_iter()
                .map(|feature| feature.as_str().to_string())
                .collect(),
            required_capabilities,
            module_dependencies,
            native_dependencies,
            custom_extension_dependencies: Vec::new(),
            bytecode_function_count: program
                .functions
                .iter()
                .filter(|function| matches!(function.kind, FunctionKind::Bytecode(_)))
                .count(),
            bytecode_instruction_count: program
                .functions
                .iter()
                .filter_map(|function| function.chunk())
                .map(|chunk| chunk.instructions.len())
                .sum(),
        },
        optimization: ProgramOptimizationReport { chunks },
    }
}

/// Collects `import ...;` dependencies from source ASTs before linking removes them.
pub fn collect_module_dependencies(programs: &[&ProgramAst]) -> Vec<String> {
    let mut dependencies = BTreeSet::new();
    for program in programs {
        collect_stmt_imports(&program.statements, &mut dependencies);
    }
    dependencies.into_iter().collect()
}

fn native_dependencies(program: &Program) -> Vec<String> {
    let mut dependencies = BTreeSet::new();
    for function in &program.functions {
        if let Some(name) = function.native_name() {
            dependencies.insert(name.to_string());
        }
    }
    dependencies.into_iter().collect()
}

fn required_capabilities_for_natives(native_dependencies: &[String]) -> Vec<String> {
    let mut capabilities = BTreeSet::new();
    for dependency in native_dependencies {
        capabilities.insert("native.call".to_string());
        if dependency == "print" {
            capabilities.insert("io.output".to_string());
        }
    }
    capabilities.into_iter().collect()
}

fn collect_stmt_imports(statements: &[Stmt], dependencies: &mut BTreeSet<String>) {
    for stmt in statements {
        match stmt {
            Stmt::Import { module, .. } => {
                dependencies.insert(module.clone());
            }
            Stmt::Let { initializer, .. } => collect_expr_imports(initializer, dependencies),
            Stmt::Function { body, .. }
            | Stmt::Block {
                statements: body, ..
            } => {
                collect_stmt_imports(body, dependencies);
            }
            Stmt::Assign { value, .. } => collect_expr_imports(value, dependencies),
            Stmt::IndexAssign {
                target,
                index,
                value,
                ..
            } => {
                collect_expr_imports(target, dependencies);
                collect_expr_imports(index, dependencies);
                collect_expr_imports(value, dependencies);
            }
            Stmt::FieldAssign { target, value, .. } => {
                collect_expr_imports(target, dependencies);
                collect_expr_imports(value, dependencies);
            }
            Stmt::Return { value, .. } | Stmt::Throw { value, .. } | Stmt::Expr { value, .. } => {
                collect_expr_imports(value, dependencies);
            }
            Stmt::TryCatch {
                try_branch,
                catch_branch,
                ..
            } => {
                collect_stmt_imports(try_branch, dependencies);
                collect_stmt_imports(catch_branch, dependencies);
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                collect_expr_imports(condition, dependencies);
                collect_stmt_imports(then_branch, dependencies);
                collect_stmt_imports(else_branch, dependencies);
            }
            Stmt::While {
                condition, body, ..
            } => {
                collect_expr_imports(condition, dependencies);
                collect_stmt_imports(body, dependencies);
            }
        }
    }
}

fn collect_expr_imports(expr: &Expr, dependencies: &mut BTreeSet<String>) {
    match expr {
        Expr::Literal { .. } | Expr::Variable { .. } => {}
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_imports(lhs, dependencies);
            collect_expr_imports(rhs, dependencies);
        }
        Expr::Call { args, .. } => {
            for arg in args {
                collect_expr_imports(arg, dependencies);
            }
        }
        Expr::MethodCall { target, args, .. } => {
            collect_expr_imports(target, dependencies);
            for arg in args {
                collect_expr_imports(arg, dependencies);
            }
        }
        Expr::Function { body, .. } => collect_stmt_imports(body, dependencies),
        Expr::Index { target, index, .. } => {
            collect_expr_imports(target, dependencies);
            collect_expr_imports(index, dependencies);
        }
        Expr::Field { target, .. } => collect_expr_imports(target, dependencies),
        Expr::Array { elements, .. } => {
            for element in elements {
                collect_expr_imports(element, dependencies);
            }
        }
        Expr::Map { entries, .. } => {
            for (key, value) in entries {
                collect_expr_imports(key, dependencies);
                collect_expr_imports(value, dependencies);
            }
        }
        Expr::Record { fields, .. } => {
            for (_, value) in fields {
                collect_expr_imports(value, dependencies);
            }
        }
        Expr::Grouping { expr, .. } => collect_expr_imports(expr, dependencies),
    }
}
