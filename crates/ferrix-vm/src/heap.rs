//! Generational-reference heap with explicit and incremental mark/sweep collection.
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
    incremental: IncrementalGc,
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

/// Public phase marker for the heap's incremental collector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IncrementalGcPhase {
    /// No incremental collection is currently active.
    #[default]
    Idle,
    /// Reachable objects are being marked from a grey-object worklist.
    Marking,
    /// Heap slots are being swept in bounded chunks.
    Sweeping,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct IncrementalGc {
    phase: IncrementalGcPhase,
    mark_stack: Vec<ObjRef>,
    sweep_index: usize,
    stats: GcStats,
}

impl Heap {
    /// Creates an empty heap.
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
            incremental: IncrementalGc::default(),
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

        let incremental_phase = self.incremental.phase;
        let incremental_sweep_index = self.incremental.sweep_index;

        if let Some((index, slot)) = self
            .objects
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.object.is_none())
        {
            let slot_index = index;
            let reference_index = u32::try_from(slot_index).map_err(|_| {
                VmError::new(
                    None,
                    VmErrorKind::HeapObjectLimitExceeded {
                        max_heap_objects: limits.max_heap_objects,
                    },
                )
            })?;
            slot.marked =
                should_mark_allocated_slot(incremental_phase, incremental_sweep_index, slot_index);
            slot.object = Some(object);
            let reference = ObjRef::new(reference_index, slot.generation);
            self.mark_allocated_children(reference);
            return Ok(reference);
        }

        let reference = ObjRef::new(index, 0);
        self.objects.push(HeapSlot {
            generation: reference.generation,
            marked: should_mark_allocated_slot(
                incremental_phase,
                incremental_sweep_index,
                usize::try_from(reference.index).unwrap(),
            ),
            object: Some(object),
        });
        self.mark_allocated_children(reference);
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
        self.incremental = IncrementalGc::default();
        self.clear_marks();

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

    /// Returns the current incremental GC phase.
    pub fn incremental_phase(&self) -> IncrementalGcPhase {
        self.incremental.phase
    }

    /// Returns true when an incremental collection is in progress.
    pub fn is_incremental_collection_active(&self) -> bool {
        self.incremental.phase != IncrementalGcPhase::Idle
    }

    /// Starts an incremental collection from the supplied root snapshot.
    ///
    /// Returns `true` when a new collection was started. If a collection is
    /// already active, callers should keep stepping that collection instead.
    pub fn start_incremental_collection(&mut self, roots: &[ObjRef]) -> bool {
        if self.is_incremental_collection_active() {
            return false;
        }

        self.clear_marks();
        self.incremental = IncrementalGc {
            phase: IncrementalGcPhase::Marking,
            mark_stack: roots.to_vec(),
            sweep_index: 0,
            stats: GcStats::default(),
        };
        true
    }

    /// Advances the active incremental collection by a bounded amount of work.
    ///
    /// Returns completed collection stats when the sweep phase finishes.
    pub fn step_incremental_collection(&mut self, budget: usize) -> Option<GcStats> {
        if !self.is_incremental_collection_active() {
            return None;
        }

        let mut remaining = budget.max(1);
        while remaining > 0 {
            match self.incremental.phase {
                IncrementalGcPhase::Idle => return None,
                IncrementalGcPhase::Marking => {
                    if let Some(reference) = self.incremental.mark_stack.pop() {
                        self.mark_incremental_reference(reference);
                        remaining -= 1;
                    } else {
                        self.incremental.phase = IncrementalGcPhase::Sweeping;
                    }
                }
                IncrementalGcPhase::Sweeping => {
                    if self.incremental.sweep_index >= self.objects.len() {
                        return Some(self.complete_incremental_collection());
                    }

                    self.sweep_incremental_slot();
                    remaining -= 1;
                }
            }
        }

        if self.incremental.phase == IncrementalGcPhase::Sweeping
            && self.incremental.sweep_index >= self.objects.len()
        {
            return Some(self.complete_incremental_collection());
        }

        None
    }

    /// Completes the active incremental collection immediately, if one exists.
    pub fn finish_incremental_collection(&mut self) -> Option<GcStats> {
        while self.is_incremental_collection_active() {
            if let Some(stats) = self.step_incremental_collection(usize::MAX) {
                return Some(stats);
            }
        }
        None
    }

    /// Preserves a newly stored object reference during an active collection.
    pub fn write_barrier_value(&mut self, value: Value) {
        let Some(reference) = value.as_obj_ref() else {
            return;
        };
        if self.is_incremental_collection_active() {
            self.mark_incremental_reference(reference);
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

    fn mark_allocated_children(&mut self, reference: ObjRef) {
        if !self.is_incremental_collection_active() {
            return;
        }

        let children = self
            .get(reference)
            .map(object_references)
            .unwrap_or_default();
        for child in children {
            self.mark_incremental_reference(child);
        }
    }

    fn mark_incremental_reference(&mut self, reference: ObjRef) {
        let children = {
            let Some(slot) = self.slot_mut(reference) else {
                return;
            };
            if slot.marked {
                return;
            }
            slot.marked = true;
            slot.object
                .as_ref()
                .map(object_references)
                .unwrap_or_default()
        };

        self.incremental.stats.marked = self.incremental.stats.marked.saturating_add(1);
        self.incremental.mark_stack.extend(children);
        if self.incremental.phase == IncrementalGcPhase::Sweeping {
            self.incremental.phase = IncrementalGcPhase::Marking;
        }
    }

    fn sweep_incremental_slot(&mut self) {
        let index = self.incremental.sweep_index;
        self.incremental.sweep_index += 1;

        let Some(slot) = self.objects.get_mut(index) else {
            return;
        };
        if slot.object.is_none() {
            return;
        }

        if slot.marked {
            slot.marked = false;
        } else {
            slot.object = None;
            slot.marked = false;
            slot.generation = slot.generation.wrapping_add(1);
            self.incremental.stats.swept = self.incremental.stats.swept.saturating_add(1);
        }
    }

    fn complete_incremental_collection(&mut self) -> GcStats {
        let mut stats = self.incremental.stats;
        stats.live = self.len();
        self.clear_marks();
        self.incremental = IncrementalGc::default();
        stats
    }

    fn clear_marks(&mut self) {
        for slot in &mut self.objects {
            slot.marked = false;
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

fn should_mark_allocated_slot(phase: IncrementalGcPhase, sweep_index: usize, index: usize) -> bool {
    match phase {
        IncrementalGcPhase::Idle => false,
        IncrementalGcPhase::Marking => true,
        IncrementalGcPhase::Sweeping => index >= sweep_index,
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
