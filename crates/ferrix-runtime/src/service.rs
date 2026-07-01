//! Runtime service implementation.

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
    bytecode::{VerifiedProgram, decode_program},
    diagnostics::SourceManager,
};
use ferrix_vm::{Heap, Vm};

use crate::{
    RunBytecodeRequest, RunResult, RunSourceRequest, RuntimeError, RuntimeErrorKind, RuntimeStats,
    output::install_output,
};

const MANIFEST_FILES: &[&str] = &["Ferrix.toml", "ferrix.toml"];

/// High-level runtime gateway used by CLI and future embeddings.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeService;

/// Source manager and verified program produced by runtime compilation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledProgram {
    /// Sources used for diagnostic rendering.
    pub sources: SourceManager,
    /// Verified program ready for VM execution.
    pub program: VerifiedProgram,
}

impl RuntimeService {
    /// Creates a runtime service with default behavior.
    pub fn new() -> Self {
        Self
    }

    /// Compiles and runs a source file, package directory, or package manifest.
    pub fn run_source(&self, request: RunSourceRequest) -> Result<RunResult, RuntimeError> {
        let compiled = self.compile_source_path(&request.path)?;
        let mut vm = Vm::with_limits(request.profile.limits());
        let capture = install_output(&mut vm, request.output);
        ferrix_stdlib::install(&mut vm, compiled.program.as_program());

        match vm.run_program(&compiled.program) {
            Ok(value) => Ok(run_result(&vm, value, capture, request.collect_stats)),
            Err(error) => {
                let diagnostic = error.to_diagnostic_with_program(compiled.program.as_program());
                Err(RuntimeError::new(
                    70,
                    RuntimeErrorKind::Diagnostic(compiled.sources.render_diagnostic(&diagnostic)),
                ))
            }
        }
    }

    /// Loads and runs a serialized bytecode program.
    pub fn run_bytecode(&self, request: RunBytecodeRequest) -> Result<RunResult, RuntimeError> {
        let bytes = fs::read(&request.path).map_err(|error| {
            RuntimeError::new(
                66,
                RuntimeErrorKind::Read {
                    path: request.path.clone(),
                    message: error.to_string(),
                },
            )
        })?;
        let program = decode_program(&bytes).map_err(|error| {
            RuntimeError::new(65, RuntimeErrorKind::DecodeBytecode(error.to_string()))
        })?;

        let mut vm = Vm::with_limits(request.profile.limits());
        let capture = install_output(&mut vm, request.output);
        ferrix_stdlib::install(&mut vm, program.as_program());

        match vm.run_program(&program) {
            Ok(value) => Ok(run_result(&vm, value, capture, request.collect_stats)),
            Err(error) => Err(RuntimeError::new(
                70,
                RuntimeErrorKind::Execution(error.to_string()),
            )),
        }
    }

    /// Compiles source into a verified program without running it.
    pub fn compile_source_path(&self, path: &Path) -> Result<CompiledProgram, RuntimeError> {
        compile_source_path(path)
    }
}

fn run_result(
    vm: &Vm,
    value: Value,
    capture: Option<crate::output::CapturedOutput>,
    collect_stats: bool,
) -> RunResult {
    let value_display = (value != Value::Nil).then(|| display_value(vm, value));
    let output = capture.map_or_else(String::new, |capture| capture.contents());
    let stats = if collect_stats {
        runtime_stats(vm)
    } else {
        RuntimeStats::default()
    };

    RunResult {
        exit_code: 0,
        value,
        value_display,
        output,
        stats,
        audit_events: Vec::new(),
    }
}

fn runtime_stats(vm: &Vm) -> RuntimeStats {
    let gc = vm.gc_stats();
    RuntimeStats {
        executed_instructions: vm.executed_instruction_count(),
        call_depth: vm.call_depth(),
        heap_objects: vm.heap().len(),
        gc_collections: gc.collections,
        incremental_gc_steps: gc.incremental_steps,
    }
}

fn compile_source_path(path: &Path) -> Result<CompiledProgram, RuntimeError> {
    let input = resolve_compile_input(path).map_err(|error| runtime_load_error(error, None))?;

    let mut sources = SourceManager::new();
    let graph = load_module_graph(&input.entry_path, input.package.as_ref(), &mut sources)
        .map_err(|error| runtime_load_error(error, Some(&sources)))?;

    let program =
        compile_program_ast_with_named_modules(graph.entry, graph.modules).map_err(|error| {
            RuntimeError::new(
                65,
                RuntimeErrorKind::Diagnostic(sources.render_diagnostic(&error.to_diagnostic())),
            )
        })?;

    Ok(CompiledProgram { sources, program })
}

struct CompileInput {
    entry_path: PathBuf,
    package: Option<PackageContext>,
}

#[derive(Clone)]
struct PackageContext {
    name: String,
    root: PathBuf,
    module_roots: Vec<PathBuf>,
    dependencies: Vec<PackageDependency>,
}

#[derive(Clone)]
struct PackageDependency {
    name: String,
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
    entry: ProgramAst,
    modules: Vec<ImportedModuleAst>,
}

enum LoadError {
    Read {
        path: PathBuf,
        error: io::Error,
    },
    Manifest {
        path: PathBuf,
        message: String,
    },
    ReadImport {
        importer: PathBuf,
        module: String,
        path: PathBuf,
        error: io::Error,
    },
    PackageImport {
        importer: PathBuf,
        package: String,
        module: String,
        searched: Vec<PathBuf>,
    },
    Compile(CompileError),
    ImportCycle {
        path: PathBuf,
    },
}

fn load_module_graph(
    entry_path: &Path,
    package: Option<&PackageContext>,
    sources: &mut SourceManager,
) -> Result<LoadedGraph, LoadError> {
    let mut loaded = HashSet::new();
    let mut visiting = HashSet::new();
    let mut modules = Vec::new();
    let entry = load_module(
        entry_path,
        package,
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
    let source = fs::read_to_string(path).map_err(|error| LoadError::Read {
        path: path.to_path_buf(),
        error,
    })?;
    let file_id = sources.add_file(path_name, source.clone());
    let ast = parse_source_with_file_id(&source, file_id).map_err(LoadError::Compile)?;

    for module in module_imports(&ast) {
        let import_path = resolve_import(path, module, package)?;
        let loaded_module = load_module(&import_path, package, sources, loaded, visiting, modules)
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

fn runtime_load_error(error: LoadError, sources: Option<&SourceManager>) -> RuntimeError {
    match error {
        LoadError::Read { path, error } => RuntimeError::new(
            66,
            RuntimeErrorKind::Read {
                path,
                message: error.to_string(),
            },
        ),
        LoadError::Manifest { path, message } => {
            RuntimeError::new(65, RuntimeErrorKind::Manifest { path, message })
        }
        LoadError::ReadImport {
            importer,
            module,
            path,
            error,
        } => RuntimeError::new(
            66,
            RuntimeErrorKind::ReadImport {
                importer,
                module,
                path,
                message: error.to_string(),
            },
        ),
        LoadError::PackageImport {
            importer,
            package,
            module,
            searched,
        } => RuntimeError::new(
            66,
            RuntimeErrorKind::PackageImport {
                importer,
                package,
                module,
                searched,
            },
        ),
        LoadError::Compile(error) => {
            let diagnostic = error.to_diagnostic();
            let rendered = sources.map_or_else(
                || diagnostic.message.clone(),
                |sources| sources.render_diagnostic(&diagnostic),
            );
            RuntimeError::new(65, RuntimeErrorKind::Diagnostic(rendered))
        }
        LoadError::ImportCycle { path } => {
            RuntimeError::new(65, RuntimeErrorKind::ImportCycle { path })
        }
    }
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
