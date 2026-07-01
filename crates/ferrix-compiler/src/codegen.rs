//! Compiler pipeline and bytecode generation for Ferrix source programs.
//!
//! Public helpers expose common entry points: parse source, compile a single
//! AST, or compile an entry file with named imported modules. Internally,
//! [`Codegen`] lowers AST nodes into register-based bytecode.

use std::collections::{HashMap, HashSet};

use ferrix_core::{
    Value,
    bytecode::{
        CaptureId, Chunk, ChunkBuildError, Function, FunctionId, Instruction, JumpTarget, Program,
        ProgramBuildError, Register, VerifiedProgram,
    },
    diagnostics::FileId,
};

use crate::{
    ast::{BinaryOp, Expr, Literal, ProgramAst, Stmt},
    error::{CompileError, CompileErrorKind},
    lexer::lex,
    parser::parse,
    sema::{analyze, analyze_with_function_aliases},
};

const BUILTIN_FUNCTIONS: &[(&str, u8)] = &[("print", 1), ("len", 1), ("type_of", 1)];

/// Imported module AST paired with the namespace visible from the entry file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedModuleAst {
    /// Namespace prefix used by calls such as `math.add(...)`.
    pub name: String,
    /// Parsed program for the imported source file.
    pub ast: ProgramAst,
}

/// Parses, analyzes, compiles, and verifies a source string using file id `0`.
pub fn compile_source(source: &str) -> Result<VerifiedProgram, CompileError> {
    compile_source_with_file_id(source, FileId(0))
}

/// Parses, analyzes, compiles, and verifies a source string with a source file id.
pub fn compile_source_with_file_id(
    source: &str,
    file_id: FileId,
) -> Result<VerifiedProgram, CompileError> {
    let ast = parse_source_with_file_id(source, file_id)?;
    compile_program_ast(ast)
}

/// Lexes and parses a source string into an AST with stable source spans.
pub fn parse_source_with_file_id(
    source: &str,
    file_id: FileId,
) -> Result<ProgramAst, CompileError> {
    let tokens = lex(source, file_id)?;
    parse(tokens)
}

/// Analyzes, compiles, and verifies one AST without external modules.
pub fn compile_program_ast(program_ast: ProgramAst) -> Result<VerifiedProgram, CompileError> {
    analyze(&program_ast)?;
    Codegen::compile_program(program_ast, &[])
}

/// Compiles an entry AST with anonymous module ASTs linked before codegen.
pub fn compile_program_ast_with_modules(
    entry: ProgramAst,
    modules: Vec<ProgramAst>,
) -> Result<VerifiedProgram, CompileError> {
    compile_program_ast_with_aliases(link_modules(entry, modules), Vec::new())
}

/// Compiles an entry AST with modules exposed through namespace aliases.
pub fn compile_program_ast_with_named_modules(
    entry: ProgramAst,
    modules: Vec<ImportedModuleAst>,
) -> Result<VerifiedProgram, CompileError> {
    let (linked, aliases) = link_named_modules(entry, modules);
    compile_program_ast_with_aliases(linked, aliases)
}

fn compile_program_ast_with_aliases(
    program_ast: ProgramAst,
    aliases: Vec<(String, String)>,
) -> Result<VerifiedProgram, CompileError> {
    analyze_with_function_aliases(&program_ast, &aliases)?;
    Codegen::compile_program(program_ast, &aliases)
}

struct Codegen {
    chunk: Chunk,
    locals: HashMap<String, Register>,
    captures: HashMap<String, CaptureId>,
    functions: HashMap<String, FunctionId>,
    next_register: u16,
    next_function_id: u16,
    generated_functions: Vec<(FunctionId, Function)>,
}

impl Codegen {
    fn new_main(functions: HashMap<String, FunctionId>, next_function_id: u16) -> Self {
        Self {
            chunk: Chunk::new("main", 0),
            locals: HashMap::new(),
            captures: HashMap::new(),
            functions,
            next_register: 0,
            next_function_id,
            generated_functions: Vec::new(),
        }
    }

    fn new_function(
        name: &str,
        params: &[String],
        functions: HashMap<String, FunctionId>,
        next_function_id: u16,
    ) -> Result<Self, CompileError> {
        let arity = u8::try_from(params.len()).map_err(|_| {
            CompileError::new(
                CompileErrorKind::TooManyParameters {
                    max: u8::MAX as usize,
                },
                None,
            )
        })?;
        let mut locals = HashMap::new();
        for (index, param) in params.iter().enumerate() {
            locals.insert(param.clone(), Register(index as u8));
        }

        Ok(Self {
            chunk: Chunk::new(name, 0).with_arity(arity),
            locals,
            captures: HashMap::new(),
            functions,
            next_register: params.len() as u16,
            next_function_id,
            generated_functions: Vec::new(),
        })
    }

    fn new_closure(
        name: &str,
        params: &[String],
        captures: &[String],
        functions: HashMap<String, FunctionId>,
        next_function_id: u16,
    ) -> Result<Self, CompileError> {
        let mut compiler = Self::new_function(name, params, functions, next_function_id)?;
        let capture_count = u8::try_from(captures.len()).map_err(|_| {
            CompileError::new(
                CompileErrorKind::TooManyArguments {
                    max: u8::MAX as usize,
                },
                None,
            )
        })?;
        compiler.chunk.capture_count = capture_count;
        compiler.captures = captures
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), CaptureId(index as u8)))
            .collect();
        Ok(compiler)
    }

    fn compile_program(
        program_ast: ProgramAst,
        aliases: &[(String, String)],
    ) -> Result<VerifiedProgram, CompileError> {
        let referenced_builtins = referenced_builtins(&program_ast);
        let function_ids = function_ids(&program_ast, &referenced_builtins, aliases)?;
        let main_function_index = program_ast
            .statements
            .iter()
            .filter(|stmt| matches!(stmt, Stmt::Function { .. }))
            .count()
            + referenced_builtins.len();
        let main_id = FunctionId(main_function_index.try_into().map_err(|_| {
            CompileError::new(
                CompileErrorKind::TooManyFunctions {
                    max: u16::MAX as usize,
                },
                None,
            )
        })?);
        let mut program = Program::new(main_id);
        let function_count = program_ast
            .statements
            .iter()
            .filter(|stmt| matches!(stmt, Stmt::Function { .. }))
            .count();
        for _ in 0..function_count {
            program
                .add_function(Function::native("__pending", 0))
                .map_err(map_program_error)?;
        }
        for (name, arity) in &referenced_builtins {
            program
                .add_function(Function::native(*name, *arity))
                .map_err(map_program_error)?;
        }

        for stmt in &program_ast.statements {
            if let Stmt::Function {
                name,
                params,
                body,
                span,
            } = stmt
            {
                let function_id = *function_ids
                    .get(name)
                    .expect("function ids are precomputed");
                let mut compiler = Self::new_function(
                    name,
                    params,
                    function_ids.clone(),
                    program.functions.len().try_into().map_err(|_| {
                        CompileError::new(
                            CompileErrorKind::TooManyFunctions {
                                max: u16::MAX as usize,
                            },
                            Some(*span),
                        )
                    })?,
                )?;
                compiler.compile_block(body)?;
                let generated_functions = std::mem::take(&mut compiler.generated_functions);
                program.functions[usize::from(function_id.0)] =
                    Function::bytecode(compiler.finish_chunk()?);
                append_generated_functions(&mut program, generated_functions)?;
            }
        }

        let mut main_compiler = Self::new_main(
            function_ids,
            program.functions.len().try_into().map_err(|_| {
                CompileError::new(
                    CompileErrorKind::TooManyFunctions {
                        max: u16::MAX as usize,
                    },
                    None,
                )
            })?,
        );
        for stmt in &program_ast.statements {
            if !matches!(stmt, Stmt::Function { .. }) {
                main_compiler.compile_stmt(stmt)?;
            }
        }
        let generated_functions = std::mem::take(&mut main_compiler.generated_functions);
        append_generated_functions(&mut program, generated_functions)?;
        let main_id = program
            .add_function(Function::bytecode(main_compiler.finish_chunk()?))
            .map_err(map_program_error)?;
        program.entry = main_id;

        VerifiedProgram::new(program)
            .map_err(|error| CompileError::new(CompileErrorKind::BytecodeVerification(error), None))
    }

    fn finish_chunk(mut self) -> Result<Chunk, CompileError> {
        self.chunk.register_count = self
            .next_register
            .try_into()
            .map_err(|_| CompileError::new(CompileErrorKind::TooManyRegisters, None))?;

        Ok(self.chunk)
    }

    fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
        match stmt {
            Stmt::Import { .. } => Ok(()),
            Stmt::Function { .. } => Ok(()),
            Stmt::Let {
                name, initializer, ..
            } => {
                let dst = self.alloc_register()?;
                self.compile_expr_into(initializer, dst)?;
                self.locals.insert(name.clone(), dst);
                Ok(())
            }
            Stmt::Assign { name, value, span } => {
                let dst = self.locals.get(name).copied().ok_or_else(|| {
                    CompileError::new(
                        CompileErrorKind::UndefinedVariable { name: name.clone() },
                        Some(*span),
                    )
                })?;
                self.compile_expr_into(value, dst)
            }
            Stmt::IndexAssign {
                target,
                index,
                value,
                span,
            } => {
                let target = self.compile_expr(target)?;
                let index = self.compile_expr(index)?;
                let value = self.compile_expr(value)?;
                self.chunk.push_instruction_with_span(
                    Instruction::IndexSet {
                        target,
                        index,
                        value,
                    },
                    Some(*span),
                );
                Ok(())
            }
            Stmt::Return { value, span } => {
                let src = self.compile_expr(value)?;
                self.chunk
                    .push_instruction_with_span(Instruction::Return { src }, Some(*span));
                Ok(())
            }
            Stmt::Expr { value, .. } => {
                self.compile_expr(value)?;
                Ok(())
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
                span,
            } => self.compile_if(condition, then_branch, else_branch, *span),
            Stmt::While {
                condition,
                body,
                span,
            } => self.compile_while(condition, body, *span),
            Stmt::Block { statements, .. } => self.compile_block(statements),
        }
    }

    fn compile_block(&mut self, statements: &[Stmt]) -> Result<(), CompileError> {
        for stmt in statements {
            self.compile_stmt(stmt)?;
        }

        Ok(())
    }

    fn compile_if(
        &mut self,
        condition: &Expr,
        then_branch: &[Stmt],
        else_branch: &[Stmt],
        span: ferrix_core::diagnostics::SourceSpan,
    ) -> Result<(), CompileError> {
        let condition = self.compile_expr(condition)?;
        let jump_to_else = self.emit_jump_if_false(condition, span)?;
        self.compile_block(then_branch)?;

        if else_branch.is_empty() {
            self.patch_jump(jump_to_else, self.current_instruction_index()?)?;
        } else {
            let jump_to_end = if statements_definitely_return(then_branch) {
                None
            } else {
                Some(self.emit_jump(span)?)
            };
            self.patch_jump(jump_to_else, self.current_instruction_index()?)?;
            self.compile_block(else_branch)?;
            if let Some(jump_to_end) = jump_to_end {
                self.patch_jump(jump_to_end, self.current_instruction_index()?)?;
            }
        }

        Ok(())
    }

    fn compile_while(
        &mut self,
        condition: &Expr,
        body: &[Stmt],
        span: ferrix_core::diagnostics::SourceSpan,
    ) -> Result<(), CompileError> {
        let loop_start = self.current_instruction_index()?;
        let condition = self.compile_expr(condition)?;
        let jump_to_end = self.emit_jump_if_false(condition, span)?;
        self.compile_block(body)?;
        self.chunk.push_instruction_with_span(
            Instruction::Jump {
                target: JumpTarget(loop_start),
            },
            Some(span),
        );
        self.patch_jump(jump_to_end, self.current_instruction_index()?)?;
        Ok(())
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<Register, CompileError> {
        match expr {
            Expr::Variable { name, span } => {
                if let Some(register) = self.locals.get(name).copied() {
                    Ok(register)
                } else if self.captures.contains_key(name) {
                    let dst = self.alloc_register()?;
                    self.compile_expr_into(expr, dst)?;
                    Ok(dst)
                } else {
                    Err(CompileError::new(
                        CompileErrorKind::UndefinedVariable { name: name.clone() },
                        Some(*span),
                    ))
                }
            }
            _ => {
                let dst = self.alloc_register()?;
                self.compile_expr_into(expr, dst)?;
                Ok(dst)
            }
        }
    }

    fn compile_expr_into(&mut self, expr: &Expr, dst: Register) -> Result<(), CompileError> {
        match expr {
            Expr::Literal { value, span } => {
                let value = match value {
                    Literal::Int(value) => Value::Int(*value),
                    Literal::Bool(value) => Value::Bool(*value),
                    Literal::Nil => Value::Nil,
                    Literal::String(value) => {
                        let string = self
                            .chunk
                            .add_string(value.clone())
                            .map_err(map_chunk_error)?;
                        self.chunk.push_instruction_with_span(
                            Instruction::LoadString { dst, string },
                            Some(*span),
                        );
                        return Ok(());
                    }
                };
                let constant = self.chunk.add_constant(value).map_err(map_chunk_error)?;
                self.chunk.push_instruction_with_span(
                    Instruction::LoadConst { dst, constant },
                    Some(*span),
                );
                Ok(())
            }
            Expr::Variable { name, span } => {
                if let Some(src) = self.locals.get(name).copied() {
                    if src != dst {
                        self.chunk.push_instruction_with_span(
                            Instruction::Move { dst, src },
                            Some(*span),
                        );
                    }
                    return Ok(());
                }
                let capture = self.captures.get(name).copied().ok_or_else(|| {
                    CompileError::new(
                        CompileErrorKind::UndefinedVariable { name: name.clone() },
                        Some(*span),
                    )
                })?;
                self.chunk.push_instruction_with_span(
                    Instruction::LoadCapture { dst, capture },
                    Some(*span),
                );
                Ok(())
            }
            Expr::Binary { op, lhs, rhs, span } => {
                let lhs_register = self.compile_expr(lhs)?;
                let rhs_register = self.compile_expr(rhs)?;
                let instruction = match op {
                    BinaryOp::Add => Instruction::Add {
                        dst,
                        lhs: lhs_register,
                        rhs: rhs_register,
                    },
                    BinaryOp::Sub => Instruction::Sub {
                        dst,
                        lhs: lhs_register,
                        rhs: rhs_register,
                    },
                    BinaryOp::Mul => Instruction::Mul {
                        dst,
                        lhs: lhs_register,
                        rhs: rhs_register,
                    },
                    BinaryOp::Div => Instruction::Div {
                        dst,
                        lhs: lhs_register,
                        rhs: rhs_register,
                    },
                    BinaryOp::Equal => Instruction::Equal {
                        dst,
                        lhs: lhs_register,
                        rhs: rhs_register,
                    },
                    BinaryOp::NotEqual => Instruction::NotEqual {
                        dst,
                        lhs: lhs_register,
                        rhs: rhs_register,
                    },
                    BinaryOp::Less => Instruction::Less {
                        dst,
                        lhs: lhs_register,
                        rhs: rhs_register,
                    },
                    BinaryOp::LessEqual => Instruction::LessEqual {
                        dst,
                        lhs: lhs_register,
                        rhs: rhs_register,
                    },
                    BinaryOp::Greater => Instruction::Greater {
                        dst,
                        lhs: lhs_register,
                        rhs: rhs_register,
                    },
                    BinaryOp::GreaterEqual => Instruction::GreaterEqual {
                        dst,
                        lhs: lhs_register,
                        rhs: rhs_register,
                    },
                };
                self.chunk
                    .push_instruction_with_span(instruction, Some(*span));
                Ok(())
            }
            Expr::Array { elements, span } => self.compile_array_into(elements, dst, *span),
            Expr::Map { entries, span } => self.compile_map_into(entries, dst, *span),
            Expr::Index {
                target,
                index,
                span,
            } => {
                let target = self.compile_expr(target)?;
                let index = self.compile_expr(index)?;
                self.chunk.push_instruction_with_span(
                    Instruction::IndexGet { dst, target, index },
                    Some(*span),
                );
                Ok(())
            }
            Expr::Call { callee, args, span } => self.compile_call_into(callee, args, dst, *span),
            Expr::Function { params, body, span } => {
                self.compile_function_literal_into(params, body, dst, *span)
            }
            Expr::Grouping { expr, .. } => self.compile_expr_into(expr, dst),
        }
    }

    fn compile_array_into(
        &mut self,
        elements: &[Expr],
        dst: Register,
        span: ferrix_core::diagnostics::SourceSpan,
    ) -> Result<(), CompileError> {
        if elements.len() > u8::MAX as usize {
            return Err(CompileError::new(
                CompileErrorKind::TooManyArrayElements {
                    max: u8::MAX as usize,
                },
                Some(span),
            ));
        }

        let elements_start = if elements.is_empty() {
            Register(0)
        } else {
            let start = self.alloc_register()?;
            let mut element_registers = vec![start];
            for _ in elements.iter().skip(1) {
                element_registers.push(self.alloc_register()?);
            }

            for (element, register) in elements.iter().zip(element_registers) {
                self.compile_expr_into(element, register)?;
            }
            start
        };

        self.chunk.push_instruction_with_span(
            Instruction::ArrayNew {
                dst,
                elements_start,
                element_count: elements.len() as u8,
            },
            Some(span),
        );
        Ok(())
    }

    fn compile_map_into(
        &mut self,
        entries: &[(Expr, Expr)],
        dst: Register,
        span: ferrix_core::diagnostics::SourceSpan,
    ) -> Result<(), CompileError> {
        if entries.len() > u8::MAX as usize {
            return Err(CompileError::new(
                CompileErrorKind::TooManyMapEntries {
                    max: u8::MAX as usize,
                },
                Some(span),
            ));
        }

        let entries_start = if entries.is_empty() {
            Register(0)
        } else {
            let start = self.alloc_register()?;
            let mut entry_registers = vec![start];
            for _ in 1..entries.len() * 2 {
                entry_registers.push(self.alloc_register()?);
            }

            for ((key, value), registers) in entries.iter().zip(entry_registers.chunks_exact(2)) {
                self.compile_expr_into(key, registers[0])?;
                self.compile_expr_into(value, registers[1])?;
            }
            start
        };

        self.chunk.push_instruction_with_span(
            Instruction::MapNew {
                dst,
                entries_start,
                entry_count: entries.len() as u8,
            },
            Some(span),
        );
        Ok(())
    }

    fn compile_call_into(
        &mut self,
        callee: &str,
        args: &[Expr],
        dst: Register,
        span: ferrix_core::diagnostics::SourceSpan,
    ) -> Result<(), CompileError> {
        if args.len() > u8::MAX as usize {
            return Err(CompileError::new(
                CompileErrorKind::TooManyArguments {
                    max: u8::MAX as usize,
                },
                Some(span),
            ));
        }
        let args_start = if args.is_empty() {
            Register(0)
        } else {
            let start = self.alloc_register()?;
            let mut arg_registers = vec![start];
            for _ in args.iter().skip(1) {
                arg_registers.push(self.alloc_register()?);
            }

            for (arg, register) in args.iter().zip(arg_registers) {
                self.compile_expr_into(arg, register)?;
            }
            start
        };

        if let Some(function) = self.functions.get(callee).copied() {
            self.chunk.push_instruction_with_span(
                Instruction::CallFunction {
                    dst,
                    function,
                    args_start,
                    arg_count: args.len() as u8,
                },
                Some(span),
            );
        } else {
            let callee = self.compile_expr(&Expr::Variable {
                name: callee.to_string(),
                span,
            })?;
            self.chunk.push_instruction_with_span(
                Instruction::CallValue {
                    dst,
                    callee,
                    args_start,
                    arg_count: args.len() as u8,
                },
                Some(span),
            );
        }
        Ok(())
    }

    fn compile_function_literal_into(
        &mut self,
        params: &[String],
        body: &[Stmt],
        dst: Register,
        span: ferrix_core::diagnostics::SourceSpan,
    ) -> Result<(), CompileError> {
        let captures = free_variables(body, params, &self.functions)
            .into_iter()
            .filter(|name| self.locals.contains_key(name) || self.captures.contains_key(name))
            .collect::<Vec<_>>();
        let function_id = FunctionId(self.next_function_id);
        self.next_function_id = self.next_function_id.checked_add(1).ok_or_else(|| {
            CompileError::new(
                CompileErrorKind::TooManyFunctions {
                    max: u16::MAX as usize,
                },
                Some(span),
            )
        })?;

        let mut compiler = Self::new_closure(
            &format!("closure#{}", function_id.0),
            params,
            &captures,
            self.functions.clone(),
            self.next_function_id,
        )?;
        compiler.compile_block(body)?;
        self.next_function_id = compiler.next_function_id;
        self.generated_functions
            .extend(std::mem::take(&mut compiler.generated_functions));
        self.generated_functions
            .push((function_id, Function::bytecode(compiler.finish_chunk()?)));

        let captures_start = if captures.is_empty() {
            Register(0)
        } else {
            let start = self.alloc_register()?;
            let mut capture_registers = vec![start];
            for _ in captures.iter().skip(1) {
                capture_registers.push(self.alloc_register()?);
            }
            for (name, register) in captures.iter().zip(capture_registers) {
                self.compile_expr_into(
                    &Expr::Variable {
                        name: name.clone(),
                        span,
                    },
                    register,
                )?;
            }
            start
        };

        self.chunk.push_instruction_with_span(
            Instruction::MakeClosure {
                dst,
                function: function_id,
                captures_start,
                capture_count: captures.len() as u8,
            },
            Some(span),
        );
        Ok(())
    }

    fn alloc_register(&mut self) -> Result<Register, CompileError> {
        let register = u8::try_from(self.next_register)
            .map_err(|_| CompileError::new(CompileErrorKind::TooManyRegisters, None))?;
        self.next_register += 1;
        Ok(Register(register))
    }

    fn emit_jump_if_false(
        &mut self,
        condition: Register,
        span: ferrix_core::diagnostics::SourceSpan,
    ) -> Result<usize, CompileError> {
        let index = self.chunk.instructions.len();
        self.chunk.push_instruction_with_span(
            Instruction::JumpIfFalse {
                condition,
                target: JumpTarget(u32::MAX),
            },
            Some(span),
        );
        Ok(index)
    }

    fn emit_jump(
        &mut self,
        span: ferrix_core::diagnostics::SourceSpan,
    ) -> Result<usize, CompileError> {
        let index = self.chunk.instructions.len();
        self.chunk.push_instruction_with_span(
            Instruction::Jump {
                target: JumpTarget(u32::MAX),
            },
            Some(span),
        );
        Ok(index)
    }

    fn patch_jump(&mut self, instruction_index: usize, target: u32) -> Result<(), CompileError> {
        let instruction = self
            .chunk
            .instructions
            .get_mut(instruction_index)
            .expect("compiler emits jump before patching it");

        match instruction {
            Instruction::Jump {
                target: jump_target,
            }
            | Instruction::JumpIfFalse {
                target: jump_target,
                ..
            } => *jump_target = JumpTarget(target),
            _ => unreachable!("only jump instructions are patched"),
        }

        Ok(())
    }

    fn current_instruction_index(&self) -> Result<u32, CompileError> {
        u32::try_from(self.chunk.instructions.len())
            .map_err(|_| CompileError::new(CompileErrorKind::TooManyInstructions, None))
    }
}

fn link_modules(entry: ProgramAst, modules: Vec<ProgramAst>) -> ProgramAst {
    let mut statements = Vec::new();

    for module in modules {
        statements.extend(
            module
                .statements
                .into_iter()
                .filter(|stmt| matches!(stmt, Stmt::Function { .. })),
        );
    }

    statements.extend(
        entry
            .statements
            .into_iter()
            .filter(|stmt| !matches!(stmt, Stmt::Import { .. })),
    );

    ProgramAst { statements }
}

fn link_named_modules(
    entry: ProgramAst,
    modules: Vec<ImportedModuleAst>,
) -> (ProgramAst, Vec<(String, String)>) {
    let mut statements = Vec::new();
    let mut aliases = Vec::new();

    for module in modules {
        for stmt in module.ast.statements {
            if let Stmt::Function { name, .. } = &stmt {
                aliases.push((format!("{}.{}", module.name, name), name.clone()));
                statements.push(stmt);
            }
        }
    }

    statements.extend(
        entry
            .statements
            .into_iter()
            .filter(|stmt| !matches!(stmt, Stmt::Import { .. })),
    );

    (ProgramAst { statements }, aliases)
}

fn map_chunk_error(error: ChunkBuildError) -> CompileError {
    match error {
        ChunkBuildError::TooManyConstants { max } => {
            CompileError::new(CompileErrorKind::TooManyConstants { max }, None)
        }
        ChunkBuildError::TooManyStrings { max } => {
            CompileError::new(CompileErrorKind::TooManyStrings { max }, None)
        }
    }
}

fn map_program_error(error: ProgramBuildError) -> CompileError {
    match error {
        ProgramBuildError::TooManyFunctions { max } => {
            CompileError::new(CompileErrorKind::TooManyFunctions { max }, None)
        }
    }
}

fn append_generated_functions(
    program: &mut Program,
    mut functions: Vec<(FunctionId, Function)>,
) -> Result<(), CompileError> {
    functions.sort_by_key(|(id, _)| id.0);
    for (function_id, function) in functions {
        if usize::from(function_id.0) != program.functions.len() {
            return Err(CompileError::new(
                CompileErrorKind::TooManyFunctions {
                    max: u16::MAX as usize,
                },
                None,
            ));
        }
        program.add_function(function).map_err(map_program_error)?;
    }
    Ok(())
}

fn function_ids(
    program_ast: &ProgramAst,
    referenced_builtins: &[(&'static str, u8)],
    aliases: &[(String, String)],
) -> Result<HashMap<String, FunctionId>, CompileError> {
    let mut ids = HashMap::new();

    for stmt in &program_ast.statements {
        if let Stmt::Function { name, span, .. } = stmt {
            let id = FunctionId(ids.len().try_into().map_err(|_| {
                CompileError::new(
                    CompileErrorKind::TooManyFunctions {
                        max: u16::MAX as usize,
                    },
                    Some(*span),
                )
            })?);
            ids.insert(name.clone(), id);
        }
    }

    for (name, _) in referenced_builtins {
        let id = FunctionId(ids.len().try_into().map_err(|_| {
            CompileError::new(
                CompileErrorKind::TooManyFunctions {
                    max: u16::MAX as usize,
                },
                None,
            )
        })?);
        ids.insert((*name).to_string(), id);
    }

    for (alias, target) in aliases {
        if let Some(function) = ids.get(target).copied() {
            ids.entry(alias.clone()).or_insert(function);
        }
    }

    Ok(ids)
}

fn referenced_builtins(program_ast: &ProgramAst) -> Vec<(&'static str, u8)> {
    let mut referenced = Vec::new();
    for (name, arity) in BUILTIN_FUNCTIONS {
        if program_ast
            .statements
            .iter()
            .any(|stmt| stmt_references_call(stmt, name))
        {
            referenced.push((*name, *arity));
        }
    }
    referenced
}

fn stmt_references_call(stmt: &Stmt, name: &str) -> bool {
    match stmt {
        Stmt::Import { .. } => false,
        Stmt::Let { initializer, .. } => expr_references_call(initializer, name),
        Stmt::Function { body, .. }
        | Stmt::Block {
            statements: body, ..
        } => body.iter().any(|stmt| stmt_references_call(stmt, name)),
        Stmt::Assign { value, .. } => expr_references_call(value, name),
        Stmt::IndexAssign {
            target,
            index,
            value,
            ..
        } => {
            expr_references_call(target, name)
                || expr_references_call(index, name)
                || expr_references_call(value, name)
        }
        Stmt::Return { value, .. } | Stmt::Expr { value, .. } => expr_references_call(value, name),
        Stmt::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            expr_references_call(condition, name)
                || then_branch
                    .iter()
                    .any(|stmt| stmt_references_call(stmt, name))
                || else_branch
                    .iter()
                    .any(|stmt| stmt_references_call(stmt, name))
        }
        Stmt::While {
            condition, body, ..
        } => {
            expr_references_call(condition, name)
                || body.iter().any(|stmt| stmt_references_call(stmt, name))
        }
    }
}

fn expr_references_call(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Literal { .. } | Expr::Variable { .. } => false,
        Expr::Binary { lhs, rhs, .. } => {
            expr_references_call(lhs, name) || expr_references_call(rhs, name)
        }
        Expr::Call { callee, args, .. } => {
            callee == name || args.iter().any(|arg| expr_references_call(arg, name))
        }
        Expr::Index { target, index, .. } => {
            expr_references_call(target, name) || expr_references_call(index, name)
        }
        Expr::Array { elements, .. } => elements
            .iter()
            .any(|element| expr_references_call(element, name)),
        Expr::Map { entries, .. } => entries.iter().any(|(key, value)| {
            expr_references_call(key, name) || expr_references_call(value, name)
        }),
        Expr::Function { body, .. } => body.iter().any(|stmt| stmt_references_call(stmt, name)),
        Expr::Grouping { expr, .. } => expr_references_call(expr, name),
    }
}

fn free_variables(
    body: &[Stmt],
    params: &[String],
    functions: &HashMap<String, FunctionId>,
) -> Vec<String> {
    let mut locals = params.iter().cloned().collect::<HashSet<_>>();
    collect_local_names(body, &mut locals);
    let mut seen = HashSet::new();
    let mut free = Vec::new();
    for stmt in body {
        collect_stmt_free_variables(stmt, &locals, functions, &mut seen, &mut free);
    }
    free
}

fn collect_local_names(statements: &[Stmt], locals: &mut HashSet<String>) {
    for stmt in statements {
        match stmt {
            Stmt::Let { name, .. } | Stmt::Function { name, .. } => {
                locals.insert(name.clone());
            }
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                collect_local_names(then_branch, locals);
                collect_local_names(else_branch, locals);
            }
            Stmt::While { body, .. }
            | Stmt::Block {
                statements: body, ..
            } => {
                collect_local_names(body, locals);
            }
            Stmt::Import { .. }
            | Stmt::Assign { .. }
            | Stmt::IndexAssign { .. }
            | Stmt::Return { .. }
            | Stmt::Expr { .. } => {}
        }
    }
}

fn collect_stmt_free_variables(
    stmt: &Stmt,
    locals: &HashSet<String>,
    functions: &HashMap<String, FunctionId>,
    seen: &mut HashSet<String>,
    free: &mut Vec<String>,
) {
    match stmt {
        Stmt::Import { .. } | Stmt::Function { .. } => {}
        Stmt::Let { initializer, .. } => {
            collect_expr_free_variables(initializer, locals, functions, seen, free)
        }
        Stmt::Assign { name, value, .. } => {
            if !locals.contains(name) && !functions.contains_key(name) && seen.insert(name.clone())
            {
                free.push(name.clone());
            }
            collect_expr_free_variables(value, locals, functions, seen, free);
        }
        Stmt::IndexAssign {
            target,
            index,
            value,
            ..
        } => {
            collect_expr_free_variables(target, locals, functions, seen, free);
            collect_expr_free_variables(index, locals, functions, seen, free);
            collect_expr_free_variables(value, locals, functions, seen, free);
        }
        Stmt::Return { value, .. } | Stmt::Expr { value, .. } => {
            collect_expr_free_variables(value, locals, functions, seen, free);
        }
        Stmt::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            collect_expr_free_variables(condition, locals, functions, seen, free);
            for stmt in then_branch {
                collect_stmt_free_variables(stmt, locals, functions, seen, free);
            }
            for stmt in else_branch {
                collect_stmt_free_variables(stmt, locals, functions, seen, free);
            }
        }
        Stmt::While {
            condition, body, ..
        } => {
            collect_expr_free_variables(condition, locals, functions, seen, free);
            for stmt in body {
                collect_stmt_free_variables(stmt, locals, functions, seen, free);
            }
        }
        Stmt::Block { statements, .. } => {
            for stmt in statements {
                collect_stmt_free_variables(stmt, locals, functions, seen, free);
            }
        }
    }
}

fn collect_expr_free_variables(
    expr: &Expr,
    locals: &HashSet<String>,
    functions: &HashMap<String, FunctionId>,
    seen: &mut HashSet<String>,
    free: &mut Vec<String>,
) {
    match expr {
        Expr::Literal { .. } => {}
        Expr::Variable { name, .. } => {
            if !locals.contains(name) && !functions.contains_key(name) && seen.insert(name.clone())
            {
                free.push(name.clone());
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_free_variables(lhs, locals, functions, seen, free);
            collect_expr_free_variables(rhs, locals, functions, seen, free);
        }
        Expr::Call { callee, args, .. } => {
            if !locals.contains(callee)
                && !functions.contains_key(callee)
                && seen.insert(callee.clone())
            {
                free.push(callee.clone());
            }
            for arg in args {
                collect_expr_free_variables(arg, locals, functions, seen, free);
            }
        }
        Expr::Index { target, index, .. } => {
            collect_expr_free_variables(target, locals, functions, seen, free);
            collect_expr_free_variables(index, locals, functions, seen, free);
        }
        Expr::Array { elements, .. } => {
            for element in elements {
                collect_expr_free_variables(element, locals, functions, seen, free);
            }
        }
        Expr::Map { entries, .. } => {
            for (key, value) in entries {
                collect_expr_free_variables(key, locals, functions, seen, free);
                collect_expr_free_variables(value, locals, functions, seen, free);
            }
        }
        Expr::Function { params, body, .. } => {
            for name in free_variables(body, params, functions) {
                if !locals.contains(&name) && seen.insert(name.clone()) {
                    free.push(name);
                }
            }
        }
        Expr::Grouping { expr, .. } => {
            collect_expr_free_variables(expr, locals, functions, seen, free)
        }
    }
}

fn statements_definitely_return(statements: &[Stmt]) -> bool {
    statements.last().is_some_and(stmt_definitely_returns)
}

fn stmt_definitely_returns(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return { .. } => true,
        Stmt::Block { statements, .. } => statements_definitely_return(statements),
        Stmt::If {
            then_branch,
            else_branch,
            ..
        } => {
            !else_branch.is_empty()
                && statements_definitely_return(then_branch)
                && statements_definitely_return(else_branch)
        }
        Stmt::Let { .. }
        | Stmt::Import { .. }
        | Stmt::Function { .. }
        | Stmt::Assign { .. }
        | Stmt::IndexAssign { .. }
        | Stmt::While { .. }
        | Stmt::Expr { .. } => false,
    }
}
