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
    bytecode::{FunctionId, VerifiedProgram, encode_program, format_instruction},
    diagnostics::{SourceLocation, SourceManager},
};
use ferrix_runtime::{
    DebugRequest, RunBytecodeRequest, RunSourceRequest, RuntimeGateway, RuntimeMode,
};
use ferrix_vm::{CallFrame, DebugAction, DebugEvent, DebugOutcome, Debugger, Heap, Vm};

const USAGE: &str = "\
Ferrix

Usage:
  ferrix run <file|package>
  ferrix check <file|package>
  ferrix compile <file|package> <output>
  ferrix run-bytecode <file>
  ferrix debug <file|package>
  ferrix --help
  ferrix --version
";

const MANIFEST_FILES: &[&str] = &["Ferrix.toml", "ferrix.toml"];
const RUNTIME_MODE_ENV: &str = "FERRIX_RUNTIME_MODE";

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
        [command, path] if command == "run" => run_file(path, stdout, stderr),
        [command, path] if command == "check" => check_file(path, &mut read_file, stderr),
        [command, path, output] if command == "compile" => {
            compile_bytecode(path, output, &mut read_file, stderr)
        }
        [command, path] if command == "run-bytecode" => run_bytecode(path, stdout, stderr),
        [command, path] if command == "debug" => debug_file(path, stdin, stdout, stderr),
        [command, ..] if command == "run" => {
            writeln!(stderr, "error: expected a file or package path\n")
                .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "check" => {
            writeln!(stderr, "error: expected a file or package path\n")
                .expect("stderr write failed");
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
            writeln!(stderr, "error: expected a file or package path\n")
                .expect("stderr write failed");
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

fn check_file(
    path: &str,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    stderr: &mut impl io::Write,
) -> i32 {
    match compile_file(path, read_file, stderr) {
        Ok(_) => 0,
        Err(code) => code,
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
    let runtime = match runtime_gateway(stderr) {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    match runtime.run_bytecode(RunBytecodeRequest::new(path)) {
        Ok(result) => write_run_result(stdout, result),
        Err(error) => {
            write!(stderr, "{}", error.render()).expect("stderr write failed");
            error.exit_code
        }
    }
}

fn run_file(path: &str, stdout: &mut impl io::Write, stderr: &mut impl io::Write) -> i32 {
    // Normal source execution is delegated to ferrix-runtime so the CLI remains
    // a thin command surface instead of wiring compiler, stdlib, and VM itself.
    let runtime = match runtime_gateway(stderr) {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    match runtime.run_source(RunSourceRequest::new(path)) {
        Ok(result) => write_run_result(stdout, result),
        Err(error) => {
            write!(stderr, "{}", error.render()).expect("stderr write failed");
            error.exit_code
        }
    }
}

fn write_run_result(stdout: &mut impl io::Write, result: ferrix_runtime::RunResult) -> i32 {
    write!(stdout, "{}", result.output).expect("stdout write failed");
    if let Some(value) = result.value_display {
        writeln!(stdout, "{value}").expect("stdout write failed");
    }
    result.exit_code
}

fn debug_file(
    path: &str,
    stdin: &mut impl io::BufRead,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    // Debugger source preparation goes through ferrix-runtime so package and
    // import resolution stay aligned with normal execution.
    let runtime = match runtime_gateway(stderr) {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    let compiled = match runtime.prepare_debug(DebugRequest::new(path)) {
        Ok(compiled) => compiled,
        Err(error) => {
            write!(stderr, "{}", error.render()).expect("stderr write failed");
            return error.exit_code;
        }
    };
    let sources = compiled.sources;
    let program = compiled.program;

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

fn runtime_gateway(stderr: &mut impl io::Write) -> Result<RuntimeGateway, i32> {
    let mode = match env::var(RUNTIME_MODE_ENV) {
        Ok(value) if !value.trim().is_empty() => match value.parse::<RuntimeMode>() {
            Ok(mode) => mode,
            Err(error) => {
                writeln!(stderr, "error: {error}").expect("stderr write failed");
                return Err(64);
            }
        },
        _ => RuntimeMode::Embedded,
    };
    Ok(RuntimeGateway::new(mode))
}

fn compile_file(
    path: &str,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    stderr: &mut impl io::Write,
) -> Result<(SourceManager, VerifiedProgram), i32> {
    // Load the import graph into one source manager so parse/codegen/runtime
    // diagnostics can all render against the same file table.
    let input = match resolve_compile_input(Path::new(path)) {
        Ok(input) => input,
        Err(LoadError::Read { path, error }) => {
            writeln!(
                stderr,
                "error: could not read `{}`: {error}",
                path.display()
            )
            .expect("stderr write failed");
            return Err(66);
        }
        Err(LoadError::Manifest { path, message }) => {
            writeln!(
                stderr,
                "error: invalid package manifest `{}`: {message}",
                path.display()
            )
            .expect("stderr write failed");
            return Err(65);
        }
        Err(_) => {
            writeln!(stderr, "error: could not prepare source input").expect("stderr write failed");
            return Err(65);
        }
    };

    let mut sources = SourceManager::new();
    let graph = match load_module_graph(
        &input.entry_path,
        input.package.as_ref(),
        read_file,
        &mut sources,
    ) {
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
        Err(LoadError::ReadImport {
            importer,
            module,
            path,
            error,
        }) => {
            writeln!(
                stderr,
                "error: could not resolve import `{module}` from `{}` as `{}`: {error}",
                importer.display(),
                path.display()
            )
            .expect("stderr write failed");
            return Err(66);
        }
        Err(LoadError::PackageImport {
            importer,
            package,
            module,
            searched,
        }) => {
            let searched = searched
                .iter()
                .map(|path| format!("`{}`", path.display()))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(
                stderr,
                "error: could not resolve package import `{module}` from `{}` in package `{package}`; searched {searched}",
                importer.display()
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
        Err(LoadError::Manifest { path, message }) => {
            writeln!(
                stderr,
                "error: invalid package manifest `{}`: {message}",
                path.display()
            )
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

struct CompileInput {
    /// Source file compiled as the program entrypoint.
    entry_path: PathBuf,
    /// Package metadata used to resolve imports when the input is a package.
    package: Option<PackageContext>,
}

#[derive(Clone)]
struct PackageContext {
    /// Human-readable package name from the manifest.
    name: String,
    /// Absolute package root used as the boundary for local module lookup.
    root: PathBuf,
    /// Absolute module search roots tried in manifest order.
    module_roots: Vec<PathBuf>,
    /// Future external package metadata retained from the manifest.
    dependencies: Vec<PackageDependency>,
}

#[derive(Clone)]
struct PackageDependency {
    /// Dependency package name or locator.
    name: String,
    /// Optional future version requirement parsed from `name@requirement`.
    requirement: Option<String>,
}

struct PackageManifest {
    name: String,
    entry: PathBuf,
    module_roots: Option<Vec<PathBuf>>,
    dependencies: Vec<PackageDependency>,
}

impl PackageContext {
    fn dependency_metadata(&self) -> Vec<(&str, Option<&str>)> {
        self.dependencies
            .iter()
            .map(|dependency| (dependency.name.as_str(), dependency.requirement.as_deref()))
            .collect()
    }
}

fn resolve_compile_input(path: &Path) -> Result<CompileInput, LoadError> {
    if path.is_dir() {
        return load_package_input(path);
    }

    if is_manifest_path(path) {
        let root = path.parent().unwrap_or_else(|| Path::new("."));
        return load_package_input_from_manifest(root, path);
    }

    Ok(CompileInput {
        entry_path: path.to_path_buf(),
        package: None,
    })
}

fn load_package_input(root: &Path) -> Result<CompileInput, LoadError> {
    let manifest_path = MANIFEST_FILES
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| LoadError::Read {
            path: root.join(MANIFEST_FILES[0]),
            error: io::Error::new(io::ErrorKind::NotFound, "package manifest not found"),
        })?;
    load_package_input_from_manifest(root, &manifest_path)
}

fn load_package_input_from_manifest(
    root: &Path,
    manifest_path: &Path,
) -> Result<CompileInput, LoadError> {
    let source = fs::read_to_string(manifest_path).map_err(|error| LoadError::Read {
        path: manifest_path.to_path_buf(),
        error,
    })?;
    let manifest = parse_package_manifest(&source, manifest_path)?;
    let root = normalize_existing_path(root);
    let entry_path = root.join(&manifest.entry);
    let module_roots = manifest
        .module_roots
        .unwrap_or_else(|| default_module_roots(&manifest.entry))
        .into_iter()
        .map(|root_path| root.join(root_path))
        .collect::<Vec<_>>();
    let package = PackageContext {
        name: manifest.name,
        root,
        module_roots,
        dependencies: manifest.dependencies,
    };
    let _ = package.dependency_metadata();

    Ok(CompileInput {
        entry_path,
        package: Some(package),
    })
}

fn parse_package_manifest(source: &str, path: &Path) -> Result<PackageManifest, LoadError> {
    let mut name = None;
    let mut entry = None;
    let mut module_roots = None;
    let mut dependencies = Vec::new();

    for (line_index, raw_line) in source.lines().enumerate() {
        let line_number = line_index + 1;
        let line = strip_manifest_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        let Some((raw_key, raw_value)) = line.split_once('=') else {
            return Err(manifest_error(path, line_number, "expected `key = value`"));
        };
        let key = raw_key.trim();
        let value = raw_value.trim();
        match key {
            "name" => name = Some(parse_manifest_string(value, path, line_number)?),
            "entry" => {
                entry = Some(PathBuf::from(parse_manifest_string(
                    value,
                    path,
                    line_number,
                )?))
            }
            "module_roots" => {
                module_roots = Some(
                    parse_manifest_string_array(value, path, line_number)?
                        .into_iter()
                        .map(PathBuf::from)
                        .collect(),
                );
            }
            "dependencies" => {
                dependencies = parse_manifest_string_array(value, path, line_number)?
                    .into_iter()
                    .map(parse_dependency)
                    .collect();
            }
            _ => return Err(manifest_error(path, line_number, "unknown manifest key")),
        }
    }

    let name = name.ok_or_else(|| manifest_error(path, 0, "missing `name`"))?;
    let entry = entry.ok_or_else(|| manifest_error(path, 0, "missing `entry`"))?;

    Ok(PackageManifest {
        name,
        entry,
        module_roots,
        dependencies,
    })
}

fn strip_manifest_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..index],
            _ => {}
        }
    }
    line
}

fn parse_manifest_string(
    value: &str,
    path: &Path,
    line_number: usize,
) -> Result<String, LoadError> {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Err(manifest_error(path, line_number, "expected quoted string"));
    };
    Ok(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn parse_manifest_string_array(
    value: &str,
    path: &Path,
    line_number: usize,
) -> Result<Vec<String>, LoadError> {
    let Some(inner) = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return Err(manifest_error(path, line_number, "expected string array"));
    };
    let inner = inner.trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }

    inner
        .split(',')
        .map(|item| parse_manifest_string(item.trim(), path, line_number))
        .collect()
}

fn parse_dependency(value: String) -> PackageDependency {
    let Some((name, requirement)) = value.split_once('@') else {
        return PackageDependency {
            name: value,
            requirement: None,
        };
    };
    PackageDependency {
        name: name.to_string(),
        requirement: Some(requirement.to_string()),
    }
}

fn manifest_error(path: &Path, line_number: usize, message: &str) -> LoadError {
    let message = if line_number == 0 {
        message.to_string()
    } else {
        format!("line {line_number}: {message}")
    };
    LoadError::Manifest {
        path: path.to_path_buf(),
        message,
    }
}

fn default_module_roots(entry: &Path) -> Vec<PathBuf> {
    vec![
        entry
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
    ]
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
    /// Package manifest syntax or required fields are invalid.
    Manifest { path: PathBuf, message: String },
    /// An imported source file could not be resolved relative to its importer.
    ReadImport {
        importer: PathBuf,
        module: String,
        path: PathBuf,
        error: io::Error,
    },
    /// A package-local import was not found under the manifest module roots.
    PackageImport {
        importer: PathBuf,
        package: String,
        module: String,
        searched: Vec<PathBuf>,
    },
    /// Lexing/parsing one source file failed.
    Compile(CompileError),
    /// Recursive imports reached a file already on the active load stack.
    ImportCycle { path: PathBuf },
}

fn load_module_graph(
    entry_path: &Path,
    package: Option<&PackageContext>,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    sources: &mut SourceManager,
) -> Result<LoadedGraph, LoadError> {
    // `loaded` prevents duplicate work; `visiting` detects import cycles.
    let mut loaded = HashSet::new();
    let mut visiting = HashSet::new();
    let mut modules = Vec::new();
    let entry = load_module(
        entry_path,
        package,
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
    package: Option<&PackageContext>,
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
        let import_path = resolve_import(path, module, package)?;
        let loaded_module = load_module(
            &import_path,
            package,
            read_file,
            sources,
            loaded,
            visiting,
            modules,
        )
        .map_err(|error| match error {
            LoadError::Read {
                path: missing_path,
                error,
            } => LoadError::ReadImport {
                importer: path.to_path_buf(),
                module: module.to_string(),
                path: missing_path,
                error,
            },
            error => error,
        })?;
        if let Some(module_ast) = loaded_module {
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

fn resolve_import(
    importer: &Path,
    module: &str,
    package: Option<&PackageContext>,
) -> Result<PathBuf, LoadError> {
    let module_path = module_file_path(module);
    let Some(package) = package else {
        return Ok(importer
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(module_path));
    };

    let importer_key = module_key(importer);
    if !importer_key.starts_with(&package.root) {
        return Err(LoadError::PackageImport {
            importer: importer.to_path_buf(),
            package: package.name.clone(),
            module: module.to_string(),
            searched: Vec::new(),
        });
    }

    let searched = package
        .module_roots
        .iter()
        .map(|root| root.join(&module_path))
        .collect::<Vec<_>>();
    searched
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .ok_or_else(|| LoadError::PackageImport {
            importer: importer.to_path_buf(),
            package: package.name.clone(),
            module: module.to_string(),
            searched,
        })
}

fn module_file_path(module: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for segment in module.split('.') {
        path.push(segment);
    }
    path.set_extension("fx");
    path
}

fn module_key(path: &Path) -> PathBuf {
    normalize_existing_path(path)
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    fs::canonicalize(&absolute).unwrap_or(absolute)
}

fn is_manifest_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| MANIFEST_FILES.contains(&name))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Breakpoint {
    /// Specific function to stop in, or `None` for any function.
    Instruction {
        function: Option<FunctionId>,
        instruction_ip: usize,
    },
    /// Source line breakpoint, optionally scoped to a file name/path.
    SourceLine { file: Option<String>, line: usize },
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
    selected_frame: usize,
    watches: Vec<String>,
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
            selected_frame: 0,
            watches: Vec::new(),
        }
    }

    fn should_stop(&self, event: &DebugEvent<'_>) -> bool {
        // Function-scoped breakpoints win when present; global breakpoints are
        // useful for tiny programs where instruction ids are enough.
        matches!(self.mode, DebugMode::Step)
            || self.breakpoints.contains(&Breakpoint::Instruction {
                function: Some(event.function),
                instruction_ip: event.instruction_ip,
            })
            || self.breakpoints.contains(&Breakpoint::Instruction {
                function: None,
                instruction_ip: event.instruction_ip,
            })
            || self.matches_source_breakpoint(event)
    }

    fn matches_source_breakpoint(&self, event: &DebugEvent<'_>) -> bool {
        let Some((location, line_text_ip)) = self.source_line_hit(event) else {
            return false;
        };

        self.breakpoints.iter().any(|breakpoint| {
            let Breakpoint::SourceLine { file, line } = breakpoint else {
                return false;
            };
            *line == location.line
                && line_text_ip == event.instruction_ip
                && file
                    .as_deref()
                    .is_none_or(|file| source_file_matches(self.sources, location, file))
        })
    }

    fn source_line_hit(&self, event: &DebugEvent<'_>) -> Option<(SourceLocation, usize)> {
        let location = self.sources.location(event.source_span?)?;
        let function = event.program.function(event.function)?;
        let chunk = function.chunk()?;
        let first_ip = chunk.source_map.iter().enumerate().find_map(|(ip, span)| {
            let span = span.as_ref().copied()?;
            let span_location = self.sources.location(span)?;
            (span_location.file_id == location.file_id && span_location.line == location.line)
                .then_some(ip)
        })?;
        Some((location, first_ip))
    }

    fn print_stop(&mut self, event: &DebugEvent<'_>) {
        self.normalize_selected_frame(event);
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
        self.print_watches(event);
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
            if matches!(command, "bt" | "stack" | "frames") {
                self.print_stack(event);
                continue;
            }
            if matches!(command, "frame") {
                self.print_selected_frame(event);
                continue;
            }
            if let Some(spec) = command.strip_prefix("frame ") {
                self.select_frame(event, spec.trim());
                continue;
            }
            if matches!(command, "i" | "instruction") {
                self.print_stop(event);
                continue;
            }
            if matches!(command, "disasm" | "disassemble" | "u") {
                self.print_disassembly(event, 2);
                continue;
            }
            if let Some(spec) = command
                .strip_prefix("disasm ")
                .or_else(|| command.strip_prefix("disassemble "))
                .or_else(|| command.strip_prefix("u "))
            {
                let radius = spec.trim().parse().unwrap_or(2);
                self.print_disassembly(event, radius);
                continue;
            }
            if let Some(expr) = command.strip_prefix("watch ") {
                self.add_watch(event, expr.trim());
                continue;
            }
            if matches!(command, "watches" | "watch") {
                self.print_watches(event);
                continue;
            }
            if let Some(spec) = command
                .strip_prefix("unwatch ")
                .or_else(|| command.strip_prefix("uw "))
            {
                self.remove_watch(spec.trim());
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
        let Some(frame) = self.selected_frame_view(event) else {
            writeln!(self.output, "frame #{} is unavailable", self.selected_frame)
                .expect("stdout write failed");
            return;
        };

        writeln!(
            self.output,
            "registers for frame #{} {} ({}, ip={})",
            self.selected_frame, frame.name, frame.function, frame.ip
        )
        .expect("stdout write failed");

        if frame.registers.is_empty() {
            writeln!(self.output, "registers: <empty>").expect("stdout write failed");
            return;
        }

        for (index, value) in frame.registers.iter().copied().enumerate() {
            writeln!(
                self.output,
                "r{index} = {}",
                display_heap_value(event.heap, value)
            )
            .expect("stdout write failed");
        }
    }

    fn print_stack(&mut self, event: &DebugEvent<'_>) {
        self.normalize_selected_frame(event);
        for (index, frame) in event.frames.iter().rev().enumerate() {
            let view = self.frame_view(event, index, frame);
            let selected = if index == self.selected_frame {
                " *"
            } else {
                ""
            };
            writeln!(
                self.output,
                "#{index}{selected} at {} ({}, ip={})",
                view.name, view.function, view.ip
            )
            .expect("stdout write failed");
        }
    }

    fn print_selected_frame(&mut self, event: &DebugEvent<'_>) {
        self.normalize_selected_frame(event);
        let Some(frame) = self.selected_frame_view(event) else {
            writeln!(self.output, "frame #{} is unavailable", self.selected_frame)
                .expect("stdout write failed");
            return;
        };

        writeln!(
            self.output,
            "selected frame #{} at {} ({}, ip={})",
            self.selected_frame, frame.name, frame.function, frame.ip
        )
        .expect("stdout write failed");
    }

    fn select_frame(&mut self, event: &DebugEvent<'_>, spec: &str) {
        let Ok(index) = spec.parse::<usize>() else {
            writeln!(self.output, "invalid frame `{spec}`").expect("stdout write failed");
            return;
        };

        if index >= event.frames.len() {
            writeln!(
                self.output,
                "frame #{index} is unavailable; stack has {} frame(s)",
                event.frames.len()
            )
            .expect("stdout write failed");
            return;
        }

        self.selected_frame = index;
        self.print_selected_frame(event);
    }

    fn add_watch(&mut self, event: &DebugEvent<'_>, expr: &str) {
        if expr.is_empty() {
            writeln!(self.output, "watch expression is empty").expect("stdout write failed");
            return;
        }

        self.watches.push(expr.to_string());
        let index = self.watches.len() - 1;
        match self.evaluate_watch(event, expr) {
            Ok(value) => writeln!(
                self.output,
                "watch #{index}: {expr} = {}",
                display_heap_value(event.heap, value)
            ),
            Err(error) => writeln!(self.output, "watch #{index}: {expr} ({error})"),
        }
        .expect("stdout write failed");
    }

    fn remove_watch(&mut self, spec: &str) {
        let Ok(index) = spec.parse::<usize>() else {
            writeln!(self.output, "invalid watch `{spec}`").expect("stdout write failed");
            return;
        };

        if index >= self.watches.len() {
            writeln!(self.output, "watch #{index} is unavailable").expect("stdout write failed");
            return;
        }

        let expr = self.watches.remove(index);
        writeln!(self.output, "removed watch #{index}: {expr}").expect("stdout write failed");
    }

    fn print_watches(&mut self, event: &DebugEvent<'_>) {
        for (index, expr) in self.watches.iter().enumerate() {
            match self.evaluate_watch(event, expr) {
                Ok(value) => writeln!(
                    self.output,
                    "watch #{index}: {expr} = {}",
                    display_heap_value(event.heap, value)
                ),
                Err(error) => writeln!(self.output, "watch #{index}: {expr} ({error})"),
            }
            .expect("stdout write failed");
        }
    }

    fn evaluate_watch(&self, event: &DebugEvent<'_>, expr: &str) -> Result<Value, String> {
        let frame = self
            .selected_frame_view(event)
            .ok_or_else(|| format!("frame #{} is unavailable", self.selected_frame))?;

        if let Some(register) = parse_register(expr) {
            return frame
                .registers
                .get(register)
                .copied()
                .ok_or_else(|| format!("register r{register} is unavailable"));
        }

        let function = event
            .program
            .function(frame.function)
            .ok_or_else(|| format!("function {} is unavailable", frame.function))?;
        let chunk = function
            .chunk()
            .ok_or_else(|| format!("{} is native and has no locals", frame.function))?;
        let Some((register, _)) = chunk
            .debug_local_names
            .iter()
            .enumerate()
            .find(|(_, name)| name.as_deref() == Some(expr))
        else {
            return Err(format!("unknown watch expression `{expr}`"));
        };

        frame
            .registers
            .get(register)
            .copied()
            .ok_or_else(|| format!("local `{expr}` is unavailable"))
    }

    fn print_disassembly(&mut self, event: &DebugEvent<'_>, radius: usize) {
        let Some(frame) = self.selected_frame_view(event) else {
            writeln!(self.output, "frame #{} is unavailable", self.selected_frame)
                .expect("stdout write failed");
            return;
        };
        let Some(function) = event.program.function(frame.function) else {
            writeln!(self.output, "function {} is unavailable", frame.function)
                .expect("stdout write failed");
            return;
        };
        let Some(chunk) = function.chunk() else {
            writeln!(
                self.output,
                "{} is native and has no bytecode",
                frame.function
            )
            .expect("stdout write failed");
            return;
        };

        let start = frame.ip.saturating_sub(radius);
        let end = frame
            .ip
            .saturating_add(radius)
            .saturating_add(1)
            .min(chunk.instructions.len());
        writeln!(
            self.output,
            "disassembly for frame #{} {} ({}, ip={})",
            self.selected_frame, frame.name, frame.function, frame.ip
        )
        .expect("stdout write failed");
        for ip in start..end {
            let marker = if ip == frame.ip { "=>" } else { "  " };
            writeln!(
                self.output,
                "{marker} {ip:04} {}",
                format_instruction(&chunk.instructions[ip])
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
  break line <line>     stop at source line in any file
  break <file>:<line>   stop at source line in a file/path
  clear [breakpoint]    clear one breakpoint or all breakpoints
  registers | r         print selected-frame registers
  stack | bt | frames   print call stack
  frame [index]         select or print the inspected frame
  watch <expr>          watch register rN or a debug local name
  watches               print watch expressions
  unwatch <index>       remove a watch expression
  disasm [radius]       print bytecode around selected frame
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
        match breakpoint {
            Breakpoint::Instruction {
                function: Some(function),
                instruction_ip,
            } => writeln!(
                self.output,
                "{prefix} breakpoint at {function}:{instruction_ip}"
            ),
            Breakpoint::Instruction {
                function: None,
                instruction_ip,
            } => writeln!(self.output, "{prefix} breakpoint at ip={instruction_ip}"),
            Breakpoint::SourceLine {
                file: Some(file),
                line,
            } => writeln!(self.output, "{prefix} breakpoint at {file}:{line}"),
            Breakpoint::SourceLine { file: None, line } => {
                writeln!(self.output, "{prefix} breakpoint at line {line}")
            }
        }
        .expect("stdout write failed");
    }

    fn normalize_selected_frame(&mut self, event: &DebugEvent<'_>) {
        if self.selected_frame >= event.frames.len() {
            self.selected_frame = 0;
        }
    }

    fn selected_frame_view<'event>(
        &self,
        event: &'event DebugEvent<'_>,
    ) -> Option<FrameView<'event>> {
        let frame = event.frames.iter().rev().nth(self.selected_frame)?;
        Some(self.frame_view(event, self.selected_frame, frame))
    }

    fn frame_view<'event>(
        &self,
        event: &'event DebugEvent<'_>,
        index: usize,
        frame: &'event CallFrame,
    ) -> FrameView<'event> {
        let function = event.program.function(frame.function_id);
        let name = function
            .map(|function| function.name.as_str())
            .unwrap_or("<unknown>");
        let registers = if index == 0 {
            event.registers
        } else {
            &frame.registers
        };
        let ip = if index == 0 {
            event.instruction_ip
        } else {
            frame.ip
        };

        FrameView {
            name,
            function: frame.function_id,
            ip,
            registers,
        }
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
    if let Some(line) = spec.strip_prefix("line ") {
        return Some(Breakpoint::SourceLine {
            file: None,
            line: line.trim().parse().ok()?,
        });
    }

    if let Some((function, ip)) = spec.split_once(':') {
        let target = function.trim();
        let ip_or_line = ip.trim().parse().ok()?;
        if let Some(function) = parse_function(event, target) {
            return Some(Breakpoint::Instruction {
                function: Some(function),
                instruction_ip: ip_or_line,
            });
        }

        return Some(Breakpoint::SourceLine {
            file: Some(target.to_string()),
            line: ip_or_line,
        });
    }

    Some(Breakpoint::Instruction {
        function: None,
        instruction_ip: spec.parse().ok()?,
    })
}

struct FrameView<'a> {
    name: &'a str,
    function: FunctionId,
    ip: usize,
    registers: &'a [Value],
}

fn parse_register(expr: &str) -> Option<usize> {
    expr.strip_prefix('r')?.parse().ok()
}

fn source_file_matches(sources: &SourceManager, location: SourceLocation, spec: &str) -> bool {
    let Some(file) = sources.file(location.file_id) else {
        return false;
    };
    if file.name == spec || file.name.ends_with(spec) {
        return true;
    }

    Path::new(&file.name)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == spec)
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
            Ok(Obj::Record(fields)) => {
                let fields = fields
                    .iter()
                    .map(|(field, value)| {
                        format!(
                            "{}: {}",
                            field,
                            display_heap_value_inner(heap, *value, depth + 1)
                        )
                    })
                    .collect::<Vec<_>>();
                format!("{{{}}}", fields.join(", "))
            }
            _ => value.to_string(),
        },
        _ => value.to_string(),
    }
}
