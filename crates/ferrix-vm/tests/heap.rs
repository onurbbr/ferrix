//! Tests for heap allocation, root collection, and mark/sweep GC behavior.

use ferrix_core::{Obj, ObjRef, Value, bytecode::Chunk};
use ferrix_vm::{Heap, IncrementalGcPhase, RootSet, RuntimeLimits, VmErrorKind};

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
        gc_incremental_step_budget: 64,
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
fn incremental_collection_finishes_over_multiple_steps() {
    let mut heap = Heap::new();
    let leaf = heap
        .allocate(Obj::String("leaf".to_string()), RuntimeLimits::default())
        .unwrap();
    let root = heap
        .allocate(Obj::Array(vec![Value::Obj(leaf)]), RuntimeLimits::default())
        .unwrap();
    let dropped = heap
        .allocate(Obj::String("dropped".to_string()), RuntimeLimits::default())
        .unwrap();

    assert!(heap.start_incremental_collection(&[root]));
    assert_eq!(heap.incremental_phase(), IncrementalGcPhase::Marking);
    assert_eq!(heap.step_incremental_collection(1), None);
    assert!(heap.is_incremental_collection_active());

    let stats = loop {
        if let Some(stats) = heap.step_incremental_collection(1) {
            break stats;
        }
    };

    assert_eq!(stats.marked, 2);
    assert_eq!(stats.swept, 1);
    assert_eq!(stats.live, 2);
    assert_eq!(heap.incremental_phase(), IncrementalGcPhase::Idle);
    assert!(heap.get(root).is_ok());
    assert!(heap.get(leaf).is_ok());
    assert!(heap.get(dropped).is_err());
}

#[test]
fn incremental_write_barrier_preserves_late_heap_mutation() {
    let mut heap = Heap::new();
    let holder = heap
        .allocate(Obj::Array(Vec::new()), RuntimeLimits::default())
        .unwrap();
    let late = heap
        .allocate(Obj::String("late".to_string()), RuntimeLimits::default())
        .unwrap();

    assert!(heap.start_incremental_collection(&[holder]));
    assert_eq!(heap.step_incremental_collection(1), None);
    heap.write_barrier_value(Value::Obj(late));
    let Obj::Array(values) = heap.get_mut(holder).unwrap() else {
        panic!("expected array holder");
    };
    values.push(Value::Obj(late));

    let stats = heap.finish_incremental_collection().unwrap();

    assert_eq!(stats.marked, 2);
    assert_eq!(stats.swept, 0);
    assert_eq!(stats.live, 2);
    assert!(heap.get(holder).is_ok());
    assert!(heap.get(late).is_ok());
}

#[test]
fn incremental_allocation_barrier_preserves_new_objects() {
    let mut heap = Heap::new();
    let root = heap
        .allocate(Obj::Array(Vec::new()), RuntimeLimits::default())
        .unwrap();
    let dropped = heap
        .allocate(Obj::String("dropped".to_string()), RuntimeLimits::default())
        .unwrap();

    assert!(heap.start_incremental_collection(&[root]));
    assert_eq!(heap.step_incremental_collection(1), None);
    let allocated = heap
        .allocate(
            Obj::String("allocated".to_string()),
            RuntimeLimits::default(),
        )
        .unwrap();
    heap.write_barrier_value(Value::Obj(allocated));
    let Obj::Array(values) = heap.get_mut(root).unwrap() else {
        panic!("expected array root");
    };
    values.push(Value::Obj(allocated));

    let stats = heap.finish_incremental_collection().unwrap();

    assert_eq!(stats.swept, 1);
    assert_eq!(stats.live, 2);
    assert!(heap.get(root).is_ok());
    assert!(heap.get(allocated).is_ok());
    assert!(heap.get(dropped).is_err());
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
fn mark_sweep_traces_record_fields() {
    let mut heap = Heap::new();
    let leaf = heap
        .allocate(Obj::String("leaf".to_string()), RuntimeLimits::default())
        .unwrap();
    let root = heap
        .allocate(
            Obj::Record(vec![("leaf".to_string(), Value::Obj(leaf))]),
            RuntimeLimits::default(),
        )
        .unwrap();
    let dropped = heap
        .allocate(Obj::String("drop".to_string()), RuntimeLimits::default())
        .unwrap();

    let stats = heap.collect_garbage(&[root]);

    assert_eq!(stats.marked, 2);
    assert_eq!(stats.swept, 1);
    assert_eq!(stats.live, 2);
    assert!(heap.get(leaf).is_ok());
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
