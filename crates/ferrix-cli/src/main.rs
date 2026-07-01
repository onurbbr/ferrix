//! Command-line entry point for Ferrix.
//!
//! Provides source execution, bytecode compile/run commands, static import
//! loading, and an instruction-level debugger.

use std::{
    collections::HashSet,
    env, fs, io,
    path::{Path, PathBuf},
};

use ferrix_compiler::{
    CompileError, ImportedModuleAst,
    ast::{ProgramAst, Stmt},
    compile_program_ast_with_named_modules, parse_source_with_file_id,
};
use ferrix_core::{
    Obj, Value,
    bytecode::{FunctionId, VerifiedProgram, decode_program, encode_program, format_instruction},
    diagnostics::SourceManager,
};
use ferrix_vm::{DebugAction, DebugEvent, DebugOutcome, Debugger, Heap, Vm};

const USAGE: &str = "\
Ferrix

Usage:
  ferrix run <file>
  ferrix compile <file> <output>
  ferrix run-bytecode <file>
  ferrix debug <file>
  ferrix --help
  ferrix --version
";

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    let code = run_cli(
        &args,
        |path| fs::read_to_string(path),
        &mut stdin,
        &mut stdout,
        &mut stderr,
    );
    std::process::exit(code);
}

fn run_cli(
    args: &[String],
    mut read_file: impl FnMut(&str) -> io::Result<String>,
    stdin: &mut impl io::BufRead,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    // Keep command dispatch small and testable by injecting I/O handles.
    match args {
        [] => {
            write!(stdout, "{USAGE}").expect("stdout write failed");
            0
        }
        [flag] if flag == "--help" || flag == "-h" => {
            write!(stdout, "{USAGE}").expect("stdout write failed");
            0
        }
        [flag] if flag == "--version" || flag == "-V" => {
            writeln!(stdout, "ferrix {}", env!("CARGO_PKG_VERSION")).expect("stdout write failed");
            0
        }
        [command, path] if command == "run" => run_file(path, &mut read_file, stdout, stderr),
        [command, path, output] if command == "compile" => {
            compile_bytecode(path, output, &mut read_file, stderr)
        }
        [command, path] if command == "run-bytecode" => run_bytecode(path, stdout, stderr),
        [command, path] if command == "debug" => {
            debug_file(path, &mut read_file, stdin, stdout, stderr)
        }
        [command, ..] if command == "run" => {
            writeln!(stderr, "error: expected a file path\n").expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "compile" => {
            writeln!(stderr, "error: expected input and output file paths\n")
                .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "run-bytecode" => {
            writeln!(stderr, "error: expected a bytecode file path\n")
                .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "debug" => {
            writeln!(stderr, "error: expected a file path\n").expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        _ => {
            writeln!(stderr, "error: unknown command\n").expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
    }
}

fn compile_bytecode(
    path: &str,
    output: &str,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    stderr: &mut impl io::Write,
) -> i32 {
    // Source compilation and bytecode encoding are separated so diagnostics
    // still point at the original source file before serialization happens.
    let (_, program) = match compile_file(path, read_file, stderr) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    let bytes = match encode_program(program.as_program()) {
        Ok(bytes) => bytes,
        Err(error) => {
            writeln!(stderr, "error: could not encode bytecode: {error}")
                .expect("stderr write failed");
            return 65;
        }
    };
    if let Err(error) = fs::write(output, bytes) {
        writeln!(stderr, "error: could not write `{output}`: {error}")
            .expect("stderr write failed");
        return 66;
    }
    0
}

fn run_bytecode(path: &str, stdout: &mut impl io::Write, stderr: &mut impl io::Write) -> i32 {
    // Bytecode files are decoded as verified programs before entering the VM.
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            writeln!(stderr, "error: could not read `{path}`: {error}")
                .expect("stderr write failed");
            return 66;
        }
    };
    let program = match decode_program(&bytes) {
        Ok(program) => program,
        Err(error) => {
            writeln!(stderr, "error: could not decode bytecode `{path}`: {error}")
                .expect("stderr write failed");
            return 65;
        }
    };
    let mut vm = Vm::new();
    ferrix_stdlib::install(&mut vm, program.as_program());
    match vm.run_program(&program) {
        Ok(value) => {
            if value != Value::Nil {
                writeln!(stdout, "{}", display_value(&vm, value)).expect("stdout write failed");
            }
            0
        }
        Err(error) => {
            writeln!(stderr, "error: {error}").expect("stderr write failed");
            70
        }
    }
}

fn run_file(
    path: &str,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    // Normal source execution compiles imports first, installs stdlib natives,
    // and then renders runtime errors with source-aware diagnostics.
    let (sources, program) = match compile_file(path, read_file, stderr) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };

    let mut vm = Vm::new();
    ferrix_stdlib::install(&mut vm, program.as_program());
    match vm.run_program(&program) {
        Ok(value) => {
            if value != Value::Nil {
                writeln!(stdout, "{}", display_value(&vm, value)).expect("stdout write failed");
            }
            0
        }
        Err(error) => {
            let diagnostic = error.to_diagnostic_with_program(program.as_program());
            write!(stderr, "{}", sources.render_diagnostic(&diagnostic))
                .expect("stderr write failed");
            70
        }
    }
}

fn debug_file(
    path: &str,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    stdin: &mut impl io::BufRead,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    // The CLI debugger is intentionally built on the same VM debugger trait
    // external tools can implement.
    let (sources, program) = match compile_file(path, read_file, stderr) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };

    let mut vm = Vm::new();
    ferrix_stdlib::install(&mut vm, program.as_program());
    let outcome = {
        let mut debugger = CliDebugger::new(stdin, stdout, &sources);
        vm.run_program_with_debugger(&program, &mut debugger)
    };

    match outcome {
        Ok(DebugOutcome::Completed(value)) => {
            if value == Value::Nil {
                writeln!(stdout, "debug: finished").expect("stdout write failed");
            } else {
                writeln!(stdout, "debug: finished with {}", display_value(&vm, value))
                    .expect("stdout write failed");
            }
            0
        }
        Ok(DebugOutcome::Quit) => {
            writeln!(stdout, "debug: quit").expect("stdout write failed");
            0
        }
        Err(error) => {
            let diagnostic = error.to_diagnostic_with_program(program.as_program());
            write!(stderr, "{}", sources.render_diagnostic(&diagnostic))
                .expect("stderr write failed");
            70
        }
    }
}

fn compile_file(
    path: &str,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    stderr: &mut impl io::Write,
) -> Result<(SourceManager, VerifiedProgram), i32> {
    // Load the import graph into one source manager so parse/codegen/runtime
    // diagnostics can all render against the same file table.
    let mut sources = SourceManager::new();
    let graph = match load_module_graph(Path::new(path), read_file, &mut sources) {
        Ok(graph) => graph,
        Err(LoadError::Read { path, error }) => {
            writeln!(
                stderr,
                "error: could not read `{}`: {error}",
                path.display()
            )
            .expect("stderr write failed");
            return Err(66);
        }
        Err(LoadError::Compile(error)) => {
            write!(
                stderr,
                "{}",
                sources.render_diagnostic(&error.to_diagnostic())
            )
            .expect("stderr write failed");
            return Err(65);
        }
        Err(LoadError::ImportCycle { path }) => {
            writeln!(stderr, "error: import cycle involving `{}`", path.display())
                .expect("stderr write failed");
            return Err(65);
        }
    };

    let program = match compile_program_ast_with_named_modules(graph.entry, graph.modules) {
        Ok(program) => program,
        Err(error) => {
            write!(
                stderr,
                "{}",
                sources.render_diagnostic(&error.to_diagnostic())
            )
            .expect("stderr write failed");
            return Err(65);
        }
    };

    Ok((sources, program))
}

struct LoadedGraph {
    /// Entry file AST compiled as the program's main module.
    entry: ProgramAst,
    /// Imported module ASTs paired with namespace names.
    modules: Vec<ImportedModuleAst>,
}

enum LoadError {
    /// A source file could not be read from disk.
    Read { path: PathBuf, error: io::Error },
    /// Lexing/parsing one source file failed.
    Compile(CompileError),
    /// Recursive imports reached a file already on the active load stack.
    ImportCycle { path: PathBuf },
}

fn load_module_graph(
    entry_path: &Path,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    sources: &mut SourceManager,
) -> Result<LoadedGraph, LoadError> {
    // `loaded` prevents duplicate work; `visiting` detects import cycles.
    let mut loaded = HashSet::new();
    let mut visiting = HashSet::new();
    let mut modules = Vec::new();
    let entry = load_module(
        entry_path,
        read_file,
        sources,
        &mut loaded,
        &mut visiting,
        &mut modules,
    )?
    .expect("entry module is always loaded");

    Ok(LoadedGraph { entry, modules })
}

fn load_module(
    path: &Path,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    sources: &mut SourceManager,
    loaded: &mut HashSet<PathBuf>,
    visiting: &mut HashSet<PathBuf>,
    modules: &mut Vec<ImportedModuleAst>,
) -> Result<Option<ProgramAst>, LoadError> {
    let key = module_key(path);
    if loaded.contains(&key) {
        return Ok(None);
    }
    if !visiting.insert(key.clone()) {
        return Err(LoadError::ImportCycle {
            path: path.to_path_buf(),
        });
    }

    let path_name = path.display().to_string();
    let source = read_file(&path_name).map_err(|error| LoadError::Read {
        path: path.to_path_buf(),
        error,
    })?;
    let file_id = sources.add_file(path_name, source.clone());
    let ast = parse_source_with_file_id(&source, file_id).map_err(LoadError::Compile)?;

    for module in module_imports(&ast) {
        let import_path = resolve_import(path, module);
        if let Some(module_ast) =
            load_module(&import_path, read_file, sources, loaded, visiting, modules)?
        {
            modules.push(ImportedModuleAst {
                name: module.to_string(),
                ast: module_ast,
            });
        }
    }

    visiting.remove(&key);
    loaded.insert(key);
    Ok(Some(ast))
}

fn module_imports(ast: &ProgramAst) -> impl Iterator<Item = &str> {
    ast.statements.iter().filter_map(|stmt| match stmt {
        Stmt::Import { module, .. } => Some(module.as_str()),
        _ => None,
    })
}

fn resolve_import(importer: &Path, module: &str) -> PathBuf {
    importer
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(format!("{module}.fx"))
}

fn module_key(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Breakpoint {
    /// Specific function to stop in, or `None` for any function.
    function: Option<FunctionId>,
    /// Instruction pointer within the selected function scope.
    instruction_ip: usize,
}

enum DebugMode {
    /// Stop before every instruction.
    Step,
    /// Run until program end or a matching breakpoint.
    Continue,
}

/// Small interactive debugger used by `ferrix debug`.
struct CliDebugger<'a, I, W>
where
    I: io::BufRead,
    W: io::Write,
{
    input: &'a mut I,
    output: &'a mut W,
    sources: &'a SourceManager,
    mode: DebugMode,
    breakpoints: HashSet<Breakpoint>,
}

impl<'a, I, W> CliDebugger<'a, I, W>
where
    I: io::BufRead,
    W: io::Write,
{
    fn new(input: &'a mut I, output: &'a mut W, sources: &'a SourceManager) -> Self {
        Self {
            input,
            output,
            sources,
            mode: DebugMode::Step,
            breakpoints: HashSet::new(),
        }
    }

    fn should_stop(&self, event: &DebugEvent<'_>) -> bool {
        // Function-scoped breakpoints win when present; global breakpoints are
        // useful for tiny programs where instruction ids are enough.
        matches!(self.mode, DebugMode::Step)
            || self.breakpoints.contains(&Breakpoint {
                function: Some(event.function),
                instruction_ip: event.instruction_ip,
            })
            || self.breakpoints.contains(&Breakpoint {
                function: None,
                instruction_ip: event.instruction_ip,
            })
    }

    fn print_stop(&mut self, event: &DebugEvent<'_>) {
        writeln!(
            self.output,
            "stopped at {} {} ip={}: {}",
            event.function,
            event.function_name,
            event.instruction_ip,
            format_instruction(event.instruction)
        )
        .expect("stdout write failed");
        if let Some(span) = event.source_span
            && let Some(location) = self.sources.location(span)
        {
            let name = self
                .sources
                .file(location.file_id)
                .map(|file| file.name.as_str())
                .unwrap_or("<source>");
            writeln!(
                self.output,
                " --> {name}:{}:{}",
                location.line, location.column
            )
            .expect("stdout write failed");
            if let Some(line) = self.sources.line_text(location.file_id, location.line) {
                writeln!(self.output, "  | {line}").expect("stdout write failed");
            }
        }
    }

    fn command_loop(&mut self, event: &DebugEvent<'_>) -> DebugAction {
        // This loop keeps state-changing debugger commands local to the CLI
        // while the VM only sees Step/Continue/Quit actions.
        loop {
            write!(self.output, "debug> ").expect("stdout write failed");
            self.output.flush().expect("stdout flush failed");

            let mut line = String::new();
            let bytes = self.input.read_line(&mut line).expect("stdin read failed");
            if bytes == 0 {
                return DebugAction::Quit;
            }

            let command = line.trim();
            if command.is_empty() || matches!(command, "s" | "step" | "n" | "next") {
                self.mode = DebugMode::Step;
                return DebugAction::Step;
            }
            if matches!(command, "c" | "continue") {
                self.mode = DebugMode::Continue;
                return DebugAction::Continue;
            }
            if matches!(command, "q" | "quit") {
                return DebugAction::Quit;
            }
            if matches!(command, "r" | "registers" | "regs") {
                self.print_registers(event);
                continue;
            }
            if matches!(command, "bt" | "stack") {
                self.print_stack(event);
                continue;
            }
            if matches!(command, "i" | "instruction") {
                self.print_stop(event);
                continue;
            }
            if matches!(command, "h" | "help") {
                self.print_help();
                continue;
            }
            if let Some(spec) = command
                .strip_prefix("break ")
                .or_else(|| command.strip_prefix("b "))
            {
                self.set_breakpoint(event, spec.trim());
                continue;
            }
            if let Some(spec) = command
                .strip_prefix("clear ")
                .or_else(|| command.strip_prefix("cl "))
            {
                self.clear_breakpoint(event, spec.trim());
                continue;
            }
            if matches!(command, "clear" | "cl") {
                self.breakpoints.clear();
                writeln!(self.output, "cleared all breakpoints").expect("stdout write failed");
                continue;
            }

            writeln!(self.output, "unknown command `{command}`; type `help`")
                .expect("stdout write failed");
        }
    }

    fn print_registers(&mut self, event: &DebugEvent<'_>) {
        if event.registers.is_empty() {
            writeln!(self.output, "registers: <empty>").expect("stdout write failed");
            return;
        }

        for (index, value) in event.registers.iter().copied().enumerate() {
            writeln!(
                self.output,
                "r{index} = {}",
                display_heap_value(event.heap, value)
            )
            .expect("stdout write failed");
        }
    }

    fn print_stack(&mut self, event: &DebugEvent<'_>) {
        for frame in event.frames.iter().rev() {
            let name = event
                .program
                .function(frame.function_id)
                .map(|function| function.name.as_str())
                .unwrap_or("<unknown>");
            writeln!(
                self.output,
                "at {name} ({}, ip={})",
                frame.function_id, frame.ip
            )
            .expect("stdout write failed");
        }
    }

    fn print_help(&mut self) {
        writeln!(
            self.output,
            "\
commands:
  step | s              execute one instruction
  continue | c          run until program end or breakpoint
  break <ip>            stop at instruction ip in any function
  break <fn>:<ip>       stop at instruction ip in function id or name
  clear [breakpoint]    clear one breakpoint or all breakpoints
  registers | r         print current registers
  stack | bt            print call stack
  instruction | i       print current instruction
  quit | q              stop debugging"
        )
        .expect("stdout write failed");
    }

    fn set_breakpoint(&mut self, event: &DebugEvent<'_>, spec: &str) {
        let Some(breakpoint) = parse_breakpoint(event, spec) else {
            writeln!(self.output, "invalid breakpoint `{spec}`").expect("stdout write failed");
            return;
        };
        self.breakpoints.insert(breakpoint.clone());
        self.print_breakpoint("set", &breakpoint);
    }

    fn clear_breakpoint(&mut self, event: &DebugEvent<'_>, spec: &str) {
        let Some(breakpoint) = parse_breakpoint(event, spec) else {
            writeln!(self.output, "invalid breakpoint `{spec}`").expect("stdout write failed");
            return;
        };
        if self.breakpoints.remove(&breakpoint) {
            self.print_breakpoint("cleared", &breakpoint);
        } else {
            self.print_breakpoint("not found", &breakpoint);
        }
    }

    fn print_breakpoint(&mut self, prefix: &str, breakpoint: &Breakpoint) {
        match breakpoint.function {
            Some(function) => writeln!(
                self.output,
                "{prefix} breakpoint at {function}:{}",
                breakpoint.instruction_ip
            ),
            None => writeln!(
                self.output,
                "{prefix} breakpoint at ip={}",
                breakpoint.instruction_ip
            ),
        }
        .expect("stdout write failed");
    }
}

impl<I, W> Debugger for CliDebugger<'_, I, W>
where
    I: io::BufRead,
    W: io::Write,
{
    fn before_instruction(&mut self, event: DebugEvent<'_>) -> DebugAction {
        if !self.should_stop(&event) {
            return DebugAction::Continue;
        }

        self.mode = DebugMode::Step;
        self.print_stop(&event);
        self.command_loop(&event)
    }
}

fn parse_breakpoint(event: &DebugEvent<'_>, spec: &str) -> Option<Breakpoint> {
    if let Some((function, ip)) = spec.split_once(':') {
        return Some(Breakpoint {
            function: Some(parse_function(event, function.trim())?),
            instruction_ip: ip.trim().parse().ok()?,
        });
    }

    Some(Breakpoint {
        function: None,
        instruction_ip: spec.parse().ok()?,
    })
}

fn parse_function(event: &DebugEvent<'_>, spec: &str) -> Option<FunctionId> {
    let id = spec.strip_prefix("fn#").unwrap_or(spec);
    if let Ok(id) = id.parse::<u16>() {
        return Some(FunctionId(id));
    }

    event
        .program
        .functions
        .iter()
        .enumerate()
        .find_map(|(index, function)| {
            if function.name == spec {
                Some(FunctionId(index as u16))
            } else {
                None
            }
        })
}

fn display_value(vm: &Vm, value: Value) -> String {
    display_heap_value(vm.heap(), value)
}

fn display_heap_value(heap: &Heap, value: Value) -> String {
    display_heap_value_inner(heap, value, 0)
}

fn display_heap_value_inner(heap: &Heap, value: Value, depth: usize) -> String {
    if depth >= 8 {
        return "...".to_string();
    }

    match value {
        Value::Obj(reference) => match heap.get(reference) {
            Ok(Obj::String(value)) => value.clone(),
            Ok(Obj::Array(values)) => {
                let values = values
                    .iter()
                    .copied()
                    .map(|value| display_heap_value_inner(heap, value, depth + 1))
                    .collect::<Vec<_>>();
                format!("[{}]", values.join(", "))
            }
            Ok(Obj::Map(entries)) => {
                let entries = entries
                    .iter()
                    .map(|(key, value)| {
                        format!(
                            "{}: {}",
                            display_heap_value_inner(heap, *key, depth + 1),
                            display_heap_value_inner(heap, *value, depth + 1)
                        )
                    })
                    .collect::<Vec<_>>();
                format!("{{{}}}", entries.join(", "))
            }
            _ => value.to_string(),
        },
        _ => value.to_string(),
    }
}
