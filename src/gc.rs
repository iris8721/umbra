use crate::value::Value;
use crate::vm::FxMap;

// Incremental tri-color mark-sweep, Lua 5.1-5.3 style. A cycle is a state
// machine — Pause → Propagate → Atomic → Sweep → Pause — advanced by
// step() calls from the VM's allocation checkpoints, with work per step
// proportional to bytes allocated since the last step (stepmul, 3200%).
// Two whites distinguish "unmarked this cycle" (cur_white) from "dead,
// awaiting sweep" (dead_white): the sweep frees dead_white and flips
// survivors to cur_white, so objects allocated mid-sweep are born the
// right white and are never mistaken for garbage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcColor { WhiteA, WhiteB, Gray, Black }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GcKind { Str, Bytes, Table, Closure, BigInt, Native, Coroutine, Proto }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcPhase { Pause, Propagate, Atomic, Sweep }

#[inline(always)]
fn is_white(c: GcColor) -> bool { matches!(c, GcColor::WhiteA | GcColor::WhiteB) }

// Every heap object starts with this header (the structs in vm.rs are
// repr(C) with `gc` first, so the object pointer IS the header pointer).
// Marking reads color/kind through the value pointer itself — no side-table
// lookup — and the sweep walks the intrusive `next` list.
#[repr(C)]
pub struct GcHeader {
    pub color: GcColor,
    pub kind: GcKind,
    pub finalized: bool,
    // Index into Gc::fin_cands for tables that had a __gc metatable attached
    // (u32::MAX = not a candidate). Lets the sweep unlink a freed candidate
    // in O(1) instead of leaving a stale pointer for the next atomic.
    pub fin_idx: u32,
    pub next: *mut GcHeader,
}

impl GcHeader {
    // Black until registered: an object that is never linked into the list
    // (the Vm's own globals table) stays Black, which keeps
    // is_white_value/mark_value treating it as live — the same "not
    // tracked, not collectable" the old map gave.
    pub fn new(kind: GcKind) -> Self {
        GcHeader { color: GcColor::Black, kind, finalized: false, fin_idx: u32::MAX, next: std::ptr::null_mut() }
    }
}

// Minimum allocation between GC steps; also the unit stepmul scales. 4KB
// keeps steps small enough that the longest pause is a small fraction of a
// full cycle while amortizing the checkpoint round-trip.
const STEP_GRANULE: usize = 4096;

pub struct Gc {
    head:        *mut GcHeader,
    count:       usize,
    // Object lists hold pointers, never addresses: an address turned back
    // into a pointer would need exposed provenance, which only the NaN box
    // (value.rs) relies on.
    gray_list:   Vec<*mut u8>,
    // Allocations since the last cycle started; a cycle is due once this
    // reaches threshold or 1.5x the live count at the end of the previous
    // cycle. Tighter than the old stop-the-world 2x: an incremental cycle
    // spans allocations of its own, so the trigger gap shrinks to keep peak
    // heap at parity.
    alloc_count: usize,
    live_after_collect: usize,
    // Same pacing in bytes: a few huge allocations (multi-KB strings, wide
    // table arrays) must not wait for the object-count threshold while dead
    // memory piles up — Lua paces its collector by bytes for the same reason.
    alloc_bytes: usize,
    live_bytes_after_collect: usize,
    // Bytes allocated since the last step; each step's work budget is this
    // times stepmul/100. Reset per step and at cycle start, so the first
    // propagate step doesn't inherit a whole Pause's worth of debt.
    bytes_since_step: usize,
    // Work per allocated byte, percent (Lua's stepmul). 3200 keeps a cycle's
    // span well under the 2x-live trigger gap, so in-flight garbage stays
    // bounded; each step is still only ~granule*32 of work against cycles
    // that run to megabytes.
    pub stepmul: u32,
    phase:       GcPhase,
    cur_white:   GcColor,
    dead_white:  GcColor,
    // Incremental sweep cursor: the last object kept so far (null = the next
    // candidate is head). Newborns prepend at head and are simply visited
    // like any other survivor — no self-pointer into `head` to maintain.
    sweep_prev:  *mut GcHeader,
    // Live bytes accumulated by the in-progress sweep; becomes
    // live_bytes_after_collect when the sweep finishes.
    live_bytes:  usize,
    // Weak tables marked this cycle; their entry cleanup is deferred to the
    // atomic step (Lua's grayagain) since liveness isn't final until then.
    weak_tables: Vec<*mut u8>,
    // Coroutines marked this cycle, scanned again at the atomic step (Lua's
    // grayagain for threads): a coroutine's stack changes with no write
    // barrier, since the interpreter writes registers directly while it
    // runs and resume swaps whole stacks in and out.
    coroutines: Vec<*mut u8>,
    // Tables that had a __gc metatable attached; the atomic step resurrects
    // whichever are still white. Entries are removed by the sweep via
    // header.fin_idx, so the list never holds a dangling pointer.
    fin_cands:   Vec<*mut u8>,
    pub threshold: usize,
    // Byte-side pacing floor; twice the live-byte count at the end of the
    // previous cycle raises it the same way live_after_collect raises
    // `threshold` (Lua's default pause is 200%).
    pub byte_threshold: usize,
    // Host-only hard ceiling on live objects (0 = unlimited); unlike threshold
    // (which just triggers a collection attempt), exceeding this after a
    // collection is a real error — the script genuinely needs more than allowed.
    pub max_objects: usize,
    // (table, __gc fn) pairs found unreachable by the last collect, waiting
    // for the VM to run them. Both stay rooted (see Vm::gc_roots) until the
    // finalizer has returned, since running one can trigger a nested cycle.
    pub pending_finalizers: Vec<(*mut u8, Value)>,
    pub running_finalizers: Vec<(*mut u8, Value)>,
    // Work accounting for the pause-bound test: the largest single
    // non-forced step and the total work of the last completed cycle.
    max_step_work: usize,
    cycle_work: usize,
    last_cycle_work: usize,
}

impl Gc {
    pub fn new() -> Self {
        Gc {
            head:        std::ptr::null_mut(),
            count:       0,
            gray_list:   Vec::new(),
            alloc_count: 0,
            live_after_collect: 0,
            alloc_bytes: 0,
            live_bytes_after_collect: 0,
            bytes_since_step: 0,
            stepmul:     3200,
            phase:       GcPhase::Pause,
            cur_white:   GcColor::WhiteA,
            dead_white:  GcColor::WhiteB,
            sweep_prev:  std::ptr::null_mut(),
            live_bytes:  0,
            weak_tables: Vec::new(),
            coroutines:  Vec::new(),
            fin_cands:   Vec::new(),
            threshold:   1024,
            byte_threshold: 1 << 20,
            max_objects: 0,
            pending_finalizers: Vec::new(),
            running_finalizers: Vec::new(),
            max_step_work: 0,
            cycle_work: 0,
            last_cycle_work: 0,
        }
    }

    // The allocation functions already filled in the header's kind; this
    // links the object into the heap list, whitens it for the current cycle,
    // and accounts for it.
    pub(crate) fn register(&mut self, ptr: *mut u8) {
        // Raw writes, so the list link keeps ptr's own provenance. A link
        // made from a &mut here would be derived from that temporary
        // borrow, and the next &mut to this header (marking) would
        // invalidate it under Rust's aliasing rules.
        let h = ptr as *mut GcHeader;
        // SAFETY: `ptr` is an object an alloc function just boxed, header
        // first (repr(C)); nothing else points at it yet.
        let kind = unsafe {
            (*h).color = self.cur_white;
            (*h).next = self.head;
            (*h).kind
        };
        self.head = h;
        self.count += 1;
        self.alloc_count += 1;
        let bytes = object_bytes(ptr, kind);
        self.alloc_bytes += bytes;
        self.bytes_since_step += bytes;
    }

    pub fn should_collect(&self) -> bool {
        self.alloc_count >= self.threshold.max(self.live_after_collect * 3 / 2)
            || self.alloc_bytes >= self.byte_threshold.max(self.live_bytes_after_collect * 3 / 2)
            || (self.max_objects != 0 && self.count >= self.max_objects)
    }

    // A checkpoint should hand control to the collector: either a cycle is
    // due (Pause) or one is in progress and enough was allocated to pay for
    // another step.
    #[inline(always)]
    pub fn should_step(&self) -> bool {
        if self.phase == GcPhase::Pause {
            self.should_collect()
        } else {
            self.bytes_since_step >= STEP_GRANULE
        }
    }

    // Cycle start and the atomic step both need the root set; the VM gathers
    // it only for those phases so ordinary steps don't pay for the scan.
    #[inline(always)]
    pub fn needs_roots(&self) -> bool {
        self.phase == GcPhase::Pause || self.phase == GcPhase::Atomic
    }

    pub fn phase(&self) -> GcPhase { self.phase }
    pub fn live_count(&self) -> usize { self.count }
    pub fn max_step_work(&self) -> usize { self.max_step_work }
    pub fn last_cycle_work(&self) -> usize { self.last_cycle_work }

    // Bytes held by every object on the heap, garbage not yet swept
    // included: collectgarbage("count"). A walk, since sizes change after
    // registration (a table's array grows) and only the objects know their
    // current ones.
    pub(crate) fn heap_bytes(&self) -> usize {
        let mut total = 0;
        let mut h = self.head;
        while !h.is_null() {
            // SAFETY: the list holds registered objects not yet freed. Reads
            // only, and &self keeps the collector from writing meanwhile.
            unsafe {
                total += object_bytes(h as *mut u8, (*h).kind);
                h = (*h).next;
            }
        }
        total
    }

    // Makes the next step do at least a granule's worth of work.
    pub(crate) fn owe_step(&mut self) {
        self.bytes_since_step = self.bytes_since_step.max(STEP_GRANULE);
    }

    // Advance the state machine by roughly bytes_since_step * stepmul/100
    // worth of work. `forced` marks steps from an explicit full collect:
    // they run with an unbounded budget and stay out of max_step_work.
    // Returns the work done, in charged bytes.
    pub(crate) fn step(
        &mut self,
        roots: Option<Vec<Value>>,
        string_cache: &mut FxMap<String, Value>,
        forced: bool,
    ) -> usize {
        let work = if forced {
            usize::MAX / 4
        } else {
            (self.bytes_since_step * self.stepmul as usize / 100).max(1)
        };
        self.bytes_since_step = 0;
        let mut done = 0usize;
        match self.phase {
            GcPhase::Pause => {
                let Some(roots) = roots else { return 0 };
                self.start_cycle(roots);
            }
            GcPhase::Propagate => {
                done += self.propagate_step(work);
                if self.gray_list.is_empty() { self.phase = GcPhase::Atomic; }
            }
            GcPhase::Atomic => {
                done += self.atomic(roots.expect("atomic step needs roots"), string_cache);
            }
            GcPhase::Sweep => {
                done += self.sweep_step(work);
            }
        }
        self.cycle_work += done;
        if !forced { self.max_step_work = self.max_step_work.max(done); }
        done
    }

    fn start_cycle(&mut self, roots: Vec<Value>) {
        debug_assert_eq!(self.phase, GcPhase::Pause);
        // Stray grays pushed by the barrier during sweep point at objects
        // that have since been re-whitened; dropping them is harmless.
        self.gray_list.clear();
        self.weak_tables.clear();
        self.coroutines.clear();
        self.cycle_work = 0;
        self.phase = GcPhase::Propagate;
        // The trigger counts allocations since the last cycle STARTED, not
        // since it ended: resetting here keeps the inter-cycle gap at the
        // same 2x-live the stop-the-world collector had, instead of adding
        // a full gap on top of whatever the in-flight cycle already spanned.
        self.alloc_count = 0;
        self.alloc_bytes = 0;
        for v in roots { self.mark_value(v); }
    }

    // Pop gray objects and mark their children until the budget is spent.
    // Always pops at least one so a tiny budget can't starve the cycle.
    fn propagate_step(&mut self, budget: usize) -> usize {
        let mut done = 0usize;
        while let Some(ptr) = self.gray_list.pop() {
            let kind = blacken(ptr);
            done += object_bytes(ptr, kind).max(1);
            self.propagate_object(ptr, kind);
            if done >= budget { break; }
        }
        done
    }

    // Marks a black object's children.
    fn propagate_object(&mut self, ptr: *mut u8, kind: GcKind) {
        match kind {
            GcKind::Str | GcKind::Bytes | GcKind::BigInt => {}
            GcKind::Table   => self.propagate_table(ptr),
            GcKind::Closure => self.propagate_closure(ptr),
            GcKind::Native  => self.propagate_native(ptr),
            GcKind::Coroutine => {
                self.coroutines.push(ptr);
                self.mark_coroutine(ptr);
            }
            GcKind::Proto => self.propagate_proto(ptr),
        }
    }

    // The one uninterruptible step: re-mark roots (registers, globals and
    // parked coroutine stacks may have changed since the cycle started),
    // drain the grays the write barrier produced, resolve ephemerons,
    // resurrect finalizable objects, clear dead weak entries, prune the
    // string cache, then flip the whites and hand off to the sweep.
    fn atomic(&mut self, roots: Vec<Value>, string_cache: &mut FxMap<String, Value>) -> usize {
        debug_assert_eq!(self.phase, GcPhase::Atomic);
        let mut done = 0usize;
        for v in roots { done += self.mark_value(v); }
        // The coroutines scanned so far may have run, yielded or been
        // resumed since; their stacks are final now.
        for i in 0..self.coroutines.len() {
            done += self.mark_coroutine(self.coroutines[i]);
        }
        self.coroutines.clear();
        done += self.propagate_step(usize::MAX / 4);
        self.mark_ephemerons();

        // Unreachable tables with a __gc metatable are resurrected for one
        // more cycle: marking the table reaches its metatable and the
        // finalizer through normal propagation, so __gc(table) can run after
        // this sweep. An object is finalized at most once. Must happen here,
        // before the sweep starts freeing — resurrecting an object whose
        // children were already freed would mark through dangling pointers.
        {
            use crate::vm::Table;
            let mut resurrected = false;
            for i in 0..self.fin_cands.len() {
                let ptr = self.fin_cands[i];
                let (h, t) = (ptr as *mut GcHeader, ptr as *const Table);
                // SAFETY: fin_cands holds live tables only (the sweep removes
                // a candidate as it frees it). Raw places, no references: the
                // metatable may be the table itself.
                let gc_fn = unsafe {
                    if !is_white((*h).color) || (*h).finalized { continue; }
                    let Some(mt) = (*t).metatable else { continue };
                    (*mt).get_str("__gc")
                };
                if let Some(gc_fn) = gc_fn {
                    // SAFETY: as above.
                    unsafe { (*h).finalized = true; }
                    done += self.mark_value(Value::table(ptr));
                    self.pending_finalizers.push((ptr, gc_fn));
                    resurrected = true;
                }
            }
            if resurrected {
                done += self.propagate_step(usize::MAX / 4);
                self.mark_ephemerons();
            }
        }

        // A table with __mode 'k'/'v' skipped marking those entries during
        // propagation, so drop any still-White (unreachable) entries here,
        // before the sweep frees them out from under a stale reference.
        {
            use crate::vm::{Table, TableKey};
            for i in 0..self.weak_tables.len() {
                let t = self.weak_tables[i] as *mut Table;
                // SAFETY: weak_tables holds tables marked this cycle, so
                // live. The field borrows leave the header uncovered:
                // is_white_value reads headers, maybe this table's own.
                let (array, hash, mt) = unsafe { (&mut (*t).array, &mut (*t).hash, (*t).metatable) };
                let (weak_keys, weak_values) = table_weak_mode(mt);
                if weak_values {
                    for slot in array.iter_mut() {
                        if self.is_white_value(*slot) { *slot = Value::nil(); }
                    }
                }
                hash.retain(|k, v| {
                    if weak_values && self.is_white_value(*v) { return false; }
                    if weak_keys {
                        if let TableKey::Ptr(bits) = k {
                            if self.is_white_value(Value::from_raw(*bits)) { return false; }
                        }
                        if let TableKey::Str(p) = k {
                            if self.is_white_value(Value::string(*p)) { return false; }
                        }
                    }
                    true
                });
            }
            done += self.weak_tables.len() + self.fin_cands.len();
        }

        // The string cache doesn't root its strings, so drop entries whose
        // string is still White — must run before the sweep re-whitens.
        done += string_cache.len();
        string_cache.retain(|_, v| !self.is_white_value(*v));

        // Flip the whites: everything still cur_white becomes dead_white and
        // is what the sweep frees; newborns from here on get the new
        // cur_white and are never mistaken for this cycle's garbage.
        std::mem::swap(&mut self.cur_white, &mut self.dead_white);
        self.sweep_prev = std::ptr::null_mut();
        self.live_bytes = 0;
        self.phase = GcPhase::Sweep;
        done
    }

    // Free dead_white objects until the budget is spent; survivors flip to
    // cur_white so the next cycle needs no reset pass. Always examines at
    // least one object. The cursor is the last kept object, so a freed head
    // just moves head forward and prepended newborns are visited in stride.
    fn sweep_step(&mut self, budget: usize) -> usize {
        let mut done = 0usize;
        loop {
            // SAFETY: sweep_prev is null or a live object the sweep kept.
            let cur = unsafe {
                if self.sweep_prev.is_null() { self.head } else { (*self.sweep_prev).next }
            };
            if cur.is_null() {
                self.finish_cycle();
                break;
            }
            // SAFETY: `cur` is on the heap list, so registered and not yet
            // freed. fin_cands entries are live tables. The object is freed
            // only after its last use here, and nothing outside the
            // collector can hold a reference to it: it is dead_white,
            // unreachable from every root.
            unsafe {
                // Sized before `h` exists: object_bytes reads the whole
                // object, header included, which a live &mut to the header
                // would forbid.
                let kind = (*cur).kind;
                let bytes = object_bytes(cur as *mut u8, kind);
                done += bytes.max(1);
                let h = &mut *cur;
                if h.color == self.dead_white {
                    if self.sweep_prev.is_null() {
                        self.head = h.next;
                    } else {
                        (*self.sweep_prev).next = h.next;
                    }
                    // Keep fin_cands free of dangling pointers: swap-remove
                    // the freed table's slot and fix the moved entry's index.
                    let idx = h.fin_idx;
                    if idx != u32::MAX {
                        let moved = *self.fin_cands.last().unwrap();
                        self.fin_cands.swap_remove(idx as usize);
                        if moved != cur as *mut u8 {
                            (*(moved as *mut GcHeader)).fin_idx = idx;
                        }
                    }
                    self.count -= 1;
                    free_object(cur as *mut u8, kind);
                } else {
                    h.color = self.cur_white;
                    self.live_bytes += bytes;
                    self.sweep_prev = cur;
                }
            }
            if done >= budget { break; }
        }
        done
    }

    fn finish_cycle(&mut self) {
        self.phase = GcPhase::Pause;
        self.live_after_collect = self.count;
        self.live_bytes_after_collect = self.live_bytes;
        self.last_cycle_work = self.cycle_work;
    }

    // Full drain, used by the atomic step and its ephemeron fixpoint.
    fn propagate(&mut self) {
        while let Some(ptr) = self.gray_list.pop() {
            let kind = blacken(ptr);
            self.propagate_object(ptr, kind);
        }
    }

    pub(crate) fn free_all(&mut self) {
        let mut h = self.head;
        while !h.is_null() {
            // SAFETY: the list holds each registered object once; each is
            // read, then freed, and never touched again.
            unsafe {
                let next = (*h).next;
                free_object(h as *mut u8, (*h).kind);
                h = next;
            }
        }
        self.head = std::ptr::null_mut();
        self.count = 0;
        self.gray_list.clear();
        self.weak_tables.clear();
        self.coroutines.clear();
        self.fin_cands.clear();
        self.pending_finalizers.clear();
        self.running_finalizers.clear();
        self.alloc_count = 0;
        self.alloc_bytes = 0;
        self.bytes_since_step = 0;
        self.live_after_collect = 0;
        self.live_bytes_after_collect = 0;
        self.phase = GcPhase::Pause;
    }


    // A GC-tracked value still White (either shade) hasn't been marked
    // reachable this cycle.
    #[inline(always)]
    pub(crate) fn is_white_value(&self, v: Value) -> bool {
        match v.gc_ptr() {
            // SAFETY: a heap Value the collector is handed points at a live
            // object (heap.rs liveness rule); a raw read of its header.
            Some(p) => unsafe { is_white((*(p as *const GcHeader)).color) },
            None => false,
        }
    }

    // White → gray (or straight to black for leaf kinds). Returns the
    // object's charged size when it newly marked it, 0 otherwise.
    fn mark_value(&mut self, v: Value) -> usize {
        let Some(ptr) = v.gc_ptr() else { return 0 };
        let h = ptr as *mut GcHeader;
        // SAFETY: live, as in is_white_value. Raw places, no reference: the
        // caller may be scanning this very object through field borrows.
        let kind = unsafe {
            if !is_white((*h).color) { return 0; }
            match (*h).kind {
                // Leaf kinds have no outgoing edges: straight to black, no
                // gray round-trip through propagate().
                GcKind::Str | GcKind::Bytes | GcKind::BigInt => (*h).color = GcColor::Black,
                _ => {
                    (*h).color = GcColor::Gray;
                    self.gray_list.push(ptr);
                }
            }
            (*h).kind
        };
        object_bytes(ptr, kind)
    }

    // Forward write barrier: storing a white value where the collector may
    // already have scanned past it (a black table, a closure's upvalue
    // array, the globals table) marks the value now instead of letting the
    // sweep free a live object. Cheap when idle: it early-outs unless a
    // cycle is actually in progress.
    #[inline(always)]
    pub(crate) fn barrier_val(&mut self, v: Value) {
        if self.phase != GcPhase::Pause && self.is_white_value(v) {
            self.mark_value(v);
        }
    }

    // Slow path for Table::raw_set's barrier: the table is known black, so
    // both the stored value and a heap-allocated key need marking. Keys
    // matter too — a white string key in a black table would dangle after
    // the sweep just like a white value would.
    #[inline(never)]
    pub(crate) fn barrier_store(&mut self, key: Value, val: Value) {
        if self.phase == GcPhase::Pause { return; }
        if self.is_white_value(key) { self.mark_value(key); }
        if self.is_white_value(val) { self.mark_value(val); }
    }

    // A table that just got a metatable with __gc becomes a finalizer
    // candidate for every future cycle until it's freed. Like Lua, __gc is
    // only noticed when the metatable is attached — adding the field to an
    // already-installed metatable doesn't make the table finalizable.
    pub(crate) fn add_fin_candidate(&mut self, t: *mut u8) {
        // SAFETY: Vm::set_metatable passes a live table; &mut self means no
        // reference from heap.rs into it is live.
        let h = unsafe { &mut *(t as *mut GcHeader) };
        if h.fin_idx == u32::MAX {
            h.fin_idx = self.fin_cands.len() as u32;
            self.fin_cands.push(t);
        }
    }

    // The propagate_* functions scan a black object through borrows of its
    // fields, never of the whole object: marking a child writes the child's
    // header, and the child may be the object itself.

    fn propagate_closure(&mut self, ptr: *mut u8) {
        use crate::vm::LuaClosure;
        let c = ptr as *const LuaClosure;
        // SAFETY: gray-list entries are live objects of their header's kind.
        let (upvals, proto) = unsafe { (&(*c).upvals, (*c).proto) };
        for &v in upvals.iter() { self.mark_value(v); }
        self.mark_value(Value::userdata(proto as *mut u8));
    }

    // A proto keeps its nested functions' protos (Op::Closure makes
    // functions from them) and the strings its constants have cached
    // (Vm::resolve_const hands those out without interning again).
    fn propagate_proto(&mut self, ptr: *mut u8) {
        use crate::chunk::{Const, GcProto};
        // SAFETY: as propagate_closure; the field borrow leaves the header
        // uncovered.
        let p = unsafe { &(*(ptr as *const GcProto)).proto };
        for &child in p.protos.iter() { self.mark_value(Value::userdata(child as *mut u8)); }
        for c in p.consts.iter() {
            if let Const::Str(sc) = c {
                let (_, bits) = sc.cached();
                if bits != 0 { self.mark_value(Value::from_raw(bits)); }
            }
        }
    }

    fn propagate_native(&mut self, ptr: *mut u8) {
        use crate::vm::NativeFn;
        // SAFETY: as propagate_closure.
        let upvals = unsafe { &(*(ptr as *const NativeFn)).upvals };
        for &v in upvals.iter() { self.mark_value(v); }
    }

    // Marks the stack parked in a coroutine: its own while it is suspended,
    // its resumer's while it runs (Vm::resume_coroutine swaps them). The
    // whole register file, like the stack scan Lua does for a thread.
    // Frames copy their varargs out of the registers and hold the callee of
    // a tail call and the running proto, so those are marked too. A dead
    // coroutine holds nothing.
    fn mark_coroutine(&mut self, ptr: *mut u8) -> usize {
        use crate::vm::{CoStatus, Coroutine};
        let co = ptr as *const Coroutine;
        // SAFETY: as propagate_closure; coroutine-list entries were marked
        // this cycle, so they are live too.
        let (regs, frames, fn_val, dead) =
            unsafe { (&(*co).regs, &(*co).frames, (*co).fn_val, (*co).status == CoStatus::Dead) };
        let mut done = 0;
        for &v in regs.iter() { done += self.mark_value(v); }
        for f in frames.iter() {
            for &v in f.varargs.iter() { done += self.mark_value(v); }
            done += self.mark_value(f.callable);
            done += self.mark_value(Value::userdata(f.proto as *mut u8));
        }
        if !dead { done += self.mark_value(fn_val); }
        done + regs.len()
    }

    fn propagate_table(&mut self, ptr: *mut u8) {
        use crate::vm::{Table, TableKey};
        let t = ptr as *const Table;
        // SAFETY: as propagate_closure.
        let (array, hash, mt) = unsafe { (&(*t).array, &(*t).hash, (*t).metatable) };
        let (weak_keys, weak_values) = table_weak_mode(mt);
        if weak_keys || weak_values {
            // Weak entries are resolved at atomic, when liveness is final;
            // record the table so it gets cleaned (and, for weak keys, its
            // ephemeron values marked) there.
            self.weak_tables.push(ptr);
        }
        if !weak_values {
            for &v in array.iter() { self.mark_value(v); }
        }
        if !weak_keys {
            for (k, v) in hash.iter() {
                match k {
                    TableKey::Ptr(bits) => { self.mark_value(Value::from_raw(*bits)); }
                    TableKey::Str(p)    => { self.mark_value(Value::string(*p)); }
                    _ => {}
                }
                if !weak_values { self.mark_value(*v); }
            }
        }
        if let Some(mt) = mt {
            self.mark_value(Value::table(mt as *mut u8));
        }
    }

    // Weak-key tables defer marking a value until its key is reachable, but
    // marking that value can make further keys (and their values) reachable,
    // so this runs to a fixpoint. Called by the atomic step after each
    // drain. Only weak-key-not-weak-value tables mark here: for 'kv' tables
    // a live key must not rescue a dead value.
    fn mark_ephemerons(&mut self) {
        use crate::vm::{Table, TableKey};
        loop {
            let mut marked_any = false;
            for i in 0..self.weak_tables.len() {
                let t = self.weak_tables[i] as *const Table;
                // SAFETY: as in atomic's weak-table cleanup.
                let (hash, mt) = unsafe { (&(*t).hash, (*t).metatable) };
                let (weak_keys, weak_values) = table_weak_mode(mt);
                if !weak_keys || weak_values { continue; }
                let pending: Vec<Value> = hash.iter()
                    .filter(|(k, v)| {
                        let key_live = match k {
                            TableKey::Ptr(bits) => !self.is_white_value(Value::from_raw(*bits)),
                            TableKey::Str(p)    => !self.is_white_value(Value::string(*p)),
                            _ => true,
                        };
                        key_live && self.is_white_value(**v)
                    })
                    .map(|(_, v)| *v)
                    .collect();
                for v in pending {
                    self.mark_value(v);
                    marked_any = true;
                }
            }
            if !marked_any { break; }
            self.propagate();
        }
    }
}

// Gray (or white, for the atomic re-mark) to black; returns the kind.
fn blacken(ptr: *mut u8) -> GcKind {
    let h = ptr as *mut GcHeader;
    // SAFETY: gray-list entries are live objects; raw places, as in
    // mark_value.
    unsafe {
        (*h).color = GcColor::Black;
        (*h).kind
    }
}

// SAFETY (callers): `ptr` came from Box::into_raw of the type its kind
// names, was registered, and is unreachable; it is never used again.
unsafe fn free_object(ptr: *mut u8, kind: GcKind) {
    use crate::vm::{Coroutine, GcBigInt, LuaClosure, NativeFn, RtBytes, RtString, Table};
    // SAFETY: per the caller.
    unsafe {
        match kind {
            GcKind::Str     => drop(Box::from_raw(ptr as *mut RtString)),
            GcKind::Bytes   => drop(Box::from_raw(ptr as *mut RtBytes)),
            GcKind::Table   => drop(Box::from_raw(ptr as *mut Table)),
            GcKind::Closure => drop(Box::from_raw(ptr as *mut LuaClosure)),
            GcKind::BigInt  => drop(Box::from_raw(ptr as *mut GcBigInt)),
            GcKind::Native  => drop(Box::from_raw(ptr as *mut NativeFn)),
            GcKind::Coroutine => drop(Box::from_raw(ptr as *mut Coroutine)),
            // Its nested protos are separate objects, freed on their own.
            GcKind::Proto   => drop(Box::from_raw(ptr as *mut crate::chunk::GcProto)),
        }
    }
}

// Heap bytes an object holds, for the byte-paced collection trigger: the
// owned buffers (string payload, table array + hash slots, closure upvalue
// vec) plus the struct itself. Capacities, not lengths — the pacing question
// is how much memory is outstanding, not how full each buffer is.
fn object_bytes(ptr: *mut u8, kind: GcKind) -> usize {
    use crate::vm::{Coroutine, Frame, GcBigInt, LuaClosure, NativeFn, RtBytes, RtString, Table};
    let v = std::mem::size_of::<Value>();
    // SAFETY: every caller passes a live object of `kind`. Field reads
    // only, so the header (which the caller may be writing) isn't covered.
    unsafe {
        match kind {
            GcKind::Str => std::mem::size_of::<RtString>() + (*(ptr as *const RtString)).len + 1,
            GcKind::Bytes => std::mem::size_of::<RtBytes>() + (*(ptr as *const RtBytes)).len,
            GcKind::Table => {
                let t = ptr as *const Table;
                std::mem::size_of::<Table>() + (*t).array.capacity() * v + (*t).hash.capacity_bytes()
            }
            GcKind::Closure => std::mem::size_of::<LuaClosure>() + (*(ptr as *const LuaClosure)).upvals.capacity() * v,
            GcKind::Native => {
                let upvals = &(*(ptr as *const NativeFn)).upvals;
                std::mem::size_of::<NativeFn>() + upvals.len() * v
            }
            GcKind::BigInt => std::mem::size_of::<GcBigInt>(),
            GcKind::Coroutine => {
                let co = ptr as *const Coroutine;
                std::mem::size_of::<Coroutine>() + (*co).regs.capacity() * v
                    + (*co).frames.capacity() * std::mem::size_of::<Frame>()
            }
            GcKind::Proto => {
                use crate::chunk::{Const, GcProto};
                let p = &(*(ptr as *const GcProto)).proto;
                std::mem::size_of::<GcProto>() + p.code.capacity() * 4 + p.lines.capacity() * 4
                    + p.consts.capacity() * std::mem::size_of::<Const>()
                    + p.global_cache.capacity() * 24 + p.protos.capacity() * 8
            }
        }
    }
}

// The weak mode a table's metatable gives it.
fn table_weak_mode(mt: Option<*mut crate::vm::Table>) -> (bool, bool) {
    let Some(mt_ptr) = mt else { return (false, false) };
    // SAFETY: a table's metatable is a live table: marking reaches it
    // through the table and setmetatable runs the write barrier.
    let mt = unsafe { &*mt_ptr };
    match mt.get_str("__mode") {
        Some(v) if v.is_string() => crate::heap::with_str(v, |s| (s.contains('k'), s.contains('v'))),
        _ => (false, false),
    }
}
