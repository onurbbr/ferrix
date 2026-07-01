//! Tests for heap allocation, root collection, and mark/sweep GC behavior.

use ferrix_core::{Obj, ObjRef, Value, bytecode::Chunk};
use ferrix_vm::{Heap, RootSet, RuntimeLimits, VmErrorKind};

#[test]
fn arena_allocates_stable_object_references() {
    let mut heap = Heap::new();

    let first = heap
        .allocate(Obj::String("hello".to_string()), RuntimeLimits::default())
        .unwrap();
    let second = heap
        .allocate(Obj::Array(vec![Value::Int(1)]), RuntimeLimits::default())
        .unwrap();

    assert_eq!(first, ObjRef::new(0, 0));
    assert_eq!(second, ObjRef::new(1, 0));
    assert_eq!(heap.get(first).unwrap(), &Obj::String("hello".to_string()));
}

#[test]
fn heap_object_limit_is_typed_error() {
    let mut heap = Heap::new();
    let limits = RuntimeLimits {
        max_instruction_count: 1,
        max_call_depth: 1,
        max_heap_objects: 1,
        gc_allocation_threshold: 0,
    };
    heap.allocate(Obj::String("one".to_string()), limits)
        .unwrap();

    let err = heap
        .allocate(Obj::String("two".to_string()), limits)
        .unwrap_err();

    assert_eq!(
        err.kind,
        VmErrorKind::HeapObjectLimitExceeded {
            max_heap_objects: 1,
        }
    );
}

#[test]
fn invalid_object_reference_is_typed_error() {
    let heap = Heap::new();

    let err = heap.get(ObjRef::new(99, 0)).unwrap_err();

    assert_eq!(
        err.kind,
        VmErrorKind::InvalidObjectRef {
            reference: ObjRef::new(99, 0),
        }
    );
}

#[test]
fn mark_sweep_collects_unreachable_objects() {
    let mut heap = Heap::new();
    let kept = heap
        .allocate(Obj::String("kept".to_string()), RuntimeLimits::default())
        .unwrap();
    let dropped = heap
        .allocate(Obj::String("dropped".to_string()), RuntimeLimits::default())
        .unwrap();

    let stats = heap.collect_garbage(&[kept]);

    assert_eq!(stats.marked, 1);
    assert_eq!(stats.swept, 1);
    assert_eq!(stats.live, 1);
    assert_eq!(heap.get(kept).unwrap(), &Obj::String("kept".to_string()));
    assert_eq!(
        heap.get(dropped).unwrap_err().kind,
        VmErrorKind::InvalidObjectRef { reference: dropped }
    );
}

#[test]
fn mark_sweep_traces_nested_array_and_map_values() {
    let mut heap = Heap::new();
    let leaf = heap
        .allocate(Obj::String("leaf".to_string()), RuntimeLimits::default())
        .unwrap();
    let key = heap
        .allocate(Obj::String("key".to_string()), RuntimeLimits::default())
        .unwrap();
    let map = heap
        .allocate(
            Obj::Map(vec![(Value::Obj(key), Value::Obj(leaf))]),
            RuntimeLimits::default(),
        )
        .unwrap();
    let root = heap
        .allocate(Obj::Array(vec![Value::Obj(map)]), RuntimeLimits::default())
        .unwrap();
    let dropped = heap
        .allocate(Obj::String("drop".to_string()), RuntimeLimits::default())
        .unwrap();

    let stats = heap.collect_garbage(&[root]);

    assert_eq!(stats.marked, 4);
    assert_eq!(stats.swept, 1);
    assert_eq!(stats.live, 4);
    assert!(heap.get(leaf).is_ok());
    assert!(heap.get(key).is_ok());
    assert!(heap.get(map).is_ok());
    assert!(heap.get(root).is_ok());
    assert!(heap.get(dropped).is_err());
}

#[test]
fn swept_slots_are_reused_with_new_generation() {
    let mut heap = Heap::new();
    let old = heap
        .allocate(Obj::String("old".to_string()), RuntimeLimits::default())
        .unwrap();

    heap.collect_garbage(&[]);

    let new = heap
        .allocate(Obj::String("new".to_string()), RuntimeLimits::default())
        .unwrap();

    assert_eq!(new.index, old.index);
    assert_ne!(new.generation, old.generation);
    assert!(heap.get(old).is_err());
    assert_eq!(heap.get(new).unwrap(), &Obj::String("new".to_string()));
}

#[test]
fn root_set_collects_unique_object_references() {
    let object = ObjRef::new(4, 0);
    let mut chunk = Chunk::new("main", 1);
    chunk.add_constant(Value::Obj(object)).unwrap();
    chunk.add_constant(Value::Obj(object)).unwrap();

    let mut roots = RootSet::new();
    roots.insert_chunk_constants(&chunk);
    roots.insert_value(Value::Obj(ObjRef::new(5, 0)));

    assert_eq!(roots.as_slice(), &[object, ObjRef::new(5, 0)]);
}
