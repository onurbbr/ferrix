//! Generational-reference heap with explicit mark/sweep collection.
//!
//! The VM stores heap values as [`Obj`] slots and gives bytecode stable
//! [`ObjRef`] handles. During collection, callers provide roots from registers,
//! call frames, and program constants; unmarked objects are swept and their
//! generation is bumped to invalidate stale references.

use ferrix_core::{
    Obj, ObjRef, Value,
    bytecode::{Chunk, FunctionKind, Program},
};

use crate::{RuntimeLimits, VmError, VmErrorKind};

/// Heap storage for Ferrix objects.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Heap {
    objects: Vec<HeapSlot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HeapSlot {
    generation: u32,
    marked: bool,
    object: Option<Obj>,
}

/// Deduplicated collection of object references that must survive GC.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootSet {
    roots: Vec<ObjRef>,
}

/// Statistics returned by a mark/sweep collection pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GcStats {
    /// Number of reachable objects marked during traversal.
    pub marked: usize,
    /// Number of unreachable objects removed from heap slots.
    pub swept: usize,
    /// Number of live objects remaining after collection.
    pub live: usize,
}

impl Heap {
    /// Creates an empty heap.
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
    }

    /// Allocates an object and returns a generation-checked reference.
    pub fn allocate(&mut self, object: Obj, limits: RuntimeLimits) -> Result<ObjRef, VmError> {
        if self.len() >= limits.max_heap_objects {
            return Err(VmError::new(
                None,
                VmErrorKind::HeapObjectLimitExceeded {
                    max_heap_objects: limits.max_heap_objects,
                },
            ));
        }

        let index = u32::try_from(self.objects.len()).map_err(|_| {
            VmError::new(
                None,
                VmErrorKind::HeapObjectLimitExceeded {
                    max_heap_objects: limits.max_heap_objects,
                },
            )
        })?;

        if let Some((index, slot)) = self
            .objects
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.object.is_none())
        {
            let index = u32::try_from(index).map_err(|_| {
                VmError::new(
                    None,
                    VmErrorKind::HeapObjectLimitExceeded {
                        max_heap_objects: limits.max_heap_objects,
                    },
                )
            })?;
            slot.marked = false;
            slot.object = Some(object);
            return Ok(ObjRef::new(index, slot.generation));
        }

        let reference = ObjRef::new(index, 0);
        self.objects.push(HeapSlot {
            generation: reference.generation,
            marked: false,
            object: Some(object),
        });
        Ok(reference)
    }

    /// Reads an object, rejecting stale or out-of-range references.
    pub fn get(&self, reference: ObjRef) -> Result<&Obj, VmError> {
        let slot = self
            .objects
            .get(usize::try_from(reference.index).unwrap_or(usize::MAX))
            .ok_or_else(|| invalid_object_ref(reference))?;

        valid_object(slot, reference).ok_or_else(|| invalid_object_ref(reference))
    }

    /// Mutably reads an object, rejecting stale or out-of-range references.
    pub fn get_mut(&mut self, reference: ObjRef) -> Result<&mut Obj, VmError> {
        let slot = self
            .objects
            .get_mut(usize::try_from(reference.index).unwrap_or(usize::MAX))
            .ok_or_else(|| invalid_object_ref(reference))?;

        valid_object_mut(slot, reference).ok_or_else(|| invalid_object_ref(reference))
    }

    /// Runs mark/sweep collection from the supplied roots.
    pub fn collect_garbage(&mut self, roots: &[ObjRef]) -> GcStats {
        let mut marked = 0;
        let mut stack = roots.to_vec();

        while let Some(reference) = stack.pop() {
            let Some(slot) = self.slot_mut(reference) else {
                continue;
            };
            if slot.marked {
                continue;
            }
            slot.marked = true;
            marked += 1;

            let children = slot
                .object
                .as_ref()
                .map(object_references)
                .unwrap_or_default();
            stack.extend(children);
        }

        let mut swept = 0;
        let mut live = 0;
        for slot in &mut self.objects {
            if slot.object.is_none() {
                continue;
            }

            if slot.marked {
                slot.marked = false;
                live += 1;
            } else {
                slot.object = None;
                slot.marked = false;
                slot.generation = slot.generation.wrapping_add(1);
                swept += 1;
            }
        }

        GcStats {
            marked,
            swept,
            live,
        }
    }

    /// Returns the number of occupied heap slots.
    pub fn len(&self) -> usize {
        self.objects
            .iter()
            .filter(|slot| slot.object.is_some())
            .count()
    }

    /// Returns true when there are no live objects.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn slot_mut(&mut self, reference: ObjRef) -> Option<&mut HeapSlot> {
        let slot = self
            .objects
            .get_mut(usize::try_from(reference.index).unwrap_or(usize::MAX))?;
        if slot.generation == reference.generation && slot.object.is_some() {
            Some(slot)
        } else {
            None
        }
    }
}

impl RootSet {
    /// Creates an empty root set.
    pub fn new() -> Self {
        Self { roots: Vec::new() }
    }

    /// Adds the object reference contained in a value, if any.
    pub fn insert_value(&mut self, value: Value) {
        if let Some(reference) = value.as_obj_ref() {
            self.insert(reference);
        }
    }

    /// Adds object references contained in a stream of values.
    pub fn insert_values(&mut self, values: impl IntoIterator<Item = Value>) {
        for value in values {
            self.insert_value(value);
        }
    }

    /// Adds object references stored in one bytecode chunk's constants.
    pub fn insert_chunk_constants(&mut self, chunk: &Chunk) {
        self.insert_values(chunk.constants.iter().copied());
    }

    /// Adds object references stored in all bytecode chunks in a program.
    pub fn insert_program_constants(&mut self, program: &Program) {
        for function in &program.functions {
            if let FunctionKind::Bytecode(chunk) = &function.kind {
                self.insert_chunk_constants(chunk);
            }
        }
    }

    /// Returns roots as a borrowed slice for collection.
    pub fn as_slice(&self) -> &[ObjRef] {
        &self.roots
    }

    /// Consumes the root set and returns its deduplicated references.
    pub fn into_vec(self) -> Vec<ObjRef> {
        self.roots
    }

    fn insert(&mut self, reference: ObjRef) {
        if !self.roots.contains(&reference) {
            self.roots.push(reference);
        }
    }
}

fn invalid_object_ref(reference: ObjRef) -> VmError {
    VmError::new(None, VmErrorKind::InvalidObjectRef { reference })
}

fn valid_object(slot: &HeapSlot, reference: ObjRef) -> Option<&Obj> {
    if slot.generation == reference.generation {
        slot.object.as_ref()
    } else {
        None
    }
}

fn valid_object_mut(slot: &mut HeapSlot, reference: ObjRef) -> Option<&mut Obj> {
    if slot.generation == reference.generation {
        slot.object.as_mut()
    } else {
        None
    }
}

fn object_references(object: &Obj) -> Vec<ObjRef> {
    match object {
        Obj::Array(values) => values.iter().filter_map(Value::as_obj_ref).collect(),
        Obj::Map(entries) => entries
            .iter()
            .flat_map(|(key, value)| [key.as_obj_ref(), value.as_obj_ref()])
            .flatten()
            .collect(),
        Obj::Record(fields) => fields
            .iter()
            .filter_map(|(_, value)| value.as_obj_ref())
            .collect(),
        Obj::Upvalue(value) => value.as_obj_ref().into_iter().collect(),
        Obj::Closure { captures, .. } => captures.iter().filter_map(Value::as_obj_ref).collect(),
        Obj::String(_) | Obj::Function(_) | Obj::NativeFunction(_) | Obj::Module(_) => Vec::new(),
    }
}
