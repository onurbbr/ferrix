//! Tests for Ferrix value display, debug formatting, and equality semantics.

use ferrix_core::{ObjRef, Value};

#[test]
fn displays_user_facing_values() {
    assert_eq!(Value::Int(42).to_string(), "42");
    assert_eq!(Value::Float(3.5).to_string(), "3.5");
    assert_eq!(Value::Bool(true).to_string(), "true");
    assert_eq!(Value::Nil.to_string(), "nil");
    assert_eq!(Value::Obj(ObjRef::new(2, 0)).to_string(), "<object #2:0>");
}

#[test]
fn displays_developer_debug_values() {
    assert_eq!(Value::Int(42).debug_display().to_string(), "Int(42)");
    assert_eq!(
        Value::Bool(false).debug_display().to_string(),
        "Bool(false)"
    );
    assert_eq!(
        Value::Obj(ObjRef::new(2, 0)).debug_display().to_string(),
        "Obj(#2:0)"
    );
}

#[test]
fn primitive_equality_is_content_based() {
    assert_eq!(Value::Int(1), Value::Int(1));
    assert_eq!(Value::Float(1.0), Value::Float(1.0));
    assert_ne!(Value::Int(1), Value::Float(1.0));
}

#[test]
fn object_equality_is_reference_based() {
    assert_eq!(Value::Obj(ObjRef::new(1, 0)), Value::Obj(ObjRef::new(1, 0)));
    assert_ne!(Value::Obj(ObjRef::new(1, 0)), Value::Obj(ObjRef::new(2, 0)));
}
