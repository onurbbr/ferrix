//! Tests for object references and heap object identity behavior.

use ferrix_core::{ObjRef, Value};

#[test]
fn object_references_have_stable_identity() {
    assert_eq!(ObjRef::new(7, 1), ObjRef::new(7, 1));
    assert_ne!(ObjRef::new(7, 1), ObjRef::new(7, 2));
}

#[test]
fn value_exposes_object_reference_for_future_gc_roots() {
    let obj = ObjRef::new(3, 0);

    assert_eq!(Value::Obj(obj).as_obj_ref(), Some(obj));
    assert_eq!(Value::Int(1).as_obj_ref(), None);
}
