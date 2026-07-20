use std::collections::HashMap;
use crate::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcColor { White, Gray, Black }

#[derive(Clone, Copy)]
enum GcKind { Str, Table, Closure, BigInt }

struct GcEntry { color: GcColor, kind: GcKind }

pub struct Gc {
    objects:     HashMap<usize, GcEntry>,
    gray_list:   Vec<usize>,
    alloc_count: usize,
    pub threshold: usize,
    // Host-only hard ceiling on live objects (0 = unlimited); unlike threshold
    // (which just triggers a collection attempt), exceeding this after a
    // collection is a real error — the script genuinely needs more than allowed.
    pub max_objects: usize,
    pub pending_finalizers: Vec<(usize, Value)>,
}

impl Gc {
    pub fn new() -> Self {
        Gc {
            objects:     HashMap::new(),
            gray_list:   Vec::new(),
            alloc_count: 0,
            threshold:   1024,
            max_objects: 0,
            pending_finalizers: Vec::new(),
        }
    }

    pub fn register_string(&mut self, ptr: *mut u8) {
        self.objects.insert(ptr as usize, GcEntry { color: GcColor::White, kind: GcKind::Str });
        self.alloc_count += 1;
    }

    pub fn register_table(&mut self, ptr: *mut u8) {
        self.objects.insert(ptr as usize, GcEntry { color: GcColor::White, kind: GcKind::Table });
        self.alloc_count += 1;
    }

    pub fn register_closure(&mut self, ptr: *mut u8) {
        self.objects.insert(ptr as usize, GcEntry { color: GcColor::White, kind: GcKind::Closure });
        self.alloc_count += 1;
    }

    pub fn register_bigint(&mut self, ptr: *mut u8) {
        self.objects.insert(ptr as usize, GcEntry { color: GcColor::White, kind: GcKind::BigInt });
        self.alloc_count += 1;
    }

    pub fn should_collect(&self) -> bool {
        self.alloc_count >= self.threshold
            || (self.max_objects != 0 && self.alloc_count >= self.max_objects)
    }

    pub fn live_count(&self) -> usize { self.objects.len() }

    pub fn collect(
        &mut self,
        roots: impl Iterator<Item = Value>,
        string_cache: &mut HashMap<String, Value>,
    ) {
        for entry in self.objects.values_mut() { entry.color = GcColor::White; }

        for v in roots { self.mark_value(v); }

        while let Some(ptr) = self.gray_list.pop() {
            let kind = match self.objects.get(&ptr) {
                Some(e) => e.kind,
                None => continue,
            };
            if let Some(e) = self.objects.get_mut(&ptr) { e.color = GcColor::Black; }
            match kind {
                GcKind::Str | GcKind::BigInt => {}
                GcKind::Table   => self.propagate_table(ptr),
                GcKind::Closure => self.propagate_closure(ptr),
            }
        }

        // A table with __mode 'k'/'v' skipped marking those keys/values in
        // propagate_table, so drop any now-White (unreachable) entries here,
        // before the general sweep frees them out from under a stale reference.
        {
            use crate::vm::{Table, TableKey};
            let weak_tables: Vec<usize> = self.objects.iter()
                .filter(|(_, e)| e.color != GcColor::White && matches!(e.kind, GcKind::Table))
                .map(|(&ptr, _)| ptr)
                .filter(|&ptr| {
                    let (wk, wv) = table_weak_mode(unsafe { &*(ptr as *const Table) });
                    wk || wv
                })
                .collect();
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
                    }
                    true
                });
            }
        }

        // Pre-pass finds White tables with __gc and marks them (and their metatable)
        // Black before anything is freed. Without this, freeing proceeds in arbitrary
        // HashMap order, so a metatable can be freed before the table that references
        // it is inspected, and the dereference below reads freed memory.
        {
            use crate::vm::{Table, TableKey};
            let white_tables: Vec<usize> = self.objects.iter()
                .filter(|(_, e)| e.color == GcColor::White && matches!(e.kind, GcKind::Table))
                .map(|(&ptr, _)| ptr)
                .collect();
            for ptr in white_tables {
                let t = unsafe { &*(ptr as *const Table) };
                if let Some(mt_ptr) = t.metatable {
                    let mt_addr = mt_ptr as usize;
                    if !self.objects.contains_key(&mt_addr) { continue; }
                    let mt = unsafe { &*mt_ptr };
                    if let Some(&gc_fn) = mt.hash.get(&TableKey::Str("__gc".to_owned())) {
                        self.objects.get_mut(&ptr).unwrap().color = GcColor::Black;
                        if let Some(e) = self.objects.get_mut(&mt_addr) {
                            e.color = GcColor::Black;
                        }
                        self.pending_finalizers.push((ptr, gc_fn));
                    }
                }
            }
        }

        let dead: Vec<(usize, GcKind)> = self.objects.iter()
            .filter(|(_, e)| e.color == GcColor::White)
            .map(|(&ptr, e)| (ptr, e.kind))
            .collect();
        for (ptr, kind) in dead {
            self.objects.remove(&ptr);
            match kind {
                GcKind::Str => {
                    use crate::vm::RtString;
                    unsafe { drop(Box::from_raw(ptr as *mut RtString)) }
                }
                GcKind::Table => {
                    use crate::vm::Table;
                    unsafe { drop(Box::from_raw(ptr as *mut Table)) }
                }
                GcKind::Closure => {
                    use crate::vm::LuaClosure;
                    unsafe { drop(Box::from_raw(ptr as *mut LuaClosure)) }
                }
                GcKind::BigInt => unsafe { drop(Box::from_raw(ptr as *mut i64)) },
            }
        }

        string_cache.retain(|_, v| {
            if v.is_string() {
                let ptr = v.as_string().unwrap() as usize;
                self.objects.contains_key(&ptr)
            } else {
                true
            }
        });

        self.alloc_count = self.objects.len();
    }

    fn mark_value(&mut self, v: Value) {
        if v.is_string() {
            Self::mark_ptr_gray(&mut self.objects, &mut self.gray_list, v.as_string().unwrap() as usize);
        } else if v.is_table() {
            Self::mark_ptr_gray(&mut self.objects, &mut self.gray_list, v.as_table().unwrap() as usize);
        } else if v.is_closure() {
            Self::mark_ptr_gray(&mut self.objects, &mut self.gray_list, v.as_closure().unwrap() as usize);
        } else if v.is_bigint() {
            Self::mark_ptr_gray(&mut self.objects, &mut self.gray_list, v.as_bigint().unwrap() as usize);
        }
    }

    fn mark_ptr_gray(objects: &mut HashMap<usize, GcEntry>, gray: &mut Vec<usize>, ptr: usize) {
        if let Some(e) = objects.get_mut(&ptr) {
            if e.color == GcColor::White {
                e.color = GcColor::Gray;
                gray.push(ptr);
            }
        }
    }

    fn propagate_closure(&mut self, ptr: usize) {
        use crate::vm::LuaClosure;
        let upvals: Vec<Value> = unsafe { (*( ptr as *const LuaClosure)).upvals.clone() };
        for v in upvals { self.mark_value(v); }
    }

    fn propagate_table(&mut self, ptr: usize) {
        use crate::vm::{Table, TableKey};
        let (weak_keys, weak_values) = table_weak_mode(unsafe { &*(ptr as *const Table) });
        let values: Vec<Value> = unsafe {
            let t = &*(ptr as *const Table);
            let mut vs: Vec<Value> = Vec::new();
            if !weak_values {
                vs.extend(t.array.iter().copied());
                vs.extend(t.hash.values().copied());
            }
            if !weak_keys {
                for k in t.hash.keys() {
                    if let TableKey::Ptr(bits) = k { vs.push(Value::from_raw(*bits)); }
                }
            }
            if let Some(mt) = t.metatable {
                vs.push(Value::table(mt as *mut u8));
            }
            vs
        };
        for v in values { self.mark_value(v); }
    }

    // A GC-tracked value that isn't in `objects` at all was never a heap object
    // (nil/bool/int/float) and can't be "dead"; one that IS tracked but currently
    // White didn't get marked reachable this cycle.
    fn is_dead_value(&self, v: Value) -> bool {
        let ptr = if v.is_string() { v.as_string() }
            else if v.is_table() { v.as_table() }
            else if v.is_closure() { v.as_closure() }
            else if v.is_bigint() { v.as_bigint() }
            else { None };
        match ptr {
            Some(p) => self.objects.get(&(p as usize)).map(|e| e.color == GcColor::White).unwrap_or(false),
            None => false,
        }
    }
}

fn table_weak_mode(t: &crate::vm::Table) -> (bool, bool) {
    use crate::vm::{TableKey, string_ref};
    let mt_ptr = match t.metatable { Some(p) => p, None => return (false, false) };
    let mt = unsafe { &*mt_ptr };
    match mt.hash.get(&TableKey::Str("__mode".to_owned())) {
        Some(&v) if v.is_string() => {
            let s = unsafe { string_ref(v) };
            (s.contains('k'), s.contains('v'))
        }
        _ => (false, false),
    }
}
