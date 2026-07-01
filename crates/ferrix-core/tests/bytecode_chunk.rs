//! Tests for bytecode chunk builders such as constant and string pools.

use ferrix_core::{Value, bytecode::Chunk};

#[test]
fn add_constant_returns_typed_constant_id() {
    let mut chunk = Chunk::new("main", 1);

    let id = chunk.add_constant(Value::Int(42)).unwrap();

    assert_eq!(id.0, 0);
    assert_eq!(chunk.constants, vec![Value::Int(42)]);
}

#[test]
fn add_string_returns_typed_string_id() {
    let mut chunk = Chunk::new("main", 1);

    let id = chunk.add_string("hello").unwrap();

    assert_eq!(id.0, 0);
    assert_eq!(chunk.strings, vec!["hello".to_string()]);
}
