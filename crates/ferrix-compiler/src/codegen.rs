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
        ProgramBuildError, Register, VerifiedProgram, optimize_chunk,
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
    let (linked, aliases) = link_modules(entry, modules);
    compile_program_ast_with_aliases(linked, aliases)
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
    locals: HashMap<String, Vec<Register>>,
    scopes: Vec<Vec<String>>,
    boxed_locals: HashSet<String>,
    captures: HashMap<String, CaptureId>,
    functions: HashMap<String, FunctionId>,
    next_register: u16,
    free_registers: Vec<Register>,
    next_function_id: u16,
    generated_functions: Vec<(FunctionId, Function)>,
}

impl Codegen {
    fn new_main(
        functions: HashMap<String, FunctionId>,
        next_function_id: u16,
        statements: &[Stmt],
    ) -> Self {
        let boxed_locals = captured_locals(statements, &[], &functions);
        Self {
            chunk: Chunk::new("main", 0),
            locals: HashMap::new(),
            scopes: vec![Vec::new()],
            boxed_locals,
            captures: HashMap::new(),
            functions,
            next_register: 0,
            free_registers: Vec::new(),
            next_function_id,
            generated_functions: Vec::new(),
        }
    }

    fn new_function(
        name: &str,
        params: &[String],
        functions: HashMap<String, FunctionId>,
        next_function_id: u16,
        body: &[Stmt],
    ) -> Result<Self, CompileError> {
        let arity = u8::try_from(params.len()).map_err(|_| {
            CompileError::new(
                CompileErrorKind::TooManyParameters {
                    max: u8::MAX as usize,
                },
                None,
            )
        })?;
        let boxed_locals = captured_locals(body, params, &functions);
        let mut compiler = Self {
            chunk: Chunk::new(name, 0).with_arity(arity),
            locals: HashMap::new(),
            scopes: vec![Vec::new()],
            boxed_locals,
            captures: HashMap::new(),
            functions,
            next_register: params.len() as u16,
            free_registers: Vec::new(),
            next_function_id,
            generated_functions: Vec::new(),
        };
        for (index, param) in params.iter().enumerate() {
            compiler.declare_local(param.clone(), Register(index as u8));
        }
        compiler.box_captured_parameters(params);
        Ok(compiler)
    }

    fn new_closure(
        name: &str,
        params: &[String],
        body: &[Stmt],
        captures: &[String],
        functions: HashMap<String, FunctionId>,
        next_function_id: u16,
    ) -> Result<Self, CompileError> {
        let mut compiler = Self::new_function(name, params, functions, next_function_id, body)?;
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
                ..
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
                    body,
                )?;
                compiler.compile_function_body(body)?;
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
            &program_ast.statements,
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

        Ok(optimize_chunk(self.chunk))
    }

    fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), CompileError> {
        match stmt {
            Stmt::Import { .. } => Ok(()),
            Stmt::Function { .. } => Ok(()),
            Stmt::Let {
                name, initializer, ..
            } => {
                let dst = self.alloc_register()?;
                if self.boxed_locals.contains(name) {
                    self.chunk
                        .push_instruction(Instruction::MakeUpvalue { dst, src: dst });
                    self.declare_local(name.clone(), dst);
                    let value = self.compile_expr(initializer)?;
                    self.chunk.push_instruction(Instruction::StoreUpvalue {
                        upvalue: dst,
                        src: value,
                    });
                } else {
                    self.compile_expr_into(initializer, dst)?;
                    self.declare_local(name.clone(), dst);
                }
                Ok(())
            }
            Stmt::Assign { name, value, span } => {
                if let Some(dst) = self.lookup_local(name) {
                    if self.boxed_locals.contains(name) {
                        let value = self.compile_expr(value)?;
                        self.chunk.push_instruction_with_span(
                            Instruction::StoreUpvalue {
                                upvalue: dst,
                                src: value,
                            },
                            Some(*span),
                        );
                        Ok(())
                    } else {
                        self.compile_expr_into(value, dst)
                    }
                } else if let Some(capture) = self.captures.get(name).copied() {
                    let value = self.compile_expr(value)?;
                    self.chunk.push_instruction_with_span(
                        Instruction::StoreCapture {
                            capture,
                            src: value,
                        },
                        Some(*span),
                    );
                    Ok(())
                } else {
                    Err(CompileError::new(
                        CompileErrorKind::UndefinedVariable { name: name.clone() },
                        Some(*span),
                    ))
                }
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
                self.release_register_if_temporary(target);
                self.release_register_if_temporary(index);
                self.release_register_if_temporary(value);
                Ok(())
            }
            Stmt::FieldAssign {
                target,
                field,
                value,
                span,
            } => {
                let target = self.compile_expr(target)?;
                let value = self.compile_expr(value)?;
                let field = self
                    .chunk
                    .add_string(field.clone())
                    .map_err(map_chunk_error)?;
                self.chunk.push_instruction_with_span(
                    Instruction::FieldSet {
                        target,
                        field,
                        value,
                    },
                    Some(*span),
                );
                self.release_register_if_temporary(target);
                self.release_register_if_temporary(value);
                Ok(())
            }
            Stmt::Return { value, span } => {
                let src = self.compile_expr(value)?;
                self.chunk
                    .push_instruction_with_span(Instruction::Return { src }, Some(*span));
                Ok(())
            }
            Stmt::Throw { value, span } => {
                let src = self.compile_expr(value)?;
                self.chunk
                    .push_instruction_with_span(Instruction::Throw { src }, Some(*span));
                Ok(())
            }
            Stmt::Expr { value, .. } => {
                let register = self.compile_expr(value)?;
                self.release_register_if_temporary(register);
                Ok(())
            }
            Stmt::TryCatch {
                try_branch,
                catch_name,
                catch_branch,
                span,
            } => self.compile_try_catch(try_branch, catch_name, catch_branch, *span),
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
            Stmt::Block { statements, .. } => self.compile_scoped_block(statements),
        }
    }

    fn compile_function_body(&mut self, statements: &[Stmt]) -> Result<(), CompileError> {
        self.compile_scoped_block(statements)
    }

    fn compile_block(&mut self, statements: &[Stmt]) -> Result<(), CompileError> {
        for stmt in statements {
            self.compile_stmt(stmt)?;
        }

        Ok(())
    }

    fn compile_scoped_block(&mut self, statements: &[Stmt]) -> Result<(), CompileError> {
        self.push_scope();
        let result = self.compile_block(statements);
        self.pop_scope();
        result
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
        self.compile_scoped_block(then_branch)?;

        if else_branch.is_empty() {
            self.patch_jump(jump_to_else, self.current_instruction_index()?)?;
        } else {
            let jump_to_end = if statements_definitely_return(then_branch) {
                None
            } else {
                Some(self.emit_jump(span)?)
            };
            self.patch_jump(jump_to_else, self.current_instruction_index()?)?;
            self.compile_scoped_block(else_branch)?;
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
        self.compile_scoped_block(body)?;
        self.chunk.push_instruction_with_span(
            Instruction::Jump {
                target: JumpTarget(loop_start),
            },
            Some(span),
        );
        self.patch_jump(jump_to_end, self.current_instruction_index()?)?;
        Ok(())
    }

    fn compile_try_catch(
        &mut self,
        try_branch: &[Stmt],
        catch_name: &str,
        catch_branch: &[Stmt],
        span: ferrix_core::diagnostics::SourceSpan,
    ) -> Result<(), CompileError> {
        let error = self.alloc_register()?;
        let handler = self.emit_push_handler(error, span)?;
        self.compile_scoped_block(try_branch)?;
        self.chunk
            .push_instruction_with_span(Instruction::PopHandler, Some(span));
        let jump_to_end = self.emit_jump(span)?;

        self.patch_handler(handler, self.current_instruction_index()?)?;
        self.push_scope();
        self.declare_local(catch_name.to_string(), error);
        let result = self.compile_block(catch_branch);
        self.pop_scope();
        result?;

        self.patch_jump(jump_to_end, self.current_instruction_index()?)?;
        Ok(())
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<Register, CompileError> {
        match expr {
            Expr::Variable { name, span } => {
                if let Some(register) = self.lookup_local(name) {
                    if self.boxed_locals.contains(name) {
                        let dst = self.alloc_register()?;
                        self.compile_expr_into(expr, dst)?;
                        Ok(dst)
                    } else {
                        Ok(register)
                    }
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
                if let Some(src) = self.lookup_local(name) {
                    if self.boxed_locals.contains(name) {
                        self.chunk.push_instruction_with_span(
                            Instruction::LoadUpvalue { dst, upvalue: src },
                            Some(*span),
                        );
                    } else if src != dst {
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
                if lhs_register != dst {
                    self.release_register_if_temporary(lhs_register);
                }
                if rhs_register != dst && rhs_register != lhs_register {
                    self.release_register_if_temporary(rhs_register);
                }
                Ok(())
            }
            Expr::Array { elements, span } => self.compile_array_into(elements, dst, *span),
            Expr::Map { entries, span } => self.compile_map_into(entries, dst, *span),
            Expr::Record { fields, span } => self.compile_record_into(fields, dst, *span),
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
                if target != dst {
                    self.release_register_if_temporary(target);
                }
                if index != dst && index != target {
                    self.release_register_if_temporary(index);
                }
                Ok(())
            }
            Expr::Field {
                target,
                field,
                span,
            } => {
                if let Some(name) = namespaced_field_name(target, field)
                    && (self.lookup_local(&name).is_some() || self.captures.contains_key(&name))
                {
                    return self.compile_expr_into(&Expr::Variable { name, span: *span }, dst);
                }

                let target = self.compile_expr(target)?;
                let field = self
                    .chunk
                    .add_string(field.clone())
                    .map_err(map_chunk_error)?;
                self.chunk.push_instruction_with_span(
                    Instruction::FieldGet { dst, target, field },
                    Some(*span),
                );
                if target != dst {
                    self.release_register_if_temporary(target);
                }
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
            let start = self.alloc_register_range(elements.len())?;
            let element_registers = register_range(start, elements.len())?;

            for (element, register) in elements.iter().zip(element_registers.iter().copied()) {
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
        self.release_register_range(elements_start, elements.len());
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
            let start = self.alloc_register_range(entries.len().saturating_mul(2))?;
            let entry_registers = register_range(start, entries.len().saturating_mul(2))?;

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
        self.release_register_range(entries_start, entries.len().saturating_mul(2));
        Ok(())
    }

    fn compile_record_into(
        &mut self,
        fields: &[(String, Expr)],
        dst: Register,
        span: ferrix_core::diagnostics::SourceSpan,
    ) -> Result<(), CompileError> {
        if fields.len() > u8::MAX as usize {
            return Err(CompileError::new(
                CompileErrorKind::TooManyRecordFields {
                    max: u8::MAX as usize,
                },
                Some(span),
            ));
        }

        let field_names = fields
            .iter()
            .map(|(field, _)| {
                self.chunk
                    .add_string(field.clone())
                    .map_err(map_chunk_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let fields_start = if fields.is_empty() {
            Register(0)
        } else {
            let start = self.alloc_register_range(fields.len())?;
            let field_registers = register_range(start, fields.len())?;

            for ((_, value), register) in fields.iter().zip(field_registers.iter().copied()) {
                self.compile_expr_into(value, register)?;
            }
            start
        };

        self.chunk.push_instruction_with_span(
            Instruction::RecordNew {
                dst,
                fields_start,
                fields: field_names,
            },
            Some(span),
        );
        self.release_register_range(fields_start, fields.len());
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
            let start = self.alloc_register_range(args.len())?;
            let arg_registers = register_range(start, args.len())?;

            for (arg, register) in args.iter().zip(arg_registers.iter().copied()) {
                self.compile_expr_into(arg, register)?;
            }
            start
        };

        if self.lookup_local(callee).is_some() || self.captures.contains_key(callee) {
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
            self.release_register_if_temporary(callee);
        } else if let Some(function) = self.functions.get(callee).copied() {
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
            self.release_register_if_temporary(callee);
        }
        self.release_register_range(args_start, args.len());
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
            .filter(|name| self.lookup_local(name).is_some() || self.captures.contains_key(name))
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
            body,
            &captures,
            self.functions.clone(),
            self.next_function_id,
        )?;
        compiler.compile_function_body(body)?;
        self.next_function_id = compiler.next_function_id;
        self.generated_functions
            .extend(std::mem::take(&mut compiler.generated_functions));
        self.generated_functions
            .push((function_id, Function::bytecode(compiler.finish_chunk()?)));

        let captures_start = if captures.is_empty() {
            Register(0)
        } else {
            let start = self.alloc_register_range(captures.len())?;
            let capture_registers = register_range(start, captures.len())?;
            for (name, register) in captures.iter().zip(capture_registers.iter().copied()) {
                if let Some(local) = self.lookup_local(name) {
                    if self.boxed_locals.contains(name) {
                        if local != register {
                            self.chunk.push_instruction_with_span(
                                Instruction::Move {
                                    dst: register,
                                    src: local,
                                },
                                Some(span),
                            );
                        }
                    } else {
                        self.chunk.push_instruction_with_span(
                            Instruction::MakeUpvalue {
                                dst: register,
                                src: local,
                            },
                            Some(span),
                        );
                    }
                } else if let Some(capture) = self.captures.get(name).copied() {
                    self.chunk.push_instruction_with_span(
                        Instruction::LoadCaptureCell {
                            dst: register,
                            capture,
                        },
                        Some(span),
                    );
                }
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
        self.release_register_range(captures_start, captures.len());
        Ok(())
    }

    fn alloc_register(&mut self) -> Result<Register, CompileError> {
        if let Some(register) = self.free_registers.pop() {
            return Ok(register);
        }

        let register = u8::try_from(self.next_register)
            .map_err(|_| CompileError::new(CompileErrorKind::TooManyRegisters, None))?;
        self.next_register += 1;
        Ok(Register(register))
    }

    fn alloc_register_range(&mut self, count: usize) -> Result<Register, CompileError> {
        if count == 0 {
            return Ok(Register(0));
        }

        if count == 1 {
            return self.alloc_register();
        }

        let start = self.next_register;
        let count = u16::try_from(count)
            .map_err(|_| CompileError::new(CompileErrorKind::TooManyRegisters, None))?;
        let next_register = self
            .next_register
            .checked_add(count)
            .ok_or_else(|| CompileError::new(CompileErrorKind::TooManyRegisters, None))?;
        if next_register > u16::from(u8::MAX) + 1 {
            return Err(CompileError::new(CompileErrorKind::TooManyRegisters, None));
        }

        self.next_register = next_register;
        Ok(Register(start as u8))
    }

    fn release_register_range(&mut self, start: Register, count: usize) {
        for offset in 0..count {
            let Some(register) = u8::try_from(offset)
                .ok()
                .and_then(|offset| start.0.checked_add(offset))
                .map(Register)
            else {
                break;
            };
            self.release_register_if_temporary(register);
        }
    }

    fn release_register_if_temporary(&mut self, register: Register) {
        if self.is_register_bound(register) || self.free_registers.contains(&register) {
            return;
        }
        self.free_registers.push(register);
    }

    fn is_register_bound(&self, register: Register) -> bool {
        self.locals
            .values()
            .any(|bindings| bindings.contains(&register))
    }

    fn lookup_local(&self, name: &str) -> Option<Register> {
        self.locals
            .get(name)
            .and_then(|bindings| bindings.last())
            .copied()
    }

    fn declare_local(&mut self, name: String, register: Register) {
        self.locals.entry(name.clone()).or_default().push(register);
        self.scopes
            .last_mut()
            .expect("codegen always has an active scope")
            .push(name);
    }

    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        let names = self
            .scopes
            .pop()
            .expect("codegen always has an active scope");
        let mut released = Vec::new();

        for name in names.into_iter().rev() {
            if let Some(bindings) = self.locals.get_mut(&name) {
                let register = bindings.pop();
                if bindings.is_empty() {
                    self.locals.remove(&name);
                }
                if let Some(register) = register {
                    released.push(register);
                }
            }
        }

        for register in released {
            self.release_register_if_temporary(register);
        }
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

    fn emit_push_handler(
        &mut self,
        error: Register,
        span: ferrix_core::diagnostics::SourceSpan,
    ) -> Result<usize, CompileError> {
        let index = self.chunk.instructions.len();
        self.chunk.push_instruction_with_span(
            Instruction::PushHandler {
                error,
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

    fn patch_handler(&mut self, instruction_index: usize, target: u32) -> Result<(), CompileError> {
        let instruction = self
            .chunk
            .instructions
            .get_mut(instruction_index)
            .expect("compiler emits handler before patching it");

        match instruction {
            Instruction::PushHandler {
                target: jump_target,
                ..
            } => *jump_target = JumpTarget(target),
            _ => unreachable!("only handler instructions are patched"),
        }

        Ok(())
    }

    fn current_instruction_index(&self) -> Result<u32, CompileError> {
        u32::try_from(self.chunk.instructions.len())
            .map_err(|_| CompileError::new(CompileErrorKind::TooManyInstructions, None))
    }

    fn box_captured_parameters(&mut self, params: &[String]) {
        for param in params {
            if let Some(register) = self.lookup_local(param)
                && self.boxed_locals.contains(param)
            {
                self.chunk.push_instruction(Instruction::MakeUpvalue {
                    dst: register,
                    src: register,
                });
            }
        }
    }
}

fn link_modules(
    entry: ProgramAst,
    modules: Vec<ProgramAst>,
) -> (ProgramAst, Vec<(String, String)>) {
    let mut statements = Vec::new();
    let mut aliases = Vec::new();

    for module in modules {
        let exports = module_exports(&module);
        for stmt in module.statements {
            match stmt {
                Stmt::Function { ref name, .. } if exports.functions.contains(name) => {
                    aliases.push((name.clone(), name.clone()));
                    statements.push(stmt);
                }
                Stmt::Let { ref name, .. } if exports.values.contains(name) => {
                    statements.push(stmt);
                }
                _ => {}
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

fn link_named_modules(
    entry: ProgramAst,
    modules: Vec<ImportedModuleAst>,
) -> (ProgramAst, Vec<(String, String)>) {
    let mut statements = Vec::new();
    let mut aliases = Vec::new();

    for module in modules {
        let exports = module_exports(&module.ast);
        let function_targets = module
            .ast
            .statements
            .iter()
            .filter_map(|stmt| match stmt {
                Stmt::Function { name, exported, .. } => {
                    let exported = exports.functions.contains(name) || *exported;
                    Some((
                        name.clone(),
                        module_function_name(&module.name, name, exported),
                    ))
                }
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let value_targets = exports
            .values
            .iter()
            .map(|name| (name.clone(), format!("{}.{}", module.name, name)))
            .collect::<HashMap<_, _>>();

        for stmt in module.ast.statements {
            match stmt {
                Stmt::Function {
                    name,
                    params,
                    body,
                    exported,
                    span,
                } => {
                    let exported = exports.functions.contains(&name) || exported;
                    let target = module_function_name(&module.name, &name, exported);
                    if exported {
                        aliases.push((format!("{}.{}", module.name, name), target.clone()));
                        aliases.push((name.clone(), target.clone()));
                    }
                    statements.push(Stmt::Function {
                        name: target,
                        params,
                        body: rewrite_module_statements(&function_targets, body),
                        exported: false,
                        span,
                    });
                }
                Stmt::Let {
                    name,
                    initializer,
                    exported,
                    span,
                } if exports.values.contains(&name) || exported => {
                    statements.push(Stmt::Let {
                        name: format!("{}.{}", module.name, name),
                        initializer: rewrite_module_value_expr(
                            &function_targets,
                            &value_targets,
                            initializer,
                        ),
                        exported: false,
                        span,
                    });
                }
                _ => {}
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

struct ModuleExports {
    functions: HashSet<String>,
    values: HashSet<String>,
}

fn module_exports(module: &ProgramAst) -> ModuleExports {
    let has_explicit_exports = module.statements.iter().any(|stmt| match stmt {
        Stmt::Function { exported, .. } | Stmt::Let { exported, .. } => *exported,
        _ => false,
    });
    let mut functions = HashSet::new();
    let mut values = HashSet::new();

    for stmt in &module.statements {
        match stmt {
            Stmt::Function { name, exported, .. } if *exported || !has_explicit_exports => {
                functions.insert(name.clone());
            }
            Stmt::Let { name, exported, .. } if *exported => {
                values.insert(name.clone());
            }
            _ => {}
        }
    }

    ModuleExports { functions, values }
}

fn module_function_name(module: &str, name: &str, exported: bool) -> String {
    if exported {
        format!("{module}.{name}")
    } else {
        format!("__module.{module}.{name}")
    }
}

fn rewrite_module_statements(
    function_targets: &HashMap<String, String>,
    statements: Vec<Stmt>,
) -> Vec<Stmt> {
    statements
        .into_iter()
        .map(|stmt| rewrite_module_stmt(function_targets, stmt))
        .collect()
}

fn rewrite_module_stmt(function_targets: &HashMap<String, String>, stmt: Stmt) -> Stmt {
    match stmt {
        Stmt::Let {
            name,
            initializer,
            exported,
            span,
        } => Stmt::Let {
            name,
            initializer: rewrite_module_expr(function_targets, initializer),
            exported,
            span,
        },
        Stmt::Function {
            name,
            params,
            body,
            exported,
            span,
        } => Stmt::Function {
            name,
            params,
            body: rewrite_module_statements(function_targets, body),
            exported,
            span,
        },
        Stmt::Assign { name, value, span } => Stmt::Assign {
            name,
            value: rewrite_module_expr(function_targets, value),
            span,
        },
        Stmt::IndexAssign {
            target,
            index,
            value,
            span,
        } => Stmt::IndexAssign {
            target: rewrite_module_expr(function_targets, target),
            index: rewrite_module_expr(function_targets, index),
            value: rewrite_module_expr(function_targets, value),
            span,
        },
        Stmt::FieldAssign {
            target,
            field,
            value,
            span,
        } => Stmt::FieldAssign {
            target: rewrite_module_expr(function_targets, target),
            field,
            value: rewrite_module_expr(function_targets, value),
            span,
        },
        Stmt::Return { value, span } => Stmt::Return {
            value: rewrite_module_expr(function_targets, value),
            span,
        },
        Stmt::Throw { value, span } => Stmt::Throw {
            value: rewrite_module_expr(function_targets, value),
            span,
        },
        Stmt::TryCatch {
            try_branch,
            catch_name,
            catch_branch,
            span,
        } => Stmt::TryCatch {
            try_branch: rewrite_module_statements(function_targets, try_branch),
            catch_name,
            catch_branch: rewrite_module_statements(function_targets, catch_branch),
            span,
        },
        Stmt::If {
            condition,
            then_branch,
            else_branch,
            span,
        } => Stmt::If {
            condition: rewrite_module_expr(function_targets, condition),
            then_branch: rewrite_module_statements(function_targets, then_branch),
            else_branch: rewrite_module_statements(function_targets, else_branch),
            span,
        },
        Stmt::While {
            condition,
            body,
            span,
        } => Stmt::While {
            condition: rewrite_module_expr(function_targets, condition),
            body: rewrite_module_statements(function_targets, body),
            span,
        },
        Stmt::Block { statements, span } => Stmt::Block {
            statements: rewrite_module_statements(function_targets, statements),
            span,
        },
        Stmt::Expr { value, span } => Stmt::Expr {
            value: rewrite_module_expr(function_targets, value),
            span,
        },
        Stmt::Import { .. } => stmt,
    }
}

fn rewrite_module_expr(function_targets: &HashMap<String, String>, expr: Expr) -> Expr {
    match expr {
        Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
            op,
            lhs: Box::new(rewrite_module_expr(function_targets, *lhs)),
            rhs: Box::new(rewrite_module_expr(function_targets, *rhs)),
            span,
        },
        Expr::Call { callee, args, span } => Expr::Call {
            callee: function_targets.get(&callee).cloned().unwrap_or(callee),
            args: args
                .into_iter()
                .map(|arg| rewrite_module_expr(function_targets, arg))
                .collect(),
            span,
        },
        Expr::Function { params, body, span } => Expr::Function {
            params,
            body: rewrite_module_statements(function_targets, body),
            span,
        },
        Expr::Index {
            target,
            index,
            span,
        } => Expr::Index {
            target: Box::new(rewrite_module_expr(function_targets, *target)),
            index: Box::new(rewrite_module_expr(function_targets, *index)),
            span,
        },
        Expr::Field {
            target,
            field,
            span,
        } => Expr::Field {
            target: Box::new(rewrite_module_expr(function_targets, *target)),
            field,
            span,
        },
        Expr::Array { elements, span } => Expr::Array {
            elements: elements
                .into_iter()
                .map(|element| rewrite_module_expr(function_targets, element))
                .collect(),
            span,
        },
        Expr::Map { entries, span } => Expr::Map {
            entries: entries
                .into_iter()
                .map(|(key, value)| {
                    (
                        rewrite_module_expr(function_targets, key),
                        rewrite_module_expr(function_targets, value),
                    )
                })
                .collect(),
            span,
        },
        Expr::Record { fields, span } => Expr::Record {
            fields: fields
                .into_iter()
                .map(|(field, value)| (field, rewrite_module_expr(function_targets, value)))
                .collect(),
            span,
        },
        Expr::Grouping { expr, span } => Expr::Grouping {
            expr: Box::new(rewrite_module_expr(function_targets, *expr)),
            span,
        },
        Expr::Literal { .. } | Expr::Variable { .. } => expr,
    }
}

fn rewrite_module_value_expr(
    function_targets: &HashMap<String, String>,
    value_targets: &HashMap<String, String>,
    expr: Expr,
) -> Expr {
    match expr {
        Expr::Variable { name, span } => Expr::Variable {
            name: value_targets.get(&name).cloned().unwrap_or(name),
            span,
        },
        Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
            op,
            lhs: Box::new(rewrite_module_value_expr(
                function_targets,
                value_targets,
                *lhs,
            )),
            rhs: Box::new(rewrite_module_value_expr(
                function_targets,
                value_targets,
                *rhs,
            )),
            span,
        },
        Expr::Call { callee, args, span } => Expr::Call {
            callee: function_targets.get(&callee).cloned().unwrap_or(callee),
            args: args
                .into_iter()
                .map(|arg| rewrite_module_value_expr(function_targets, value_targets, arg))
                .collect(),
            span,
        },
        Expr::Function { params, body, span } => Expr::Function {
            params,
            body: rewrite_module_statements(function_targets, body),
            span,
        },
        Expr::Index {
            target,
            index,
            span,
        } => Expr::Index {
            target: Box::new(rewrite_module_value_expr(
                function_targets,
                value_targets,
                *target,
            )),
            index: Box::new(rewrite_module_value_expr(
                function_targets,
                value_targets,
                *index,
            )),
            span,
        },
        Expr::Field {
            target,
            field,
            span,
        } => Expr::Field {
            target: Box::new(rewrite_module_value_expr(
                function_targets,
                value_targets,
                *target,
            )),
            field,
            span,
        },
        Expr::Array { elements, span } => Expr::Array {
            elements: elements
                .into_iter()
                .map(|element| rewrite_module_value_expr(function_targets, value_targets, element))
                .collect(),
            span,
        },
        Expr::Map { entries, span } => Expr::Map {
            entries: entries
                .into_iter()
                .map(|(key, value)| {
                    (
                        rewrite_module_value_expr(function_targets, value_targets, key),
                        rewrite_module_value_expr(function_targets, value_targets, value),
                    )
                })
                .collect(),
            span,
        },
        Expr::Record { fields, span } => Expr::Record {
            fields: fields
                .into_iter()
                .map(|(field, value)| {
                    (
                        field,
                        rewrite_module_value_expr(function_targets, value_targets, value),
                    )
                })
                .collect(),
            span,
        },
        Expr::Grouping { expr, span } => Expr::Grouping {
            expr: Box::new(rewrite_module_value_expr(
                function_targets,
                value_targets,
                *expr,
            )),
            span,
        },
        Expr::Literal { .. } => expr,
    }
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

fn namespaced_field_name(target: &Expr, field: &str) -> Option<String> {
    let Expr::Variable { name, .. } = target else {
        return None;
    };
    Some(format!("{name}.{field}"))
}

fn register_range(start: Register, count: usize) -> Result<Vec<Register>, CompileError> {
    (0..count)
        .map(|offset| {
            let offset = u8::try_from(offset)
                .map_err(|_| CompileError::new(CompileErrorKind::TooManyRegisters, None))?;
            start
                .0
                .checked_add(offset)
                .map(Register)
                .ok_or_else(|| CompileError::new(CompileErrorKind::TooManyRegisters, None))
        })
        .collect()
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
        Stmt::FieldAssign { target, value, .. } => {
            expr_references_call(target, name) || expr_references_call(value, name)
        }
        Stmt::Return { value, .. } | Stmt::Throw { value, .. } | Stmt::Expr { value, .. } => {
            expr_references_call(value, name)
        }
        Stmt::TryCatch {
            try_branch,
            catch_branch,
            ..
        } => {
            try_branch
                .iter()
                .any(|stmt| stmt_references_call(stmt, name))
                || catch_branch
                    .iter()
                    .any(|stmt| stmt_references_call(stmt, name))
        }
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
        Expr::Field { target, .. } => expr_references_call(target, name),
        Expr::Array { elements, .. } => elements
            .iter()
            .any(|element| expr_references_call(element, name)),
        Expr::Map { entries, .. } => entries.iter().any(|(key, value)| {
            expr_references_call(key, name) || expr_references_call(value, name)
        }),
        Expr::Record { fields, .. } => fields
            .iter()
            .any(|(_, value)| expr_references_call(value, name)),
        Expr::Function { body, .. } => body.iter().any(|stmt| stmt_references_call(stmt, name)),
        Expr::Grouping { expr, .. } => expr_references_call(expr, name),
    }
}

fn captured_locals(
    statements: &[Stmt],
    params: &[String],
    functions: &HashMap<String, FunctionId>,
) -> HashSet<String> {
    let mut scopes = NameScopes::new();
    for param in params {
        scopes.declare(param.clone());
    }
    let mut captured = HashSet::new();

    collect_scoped_captures(statements, &mut scopes, functions, &mut captured);
    captured
}

fn free_variables(
    body: &[Stmt],
    params: &[String],
    functions: &HashMap<String, FunctionId>,
) -> Vec<String> {
    let mut scopes = NameScopes::new();
    for param in params {
        scopes.declare(param.clone());
    }
    scopes.push_scope();
    let mut seen = HashSet::new();
    let mut free = Vec::new();

    collect_stmt_free_variables(body, &mut scopes, functions, &mut seen, &mut free);
    free
}

#[derive(Clone, Debug)]
struct NameScopes {
    scopes: Vec<HashSet<String>>,
}

impl NameScopes {
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
            .expect("name analysis always keeps one active scope");
    }

    fn contains(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|scope| scope.contains(name))
    }

    fn declare(&mut self, name: String) {
        self.scopes
            .last_mut()
            .expect("name analysis always has a current scope")
            .insert(name);
    }
}

fn collect_scoped_captures(
    statements: &[Stmt],
    scopes: &mut NameScopes,
    functions: &HashMap<String, FunctionId>,
    captured: &mut HashSet<String>,
) {
    scopes.push_scope();
    collect_stmt_captured_locals(statements, scopes, functions, captured);
    scopes.pop_scope();
}

fn collect_stmt_captured_locals(
    statements: &[Stmt],
    scopes: &mut NameScopes,
    functions: &HashMap<String, FunctionId>,
    captured: &mut HashSet<String>,
) {
    for stmt in statements {
        match stmt {
            Stmt::Import { .. } | Stmt::Function { .. } => {}
            Stmt::Let {
                name, initializer, ..
            } => {
                if matches!(initializer, Expr::Function { .. }) {
                    scopes.declare(name.clone());
                    collect_expr_captured_locals(initializer, scopes, functions, captured);
                } else {
                    collect_expr_captured_locals(initializer, scopes, functions, captured);
                    scopes.declare(name.clone());
                }
            }
            Stmt::Assign { value, .. } => {
                collect_expr_captured_locals(value, scopes, functions, captured);
            }
            Stmt::IndexAssign {
                target,
                index,
                value,
                ..
            } => {
                collect_expr_captured_locals(target, scopes, functions, captured);
                collect_expr_captured_locals(index, scopes, functions, captured);
                collect_expr_captured_locals(value, scopes, functions, captured);
            }
            Stmt::FieldAssign { target, value, .. } => {
                collect_expr_captured_locals(target, scopes, functions, captured);
                collect_expr_captured_locals(value, scopes, functions, captured);
            }
            Stmt::Return { value, .. } | Stmt::Throw { value, .. } | Stmt::Expr { value, .. } => {
                collect_expr_captured_locals(value, scopes, functions, captured);
            }
            Stmt::TryCatch {
                try_branch,
                catch_name,
                catch_branch,
                ..
            } => {
                collect_scoped_captures(try_branch, scopes, functions, captured);
                scopes.push_scope();
                scopes.declare(catch_name.clone());
                collect_stmt_captured_locals(catch_branch, scopes, functions, captured);
                scopes.pop_scope();
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                collect_expr_captured_locals(condition, scopes, functions, captured);
                collect_scoped_captures(then_branch, scopes, functions, captured);
                collect_scoped_captures(else_branch, scopes, functions, captured);
            }
            Stmt::While {
                condition, body, ..
            } => {
                collect_expr_captured_locals(condition, scopes, functions, captured);
                collect_scoped_captures(body, scopes, functions, captured);
            }
            Stmt::Block { statements, .. } => {
                collect_scoped_captures(statements, scopes, functions, captured);
            }
        }
    }
}

fn collect_expr_captured_locals(
    expr: &Expr,
    scopes: &mut NameScopes,
    functions: &HashMap<String, FunctionId>,
    captured: &mut HashSet<String>,
) {
    match expr {
        Expr::Literal { .. } | Expr::Variable { .. } => {}
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_captured_locals(lhs, scopes, functions, captured);
            collect_expr_captured_locals(rhs, scopes, functions, captured);
        }
        Expr::Call { args, .. } => {
            for arg in args {
                collect_expr_captured_locals(arg, scopes, functions, captured);
            }
        }
        Expr::Index { target, index, .. } => {
            collect_expr_captured_locals(target, scopes, functions, captured);
            collect_expr_captured_locals(index, scopes, functions, captured);
        }
        Expr::Field { target, .. } => {
            collect_expr_captured_locals(target, scopes, functions, captured);
        }
        Expr::Array { elements, .. } => {
            for element in elements {
                collect_expr_captured_locals(element, scopes, functions, captured);
            }
        }
        Expr::Map { entries, .. } => {
            for (key, value) in entries {
                collect_expr_captured_locals(key, scopes, functions, captured);
                collect_expr_captured_locals(value, scopes, functions, captured);
            }
        }
        Expr::Record { fields, .. } => {
            for (_, value) in fields {
                collect_expr_captured_locals(value, scopes, functions, captured);
            }
        }
        Expr::Function { params, body, .. } => {
            for name in free_variables(body, params, functions) {
                if scopes.contains(&name) {
                    captured.insert(name);
                }
            }
        }
        Expr::Grouping { expr, .. } => {
            collect_expr_captured_locals(expr, scopes, functions, captured);
        }
    }
}

fn collect_stmt_free_variables(
    statements: &[Stmt],
    scopes: &mut NameScopes,
    functions: &HashMap<String, FunctionId>,
    seen: &mut HashSet<String>,
    free: &mut Vec<String>,
) {
    for stmt in statements {
        match stmt {
            Stmt::Import { .. } | Stmt::Function { .. } => {}
            Stmt::Let {
                name, initializer, ..
            } => {
                if matches!(initializer, Expr::Function { .. }) {
                    scopes.declare(name.clone());
                    collect_expr_free_variables(initializer, scopes, functions, seen, free);
                } else {
                    collect_expr_free_variables(initializer, scopes, functions, seen, free);
                    scopes.declare(name.clone());
                }
            }
            Stmt::Assign { name, value, .. } => {
                if !scopes.contains(name)
                    && !functions.contains_key(name)
                    && seen.insert(name.clone())
                {
                    free.push(name.clone());
                }
                collect_expr_free_variables(value, scopes, functions, seen, free);
            }
            Stmt::IndexAssign {
                target,
                index,
                value,
                ..
            } => {
                collect_expr_free_variables(target, scopes, functions, seen, free);
                collect_expr_free_variables(index, scopes, functions, seen, free);
                collect_expr_free_variables(value, scopes, functions, seen, free);
            }
            Stmt::FieldAssign { target, value, .. } => {
                collect_expr_free_variables(target, scopes, functions, seen, free);
                collect_expr_free_variables(value, scopes, functions, seen, free);
            }
            Stmt::Return { value, .. } | Stmt::Throw { value, .. } | Stmt::Expr { value, .. } => {
                collect_expr_free_variables(value, scopes, functions, seen, free);
            }
            Stmt::TryCatch {
                try_branch,
                catch_name,
                catch_branch,
                ..
            } => {
                collect_scoped_free_variables(try_branch, scopes, functions, seen, free);
                scopes.push_scope();
                scopes.declare(catch_name.clone());
                collect_stmt_free_variables(catch_branch, scopes, functions, seen, free);
                scopes.pop_scope();
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                collect_expr_free_variables(condition, scopes, functions, seen, free);
                collect_scoped_free_variables(then_branch, scopes, functions, seen, free);
                collect_scoped_free_variables(else_branch, scopes, functions, seen, free);
            }
            Stmt::While {
                condition, body, ..
            } => {
                collect_expr_free_variables(condition, scopes, functions, seen, free);
                collect_scoped_free_variables(body, scopes, functions, seen, free);
            }
            Stmt::Block { statements, .. } => {
                collect_scoped_free_variables(statements, scopes, functions, seen, free);
            }
        }
    }
}

fn collect_scoped_free_variables(
    statements: &[Stmt],
    scopes: &mut NameScopes,
    functions: &HashMap<String, FunctionId>,
    seen: &mut HashSet<String>,
    free: &mut Vec<String>,
) {
    scopes.push_scope();
    collect_stmt_free_variables(statements, scopes, functions, seen, free);
    scopes.pop_scope();
}

fn collect_expr_free_variables(
    expr: &Expr,
    scopes: &mut NameScopes,
    functions: &HashMap<String, FunctionId>,
    seen: &mut HashSet<String>,
    free: &mut Vec<String>,
) {
    match expr {
        Expr::Literal { .. } => {}
        Expr::Variable { name, .. } => {
            if !scopes.contains(name) && !functions.contains_key(name) && seen.insert(name.clone())
            {
                free.push(name.clone());
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_free_variables(lhs, scopes, functions, seen, free);
            collect_expr_free_variables(rhs, scopes, functions, seen, free);
        }
        Expr::Call { callee, args, .. } => {
            if !scopes.contains(callee)
                && !functions.contains_key(callee)
                && seen.insert(callee.clone())
            {
                free.push(callee.clone());
            }
            for arg in args {
                collect_expr_free_variables(arg, scopes, functions, seen, free);
            }
        }
        Expr::Index { target, index, .. } => {
            collect_expr_free_variables(target, scopes, functions, seen, free);
            collect_expr_free_variables(index, scopes, functions, seen, free);
        }
        Expr::Field { target, .. } => {
            collect_expr_free_variables(target, scopes, functions, seen, free);
        }
        Expr::Array { elements, .. } => {
            for element in elements {
                collect_expr_free_variables(element, scopes, functions, seen, free);
            }
        }
        Expr::Map { entries, .. } => {
            for (key, value) in entries {
                collect_expr_free_variables(key, scopes, functions, seen, free);
                collect_expr_free_variables(value, scopes, functions, seen, free);
            }
        }
        Expr::Record { fields, .. } => {
            for (_, value) in fields {
                collect_expr_free_variables(value, scopes, functions, seen, free);
            }
        }
        Expr::Function { params, body, .. } => {
            for name in free_variables(body, params, functions) {
                if !scopes.contains(&name) && seen.insert(name.clone()) {
                    free.push(name);
                }
            }
        }
        Expr::Grouping { expr, .. } => {
            collect_expr_free_variables(expr, scopes, functions, seen, free)
        }
    }
}

fn statements_definitely_return(statements: &[Stmt]) -> bool {
    statements.last().is_some_and(stmt_definitely_returns)
}

fn stmt_definitely_returns(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return { .. } | Stmt::Throw { .. } => true,
        Stmt::Block { statements, .. } => statements_definitely_return(statements),
        Stmt::TryCatch {
            try_branch,
            catch_branch,
            ..
        } => statements_definitely_return(try_branch) && statements_definitely_return(catch_branch),
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
        | Stmt::FieldAssign { .. }
        | Stmt::While { .. }
        | Stmt::Expr { .. } => false,
    }
}
