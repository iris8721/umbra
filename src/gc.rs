use std::collections::HashMap;
use crate::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcColor { White, Gray, Black }

#[derive(Clone, Copy)]
enum GcKind { Str, Table, Closure }

struct GcEntry { color: GcColor, kind: GcKind }

pub struct Gc {
    objects:     HashMap<usize, GcEntry>,
    gray_list:   Vec<usize>,
    alloc_count: usize,
    pub threshold: usize,
    pub pending_finalizers: Vec<(usize, Value)>,
}

impl Gc {
    pub fn new() -> Self {
        Gc {
            objects:     HashMap::new(),
            gray_list:   Vec::new(),
            alloc_count: 0,
            threshold:   1024,
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

    pub fn should_collect(&self) -> bool { self.alloc_count >= self.threshold }

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
                GcKind::Str => {}
                GcKind::Table   => self.propagate_table(ptr),
                GcKind::Closure => self.propagate_closure(ptr),
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
                GcKind::Str => unsafe { drop(Box::from_raw(ptr as *mut String)) },
                GcKind::Table => {
                    use crate::vm::Table;
                    unsafe { drop(Box::from_raw(ptr as *mut Table)) }
                }
                GcKind::Closure => {
                    use crate::vm::LuaClosure;
                    unsafe { drop(Box::from_raw(ptr as *mut LuaClosure)) }
                }
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
        use crate::vm::Table;
        let values: Vec<Value> = unsafe {
            let t = &*(ptr as *const Table);
            let mut vs: Vec<Value> = t.array.iter().copied().collect();
            vs.extend(t.hash.values().copied());
            if let Some(mt) = t.metatable {
                vs.push(Value::table(mt as *mut u8));
            }
            vs
        };
        for v in values { self.mark_value(v); }
    }
}
