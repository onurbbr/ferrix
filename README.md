# Ferrix

Ferrix is a small Rust implementation of a bytecode VM, assembler, source compiler, diagnostics renderer, and native standard-library layer.

## Current Scope

Implemented:

- Register-based bytecode VM.
- Structural verifier and disassembler.
- Assembler builder with labels.
- Function table, call frames, recursion, and native functions.
- Source compiler for `let`, assignment, `return`, arithmetic, comparisons, `if/else`, `while`, blocks, source-level functions/calls, static function imports, booleans, integers, strings, arrays, maps, indexing, and `nil`.
- Mark/sweep heap with generational object references and explicit VM root tracing.
- Native stdlib MVP: `print`, `len`, `type_of`, callable from source when referenced.
- Source diagnostics with caret rendering and runtime stack traces.
- CLI runner, bytecode compiler/runner, and instruction-level debugger for source files and same-directory static imports.

## CLI

Run a Ferrix source file:

```sh
cargo run -p ferrix-cli -- run path/to/file.fx
```

Example source:

```text
let x = 40;
fn add(a, b) {
    return a + b;
}
return add(x, 2);
```

Static imports load `<module>.fx` from the importing file's directory and make top-level functions available to the importing program:

```text
import math;
return math.add(40, 2);
```

The CLI prints non-`nil` return values to stdout and diagnostics to stderr.

Compile source to Ferrix bytecode and run it later:

```sh
cargo run -p ferrix-cli -- compile path/to/file.fx path/to/file.fxb
cargo run -p ferrix-cli -- run-bytecode path/to/file.fxb
```

Run a source file under the instruction-level debugger:

```sh
cargo run -p ferrix-cli -- debug path/to/file.fx
```

Debugger commands:

```text
step | s              execute one instruction
continue | c          run until program end or breakpoint
break <ip>            stop at instruction ip in any function
break <fn>:<ip>       stop at instruction ip in function id or name
clear [breakpoint]    clear one breakpoint or all breakpoints
registers | r         print current registers
stack | bt            print call stack
instruction | i       print current instruction
quit | q              stop debugging
```

Other commands:

```sh
cargo run -p ferrix-cli -- --help
cargo run -p ferrix-cli -- --version
cargo run -p ferrix-benchmarks --release
```

Fuzz targets are available through `cargo-fuzz`:

```sh
cargo fuzz run compile_source
cargo fuzz run decode_bytecode
```

## Validation

Release-quality checks used for the current milestone:

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p ferrix-benchmarks --release
```
