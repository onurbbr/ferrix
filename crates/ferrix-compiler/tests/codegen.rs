//! End-to-end compiler codegen tests that execute emitted programs on the VM.

use ferrix_compiler::{
    CompileErrorKind, ImportedModuleAst, compile_program_ast_with_modules,
    compile_program_ast_with_named_modules, compile_source, parse_source_with_file_id,
};
use ferrix_core::{
    Obj, Value,
    bytecode::{FunctionKind, Instruction},
    diagnostics::FileId,
};
use ferrix_vm::Vm;

#[test]
fn compiles_and_runs_let_return_arithmetic() {
    let program = compile_source(
        "\
let x = 10;
let y = 32;
return x + y;
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn respects_expression_precedence() {
    let program = compile_source("return 1 + 2 * 3;").unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(7));
}

#[test]
fn compiles_comparisons() {
    let program = compile_source("return (1 + 1) == 2;").unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Bool(true));
}

#[test]
fn compiles_string_literal_to_heap_string() {
    let program = compile_source("return \"ferrix\";").unwrap();
    let mut vm = Vm::new();

    let result = vm.run_program(&program).unwrap();
    let reference = result.as_obj_ref().unwrap();

    assert_eq!(
        vm.heap_object(reference).unwrap(),
        &ferrix_core::Obj::String("ferrix".to_string())
    );
}

#[test]
fn compiles_string_escape_sequences() {
    let program = compile_source("return \"line\\nquote\\\"\";").unwrap();
    let mut vm = Vm::new();

    let result = vm.run_program(&program).unwrap();
    let reference = result.as_obj_ref().unwrap();

    assert_eq!(
        vm.heap_object(reference).unwrap(),
        &ferrix_core::Obj::String("line\nquote\"".to_string())
    );
}

#[test]
fn compiles_array_literal_and_index_read() {
    let program = compile_source("let values = [10, 32, 7]; return values[1];").unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(32));
}

#[test]
fn compiles_array_index_assignment() {
    let program =
        compile_source("let values = [1, 2]; values[0] = 40 + 2; return values[0];").unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn compiles_empty_array_literal_to_heap_array() {
    let program = compile_source("return [];").unwrap();
    let mut vm = Vm::new();

    let result = vm.run_program(&program).unwrap();
    let reference = result.as_obj_ref().unwrap();

    assert_eq!(vm.heap_object(reference).unwrap(), &Obj::Array(Vec::new()));
}

#[test]
fn compiles_source_calls_to_stdlib_natives() {
    let program = compile_source("return len([1, 2, 3]);").unwrap();
    let mut vm = Vm::new();
    ferrix_stdlib::install(&mut vm, program.as_program());

    let result = vm.run_program(&program).unwrap();

    assert_eq!(result, Value::Int(3));
}

#[test]
fn compiles_map_literal_and_string_key_lookup() {
    let program =
        compile_source("let user = { \"name\": \"Onur\", \"age\": 30 }; return user[\"name\"];")
            .unwrap();
    let mut vm = Vm::new();

    let result = vm.run_program(&program).unwrap();
    let reference = result.as_obj_ref().unwrap();

    assert_eq!(
        vm.heap_object(reference).unwrap(),
        &Obj::String("Onur".to_string())
    );
}

#[test]
fn compiles_map_index_assignment() {
    let program = compile_source(
        "\
let user = { \"name\": \"Onur\" };
user[\"age\"] = 30;
return user[\"age\"];
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(30));
}

#[test]
fn compiles_empty_map_literal_to_heap_map() {
    let program = compile_source("return {};").unwrap();
    let mut vm = Vm::new();

    let result = vm.run_program(&program).unwrap();
    let reference = result.as_obj_ref().unwrap();

    assert_eq!(vm.heap_object(reference).unwrap(), &Obj::Map(Vec::new()));
}

#[test]
fn compiles_function_declaration_and_call() {
    let program = compile_source(
        "\
fn add(a, b) {
    return a + b;
}

return add(10, 32);
",
    )
    .unwrap();

    assert_eq!(program.as_program().entry.0, 1);
    assert_eq!(program.as_program().functions.len(), 2);

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn compiles_function_literal_without_captures() {
    let program = compile_source(
        "\
let id = fn(value) {
    return value;
};

return id(42);
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn compiles_closure_that_captures_outer_local() {
    let program = compile_source(
        "\
let x = 40;
let add = fn(y) {
    return x + y;
};

return add(2);
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn compiles_escaping_nested_closure() {
    let program = compile_source(
        "\
let x = 40;
let make_add = fn() {
    return fn(y) {
        return x + y;
    };
};

let add = make_add();
return add(2);
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn compiles_closure_that_mutates_captured_local() {
    let program = compile_source(
        "\
let x = 0;
let inc = fn() {
    x = x + 1;
    return x;
};

inc();
return inc();
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(2));
}

#[test]
fn compiles_shared_capture_between_closures() {
    let program = compile_source(
        "\
let x = 0;
let inc = fn() {
    x = x + 1;
    return x;
};
let get = fn() {
    return x;
};

inc();
inc();
return get();
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(2));
}

#[test]
fn compiles_closure_that_observes_later_assignment() {
    let program = compile_source(
        "\
let x = 1;
let get = fn() {
    return x;
};

x = 42;
return get();
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn compiles_recursive_function_literal() {
    let program = compile_source(
        "\
let fact = fn(n) {
    if (n == 0) {
        return 1;
    } else {
        return n * fact(n - 1);
    }
};

return fact(5);
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(120));
}

#[test]
fn compiles_recursive_function_call() {
    let program = compile_source(
        "\
fn fact(n) {
    if (n == 0) {
        return 1;
    } else {
        return n * fact(n - 1);
    }
}

return fact(5);
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(120));
}

#[test]
fn compiles_static_function_import_modules() {
    let module = parse_source_with_file_id(
        "\
fn add(a, b) {
    return a + b;
}
",
        FileId(1),
    )
    .unwrap();
    let entry = parse_source_with_file_id(
        "\
import math;
return add(40, 2);
",
        FileId(0),
    )
    .unwrap();

    let program = compile_program_ast_with_modules(entry, vec![module]).unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn imported_module_top_level_code_is_not_entry_code() {
    let module = parse_source_with_file_id(
        "\
fn answer() {
    return 42;
}
return 99;
",
        FileId(1),
    )
    .unwrap();
    let entry =
        parse_source_with_file_id("import answers;\nreturn answer();\n", FileId(0)).unwrap();

    let program = compile_program_ast_with_modules(entry, vec![module]).unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn compiles_namespaced_static_function_import_modules() {
    let module = parse_source_with_file_id(
        "\
fn add(a, b) {
    return a + b;
}
",
        FileId(1),
    )
    .unwrap();
    let entry = parse_source_with_file_id(
        "\
import math;
return math.add(40, 2);
",
        FileId(0),
    )
    .unwrap();

    let program = compile_program_ast_with_named_modules(
        entry,
        vec![ImportedModuleAst {
            name: "math".to_string(),
            ast: module,
        }],
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn compiles_if_else_assignment() {
    let program = compile_source(
        "\
let x = 0;
if (false) {
    x = 10;
} else {
    x = 20;
}
return x;
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(20));
}

#[test]
fn compiles_final_if_else_when_then_branch_returns() {
    let program = compile_source(
        "\
if (true) {
    return 1;
} else {
    return 2;
}
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(1));
}

#[test]
fn compiles_while_loop() {
    let program = compile_source(
        "\
let i = 0;
let sum = 0;
let one = 1;
let limit = 5;
while (i < limit) {
    sum = sum + i;
    i = i + one;
}
return sum;
",
    )
    .unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(10));
}

#[test]
fn assignment_to_undefined_variable_is_compile_error() {
    let err = compile_source("missing = 1; return missing;").unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::UndefinedVariable {
            name: "missing".to_string(),
        }
    );
}

#[test]
fn generated_program_is_bytecode_function() {
    let program = compile_source("let x = 1; return x;").unwrap();
    let function = &program.as_program().functions[0];

    let FunctionKind::Bytecode(chunk) = &function.kind else {
        panic!("compiler should emit bytecode");
    };

    assert!(matches!(
        chunk.instructions.last(),
        Some(Instruction::Return { .. })
    ));
    assert!(chunk.register_count > 0);
}

#[test]
fn undefined_variable_is_compile_error() {
    let err = compile_source("return missing;").unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::UndefinedVariable {
            name: "missing".to_string(),
        }
    );
}

#[test]
fn duplicate_variable_is_compile_error() {
    let err = compile_source("let x = 1; let x = 2; return x;").unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::DuplicateVariable {
            name: "x".to_string(),
        }
    );
}
