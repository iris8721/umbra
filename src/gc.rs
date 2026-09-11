use crate::value::Value;
use crate::vm::FxMap;

// Incremental tri-color mark-sweep, Lua 5.1-5.3 style. A cycle is a state
// machine — Pause → Propagate → Atomic → Sweep → Pause — advanced by
// step() calls from the VM's allocation checkpoints, with work per step
// proportional to bytes allocated since the last step (stepmul, 200%).
// Two whites distinguish "unmarked this cycle" (cur_white) from "dead,
// awaiting sweep" (dead_white): the sweep frees dead_white and flips
// survivors to cur_white, so objects allocated mid-sweep are born the
// right white and are never mistaken for garbage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcColor { WhiteA, WhiteB, Gray, Black }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GcKind { Str, Bytes, Table, Closure, BigInt }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcPhase { Pause, Propagate, Atomic, Sweep }

#[inline(always)]
fn is_white(c: GcColor) -> bool { matches!(c, GcColor::WhiteA | GcColor::WhiteB) }

// Nonzero while any collection cycle is in progress on this thread. The
// write barrier's fast path is a single load+branch on this; the slow path
// re-checks the running VM's phase, so a paused VM's stores stay cheap even
// while another VM on the same thread is mid-cycle.
thread_local! {
    pub(crate) static GC_ACTIVE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

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
    // Black until registered: an object allocated without a Vm is never
    // linked into the list, and Black keeps is_white_value/mark_value
    // treating it as live — the same "not tracked, not collectable" the old
    // map gave.
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
    gray_list:   Vec<usize>,
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
    weak_tables: Vec<usize>,
    // Tables that had a __gc metatable attached; the atomic step resurrects
    // whichever are still white. Entries are removed by the sweep via
    // header.fin_idx, so the list never holds a dangling pointer.
    fin_cands:   Vec<usize>,
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
    pub pending_finalizers: Vec<(usize, Value)>,
    pub running_finalizers: Vec<(usize, Value)>,
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
    pub fn register(&mut self, ptr: *mut u8) {
        let h = unsafe { &mut *(ptr as *mut GcHeader) };
        h.color = self.cur_white;
        h.next = self.head;
        self.head = h;
        self.count += 1;
        self.alloc_count += 1;
        let bytes = object_bytes(ptr as usize, h.kind);
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

    // Advance the state machine by roughly bytes_since_step * stepmul/100
    // worth of work. `forced` marks steps from an explicit full collect:
    // they run with an unbounded budget and stay out of max_step_work.
    // Returns the work done, in charged bytes.
    pub fn step(
        &mut self,
        roots: Option<Vec<Value>>,
        string_cache: &mut FxMap<String, Value>,
        forced: bool,
    ) -> usize {
        let work = if forced {
            usize::MAX / 4
        } else {
            (self.bytes_since_step / 64).max(1) * (self.stepmul as usize / 2).max(1)
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
                done += self.atomic(roots.unwrap_or_default(), string_cache);
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
        self.cycle_work = 0;
        self.phase = GcPhase::Propagate;
        // The trigger counts allocations since the last cycle STARTED, not
        // since it ended: resetting here keeps the inter-cycle gap at the
        // same 2x-live the stop-the-world collector had, instead of adding
        // a full gap on top of whatever the in-flight cycle already spanned.
        self.alloc_count = 0;
        self.alloc_bytes = 0;
        GC_ACTIVE.with(|a| a.set(a.get() + 1));
        for v in roots { self.mark_value(v); }
    }

    // Pop gray objects and mark their children until the budget is spent.
    // Always pops at least one so a tiny budget can't starve the cycle.
    fn propagate_step(&mut self, budget: usize) -> usize {
        let mut done = 0usize;
        while let Some(ptr) = self.gray_list.pop() {
            let kind = unsafe {
                let h = &mut *(ptr as *mut GcHeader);
                h.color = GcColor::Black;
                h.kind
            };
            done += object_bytes(ptr, kind).max(1);
            match kind {
                GcKind::Str | GcKind::Bytes | GcKind::BigInt => {}
                GcKind::Table   => self.propagate_table(ptr),
                GcKind::Closure => self.propagate_closure(ptr),
            }
            if done >= budget { break; }
        }
        done
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
                unsafe {
                    let h = &mut *(ptr as *mut GcHeader);
                    if !is_white(h.color) || h.finalized { continue; }
                    let t = &*(ptr as *const Table);
                    let mt_ptr = match t.metatable { Some(p) => p, None => continue };
                    let mt = &*mt_ptr;
                    if let Some(gc_fn) = mt.get_str("__gc") {
                        h.finalized = true;
                        done += self.mark_value(Value::table(ptr as *mut u8));
                        self.pending_finalizers.push((ptr, gc_fn));
                        resurrected = true;
                    }
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
                let ptr = self.weak_tables[i];
                let t = unsafe { &mut *(ptr as *mut Table) };
                let (weak_keys, weak_values) = table_weak_mode(t);
                if weak_values {
                    for slot in t.array.iter_mut() {
                        if self.is_white_value(*slot) { *slot = Value::nil(); }
                    }
                }
                t.hash.retain(|k, v| {
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
            let cur = unsafe {
                if self.sweep_prev.is_null() { self.head } else { (*self.sweep_prev).next }
            };
            if cur.is_null() {
                self.finish_cycle();
                break;
            }
            unsafe {
                let h = &mut *cur;
                let kind = h.kind;
                done += object_bytes(cur as usize, kind).max(1);
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
                        if moved != cur as usize {
                            (*(moved as *mut GcHeader)).fin_idx = idx;
                        }
                    }
                    self.count -= 1;
                    free_object(cur as usize, kind);
                } else {
                    h.color = self.cur_white;
                    self.live_bytes += object_bytes(cur as usize, kind);
                    self.sweep_prev = cur;
                }
            }
            if done >= budget { break; }
        }
        done
    }

    fn finish_cycle(&mut self) {
        self.phase = GcPhase::Pause;
        GC_ACTIVE.with(|a| a.set(a.get().saturating_sub(1)));
        self.live_after_collect = self.count;
        self.live_bytes_after_collect = self.live_bytes;
        self.last_cycle_work = self.cycle_work;
    }

    // Full drain, used by the atomic step and its ephemeron fixpoint.
    fn propagate(&mut self) {
        while let Some(ptr) = self.gray_list.pop() {
            let kind = unsafe {
                let h = &mut *(ptr as *mut GcHeader);
                h.color = GcColor::Black;
                h.kind
            };
            match kind {
                GcKind::Str | GcKind::Bytes | GcKind::BigInt => {}
                GcKind::Table   => self.propagate_table(ptr),
                GcKind::Closure => self.propagate_closure(ptr),
            }
        }
    }

    pub fn free_all(&mut self) {
        let mut h = self.head;
        while !h.is_null() {
            unsafe {
                let next = (*h).next;
                free_object(h as usize, (*h).kind);
                h = next;
            }
        }
        self.head = std::ptr::null_mut();
        self.count = 0;
        self.gray_list.clear();
        self.weak_tables.clear();
        self.fin_cands.clear();
        self.pending_finalizers.clear();
        self.running_finalizers.clear();
        self.alloc_count = 0;
        self.alloc_bytes = 0;
        self.bytes_since_step = 0;
        self.live_after_collect = 0;
        self.live_bytes_after_collect = 0;
        if self.phase != GcPhase::Pause {
            self.phase = GcPhase::Pause;
            GC_ACTIVE.with(|a| a.set(a.get().saturating_sub(1)));
        }
    }

    // The GC pointer inside a value, if it has one.
    #[inline(always)]
    fn gc_ptr(v: Value) -> Option<*mut u8> {
        if v.is_string() { v.as_string() }
        else if v.is_table() { v.as_table() }
        else if v.is_closure() { v.as_closure() }
        else if v.is_bigint() { v.as_bigint() }
        else if v.is_bytes() { v.as_bytes_ptr() }
        else { None }
    }

    // A GC-tracked value still White (either shade) hasn't been marked
    // reachable this cycle. Objects allocated without a Vm (CURRENT_VM null)
    // are never linked into the list, so they can't be swept — but they can
    // sit in weak tables, and treating them as collectable matches what a
    // registered dead object gets.
    #[inline(always)]
    pub(crate) fn is_white_value(&self, v: Value) -> bool {
        match Self::gc_ptr(v) {
            Some(p) => unsafe { is_white((*(p as *const GcHeader)).color) },
            None => false,
        }
    }

    // White → gray (or straight to black for leaf kinds). Returns the
    // object's charged size when it newly marked it, 0 otherwise.
    fn mark_value(&mut self, v: Value) -> usize {
        let Some(ptr) = Self::gc_ptr(v) else { return 0 };
        let h = unsafe { &mut *(ptr as *mut GcHeader) };
        if !is_white(h.color) { return 0; }
        match h.kind {
            // Leaf kinds have no outgoing edges: straight to black, no gray
            // round-trip through propagate().
            GcKind::Str | GcKind::Bytes | GcKind::BigInt => { h.color = GcColor::Black; }
            _ => {
                h.color = GcColor::Gray;
                self.gray_list.push(ptr as usize);
            }
        }
        object_bytes(ptr as usize, h.kind)
    }

    // Forward write barrier: storing a white value where the collector may
    // already have scanned past it (a black table, a closure's upvalue
    // array, the globals table) marks the value now instead of letting the
    // sweep free a live object. Cheap when idle: callers gate this behind a
    // single GC_ACTIVE/color check, and it early-outs unless a cycle is
    // actually in progress.
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
        let h = unsafe { &mut *(t as *mut GcHeader) };
        if h.fin_idx == u32::MAX {
            h.fin_idx = self.fin_cands.len() as u32;
            self.fin_cands.push(t as usize);
        }
    }

    fn propagate_closure(&mut self, ptr: usize) {
        use crate::vm::LuaClosure;
        let c = unsafe { &*(ptr as *const LuaClosure) };
        for &v in c.upvals.iter() { self.mark_value(v); }
    }

    fn propagate_table(&mut self, ptr: usize) {
        use crate::vm::{Table, TableKey};
        let t = unsafe { &*(ptr as *const Table) };
        let (weak_keys, weak_values) = table_weak_mode(t);
        if weak_keys || weak_values {
            // Weak entries are resolved at atomic, when liveness is final;
            // record the table so it gets cleaned (and, for weak keys, its
            // ephemeron values marked) there.
            self.weak_tables.push(ptr);
        }
        if !weak_values {
            for &v in t.array.iter() { self.mark_value(v); }
        }
        if !weak_keys {
            for (k, v) in t.hash.iter() {
                match k {
                    TableKey::Ptr(bits) => { self.mark_value(Value::from_raw(*bits)); }
                    TableKey::Str(p)    => { self.mark_value(Value::string(*p)); }
                    _ => {}
                }
                if !weak_values { self.mark_value(*v); }
            }
        }
        if let Some(mt) = t.metatable {
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
                let ptr = self.weak_tables[i];
                let t = unsafe { &*(ptr as *const Table) };
                let (weak_keys, weak_values) = table_weak_mode(t);
                if !weak_keys || weak_values { continue; }
                let pending: Vec<Value> = t.hash.iter()
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

fn free_object(ptr: usize, kind: GcKind) {
    use crate::vm::{GcBigInt, LuaClosure, RtBytes, RtString, Table};
    unsafe {
        match kind {
            GcKind::Str     => drop(Box::from_raw(ptr as *mut RtString)),
            GcKind::Bytes   => drop(Box::from_raw(ptr as *mut RtBytes)),
            GcKind::Table   => drop(Box::from_raw(ptr as *mut Table)),
            GcKind::Closure => drop(Box::from_raw(ptr as *mut LuaClosure)),
            GcKind::BigInt  => drop(Box::from_raw(ptr as *mut GcBigInt)),
        }
    }
}

// Heap bytes an object holds, for the byte-paced collection trigger: the
// owned buffers (string payload, table array + hash slots, closure upvalue
// vec) plus the struct itself. Capacities, not lengths — the pacing question
// is how much memory is outstanding, not how full each buffer is.
fn object_bytes(ptr: usize, kind: GcKind) -> usize {
    use crate::vm::{GcBigInt, LuaClosure, RtBytes, RtString, Table};
    unsafe {
        match kind {
            GcKind::Str => {
                let s = &*(ptr as *const RtString);
                std::mem::size_of::<RtString>() + s.len + 1
            }
            GcKind::Bytes => {
                let b = &*(ptr as *const RtBytes);
                std::mem::size_of::<RtBytes>() + b.data.len()
            }
            GcKind::Table => {
                let t = &*(ptr as *const Table);
                std::mem::size_of::<Table>()
                    + t.array.capacity() * std::mem::size_of::<Value>()
                    + t.hash.capacity_bytes()
            }
            GcKind::Closure => {
                let c = &*(ptr as *const LuaClosure);
                std::mem::size_of::<LuaClosure>()
                    + c.upvals.capacity() * std::mem::size_of::<Value>()
            }
            GcKind::BigInt => std::mem::size_of::<GcBigInt>(),
        }
    }
}

fn table_weak_mode(t: &crate::vm::Table) -> (bool, bool) {
    use crate::vm::string_ref;
    let mt_ptr = match t.metatable { Some(p) => p, None => return (false, false) };
    let mt = unsafe { &*mt_ptr };
    match mt.get_str("__mode") {
        Some(v) if v.is_string() => {
            let s = unsafe { string_ref(v) };
            (s.contains('k'), s.contains('v'))
        }
        _ => (false, false),
    }
}
