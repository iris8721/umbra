use crate::value::Value;
use crate::vm::FxMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcColor { White, Gray, Black }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GcKind { Str, Table, Closure, BigInt }

// Every heap object starts with this header (the structs in vm.rs are
// repr(C) with `gc` first, so the object pointer IS the header pointer).
// Marking reads color/kind through the value pointer itself — no side-table
// lookup — and the sweep walks the intrusive `next` list.
#[repr(C)]
pub struct GcHeader {
    pub color: GcColor,
    pub kind: GcKind,
    pub finalized: bool,
    pub next: *mut GcHeader,
}

impl GcHeader {
    // Black until registered: an object allocated without a Vm is never
    // linked into the list, and Black keeps is_dead_value/mark_value treating
    // it as live — the same "not tracked, not collectable" the old map gave.
    pub fn new(kind: GcKind) -> Self {
        GcHeader { color: GcColor::Black, kind, finalized: false, next: std::ptr::null_mut() }
    }
}

pub struct Gc {
    head:        *mut GcHeader,
    count:       usize,
    gray_list:   Vec<usize>,
    // Allocations since the last collection; a cycle is due once this reaches
    // threshold or twice the live count at the end of the previous cycle —
    // the same 200% pause the byte trigger uses, so a big live heap isn't
    // re-marked every 1024 allocs.
    alloc_count: usize,
    live_after_collect: usize,
    // Same pacing in bytes: a few huge allocations (multi-KB strings, wide
    // table arrays) must not wait for the object-count threshold while dead
    // memory piles up — Lua paces its collector by bytes for the same reason.
    alloc_bytes: usize,
    live_bytes_after_collect: usize,
    // Weak-key (and not weak-value) tables marked this cycle, collected by
    // propagate_table so mark_ephemerons scans them instead of the whole heap.
    weak_key_tables: Vec<usize>,
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
            weak_key_tables: Vec::new(),
            threshold:   1024,
            byte_threshold: 1 << 20,
            max_objects: 0,
            pending_finalizers: Vec::new(),
            running_finalizers: Vec::new(),
        }
    }

    // The allocation functions already filled in the header's kind; this
    // links the object into the heap list, whitens it for the current cycle,
    // and accounts for it.
    pub fn register(&mut self, ptr: *mut u8) {
        let h = unsafe { &mut *(ptr as *mut GcHeader) };
        h.color = GcColor::White;
        h.next = self.head;
        self.head = h;
        self.count += 1;
        self.alloc_count += 1;
        self.alloc_bytes += object_bytes(ptr as usize, h.kind);
    }

    pub fn should_collect(&self) -> bool {
        self.alloc_count >= self.threshold.max(2 * self.live_after_collect)
            || self.alloc_bytes >= self.byte_threshold.max(2 * self.live_bytes_after_collect)
            || (self.max_objects != 0 && self.count >= self.max_objects)
    }

    pub fn live_count(&self) -> usize { self.count }

    pub fn collect(
        &mut self,
        roots: impl Iterator<Item = Value>,
        string_cache: &mut FxMap<String, Value>,
    ) {
        self.weak_key_tables.clear();

        for v in roots { self.mark_value(v); }
        self.propagate();
        self.mark_ephemerons();

        // One pass collects both the white-table candidates for __gc
        // resurrection and the live weak tables needing entry cleanup.
        // Resurrection is rare; when it marks new tables the weak list is
        // rescanned below.
        let mut white_tables: Vec<usize> = Vec::new();
        let mut weak_tables: Vec<usize> = Vec::new();
        {
            use crate::vm::Table;
            let mut h = self.head;
            while !h.is_null() {
                unsafe {
                    if (*h).kind == GcKind::Table {
                        let ptr = h as usize;
                        if (*h).color == GcColor::White {
                            if !(*h).finalized { white_tables.push(ptr); }
                        } else {
                            let (wk, wv) = table_weak_mode(&*(ptr as *const Table));
                            if wk || wv { weak_tables.push(ptr); }
                        }
                    }
                    h = (*h).next;
                }
            }
        }

        // Unreachable tables with a __gc metamethod are resurrected for one
        // more cycle: marking the table reaches its metatable and the finalizer
        // through normal propagation, so __gc(table) can run after this sweep.
        // An object is finalized at most once.
        {
            use crate::vm::Table;
            let mut resurrected = false;
            for ptr in white_tables {
                let t = unsafe { &*(ptr as *const Table) };
                let mt_ptr = match t.metatable { Some(p) => p, None => continue };
                let mt = unsafe { &*mt_ptr };
                if let Some(gc_fn) = mt.get_str("__gc") {
                    unsafe { (*(ptr as *mut GcHeader)).finalized = true; }
                    self.mark_value(Value::table(ptr as *mut u8));
                    self.pending_finalizers.push((ptr, gc_fn));
                    resurrected = true;
                }
            }
            if resurrected {
                self.propagate();
                self.mark_ephemerons();
                // Resurrection may have marked weak tables the first scan saw
                // as white; rebuild the list so their entries still get
                // cleaned this cycle.
                weak_tables.clear();
                let mut h = self.head;
                while !h.is_null() {
                    unsafe {
                        if (*h).kind == GcKind::Table && (*h).color != GcColor::White {
                            let (wk, wv) = table_weak_mode(&*(h as *const Table));
                            if wk || wv { weak_tables.push(h as usize); }
                        }
                        h = (*h).next;
                    }
                }
            }
        }

        // A table with __mode 'k'/'v' skipped marking those keys/values in
        // propagate_table, so drop any now-White (unreachable) entries here,
        // before the general sweep frees them out from under a stale reference.
        {
            use crate::vm::{Table, TableKey};
            for ptr in weak_tables {
                let t = unsafe { &mut *(ptr as *mut Table) };
                let (weak_keys, weak_values) = table_weak_mode(t);
                if weak_values {
                    for slot in t.array.iter_mut() {
                        if self.is_dead_value(*slot) { *slot = Value::nil(); }
                    }
                }
                t.hash.retain(|k, v| {
                    if weak_values && self.is_dead_value(*v) { return false; }
                    if weak_keys {
                        if let TableKey::Ptr(bits) = k {
                            if self.is_dead_value(Value::from_raw(*bits)) { return false; }
                        }
                        if let TableKey::Str(p) = k {
                            if self.is_dead_value(Value::string(*p)) { return false; }
                        }
                    }
                    true
                });
            }
        }

        // The string cache doesn't root its strings, so drop entries whose
        // string is still White — must run before the sweep re-whitens.
        string_cache.retain(|_, v| {
            if v.is_string() {
                let h = v.as_string().unwrap() as *const GcHeader;
                unsafe { (*h).color != GcColor::White }
            } else {
                true
            }
        });

        // Sweep, re-whiten, and recount live bytes in one pass over the
        // intrusive list: survivors go back to White so the next cycle needs
        // no reset pass, and their sizes become next cycle's pacing floor.
        let mut live_bytes = 0usize;
        unsafe {
            let mut prev: *mut *mut GcHeader = &mut self.head;
            while !(*prev).is_null() {
                let h = *prev;
                if (*h).color == GcColor::White {
                    *prev = (*h).next;
                    self.count -= 1;
                    free_object(h as usize, (*h).kind);
                } else {
                    (*h).color = GcColor::White;
                    live_bytes += object_bytes(h as usize, (*h).kind);
                    prev = &mut (*h).next;
                }
            }
        }

        self.alloc_count = 0;
        self.alloc_bytes = 0;
        self.live_after_collect = self.count;
        self.live_bytes_after_collect = live_bytes;
    }

    fn propagate(&mut self) {
        while let Some(ptr) = self.gray_list.pop() {
            let kind = unsafe {
                let h = &mut *(ptr as *mut GcHeader);
                h.color = GcColor::Black;
                h.kind
            };
            match kind {
                GcKind::Str | GcKind::BigInt => {}
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
        self.pending_finalizers.clear();
        self.running_finalizers.clear();
        self.weak_key_tables.clear();
        self.alloc_count = 0;
        self.alloc_bytes = 0;
        self.live_after_collect = 0;
        self.live_bytes_after_collect = 0;
    }

    fn mark_value(&mut self, v: Value) {
        let ptr = if v.is_string() { v.as_string() }
            else if v.is_table() { v.as_table() }
            else if v.is_closure() { v.as_closure() }
            else if v.is_bigint() { v.as_bigint() }
            else { None };
        let Some(ptr) = ptr else { return };
        let h = unsafe { &mut *(ptr as *mut GcHeader) };
        if h.color != GcColor::White { return; }
        match h.kind {
            // Leaf kinds have no outgoing edges: straight to black, no gray
            // round-trip through propagate().
            GcKind::Str | GcKind::BigInt => { h.color = GcColor::Black; }
            _ => {
                h.color = GcColor::Gray;
                self.gray_list.push(ptr as usize);
            }
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
        if weak_keys && !weak_values { self.weak_key_tables.push(ptr); }
        if !weak_values {
            for &v in t.array.iter() { self.mark_value(v); }
        }
        for (k, v) in t.hash.iter() {
            if weak_keys {
                // Ephemeron semantics: the value is only marked once its
                // key is known reachable, so a value that points back at
                // its own key can't keep the entry alive. Keys still white
                // here may be marked later; mark_ephemerons revisits them.
                let key_live = match k {
                    TableKey::Ptr(bits) => !self.is_dead_value(Value::from_raw(*bits)),
                    TableKey::Str(p)    => !self.is_dead_value(Value::string(*p)),
                    _ => true,
                };
                if key_live && !weak_values { self.mark_value(*v); }
            } else {
                match k {
                    TableKey::Ptr(bits) => self.mark_value(Value::from_raw(*bits)),
                    TableKey::Str(p)    => self.mark_value(Value::string(*p)),
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
    // so this runs to a fixpoint. Called after each propagate() in collect.
    // Only tables propagate_table recorded in weak_key_tables are scanned —
    // including any it discovered during this fixpoint's own propagate()s.
    fn mark_ephemerons(&mut self) {
        use crate::vm::{Table, TableKey};
        loop {
            let mut marked_any = false;
            for i in 0..self.weak_key_tables.len() {
                let ptr = self.weak_key_tables[i];
                let t = unsafe { &*(ptr as *const Table) };
                let pending: Vec<Value> = t.hash.iter()
                    .filter(|(k, v)| {
                        let key_live = match k {
                            TableKey::Ptr(bits) => !self.is_dead_value(Value::from_raw(*bits)),
                            TableKey::Str(p)    => !self.is_dead_value(Value::string(*p)),
                            _ => true,
                        };
                        key_live && self.is_dead_value(**v)
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

    // A GC-tracked value still White didn't get marked reachable this cycle.
    // Objects allocated without a Vm (CURRENT_VM null) are never linked into
    // the list, so they can't be swept — but they can sit in weak tables, and
    // treating them as collectable matches what a registered dead object gets.
    fn is_dead_value(&self, v: Value) -> bool {
        let ptr = if v.is_string() { v.as_string() }
            else if v.is_table() { v.as_table() }
            else if v.is_closure() { v.as_closure() }
            else if v.is_bigint() { v.as_bigint() }
            else { None };
        match ptr {
            Some(p) => unsafe { (*(p as *const GcHeader)).color == GcColor::White },
            None => false,
        }
    }
}

fn free_object(ptr: usize, kind: GcKind) {
    use crate::vm::{GcBigInt, LuaClosure, RtString, Table};
    unsafe {
        match kind {
            GcKind::Str     => drop(Box::from_raw(ptr as *mut RtString)),
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
    use crate::vm::{GcBigInt, LuaClosure, RtString, Table};
    unsafe {
        match kind {
            GcKind::Str => {
                let s = &*(ptr as *const RtString);
                std::mem::size_of::<RtString>() + s.len + 1
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
