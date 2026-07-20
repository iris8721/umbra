use std::cell::Cell;
use std::collections::HashMap;
use crate::chunk::{Const, Op, Proto, ia, ib, ic, ibx, isbx, iop, is_rk, rk_idx};
use crate::gc::Gc;

thread_local! {
    static PRINT_HOOK: std::cell::RefCell<Option<Box<dyn Fn(String)>>> = std::cell::RefCell::new(None);
    static CURRENT_VM: Cell<*mut Vm> = const { Cell::new(std::ptr::null_mut()) };
    static RNG: Cell<u64> = const { Cell::new(6364136223846793005) };
}


use crate::value::Value;

// Caps any single allocation a script can request (string.rep/format, table
// range ops) — an allocator failure on an oversized request aborts the
// process unconditionally and can't be caught, unlike a normal panic.
pub(crate) const MAX_ALLOC_LEN: usize = 64 * 1024 * 1024;

// bytes[..len] is valid UTF-8; bytes[len] == 0. The trailing NUL makes
// as_c_ptr() safe to hand to a C host expecting a NUL-terminated string
// (a plain Rust String's buffer has no such guarantee).
pub struct RtString {
    pub len: usize,
    bytes: Box<[u8]>,
}

impl RtString {
    pub fn as_c_ptr(&self) -> *const u8 { self.bytes.as_ptr() }
}

fn alloc_string_raw(s: &str) -> *mut u8 {
    let mut bytes = Vec::with_capacity(s.len() + 1);
    bytes.extend_from_slice(s.as_bytes());
    bytes.push(0);
    let rt = RtString { len: s.len(), bytes: bytes.into_boxed_slice() };
    Box::into_raw(Box::new(rt)) as *mut u8
}

// GC-registered but not interned/deduplicated by content like Vm::intern —
// computed/throwaway strings shouldn't pay a cache lookup+clone every call.
fn alloc_string_val(s: &str) -> Value {
    CURRENT_VM.with(|c| {
        let ptr = c.get();
        if ptr.is_null() { return Value::string(alloc_string_raw(s)); }
        let vm = unsafe { &mut *ptr };
        let raw = alloc_string_raw(s);
        vm.gc.register_string(raw);
        Value::string(raw)
    })
}

fn alloc_bigint_raw(n: i64) -> *mut u8 {
    Box::into_raw(Box::new(n)) as *mut u8
}

// For plain cfn closures with no direct &mut Vm; falls back to the truncating
// fast path only if there's truly no VM context, which shouldn't happen
// while a script is running.
fn make_int_via_current_vm(n: i64) -> Value {
    CURRENT_VM.with(|c| {
        let ptr = c.get();
        if ptr.is_null() { Value::int(n) } else { unsafe { &mut *ptr }.make_int(n) }
    })
}

pub(crate) unsafe fn string_ref<'a>(v: Value) -> &'a str {
    let ptr = v.as_string().unwrap() as *mut RtString;
    unsafe {
        let rt = &*ptr;
        std::str::from_utf8_unchecked(&rt.bytes[..rt.len])
    }
}

pub struct Table {
    pub array: Vec<Value>,
    pub hash: HashMap<TableKey, Value>,
    pub metatable: Option<*mut Table>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TableKey {
    Int(i64),
    Str(String),
    Bool(bool),
    Ptr(u64),
}

impl TableKey {
    fn from_value(v: Value) -> Option<Self> {
        if v.is_nil() { return None; }
        if v.is_int_like() { return Some(TableKey::Int(v.as_int().unwrap())); }
        if v.is_float() {
            let f = v.as_float().unwrap();
            let i = f as i64;
            if i as f64 == f { return Some(TableKey::Int(i)); }
            return Some(TableKey::Ptr(f.to_bits()));
        }
        if v.is_bool() { return Some(TableKey::Bool(v.as_bool().unwrap())); }
        if v.is_string() {
            let s = unsafe { string_ref(v) };
            return Some(TableKey::Str(s.to_owned()));
        }
        Some(TableKey::Ptr(v.raw_bits()))
    }
}

impl Table {
    pub fn new() -> Self {
        Table { array: Vec::new(), hash: HashMap::new(), metatable: None }
    }

    pub fn raw_get(&self, key: Value) -> Value {
        if key.is_int() {
            let i = key.as_int().unwrap();
            if i >= 1 && (i as usize) <= self.array.len() {
                return self.array[(i - 1) as usize];
            }
        }
        if let Some(k) = TableKey::from_value(key) {
            return self.hash.get(&k).copied().unwrap_or(Value::nil());
        }
        Value::nil()
    }

    pub fn raw_set(&mut self, key: Value, val: Value) {
        if key.is_int() {
            let i = key.as_int().unwrap();
            if i >= 1 && i <= (self.array.len() + 1) as i64 {
                let idx = (i - 1) as usize;
                if idx == self.array.len() {
                    self.array.push(val);
                } else {
                    self.array[idx] = val;
                }
                return;
            }
        }
        if let Some(k) = TableKey::from_value(key) {
            if val.is_nil() { self.hash.remove(&k); } else { self.hash.insert(k, val); }
        }
    }

    pub fn length(&self) -> i64 {
        if self.array.is_empty() { return 0; }
        let mut lo = 0usize;
        let mut hi = self.array.len();
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if self.array[mid - 1].is_nil() { hi = mid - 1; } else { lo = mid; }
        }
        lo as i64
    }
}

fn alloc_table_raw() -> *mut u8 {
    Box::into_raw(Box::new(Table::new())) as *mut u8
}

unsafe fn table_ref<'a>(v: Value) -> &'a mut Table {
    unsafe { &mut *(v.as_table().unwrap() as *mut Table) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoStatus { Suspended, Running, Dead }

pub struct Coroutine {
    pub regs: Vec<Value>,
    pub frames: Vec<Frame>,
    pub status: CoStatus,
    pub fn_val: Value,
    pub started: bool,
    pub yield_result_base: usize,
    pub yield_nresults: u8,
}

pub struct LuaClosure {
    pub proto: *const Proto,
    pub upvals: Vec<Value>,
}

pub struct Frame {
    pub proto: *const Proto,
    pub pc: usize,
    pub base: usize,
    pub expected_results: u8,
    pub upvals_ptr: *mut Value,
    pub upvals_len: usize,
    pub varargs: Box<[Value]>,
    pub top: usize,
}

pub struct Vm {
    regs: Vec<Value>,
    frames: Vec<Frame>,
    pub globals: Table,
    string_cache: HashMap<String, Value>,
    pub top_level_results: Vec<Value>,
    pub gc: Gc,
    pub coroutines: Vec<*mut Coroutine>,
    pub owned_protos: Vec<Box<crate::chunk::Proto>>,
    pub string_lib: Value,
    // Set after a caught panic; further execution is refused rather than risk
    // UB from continuing on possibly-inconsistent GC/register/frame state.
    pub poisoned: bool,
    // Captured when an error is first enriched (frames still intact), so
    // debug.traceback can report it later even after the stack has unwound.
    pub last_traceback: Option<String>,
    // Host-set instruction budget for bounding a runaway script; 0 = unlimited.
    // Not script-settable — only the embedder (via the C API) controls this.
    pub step_limit: u64,
    pub step_count: u64,
    pub loaded_modules: HashMap<String, Value>,
}

// Function names aren't tracked in Proto, so entries are line-only (innermost
// first) rather than real Lua's "in function 'foo'" — still shows the call
// chain, just not by name.
fn build_traceback(frames: &[Frame]) -> String {
    frames.iter().enumerate().rev().map(|(i, f)| {
        let proto = unsafe { &*f.proto };
        let line = proto.lines.get(f.pc.saturating_sub(1)).copied().unwrap_or(0);
        let where_ = if i == 0 { "in main chunk" } else { "in function" };
        format!("\tline {line}: {where_}")
    }).collect::<Vec<_>>().join("\n")
}

pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() { return (*s).to_owned(); }
    if let Some(s) = payload.downcast_ref::<String>() { return s.clone(); }
    "unknown panic".to_owned()
}

impl Drop for Vm {
    fn drop(&mut self) {
        for &ptr in &self.coroutines {
            unsafe { drop(Box::from_raw(ptr)); }
        }
    }
}

#[derive(Debug)]
pub enum VmError {
    RuntimeError(String),
    StackOverflow,
    Yield(Vec<Value>),
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmError::RuntimeError(s) => write!(f, "{s}"),
            VmError::StackOverflow => write!(f, "stack overflow"),
            VmError::Yield(_) => write!(f, "attempt to yield from outside a coroutine"),
        }
    }
}

pub type VmResult<T> = Result<T, VmError>;

impl Vm {
    pub fn new() -> Self {
        Self::new_with_print(|line| println!("{line}"))
    }

    pub fn new_with_print(hook: impl Fn(String) + 'static) -> Self {
        PRINT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        let mut vm = Vm {
            regs: vec![Value::nil(); 256],
            frames: Vec::with_capacity(64),
            globals: Table::new(),
            string_cache: HashMap::new(),
            top_level_results: Vec::new(),
            gc: Gc::new(),
            coroutines: Vec::new(),
            owned_protos: Vec::new(),
            string_lib: Value::nil(),
            poisoned: false,
            last_traceback: None,
            step_limit: 0,
            step_count: 0,
            loaded_modules: HashMap::new(),
        };
        vm.register_stdlib();
        vm
    }



    fn intern(&mut self, s: &str) -> Value {
        if let Some(&v) = self.string_cache.get(s) { return v; }
        let ptr = alloc_string_raw(s);
        self.gc.register_string(ptr);
        let v = Value::string(ptr);
        self.string_cache.insert(s.to_owned(), v);
        v
    }

    /// The only correct way to box an arbitrary i64: Value::int() alone
    /// truncates anything outside INLINE_INT_MIN..=INLINE_INT_MAX.
    pub fn make_int(&mut self, n: i64) -> Value {
        if (crate::value::INLINE_INT_MIN..=crate::value::INLINE_INT_MAX).contains(&n) {
            return Value::int(n);
        }
        let ptr = alloc_bigint_raw(n);
        self.gc.register_bigint(ptr);
        Value::bigint(ptr)
    }

    // resume() swaps a coroutine's regs into self.regs for its run, so the live
    // state at any moment is split between self.regs and every other coroutine's
    // parked regs — missing the latter frees values a suspended coroutine still holds.
    fn gc_roots(&self) -> Vec<Value> {
        let reg_top = self.frames.iter().map(|f| {
            f.base + unsafe { &*f.proto }.max_regs as usize
        }).max().unwrap_or(0).min(self.regs.len());
        let mut roots: Vec<Value> = self.regs[..reg_top].to_vec();
        roots.extend(self.globals.hash.values().copied());
        for &ptr in &self.coroutines {
            let co = unsafe { &*ptr };
            roots.extend(co.regs.iter().copied());
            roots.push(co.fn_val);
        }
        roots
    }

    pub fn gc_collect(&mut self) {
        let roots = self.gc_roots();
        self.gc.collect(roots.into_iter(), &mut self.string_cache);

        let finalizers = std::mem::take(&mut self.gc.pending_finalizers);
        if !finalizers.is_empty() {
            for (ptr, gc_fn) in finalizers {
                let tbl_val = Value::table(ptr as *mut u8);
                unsafe {
                    if let Some(mt_ptr) = (*(ptr as *const Table)).metatable {
                        (*mt_ptr).hash.remove(&TableKey::Str("__gc".to_owned()));
                    }
                }
                let _ = self.call_value_isolated(gc_fn, &[tbl_val]);
            }
            let roots2 = self.gc_roots();
            self.gc.collect(roots2.into_iter(), &mut self.string_cache);
        }
    }

    // Runs `fn_val` in an isolated frame above the active call stack, so a
    // metamethod invocation mid-instruction can't clobber the caller's
    // registers; the scratch base must be computed before frames are saved,
    // since saving swaps `self.frames` out to empty.
    pub fn call_value_isolated(&mut self, fn_val: Value, args: &[Value]) -> VmResult<Vec<Value>> {
        if self.poisoned {
            return Err(VmError::RuntimeError("VM is poisoned by a previous internal error".into()));
        }
        if let Some(cfn) = get_cfn(fn_val) {
            return match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cfn(args))) {
                Ok(r) => r,
                Err(payload) => {
                    self.poisoned = true;
                    Err(VmError::RuntimeError(format!("internal error (panic): {}", panic_message(&*payload))))
                }
            };
        }
        if let Some(cp) = get_proto_callable(fn_val) {
            let scratch_base = self.frames.iter().map(|f| {
                f.base + unsafe { &*f.proto }.max_regs as usize
            }).max().unwrap_or(0) + 8;

            let saved_frames = std::mem::take(&mut self.frames);
            let proto = unsafe { &*cp.proto };
            let needed = scratch_base + proto.max_regs as usize + 8;
            if needed > self.regs.len() { self.regs.resize(needed + 64, Value::nil()); }
            for (i, &v) in args.iter().enumerate() { self.regs[scratch_base + i] = v; }
            if let Err(e) = self.push_frame(cp.proto, cp.upvals_ptr, cp.upvals_len, scratch_base, args.len() as u8, 255) {
                self.frames = saved_frames;
                return Err(e);
            }
            let run_result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_inner())) {
                Ok(r) => r,
                Err(payload) => {
                    self.poisoned = true;
                    self.frames = saved_frames;
                    return Err(VmError::RuntimeError(format!("internal error (panic): {}", panic_message(&*payload))));
                }
            };
            let results = std::mem::take(&mut self.top_level_results);
            // Enriched before restoring frames: the failing frame (with the line
            // table needed) is only reachable through self.frames up to this point.
            let run_result = run_result.map_err(|e| self.enrich_error_line(e));
            self.frames = saved_frames;
            run_result?;
            Ok(results)
        } else {
            Err(VmError::RuntimeError(format!("attempt to call a {} value", fn_val.type_name())))
        }
    }

    fn get_mm(&mut self, obj: Value, name: &str) -> Value {
        if !obj.is_table() { return Value::nil(); }
        let mt_ptr = unsafe { &*(obj.as_table().unwrap() as *const Table) }.metatable;
        let mt_ptr = match mt_ptr { Some(p) => p, None => return Value::nil() };
        let key = self.intern(name);
        unsafe { (*mt_ptr).raw_get(key) }
    }

    fn get_mm2(&mut self, a: Value, b: Value, name: &str) -> Value {
        let key = self.intern(name);
        if a.is_table() {
            let mt_ptr = unsafe { &*(a.as_table().unwrap() as *const Table) }.metatable;
            if let Some(mt) = mt_ptr {
                let mm = unsafe { (*mt).raw_get(key) };
                if !mm.is_nil() { return mm; }
            }
        }
        if b.is_table() {
            let mt_ptr = unsafe { &*(b.as_table().unwrap() as *const Table) }.metatable;
            if let Some(mt) = mt_ptr {
                let mm = unsafe { (*mt).raw_get(key) };
                if !mm.is_nil() { return mm; }
            }
        }
        Value::nil()
    }

    fn table_index_chain(&mut self, tbl: Value, key: Value) -> VmResult<Value> {
        let mut current = tbl;
        for _ in 0..100 {
            if !current.is_table() { break; }
            let (raw, mt_ptr) = {
                let t = unsafe { &*(current.as_table().unwrap() as *const Table) };
                (t.raw_get(key), t.metatable)
            };
            if !raw.is_nil() { return Ok(raw); }
            let mt_ptr = match mt_ptr { None => return Ok(Value::nil()), Some(p) => p };
            let idx_key = self.intern("__index");
            let mm = unsafe { (*mt_ptr).raw_get(idx_key) };
            if mm.is_nil() { return Ok(Value::nil()); }
            if mm.is_table() {
                current = mm;
            } else {
                let res = self.call_value_isolated(mm, &[current, key])?;
                return Ok(res.into_iter().next().unwrap_or(Value::nil()));
            }
        }
        Err(VmError::RuntimeError("'__index' chain too long; possible loop".into()))
    }

    fn table_newindex_chain(&mut self, tbl: Value, key: Value, val: Value) -> VmResult<()> {
        let mut current = tbl;
        for _ in 0..100 {
            if !current.is_table() { break; }
            let (existing, mt_ptr) = {
                let t = unsafe { &*(current.as_table().unwrap() as *const Table) };
                (t.raw_get(key), t.metatable)
            };
            if !existing.is_nil() {
                unsafe { table_ref(current) }.raw_set(key, val);
                return Ok(());
            }
            let mt_ptr = match mt_ptr {
                None => { unsafe { table_ref(current) }.raw_set(key, val); return Ok(()); }
                Some(p) => p,
            };
            let ni_key = self.intern("__newindex");
            let mm = unsafe { (*mt_ptr).raw_get(ni_key) };
            if mm.is_nil() {
                unsafe { table_ref(current) }.raw_set(key, val);
                return Ok(());
            }
            if mm.is_table() {
                current = mm;
            } else {
                let _ = self.call_value_isolated(mm, &[current, key, val])?;
                return Ok(());
            }
        }
        Err(VmError::RuntimeError("'__newindex' chain too long; possible loop".into()))
    }

    fn concat_two(&mut self, left: Value, right: Value) -> VmResult<Value> {
        if (left.is_string() || left.is_number()) && (right.is_string() || right.is_number()) {
            let ls = coerce_to_concat_str(left);
            let rs = coerce_to_concat_str(right);
            return Ok(self.intern(&(ls + &rs)));
        }
        let mm = self.get_mm2(left, right, "__concat");
        if mm.is_nil() {
            let bad = if !left.is_string() && !left.is_number() { left } else { right };
            return Err(VmError::RuntimeError(format!(
                "attempt to concatenate a {} value", bad.type_name())));
        }
        let res = self.call_value_isolated(mm, &[left, right])?;
        Ok(res.into_iter().next().unwrap_or(Value::nil()))
    }

    fn values_eq_mm(&mut self, a: Value, b: Value) -> VmResult<bool> {
        if a.raw_bits() == b.raw_bits() { return Ok(true); }
        if a.is_table() && b.is_table() {
            let mm = self.get_mm2(a, b, "__eq");
            if !mm.is_nil() {
                let res = self.call_value_isolated(mm, &[a, b])?;
                return Ok(res.into_iter().next().unwrap_or(Value::nil()).is_truthy());
            }
        }
        Ok(values_equal(a, b))
    }

    fn value_lt_mm(&mut self, a: Value, b: Value) -> VmResult<bool> {
        if (a.is_number() && b.is_number()) || (a.is_string() && b.is_string()) {
            return value_lt(a, b);
        }
        let mm = self.get_mm2(a, b, "__lt");
        if mm.is_nil() {
            return Err(VmError::RuntimeError(format!(
                "attempt to compare {} with {}", a.type_name(), b.type_name())));
        }
        let res = self.call_value_isolated(mm, &[a, b])?;
        Ok(res.into_iter().next().unwrap_or(Value::nil()).is_truthy())
    }

    fn value_le_mm(&mut self, a: Value, b: Value) -> VmResult<bool> {
        if (a.is_number() && b.is_number()) || (a.is_string() && b.is_string()) {
            return value_le(a, b);
        }
        let mm_le = self.get_mm2(a, b, "__le");
        if !mm_le.is_nil() {
            let res = self.call_value_isolated(mm_le, &[a, b])?;
            return Ok(res.into_iter().next().unwrap_or(Value::nil()).is_truthy());
        }
        let mm_lt = self.get_mm2(b, a, "__lt");
        if !mm_lt.is_nil() {
            let res = self.call_value_isolated(mm_lt, &[b, a])?;
            return Ok(!res.into_iter().next().unwrap_or(Value::nil()).is_truthy());
        }
        Err(VmError::RuntimeError(format!(
            "attempt to compare {} with {}", a.type_name(), b.type_name())))
    }

    fn resolve_const(&mut self, proto: &Proto, idx: usize) -> Value {
        match &proto.consts[idx] {
            Const::Nil       => Value::nil(),
            Const::Bool(b)   => Value::bool(*b),
            Const::Int(n)    => self.make_int(*n),
            Const::Float(f)  => Value::float(*f),
            Const::Str(s)    => self.intern(s),
        }
    }

    pub fn exec(&mut self, proto: &Proto) -> VmResult<()> {
        self.push_frame(proto, std::ptr::null_mut(), 0, 0, 0, 1)?;
        self.run()
    }

    // Proto must be owned here (not borrowed) so raw closure pointers into it
    // stay valid after the caller's source/AST goes out of scope.
    pub fn exec_owned(&mut self, proto: crate::chunk::Proto) -> VmResult<()> {
        let boxed = Box::new(proto);
        let ptr: *const Proto = &*boxed;
        self.owned_protos.push(boxed);
        self.push_frame(ptr, std::ptr::null_mut(), 0, 0, 0, 1)?;
        self.run()
    }

    // Like exec_owned, but safe to call while other frames are already on the
    // stack (e.g. require() invoked from a running script): exec_owned's own
    // base=0 frame would otherwise collide with the caller's real frames, and
    // Op::Return's base_save - caller_base arithmetic underflows. Saves and
    // restores self.frames the same way call_value_isolated does.
    pub fn exec_owned_isolated(&mut self, proto: crate::chunk::Proto) -> VmResult<Vec<Value>> {
        let saved_frames = std::mem::take(&mut self.frames);
        let boxed = Box::new(proto);
        let ptr: *const Proto = &*boxed;
        self.owned_protos.push(boxed);
        if let Err(e) = self.push_frame(ptr, std::ptr::null_mut(), 0, 0, 0, 1) {
            self.frames = saved_frames;
            return Err(e);
        }
        let run_result = self.run();
        let results = std::mem::take(&mut self.top_level_results);
        self.frames = saved_frames;
        run_result?;
        Ok(results)
    }

    pub fn exec_call(&mut self, proto_ptr: *const Proto, args: &[Value]) -> VmResult<Vec<Value>> {
        let base = 1;
        let needed = base + unsafe { &*proto_ptr }.max_regs as usize + 8;
        if needed > self.regs.len() { self.regs.resize(needed + 64, Value::nil()); }
        for (i, &v) in args.iter().enumerate() { self.regs[base + i] = v; }
        self.push_frame(proto_ptr, std::ptr::null_mut(), 0, base, args.len() as u8, 255)?;
        self.run()?;
        Ok(std::mem::take(&mut self.top_level_results))
    }

    pub fn push_frame(&mut self, proto: *const Proto, upvals_ptr: *mut Value, upvals_len: usize, base: usize, nargs: u8, expected: u8) -> VmResult<()> {
        if self.frames.len() >= 200 { return Err(VmError::StackOverflow); }
        let p = unsafe { &*proto };
        let needed = base + p.max_regs as usize + 8;
        if needed > self.regs.len() {
            self.regs.resize(needed + 64, Value::nil());
        }
        let varargs: Box<[Value]> = if p.is_vararg && nargs as usize > p.params as usize {
            let lo = base + p.params as usize;
            let hi = (base + nargs as usize).min(self.regs.len());
            self.regs[lo..hi].to_vec().into_boxed_slice()
        } else {
            Box::new([])
        };
        self.frames.push(Frame { proto, pc: 0, base, expected_results: expected, upvals_ptr, upvals_len, varargs, top: base });
        Ok(())
    }

    pub fn run(&mut self) -> VmResult<()> {
        if self.poisoned {
            return Err(VmError::RuntimeError("VM is poisoned by a previous internal error".into()));
        }
        CURRENT_VM.with(|c| c.set(self as *mut Vm));
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_inner())) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(self.enrich_error_line(e)),
            Err(payload) => {
                self.poisoned = true;
                Err(VmError::RuntimeError(format!("internal error (panic): {}", panic_message(&*payload))))
            }
        }
    }

    // Idempotent via the prefix check, so re-enrichment at an outer
    // call_value_isolated/run layer, as an error propagates through nested
    // calls, is a no-op rather than stacking "line N: line M: ...".
    fn enrich_error_line(&mut self, e: VmError) -> VmError {
        if let VmError::RuntimeError(msg) = &e {
            if !msg.starts_with("line ") {
                self.last_traceback = Some(build_traceback(&self.frames));
                if let Some(frame) = self.frames.last() {
                    let proto = unsafe { &*frame.proto };
                    let pc = frame.pc.saturating_sub(1);
                    if let Some(&line) = proto.lines.get(pc) {
                        return VmError::RuntimeError(format!("line {line}: {msg}"));
                    }
                }
            }
        }
        e
    }

    pub fn run_inner(&mut self) -> VmResult<()> {
        'outer: loop {
            // Checked here (before `frame` becomes a raw pointer into self.frames)
            // rather than inside the inner dispatch loop, so gc_collect()'s &mut
            // self (which can itself mutate self.frames, running __gc finalizers)
            // never aliases a live &mut Frame. The inner loop below only lands
            // back here — via `continue 'outer` — once a threshold is actually
            // due; most instructions never leave the fast path at all.
            if self.gc.should_collect() { self.gc_collect(); }

            // >= not >: once live_count reaches max_objects with nothing left to
            // collect, a script can never complete another allocation anyway (the
            // fast path keeps bouncing back here before the allocating instruction
            // ever runs) — using > would get stuck retrying forever instead of
            // ever reporting the error.
            if self.gc.max_objects != 0 && self.gc.live_count() >= self.gc.max_objects {
                return Err(VmError::RuntimeError("memory limit exceeded".into()));
            }

            if self.step_limit != 0 && self.step_count > self.step_limit {
                return Err(VmError::RuntimeError("instruction budget exceeded".into()));
            }

            let frame = self.frames.last_mut().unwrap() as *mut Frame;
            let frame = unsafe { &mut *frame };
            let proto = unsafe { &*frame.proto };
            let base = frame.base;

            macro_rules! R {
                ($r:expr) => { self.regs[base + $r] }
            }
            macro_rules! RK {
                ($x:expr) => {
                    if is_rk($x) { self.resolve_const(proto, rk_idx($x)) }
                    else { self.regs[base + $x] }
                }
            }
            macro_rules! arith_op {
                ($dst:expr, $b:expr, $c:expr, $int_op:expr, $float_op:expr, $mm_name:expr) => {{
                    let bv = RK!($b);
                    let cv = RK!($c);
                    if bv.is_number() && cv.is_number() {
                        let res = if bv.is_int_like() && cv.is_int_like() {
                            self.make_int($int_op(bv.as_int().unwrap(), cv.as_int().unwrap()))
                        } else {
                            Value::float($float_op(bv.to_float().unwrap(), cv.to_float().unwrap()))
                        };
                        R!($dst) = res;
                    } else {
                        let mm = self.get_mm2(bv, cv, $mm_name);
                        if mm.is_nil() {
                            let bad = if !bv.is_number() { bv } else { cv };
                            return Err(VmError::RuntimeError(format!(
                                "attempt to perform arithmetic on a {} value", bad.type_name())));
                        }
                        let res = self.call_value_isolated(mm, &[bv, cv])?;
                        self.regs[base + $dst] = res.into_iter().next().unwrap_or(Value::nil());
                        continue 'outer;
                    }
                }}
            }

            loop {
                if frame.pc >= proto.code.len() { break; }

                // Cheap per-instruction counting only; the actual gc_collect()/
                // budget-error handling happens back at 'outer's top, where it's
                // safe to mutate self.frames (frame/proto aren't held past this point).
                self.step_count += 1;
                if self.gc.should_collect() || (self.step_limit != 0 && self.step_count > self.step_limit) {
                    continue 'outer;
                }

                let instr = proto.code[frame.pc];
                frame.pc += 1;
                let a  = ia(instr);
                let b  = ib(instr);
                let c  = ic(instr);
                let bx = ibx(instr);
                let sbx= isbx(instr);

                match Op::from_u8(iop(instr)).ok_or_else(|| VmError::RuntimeError("illegal opcode".into()))? {
                    Op::LoadNil  => R!(a) = Value::nil(),
                    Op::LoadBool => {
                        R!(a) = Value::bool(b != 0);
                        if c != 0 { frame.pc += 1; }
                    }
                    Op::LoadInt  => R!(a) = Value::int(sbx as i64),
                    Op::LoadK    => {
                        let v = self.resolve_const(proto, bx);
                        self.regs[base + a] = v;
                    }
                    Op::Move     => R!(a) = R!(b),

                    Op::Add  => arith_op!(a, b, c, |x: i64, y: i64| x.wrapping_add(y), |x: f64, y: f64| x + y, "__add"),
                    Op::Sub  => arith_op!(a, b, c, |x: i64, y: i64| x.wrapping_sub(y), |x: f64, y: f64| x - y, "__sub"),
                    Op::Mul  => arith_op!(a, b, c, |x: i64, y: i64| x.wrapping_mul(y), |x: f64, y: f64| x * y, "__mul"),
                    Op::Div  => {
                        let bv = RK!(b); let cv = RK!(c);
                        if bv.is_number() && cv.is_number() {
                            R!(a) = Value::float(bv.to_float().unwrap() / cv.to_float().unwrap());
                        } else {
                            let mm = self.get_mm2(bv, cv, "__div");
                            if mm.is_nil() {
                                let bad = if !bv.is_number() { bv } else { cv };
                                return Err(VmError::RuntimeError(format!(
                                    "attempt to perform arithmetic on a {} value", bad.type_name())));
                            }
                            let res = self.call_value_isolated(mm, &[bv, cv])?;
                            self.regs[base + a] = res.into_iter().next().unwrap_or(Value::nil());
                            continue 'outer;
                        }
                    }
                    Op::IDiv => arith_op!(a, b, c,
                        |x: i64, y: i64| if y == 0 { 0 } else { x.wrapping_div_euclid(y) },
                        |x: f64, y: f64| (x / y).floor(), "__idiv"),
                    Op::Mod  => arith_op!(a, b, c,
                        |x: i64, y: i64| if y == 0 { 0 } else { x.wrapping_rem_euclid(y) },
                        |x: f64, y: f64| x - (x / y).floor() * y, "__mod"),
                    Op::Pow  => {
                        let bv = RK!(b); let cv = RK!(c);
                        if bv.is_number() && cv.is_number() {
                            R!(a) = Value::float(bv.to_float().unwrap().powf(cv.to_float().unwrap()));
                        } else {
                            let mm = self.get_mm2(bv, cv, "__pow");
                            if mm.is_nil() {
                                let bad = if !bv.is_number() { bv } else { cv };
                                return Err(VmError::RuntimeError(format!(
                                    "attempt to perform arithmetic on a {} value", bad.type_name())));
                            }
                            let res = self.call_value_isolated(mm, &[bv, cv])?;
                            self.regs[base + a] = res.into_iter().next().unwrap_or(Value::nil());
                            continue 'outer;
                        }
                    }
                    Op::Unm => {
                        let v = R!(b);
                        if v.is_int_like() {
                            let n = self.make_int(v.as_int().unwrap().wrapping_neg());
                            R!(a) = n;
                        } else if v.is_float() {
                            R!(a) = Value::float(-v.as_float().unwrap());
                        } else {
                            let mm = self.get_mm(v, "__unm");
                            if mm.is_nil() {
                                return Err(VmError::RuntimeError(format!(
                                    "attempt to perform arithmetic on a {} value", v.type_name())));
                            }
                            let res = self.call_value_isolated(mm, &[v, v])?;
                            self.regs[base + a] = res.into_iter().next().unwrap_or(Value::nil());
                            continue 'outer;
                        }
                    }
                    Op::BAnd => {
                        let bv = int_val(RK!(b))?; let cv = int_val(RK!(c))?;
                        let res = self.make_int(bv & cv);
                        R!(a) = res;
                    }
                    Op::BOr  => {
                        let bv = int_val(RK!(b))?; let cv = int_val(RK!(c))?;
                        let res = self.make_int(bv | cv);
                        R!(a) = res;
                    }
                    Op::BXor => {
                        let bv = int_val(RK!(b))?; let cv = int_val(RK!(c))?;
                        let res = self.make_int(bv ^ cv);
                        R!(a) = res;
                    }
                    Op::Shl  => {
                        let bv = int_val(RK!(b))?; let cv = int_val(RK!(c))?;
                        let res = self.make_int(bv.wrapping_shl(cv as u32));
                        R!(a) = res;
                    }
                    Op::Shr  => {
                        let bv = int_val(RK!(b))?; let cv = int_val(RK!(c))?;
                        let res = self.make_int(((bv as u64).wrapping_shr(cv as u32)) as i64);
                        R!(a) = res;
                    }
                    Op::BNot => {
                        let v = int_val(R!(b))?;
                        let res = self.make_int(!v);
                        R!(a) = res;
                    }
                    Op::Not  => R!(a) = Value::bool(!R!(b).is_truthy()),
                    Op::Len  => {
                        let v = R!(b);
                        if v.is_string() {
                            let s = unsafe { string_ref(v) };
                            R!(a) = Value::int(s.len() as i64);
                        } else if v.is_table() {
                            let (len, mt_ptr) = {
                                let t = unsafe { &*(v.as_table().unwrap() as *const Table) };
                                (t.length(), t.metatable)
                            };
                            if let Some(mt) = mt_ptr {
                                let key = self.intern("__len");
                                let mm = unsafe { (*mt).raw_get(key) };
                                if !mm.is_nil() {
                                    let res = self.call_value_isolated(mm, &[v])?;
                                    self.regs[base + a] = res.into_iter().next().unwrap_or(Value::nil());
                                    continue 'outer;
                                }
                            }
                            self.regs[base + a] = Value::int(len);
                        } else {
                            return Err(VmError::RuntimeError(format!("attempt to get length of a {} value", v.type_name())));
                        }
                    }
                    Op::Concat => {
                        let vals: Vec<Value> = (b..=c).map(|i| self.regs[base + i]).collect();
                        let mut acc = *vals.last().unwrap();
                        for &left in vals[..vals.len() - 1].iter().rev() {
                            acc = self.concat_two(left, acc)?;
                        }
                        self.regs[base + a] = acc;
                        continue 'outer;
                    }

                    Op::Eq => {
                        let bv = RK!(b); let cv = RK!(c);
                        let eq = self.values_eq_mm(bv, cv)?;
                        if eq != (a != 0) { frame.pc += 1; }
                        continue 'outer;
                    }
                    Op::Lt => {
                        let bv = RK!(b); let cv = RK!(c);
                        let lt = self.value_lt_mm(bv, cv)?;
                        if lt != (a != 0) { frame.pc += 1; }
                        continue 'outer;
                    }
                    Op::Le => {
                        let bv = RK!(b); let cv = RK!(c);
                        let le = self.value_le_mm(bv, cv)?;
                        if le != (a != 0) { frame.pc += 1; }
                        continue 'outer;
                    }

                    Op::Test => {
                        if R!(a).is_truthy() != (c != 0) { frame.pc += 1; }
                    }
                    Op::TestSet => {
                        let bv = R!(b);
                        if bv.is_truthy() == (c != 0) {
                            R!(a) = bv;
                        } else {
                            frame.pc += 1;
                        }
                    }
                    Op::Jmp => {
                        frame.pc = (frame.pc as i32 + sbx) as usize;
                    }

                    Op::NewTable => {
                        let ptr = alloc_table_raw();
                        self.gc.register_table(ptr);
                        self.regs[base + a] = Value::table(ptr);
                    }
                    Op::GetTable => {
                        let tv = R!(b);
                        let kv = RK!(c);
                        if tv.is_string() {
                            let result = if self.string_lib.is_table() {
                                unsafe { &*(self.string_lib.as_table().unwrap() as *const Table) }.raw_get(kv)
                            } else {
                                Value::nil()
                            };
                            self.regs[base + a] = result;
                            continue 'outer;
                        }
                        if !tv.is_table() {
                            return Err(VmError::RuntimeError(format!("attempt to index a {} value", tv.type_name())));
                        }
                        let result = self.table_index_chain(tv, kv)?;
                        self.regs[base + a] = result;
                        continue 'outer;
                    }
                    Op::SetTable => {
                        let tv = R!(a);
                        let kv = RK!(b);
                        let vv = RK!(c);
                        if !tv.is_table() {
                            return Err(VmError::RuntimeError(format!("attempt to index a {} value", tv.type_name())));
                        }
                        self.table_newindex_chain(tv, kv, vv)?;
                        continue 'outer;
                    }
                    Op::SetList => {
                        let tv = R!(a);
                        let t = unsafe { table_ref(tv) };
                        let offset = (c as usize - 1) * 50;
                        for i in 1..=b {
                            let v = R!(a + i);
                            t.raw_set(Value::int((offset + i) as i64), v);
                        }
                    }

                    Op::GetGlobal => {
                        let k = self.resolve_const(proto, bx);
                        let name = if k.is_string() { unsafe { string_ref(k) }.to_owned() }
                                   else { return Err(VmError::RuntimeError("invalid global name".into())); };
                        let key = self.intern(&name);
                        let v = self.globals.raw_get(key);
                        self.regs[base + a] = v;
                    }
                    Op::SetGlobal => {
                        let k = self.resolve_const(proto, bx);
                        let name = if k.is_string() { unsafe { string_ref(k) }.to_owned() }
                                   else { return Err(VmError::RuntimeError("invalid global name".into())); };
                        let key = self.intern(&name);
                        let v = self.regs[base + a];
                        self.globals.raw_set(key, v);
                    }

                    Op::Call => {
                        let fn_val = R!(a);
                        let nargs = if b == 0 { frame.top.saturating_sub(base + a + 1) as u8 } else { (b - 1) as u8 };
                        let nresults = if c == 0 { 255 } else { (c - 1) as u8 };

                        if let Some(cfn) = get_cfn(fn_val) {
                            let args_base = base + a + 1;
                            let args: Vec<Value> = (0..nargs as usize)
                                .map(|i| self.regs[args_base + i])
                                .collect();
                            let results = cfn(&args)?;
                            let nr = results.len();
                            for (i, v) in results.into_iter().enumerate() {
                                self.regs[base + a + i] = v;
                            }
                            if nresults != 255 {
                                for i in nr..nresults as usize {
                                    self.regs[base + a + i] = Value::nil();
                                }
                            }
                            // `continue 'outer` re-fetches `frame`/`proto` from self.frames;
                            // a CFunction may have swapped frames (e.g. coroutine.resume),
                            // so the old references must not be touched past this point.
                            continue 'outer;
                        } else if fn_val.is_table() {
                            let mm = self.get_mm(fn_val, "__call");
                            if mm.is_nil() {
                                return Err(VmError::RuntimeError("attempt to call a table value".into()));
                            }
                            let args_base = base + a + 1;
                            let mut mm_args = Vec::with_capacity(nargs as usize + 1);
                            mm_args.push(fn_val);
                            for i in 0..nargs as usize { mm_args.push(self.regs[args_base + i]); }
                            let results = self.call_value_isolated(mm, &mm_args)?;
                            let nr = results.len();
                            for (i, v) in results.into_iter().enumerate() {
                                self.regs[base + a + i] = v;
                            }
                            if nresults != 255 {
                                for i in nr..nresults as usize {
                                    self.regs[base + a + i] = Value::nil();
                                }
                            }
                            continue 'outer;
                        } else {
                            let cp = get_proto_callable(fn_val)
                                .ok_or_else(|| VmError::RuntimeError(format!("attempt to call a {}", fn_val.type_name())))?;
                            let new_base = base + a + 1;
                            let needed_regs = new_base + unsafe { &*cp.proto }.max_regs as usize + 8;
                            if needed_regs > self.regs.len() {
                                self.regs.resize(needed_regs, Value::nil());
                            }
                            self.push_frame(cp.proto, cp.upvals_ptr, cp.upvals_len, new_base, nargs, nresults)?;
                            continue 'outer;
                        }
                    }

                    Op::TailCall => {
                        let fn_val = R!(a);
                        let nargs = if b == 0 { frame.top.saturating_sub(base + a + 1) as u8 } else { (b - 1) as u8 };
                        if let Some(cfn) = get_cfn(fn_val) {
                            let args_base = base + a + 1;
                            let args: Vec<Value> = (0..nargs as usize)
                                .map(|i| self.regs[args_base + i])
                                .collect();
                            let results = cfn(&args)?;
                            let nr = results.len();
                            for (i, v) in results.into_iter().enumerate() {
                                self.regs[base + i] = v;
                            }
                            self.frames.pop();
                            if self.frames.is_empty() {
                                self.top_level_results = self.regs[base..base + nr].to_vec();
                                return Ok(());
                            }
                            continue 'outer;
                        } else {
                            let cp = get_proto_callable(fn_val)
                                .ok_or_else(|| VmError::RuntimeError(format!("attempt to call a {}", fn_val.type_name())))?;
                            let new_base = base + a + 1;
                            let needed = new_base + unsafe { &*cp.proto }.max_regs as usize + 8;
                            if needed > self.regs.len() { self.regs.resize(needed, Value::nil()); }
                            let cur = self.frames.last_mut().unwrap();
                            let expected = cur.expected_results;
                            *cur = Frame { proto: cp.proto, pc: 0, base: new_base, expected_results: expected, upvals_ptr: cp.upvals_ptr, upvals_len: cp.upvals_len, varargs: Box::new([]), top: new_base };
                            continue 'outer;
                        }
                    }

                    Op::Return => {
                        let nv = if b == 0 { frame.top.saturating_sub(base + a) } else { b - 1 };
                        let results: Vec<Value> = (0..nv).map(|i| self.regs[base + a + i]).collect();
                        let expected = frame.expected_results;
                        let base_save = base;
                        self.frames.pop();
                        if self.frames.is_empty() {
                            self.top_level_results = results;
                            return Ok(());
                        }
                        let caller = self.frames.last().unwrap();
                        let caller_base = caller.base;
                        let call_a = base_save - caller_base - 1;
                        let nr = results.len();
                        let fill = if expected == 255 { nr } else { expected as usize };
                        for i in 0..fill {
                            self.regs[caller_base + call_a + i] =
                                if i < nr { results[i] } else { Value::nil() };
                        }
                        continue 'outer;
                    }

                    Op::ForPrep => {
                        let init = to_number(R!(a))?;
                        let step = to_number(R!(a + 2))?;
                        R!(a) = num_sub(init, step);
                        frame.pc = (frame.pc as i32 + sbx) as usize;
                    }
                    Op::ForLoop => {
                        let idx  = to_number(R!(a))?;
                        let lim  = to_number(R!(a + 1))?;
                        let step = to_number(R!(a + 2))?;
                        let new_idx_v = num_add(idx, step);
                        R!(a) = new_idx_v;
                        let new_idx = to_number(new_idx_v)?;
                        let in_range = if num_is_positive(step) {
                            num_le(new_idx, lim)
                        } else {
                            num_le(lim, new_idx)
                        };
                        if in_range {
                            R!(a + 3) = new_idx_v;
                            frame.pc = (frame.pc as i32 + sbx) as usize;
                        }
                    }

                    Op::TForCall => {
                        let fn_val = R!(a);
                        let state  = R!(a + 1);
                        let ctrl   = R!(a + 2);
                        let nresults = c;
                        if let Some(cfn) = get_cfn(fn_val) {
                            let results = cfn(&[state, ctrl])?;
                            for i in 0..nresults {
                                self.regs[base + a + 4 + i] =
                                    if i < results.len() { results[i] } else { Value::nil() };
                            }
                        } else {
                            let cp = get_proto_callable(fn_val)
                                .ok_or_else(|| VmError::RuntimeError("attempt to call non-function in for-in".into()))?;
                            let new_base = base + a + 1;
                            let needed = new_base + unsafe { &*cp.proto }.max_regs as usize + 8;
                            if needed > self.regs.len() { self.regs.resize(needed, Value::nil()); }
                            self.regs[new_base] = state;
                            self.regs[new_base + 1] = ctrl;
                            self.push_frame(cp.proto, cp.upvals_ptr, cp.upvals_len, new_base, 2, nresults as u8)?;
                            continue 'outer;
                        }
                    }
                    Op::TForLoop => {
                        if !R!(a + 4).is_nil() {
                            R!(a + 2) = R!(a + 4);
                            frame.pc = (frame.pc as i32 + sbx) as usize;
                        }
                    }

                    Op::Closure => {
                        let inner_proto = &proto.protos[bx];
                        let closure_val = if inner_proto.upvals.is_empty() {
                            Value::userdata(inner_proto as *const Proto as *mut u8)
                        } else {
                            let upvals: Vec<Value> = inner_proto.upvals.iter().map(|desc| {
                                if desc.in_stack {
                                    self.regs[base + desc.idx as usize]
                                } else if (desc.idx as usize) < frame.upvals_len {
                                    unsafe { *frame.upvals_ptr.add(desc.idx as usize) }
                                } else {
                                    Value::nil()
                                }
                            }).collect();
                            let lc = Box::new(LuaClosure {
                                proto: inner_proto as *const Proto,
                                upvals,
                            });
                            let ptr = Box::into_raw(lc) as *mut u8;
                            self.gc.register_closure(ptr);
                            Value::closure(ptr)
                        };
                        self.regs[base + a] = closure_val;
                    }

                    Op::GetUpval => {
                        let v = if b < frame.upvals_len {
                            unsafe { *frame.upvals_ptr.add(b) }
                        } else {
                            Value::nil()
                        };
                        R!(a) = v;
                    }
                    Op::SetUpval => {
                        if b < frame.upvals_len {
                            unsafe { *frame.upvals_ptr.add(b) = R!(a); }
                        }
                    }

                    Op::Vararg => {
                        let n = if b == 0 { frame.varargs.len() } else { b - 1 };
                        for i in 0..n {
                            R!(a + i) = frame.varargs.get(i).copied().unwrap_or(Value::nil());
                        }
                        if b == 0 { frame.top = base + a + n; }
                    }

                    Op::Yield => {
                        let nv = if b == 0 { 0 } else { b - 1 };
                        let values: Vec<Value> = (0..nv).map(|i| self.regs[base + a + i]).collect();
                        return Err(VmError::Yield(values));
                    }
                }
            }

            let base_save = base;
            self.frames.pop();
            if self.frames.is_empty() { self.top_level_results = vec![]; return Ok(()); }
            let caller = self.frames.last().unwrap();
            let call_a = base_save - caller.base - 1;
            let expected = caller.expected_results;
            let fill = if expected == 255 { 0 } else { expected as usize };
            let cb = caller.base;
            for i in 0..fill {
                self.regs[cb + call_a + i] = Value::nil();
            }
        }
    }

    fn register_stdlib(&mut self) {
        self.set_global_cfn("print", |args| {
            let parts: Vec<String> = args.iter().map(|&v| {
                if v.is_string() { return unsafe { string_ref(v) }.to_owned(); }
                if v.is_table() {
                    let mm_str: Option<String> = CURRENT_VM.with(|c| {
                        let vm_ptr = c.get();
                        if vm_ptr.is_null() { return None; }
                        let vm = unsafe { &mut *vm_ptr };
                        let mm = vm.get_mm(v, "__tostring");
                        if mm.is_nil() { return None; }
                        if let Ok(res) = vm.call_value_isolated(mm, &[v]) {
                            let sv = res.into_iter().next().unwrap_or(Value::nil());
                            return Some(if sv.is_string() {
                                unsafe { string_ref(sv) }.to_owned()
                            } else {
                                format!("{sv}")
                            });
                        }
                        None
                    });
                    if let Some(s) = mm_str { return s; }
                }
                format!("{v}")
            }).collect();
            let line = parts.join("\t");
            PRINT_HOOK.with(move |h| {
                match h.borrow().as_ref() {
                    Some(f) => (f)(line),
                    None => println!("{line}"),
                }
            });
            Ok(vec![])
        });

        self.set_global_cfn("tostring", |args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_string() { return Ok(vec![v]); }
            if v.is_table() {
                let mm_res: Option<VmResult<Vec<Value>>> = CURRENT_VM.with(|c| {
                    let vm_ptr = c.get();
                    if vm_ptr.is_null() { return None; }
                    let vm = unsafe { &mut *vm_ptr };
                    let mm = vm.get_mm(v, "__tostring");
                    if mm.is_nil() { return None; }
                    Some(vm.call_value_isolated(mm, &[v]).map(|r| {
                        let sv = r.into_iter().next().unwrap_or(Value::nil());
                        if sv.is_string() { vec![sv] } else { vec![alloc_string_val(&format!("{sv}"))] }
                    }))
                });
                if let Some(res) = mm_res { return res; }
            }
            Ok(vec![alloc_string_val(&format!("{v}"))])
        });

        self.set_global_cfn("tonumber", |args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() || v.is_float() { return Ok(vec![v]); }
            if v.is_string() {
                let s = unsafe { string_ref(v) };
                if let Ok(n) = s.trim().parse::<i64>() {
                    return Ok(vec![make_int_via_current_vm(n)]);
                }
                if let Ok(f) = s.trim().parse::<f64>() { return Ok(vec![Value::float(f)]); }
            }
            Ok(vec![Value::nil()])
        });

        self.set_global_cfn("type", |args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            Ok(vec![alloc_string_val(v.type_name())])
        });

        self.set_global_cfn("assert", |args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if !v.is_truthy() {
                let msg = args.get(1).copied().unwrap_or(Value::nil());
                let s = if msg.is_string() { unsafe { string_ref(msg) }.to_owned() }
                        else { "assertion failed!".into() };
                return Err(VmError::RuntimeError(s));
            }
            Ok(args.to_vec())
        });

        self.set_global_cfn("error", |args| {
            let msg = args.first().copied().unwrap_or(Value::nil());
            let level = args.get(1).map(|v| v.as_int().unwrap_or(1)).unwrap_or(1);
            let s = if msg.is_string() { unsafe { string_ref(msg) }.to_owned() }
                    else { format!("{msg}") };
            // level 1 (default) is handled by the generic enrich_error_line pass,
            // which already prefixes using the innermost frame. level >= 2 means
            // attribute the error to an outer caller's frame instead, so prepend
            // the line here — enrich_error_line skips messages that already start
            // with "line " and leaves this alone.
            if level >= 2 {
                let prefixed = CURRENT_VM.with(|c| {
                    let vm_ptr = c.get();
                    if vm_ptr.is_null() { return s.clone(); }
                    let vm = unsafe { &*vm_ptr };
                    match vm.frames.len().checked_sub(level as usize).and_then(|i| vm.frames.get(i)) {
                        Some(frame) => {
                            let proto = unsafe { &*frame.proto };
                            let line = proto.lines.get(frame.pc.saturating_sub(1)).copied().unwrap_or(0);
                            format!("line {line}: {s}")
                        }
                        None => s.clone(),
                    }
                });
                return Err(VmError::RuntimeError(prefixed));
            }
            Err(VmError::RuntimeError(s))
        });

        // Test-only hook to exercise the panic-catching path in call_value_isolated
        // without needing a real bug; never registered in a non-test build.
        #[cfg(test)]
        self.set_global_cfn("__debug_panic", |_args| {
            panic!("__debug_panic: deliberate test panic");
        });

        self.set_global_cfn("pcall", |args| {
            let fn_val = args.first().copied().unwrap_or(Value::nil());
            let call_args = if args.len() > 1 { args[1..].to_vec() } else { vec![] };
            CURRENT_VM.with(|c| {
                let vm_ptr = c.get();
                if vm_ptr.is_null() {
                    return Ok(vec![Value::bool(false), alloc_string_val("no VM context")]);
                }
                let vm = unsafe { &mut *vm_ptr };
                match vm.call_value_isolated(fn_val, &call_args) {
                    Ok(results) => {
                        let mut ret = vec![Value::bool(true)];
                        ret.extend(results);
                        Ok(ret)
                    }
                    Err(e) => {
                        let msg = vm.intern_pub(&e.to_string());
                        Ok(vec![Value::bool(false), msg])
                    }
                }
            })
        });

        {
            let iter_val = self.make_cfn_val(|args| {
                let tbl = args.first().copied().unwrap_or(Value::nil());
                let idx = args.get(1).copied().unwrap_or(Value::nil());
                if !tbl.is_table() { return Ok(vec![Value::nil()]); }
                let i = idx.as_int()
                    .or_else(|| idx.as_float().map(|f| f as i64))
                    .unwrap_or(0);
                let next_i = i + 1;
                let v = unsafe { (*(tbl.as_table().unwrap() as *const Table)).raw_get(Value::int(next_i)) };
                if v.is_nil() { Ok(vec![Value::nil()]) }
                else { Ok(vec![Value::int(next_i), v]) }
            });
            self.set_global_cfn("ipairs", move |args| {
                let t = args.first().copied().unwrap_or(Value::nil());
                if !t.is_table() {
                    return Err(VmError::RuntimeError("ipairs: expected table".into()));
                }
                Ok(vec![iter_val, t, Value::int(0)])
            });
        }

        {
            let next_val = self.make_cfn_val(|args| {
                let tbl = args.first().copied().unwrap_or(Value::nil());
                let key = args.get(1).copied().unwrap_or(Value::nil());
                if !tbl.is_table() {
                    return Err(VmError::RuntimeError("next: expected table".into()));
                }
                let t = unsafe { &*(tbl.as_table().unwrap() as *const Table) };
                let mut found = key.is_nil();
                for (i, &v) in t.array.iter().enumerate() {
                    if v.is_nil() { continue; }
                    let k = Value::int((i + 1) as i64);
                    if found { return Ok(vec![k, v]); }
                    if values_equal(k, key) { found = true; }
                }
                let vm_ptr = CURRENT_VM.with(|c| c.get());
                for (tk, &v) in &t.hash {
                    let k = match tk {
                        TableKey::Int(n)  => Value::int(*n),
                        TableKey::Bool(b) => Value::bool(*b),
                        TableKey::Str(s)  => if vm_ptr.is_null() { alloc_string_val(s) }
                                             else { unsafe { (*vm_ptr).intern_pub(s) } },
                        TableKey::Ptr(p)  => Value::from_raw(*p),
                    };
                    if found { return Ok(vec![k, v]); }
                    if values_equal(k, key) { found = true; }
                }
                Ok(vec![Value::nil()])
            });
            let k_next = self.intern("next");
            self.globals.raw_set(k_next, next_val);
            self.set_global_cfn("pairs", move |args| {
                let t = args.first().copied().unwrap_or(Value::nil());
                if !t.is_table() {
                    return Err(VmError::RuntimeError("pairs: expected table".into()));
                }
                Ok(vec![next_val, t, Value::nil()])
            });
        }

        self.set_global_cfn("unpack", |args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() { return Ok(vec![]); }
            let table = unsafe { table_ref(t) };
            let n = table.length();
            let result: Vec<Value> = (1..=n).map(|i| table.raw_get(Value::int(i))).collect();
            Ok(result)
        });

        self.set_global_cfn("select", |args| {
            let sel = args.first().copied().unwrap_or(Value::nil());
            if sel.is_string() && unsafe { string_ref(sel) } == "#" {
                return Ok(vec![Value::int(args.len() as i64 - 1)]);
            }
            if let Some(n) = sel.as_int() {
                let idx = n as usize;
                if idx >= 1 && idx < args.len() {
                    return Ok(args[idx..].to_vec());
                }
            }
            Ok(vec![])
        });


        self.set_global_cfn("setmetatable", |args| {
            let tbl = args.first().copied().unwrap_or(Value::nil());
            let mt  = args.get(1).copied().unwrap_or(Value::nil());
            if !tbl.is_table() {
                return Err(VmError::RuntimeError("setmetatable: first arg must be a table".into()));
            }
            if !mt.is_table() && !mt.is_nil() {
                return Err(VmError::RuntimeError("setmetatable: second arg must be a table or nil".into()));
            }
            let t = unsafe { table_ref(tbl) };
            t.metatable = if mt.is_table() {
                Some(mt.as_table().unwrap() as *mut Table)
            } else {
                None
            };
            Ok(vec![tbl])
        });

        self.set_global_cfn("getmetatable", |args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if !v.is_table() { return Ok(vec![Value::nil()]); }
            let t = unsafe { table_ref(v) };
            let mt_ptr = match t.metatable { None => return Ok(vec![Value::nil()]), Some(p) => p };
            let mt = unsafe { &*mt_ptr };
            if let Some(&guard) = mt.hash.get(&TableKey::Str("__metatable".to_owned())) {
                return Ok(vec![guard]);
            }
            Ok(vec![Value::table(mt_ptr as *mut u8)])
        });

        self.set_global_cfn("rawget", |args| {
            let tbl = args.first().copied().unwrap_or(Value::nil());
            let key = args.get(1).copied().unwrap_or(Value::nil());
            if !tbl.is_table() {
                return Err(VmError::RuntimeError("rawget: expected table".into()));
            }
            Ok(vec![unsafe { table_ref(tbl) }.raw_get(key)])
        });

        self.set_global_cfn("rawset", |args| {
            let tbl = args.first().copied().unwrap_or(Value::nil());
            let key = args.get(1).copied().unwrap_or(Value::nil());
            let val = args.get(2).copied().unwrap_or(Value::nil());
            if !tbl.is_table() {
                return Err(VmError::RuntimeError("rawset: expected table".into()));
            }
            unsafe { table_ref(tbl) }.raw_set(key, val);
            Ok(vec![tbl])
        });

        self.set_global_cfn("rawequal", |args| {
            let a = args.first().copied().unwrap_or(Value::nil());
            let b = args.get(1).copied().unwrap_or(Value::nil());
            Ok(vec![Value::bool(a.raw_bits() == b.raw_bits())])
        });

        self.set_global_cfn("yield", |args| {
            Err(VmError::Yield(args.to_vec()))
        });

        let co_table_ptr = alloc_table_raw();
        self.gc.register_table(co_table_ptr);

        let create_val = self.make_cfn_val(|args| {
            let fn_val = args.first().copied().unwrap_or(Value::nil());
            if get_closure(fn_val).is_none() {
                return Err(VmError::RuntimeError("coroutine.create: expected function".into()));
            }
            let co = Box::new(Coroutine {
                regs: vec![Value::nil(); 256],
                frames: Vec::with_capacity(8),
                status: CoStatus::Suspended,
                fn_val,
                started: false,
                yield_result_base: 0,
                yield_nresults: 0,
            });
            let ptr = Box::into_raw(co);
            CURRENT_VM.with(|c| {
                let vm_ptr = c.get();
                if !vm_ptr.is_null() {
                    unsafe { (*vm_ptr).coroutines.push(ptr); }
                }
            });
            Ok(vec![Value::coroutine(ptr as *mut u8)])
        });

        let resume_val = self.make_cfn_val(|args| {
            let co_val = args.first().copied().unwrap_or(Value::nil());
            if !co_val.is_coroutine() {
                return Err(VmError::RuntimeError("coroutine.resume: expected coroutine".into()));
            }
            let resume_args: Vec<Value> = args[1..].to_vec();

            CURRENT_VM.with(|c| {
                let vm_ptr = c.get();
                if vm_ptr.is_null() {
                    return Err(VmError::RuntimeError("no VM context".into()));
                }
                let vm = unsafe { &mut *vm_ptr };
                let co = unsafe { &mut *(co_val.as_coroutine().unwrap() as *mut Coroutine) };

                match co.status {
                    CoStatus::Dead => {
                        let msg = vm.intern_pub("cannot resume dead coroutine");
                        return Ok(vec![Value::bool(false), msg]);
                    }
                    CoStatus::Running => {
                        return Err(VmError::RuntimeError("cannot resume non-suspended coroutine".into()));
                    }
                    CoStatus::Suspended => {}
                }

                co.status = CoStatus::Running;
                std::mem::swap(&mut vm.regs, &mut co.regs);
                std::mem::swap(&mut vm.frames, &mut co.frames);

                if !co.started {
                    co.started = true;
                    let fn_val = co.fn_val;
                    if let Some(cp) = get_proto_callable(fn_val) {
                        let base = 1usize;
                        let proto = unsafe { &*cp.proto };
                        let needed = base + proto.max_regs as usize + 8;
                        if vm.regs.len() < needed { vm.regs.resize(needed + 64, Value::nil()); }
                        for (i, &v) in resume_args.iter().enumerate() { vm.regs[base + i] = v; }
                        if let Err(e) = vm.push_frame(cp.proto, cp.upvals_ptr, cp.upvals_len, base, resume_args.len() as u8, 255) {
                            std::mem::swap(&mut vm.regs, &mut co.regs);
                            std::mem::swap(&mut vm.frames, &mut co.frames);
                            co.status = CoStatus::Dead;
                            return Err(e);
                        }
                    }
                }

                if co.started && co.yield_result_base > 0 {
                    let base = co.yield_result_base;
                    let n = if co.yield_nresults == 255 { resume_args.len() } else { co.yield_nresults as usize };
                    for (i, &v) in resume_args.iter().take(n).enumerate() { vm.regs[base + i] = v; }
                    for i in resume_args.len().min(n)..n { vm.regs[base + i] = Value::nil(); }
                    co.yield_result_base = 0;
                }

                let run_result = vm.run_inner();
                let co_results = std::mem::take(&mut vm.top_level_results);

                std::mem::swap(&mut vm.regs, &mut co.regs);
                std::mem::swap(&mut vm.frames, &mut co.frames);

                match run_result {
                    Ok(()) => {
                        co.status = CoStatus::Dead;
                        let mut ret = vec![Value::bool(true)];
                        ret.extend(co_results);
                        Ok(ret)
                    }
                    Err(VmError::Yield(values)) => {
                        co.status = CoStatus::Suspended;
                        capture_yield_site(co);
                        let mut ret = vec![Value::bool(true)];
                        ret.extend(values);
                        Ok(ret)
                    }
                    Err(e) => {
                        co.status = CoStatus::Dead;
                        let msg = vm.intern_pub(&e.to_string());
                        Ok(vec![Value::bool(false), msg])
                    }
                }
            })
        });

        let status_val = self.make_cfn_val(|args| {
            let co_val = args.first().copied().unwrap_or(Value::nil());
            if !co_val.is_coroutine() {
                return Err(VmError::RuntimeError("coroutine.status: expected coroutine".into()));
            }
            let co = unsafe { &*(co_val.as_coroutine().unwrap() as *const Coroutine) };
            let s = match co.status {
                CoStatus::Suspended => "suspended",
                CoStatus::Running   => "running",
                CoStatus::Dead      => "dead",
            };
            Ok(vec![alloc_string_val(s)])
        });

        let wrap_val = self.make_cfn_val(|args| {
            let fn_val = args.first().copied().unwrap_or(Value::nil());
            if get_closure(fn_val).is_none() {
                return Err(VmError::RuntimeError("coroutine.wrap: expected function".into()));
            }
            CURRENT_VM.with(|c| {
                let vm_ptr = c.get();
                if vm_ptr.is_null() {
                    return Err(VmError::RuntimeError("no VM context".into()));
                }
                let vm = unsafe { &mut *vm_ptr };
                let co = Box::new(Coroutine {
                    regs: vec![Value::nil(); 256],
                    frames: Vec::with_capacity(8),
                    status: CoStatus::Suspended,
                    fn_val,
                    started: false,
                    yield_result_base: 0,
                    yield_nresults: 0,
                });
                let co_ptr = Box::into_raw(co);
                vm.coroutines.push(co_ptr);
                let co_val = Value::coroutine(co_ptr as *mut u8);
                let wrapper = vm.make_cfn_val(move |wargs| {
                    CURRENT_VM.with(|c| {
                        let vm_ptr = c.get();
                        if vm_ptr.is_null() {
                            return Err(VmError::RuntimeError("no VM context".into()));
                        }
                        let vm = unsafe { &mut *vm_ptr };
                        let co = unsafe { &mut *(co_val.as_coroutine().unwrap() as *mut Coroutine) };
                        if co.status == CoStatus::Dead {
                            return Err(VmError::RuntimeError("cannot resume dead coroutine".into()));
                        }
                        co.status = CoStatus::Running;
                        std::mem::swap(&mut vm.regs, &mut co.regs);
                        std::mem::swap(&mut vm.frames, &mut co.frames);
                        if !co.started {
                            co.started = true;
                            if let Some(cp) = get_proto_callable(co.fn_val) {
                                let base = 1usize;
                                let proto = unsafe { &*cp.proto };
                                let needed = base + proto.max_regs as usize + 8;
                                if vm.regs.len() < needed { vm.regs.resize(needed + 64, Value::nil()); }
                                for (i, &v) in wargs.iter().enumerate() { vm.regs[base + i] = v; }
                                if let Err(e) = vm.push_frame(cp.proto, cp.upvals_ptr, cp.upvals_len, base, wargs.len() as u8, 255) {
                                    std::mem::swap(&mut vm.regs, &mut co.regs);
                                    std::mem::swap(&mut vm.frames, &mut co.frames);
                                    co.status = CoStatus::Dead;
                                    return Err(e);
                                }
                            }
                        } else if co.yield_result_base > 0 {
                            let base = co.yield_result_base;
                            let n = if co.yield_nresults == 255 { wargs.len() } else { co.yield_nresults as usize };
                            for (i, &v) in wargs.iter().take(n).enumerate() { vm.regs[base + i] = v; }
                            for i in wargs.len().min(n)..n { vm.regs[base + i] = Value::nil(); }
                            co.yield_result_base = 0;
                        }
                        let run_result = vm.run_inner();
                        let co_results = std::mem::take(&mut vm.top_level_results);
                        std::mem::swap(&mut vm.regs, &mut co.regs);
                        std::mem::swap(&mut vm.frames, &mut co.frames);
                        match run_result {
                            Ok(()) => { co.status = CoStatus::Dead; Ok(co_results) }
                            Err(VmError::Yield(vals)) => {
                                co.status = CoStatus::Suspended;
                                capture_yield_site(co);
                                Ok(vals)
                            }
                            Err(e) => { co.status = CoStatus::Dead; Err(e) }
                        }
                    })
                });
                Ok(vec![wrapper])
            })
        });

        let isyieldable_val = self.make_cfn_val(|_args| {
            Ok(vec![Value::bool(false)])
        });

        let ct = unsafe { &mut *(co_table_ptr as *mut Table) };
        let k_create      = self.intern("create");
        let k_resume      = self.intern("resume");
        let k_status      = self.intern("status");
        let k_wrap        = self.intern("wrap");
        let k_isyieldable = self.intern("isyieldable");
        ct.raw_set(k_create,      create_val);
        ct.raw_set(k_resume,      resume_val);
        ct.raw_set(k_status,      status_val);
        ct.raw_set(k_wrap,        wrap_val);
        ct.raw_set(k_isyieldable, isyieldable_val);

        let co_table_val = Value::table(co_table_ptr);
        let k_coroutine = self.intern("coroutine");
        self.globals.raw_set(k_coroutine, co_table_val);

        let str_table_ptr = alloc_table_raw();
        self.gc.register_table(str_table_ptr);

        let v_str_len = self.make_cfn_val(|args| {
            Ok(vec![Value::int(str_arg(args, 0, "string.len")?.len() as i64)])
        });
        let v_str_sub = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "string.sub")?;
            let len = s.len();
            let i = int_from_val(args.get(1).copied().unwrap_or(Value::int(1)));
            let j = int_from_val(args.get(2).copied().unwrap_or(Value::int(-1)));
            let start = lua_str_start(len, i);
            let end   = lua_str_end(len, j);
            if start >= end { return Ok(vec![alloc_string_val("")]); }
            Ok(vec![alloc_string_val(&String::from_utf8_lossy(&s.as_bytes()[start..end]))])
        });
        let v_str_rep = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "string.rep")?;
            let n = int_from_val(args.get(1).copied().unwrap_or(Value::int(0)));
            let sep = args.get(2).filter(|v| v.is_string())
                .map(|&v| unsafe { string_ref(v) }.to_owned())
                .unwrap_or_default();
            if n <= 0 { return Ok(vec![alloc_string_val("")]); }
            let n = n as usize;
            let total = n.saturating_mul(s.len() + sep.len());
            if total > MAX_ALLOC_LEN {
                return Err(VmError::RuntimeError("string.rep: result too large".into()));
            }
            let parts: Vec<&str> = std::iter::repeat(s).take(n).collect();
            Ok(vec![alloc_string_val(&parts.join(&sep))])
        });
        let v_str_upper = self.make_cfn_val(|args| {
            Ok(vec![alloc_string_val(&str_arg(args, 0, "string.upper")?.to_uppercase())])
        });
        let v_str_lower = self.make_cfn_val(|args| {
            Ok(vec![alloc_string_val(&str_arg(args, 0, "string.lower")?.to_lowercase())])
        });
        let v_str_reverse = self.make_cfn_val(|args| {
            let rev: String = str_arg(args, 0, "string.reverse")?.chars().rev().collect();
            Ok(vec![alloc_string_val(&rev)])
        });
        let v_str_byte = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "string.byte")?;
            let i = int_from_val(args.get(1).copied().unwrap_or(Value::int(1)));
            let j = int_from_val(args.get(2).copied().unwrap_or(Value::int(i)));
            let start = lua_str_start(s.len(), i);
            let end   = lua_str_end(s.len(), j);
            let bytes = s.as_bytes();
            Ok((start..end.min(bytes.len())).map(|k| Value::int(bytes[k] as i64)).collect())
        });
        let v_str_char = self.make_cfn_val(|args| {
            let mut s = String::with_capacity(args.len());
            for &v in args {
                let n = int_from_val(v);
                let b = u8::try_from(n).map_err(|_| VmError::RuntimeError("string.char: value out of range".into()))?;
                s.push(b as char);
            }
            Ok(vec![alloc_string_val(&s)])
        });
        let v_str_find = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "string.find")?;
            if args.len() < 2 || !args[1].is_string() {
                return Err(VmError::RuntimeError("string.find: string expected for pattern".into()));
            }
            let pat = unsafe { string_ref(args[1]) };
            let init = if args.len() > 2 { int_from_val(args[2]) } else { 1 };
            let start = lua_str_start(s.len(), init);
            match s[start..].find(pat) {
                None => Ok(vec![Value::nil()]),
                Some(pos) => Ok(vec![
                    Value::int((start + pos + 1) as i64),
                    Value::int((start + pos + pat.len()) as i64),
                ]),
            }
        });
        let v_str_format = self.make_cfn_val(|args| {
            let fmt = str_arg(args, 0, "string.format")?;
            string_format(fmt, if args.len() > 1 { &args[1..] } else { &[] })
        });

        {
            let st = unsafe { &mut *(str_table_ptr as *mut Table) };
            let k = self.intern("len");     st.raw_set(k, v_str_len);
            let k = self.intern("sub");     st.raw_set(k, v_str_sub);
            let k = self.intern("rep");     st.raw_set(k, v_str_rep);
            let k = self.intern("upper");   st.raw_set(k, v_str_upper);
            let k = self.intern("lower");   st.raw_set(k, v_str_lower);
            let k = self.intern("reverse"); st.raw_set(k, v_str_reverse);
            let k = self.intern("byte");    st.raw_set(k, v_str_byte);
            let k = self.intern("char");    st.raw_set(k, v_str_char);
            let k = self.intern("find");    st.raw_set(k, v_str_find);
            let k = self.intern("format");  st.raw_set(k, v_str_format);
        }
        let str_table_val = Value::table(str_table_ptr);
        self.string_lib = str_table_val;
        let k_string = self.intern("string");
        self.globals.raw_set(k_string, str_table_val);

        let math_table_ptr = alloc_table_raw();
        self.gc.register_table(math_table_ptr);

        let v_math_floor = self.make_cfn_val(|args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() { return Ok(vec![v]); }
            let f = v.as_float().ok_or_else(|| VmError::RuntimeError("math.floor: number expected".into()))?;
            Ok(vec![make_int_via_current_vm(f.floor() as i64)])
        });
        let v_math_ceil = self.make_cfn_val(|args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() { return Ok(vec![v]); }
            let f = v.as_float().ok_or_else(|| VmError::RuntimeError("math.ceil: number expected".into()))?;
            Ok(vec![make_int_via_current_vm(f.ceil() as i64)])
        });
        let v_math_abs = self.make_cfn_val(|args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() { return Ok(vec![make_int_via_current_vm(v.as_int().unwrap().wrapping_abs())]); }
            let f = v.as_float().ok_or_else(|| VmError::RuntimeError("math.abs: number expected".into()))?;
            Ok(vec![Value::float(f.abs())])
        });
        let v_math_sqrt = self.make_cfn_val(|args| {
            let f = args.first().copied().unwrap_or(Value::nil()).to_float()
                .ok_or_else(|| VmError::RuntimeError("math.sqrt: number expected".into()))?;
            Ok(vec![Value::float(f.sqrt())])
        });
        let v_math_max = self.make_cfn_val(|args| {
            if args.is_empty() { return Err(VmError::RuntimeError("math.max: at least one arg required".into())); }
            let mut best = args[0];
            for &v in &args[1..] {
                let better = if best.is_int_like() && v.is_int_like() {
                    best.as_int().unwrap() < v.as_int().unwrap()
                } else {
                    best.to_float().unwrap_or(f64::NEG_INFINITY) < v.to_float().unwrap_or(f64::NEG_INFINITY)
                };
                if better { best = v; }
            }
            Ok(vec![best])
        });
        let v_math_min = self.make_cfn_val(|args| {
            if args.is_empty() { return Err(VmError::RuntimeError("math.min: at least one arg required".into())); }
            let mut best = args[0];
            for &v in &args[1..] {
                let better = if best.is_int_like() && v.is_int_like() {
                    best.as_int().unwrap() > v.as_int().unwrap()
                } else {
                    best.to_float().unwrap_or(f64::INFINITY) > v.to_float().unwrap_or(f64::INFINITY)
                };
                if better { best = v; }
            }
            Ok(vec![best])
        });
        let v_math_sin  = self.make_cfn_val(|args| {
            Ok(vec![Value::float(args.first().copied().unwrap_or(Value::nil()).to_float()
                .ok_or_else(|| VmError::RuntimeError("math.sin: number expected".into()))?.sin())])
        });
        let v_math_cos  = self.make_cfn_val(|args| {
            Ok(vec![Value::float(args.first().copied().unwrap_or(Value::nil()).to_float()
                .ok_or_else(|| VmError::RuntimeError("math.cos: number expected".into()))?.cos())])
        });
        let v_math_tan  = self.make_cfn_val(|args| {
            Ok(vec![Value::float(args.first().copied().unwrap_or(Value::nil()).to_float()
                .ok_or_else(|| VmError::RuntimeError("math.tan: number expected".into()))?.tan())])
        });
        let v_math_exp  = self.make_cfn_val(|args| {
            Ok(vec![Value::float(args.first().copied().unwrap_or(Value::nil()).to_float()
                .ok_or_else(|| VmError::RuntimeError("math.exp: number expected".into()))?.exp())])
        });
        let v_math_log = self.make_cfn_val(|args| {
            let x = args.first().copied().unwrap_or(Value::nil()).to_float()
                .ok_or_else(|| VmError::RuntimeError("math.log: number expected".into()))?;
            let result = match args.get(1) {
                Some(&b) => x.log(b.to_float().ok_or_else(|| VmError::RuntimeError("math.log: number expected for base".into()))?),
                None => x.ln(),
            };
            Ok(vec![Value::float(result)])
        });
        let v_math_modf = self.make_cfn_val(|args| {
            let f = args.first().copied().unwrap_or(Value::nil()).to_float()
                .ok_or_else(|| VmError::RuntimeError("math.modf: number expected".into()))?;
            Ok(vec![Value::float(f.trunc()), Value::float(f.fract())])
        });
        let v_math_type = self.make_cfn_val(|args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() { return Ok(vec![alloc_string_val("integer")]); }
            if v.is_float() { return Ok(vec![alloc_string_val("float")]); }
            Ok(vec![Value::bool(false)])
        });
        let v_math_tointeger = self.make_cfn_val(|args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() { return Ok(vec![v]); }
            if v.is_float() {
                let f = v.as_float().unwrap();
                let i = f as i64;
                if i as f64 == f { return Ok(vec![make_int_via_current_vm(i)]); }
            }
            Ok(vec![Value::nil()])
        });
        let v_math_random = self.make_cfn_val(|args| {
            let r = RNG.with(|c| { let n = splitmix64(c.get()); c.set(n); n });
            match args.len() {
                0 => Ok(vec![Value::float((r >> 11) as f64 * (1.0_f64 / (1u64 << 53) as f64))]),
                1 => {
                    let m = int_from_val(args[0]);
                    if m < 1 { return Err(VmError::RuntimeError("math.random: interval is empty".into())); }
                    Ok(vec![make_int_via_current_vm(1 + (r % m as u64) as i64)])
                }
                _ => {
                    let lo = int_from_val(args[0]);
                    let hi = int_from_val(args[1]);
                    if lo > hi { return Err(VmError::RuntimeError("math.random: interval is empty".into())); }
                    let range = hi as i128 - lo as i128 + 1;
                    if range > u64::MAX as i128 {
                        return Err(VmError::RuntimeError("math.random: range too large".into()));
                    }
                    Ok(vec![make_int_via_current_vm(lo + (r % range as u64) as i64)])
                }
            }
        });
        let v_math_randomseed = self.make_cfn_val(|args| {
            RNG.with(|c| c.set(int_from_val(args.first().copied().unwrap_or(Value::int(0))) as u64));
            Ok(vec![])
        });

        {
            let mt = unsafe { &mut *(math_table_ptr as *mut Table) };
            let k = self.intern("pi");          mt.raw_set(k, Value::float(std::f64::consts::PI));
            let k = self.intern("huge");        mt.raw_set(k, Value::float(f64::INFINITY));
            let k = self.intern("maxinteger");  let v = self.make_int(i64::MAX); mt.raw_set(k, v);
            let k = self.intern("mininteger");  let v = self.make_int(i64::MIN); mt.raw_set(k, v);
            let k = self.intern("floor");       mt.raw_set(k, v_math_floor);
            let k = self.intern("ceil");        mt.raw_set(k, v_math_ceil);
            let k = self.intern("abs");         mt.raw_set(k, v_math_abs);
            let k = self.intern("sqrt");        mt.raw_set(k, v_math_sqrt);
            let k = self.intern("max");         mt.raw_set(k, v_math_max);
            let k = self.intern("min");         mt.raw_set(k, v_math_min);
            let k = self.intern("sin");         mt.raw_set(k, v_math_sin);
            let k = self.intern("cos");         mt.raw_set(k, v_math_cos);
            let k = self.intern("tan");         mt.raw_set(k, v_math_tan);
            let k = self.intern("exp");         mt.raw_set(k, v_math_exp);
            let k = self.intern("log");         mt.raw_set(k, v_math_log);
            let k = self.intern("modf");        mt.raw_set(k, v_math_modf);
            let k = self.intern("type");        mt.raw_set(k, v_math_type);
            let k = self.intern("tointeger");   mt.raw_set(k, v_math_tointeger);
            let k = self.intern("random");      mt.raw_set(k, v_math_random);
            let k = self.intern("randomseed");  mt.raw_set(k, v_math_randomseed);
        }
        let k_math = self.intern("math");
        self.globals.raw_set(k_math, Value::table(math_table_ptr));

        let tbl_table_ptr = alloc_table_raw();
        self.gc.register_table(tbl_table_ptr);

        let v_tbl_insert = self.make_cfn_val(|args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() { return Err(VmError::RuntimeError("table.insert: table expected".into())); }
            let tbl = unsafe { table_ref(t) };
            match args.len() {
                2 => {
                    let n = tbl.length() + 1;
                    tbl.raw_set(Value::int(n), args[1]);
                }
                3 => {
                    let pos = int_from_val(args[1]);
                    let n   = tbl.length();
                    if pos < 1 || pos > n + 1 {
                        return Err(VmError::RuntimeError("table.insert: position out of bounds".into()));
                    }
                    for i in (pos..=n).rev() {
                        let elem = tbl.raw_get(Value::int(i));
                        tbl.raw_set(Value::int(i + 1), elem);
                    }
                    tbl.raw_set(Value::int(pos), args[2]);
                }
                _ => return Err(VmError::RuntimeError("table.insert: wrong number of args".into())),
            }
            Ok(vec![])
        });
        let v_tbl_remove = self.make_cfn_val(|args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() { return Err(VmError::RuntimeError("table.remove: table expected".into())); }
            let tbl = unsafe { table_ref(t) };
            let n = tbl.length();
            if n == 0 { return Ok(vec![Value::nil()]); }
            let pos = args.get(1).map(|&v| int_from_val(v)).unwrap_or(n);
            let removed = tbl.raw_get(Value::int(pos));
            for i in pos..n {
                let next = tbl.raw_get(Value::int(i + 1));
                tbl.raw_set(Value::int(i), next);
            }
            tbl.array.truncate((n - 1) as usize);
            Ok(vec![removed])
        });
        let v_tbl_concat = self.make_cfn_val(|args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() { return Err(VmError::RuntimeError("table.concat: table expected".into())); }
            let tbl = unsafe { &*(t.as_table().unwrap() as *const Table) };
            let sep = args.get(1).filter(|v| v.is_string())
                .map(|&v| unsafe { string_ref(v) }.to_owned())
                .unwrap_or_default();
            let n = tbl.length();
            let i = args.get(2).map(|&v| int_from_val(v)).unwrap_or(1);
            let j = args.get(3).map(|&v| int_from_val(v)).unwrap_or(n);
            if checked_range_len(i, j).is_none() {
                return Err(VmError::RuntimeError("table.concat: range too large".into()));
            }
            let mut parts: Vec<String> = Vec::new();
            for k in i..=j {
                let v = tbl.raw_get(make_int_via_current_vm(k));
                let s = if v.is_string() { unsafe { string_ref(v) }.to_owned() }
                        else if v.is_number() { coerce_to_concat_str(v) }
                        else { return Err(VmError::RuntimeError("table.concat: invalid value (not string or number)".into())); };
                parts.push(s);
            }
            Ok(vec![alloc_string_val(&parts.join(&sep))])
        });
        let v_tbl_sort = self.make_cfn_val(|args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() { return Err(VmError::RuntimeError("table.sort: table expected".into())); }
            let comp_val: Option<Value> = args.get(1).copied().filter(|v| !v.is_nil());
            let mut arr = std::mem::take(&mut unsafe { table_ref(t) }.array);
            let mut sort_err: Option<VmError> = None;
            arr.sort_by(|a, b| {
                if sort_err.is_some() { return std::cmp::Ordering::Equal; }
                let lt = match comp_val {
                    Some(cf) => CURRENT_VM.with(|c| {
                        let vm_ptr = c.get();
                        if vm_ptr.is_null() { return Ok(false); }
                        let vm = unsafe { &mut *vm_ptr };
                        match vm.call_value_isolated(cf, &[*a, *b]) {
                            Ok(res) => Ok(res.into_iter().next().unwrap_or(Value::nil()).is_truthy()),
                            Err(e)  => Err(e),
                        }
                    }),
                    None => value_lt(*a, *b),
                };
                match lt {
                    Ok(true)  => std::cmp::Ordering::Less,
                    Ok(false) => std::cmp::Ordering::Greater,
                    Err(e)    => { sort_err = Some(e); std::cmp::Ordering::Equal }
                }
            });
            if let Some(e) = sort_err { return Err(e); }
            unsafe { table_ref(t) }.array = arr;
            Ok(vec![])
        });
        let v_tbl_unpack = self.make_cfn_val(|args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() { return Ok(vec![]); }
            let tbl = unsafe { &*(t.as_table().unwrap() as *const Table) };
            let n = tbl.length();
            let i = args.get(1).map(|&v| int_from_val(v)).unwrap_or(1);
            let j = args.get(2).map(|&v| int_from_val(v)).unwrap_or(n);
            if checked_range_len(i, j).is_none() {
                return Err(VmError::RuntimeError("table.unpack: range too large".into()));
            }
            Ok((i..=j).map(|k| tbl.raw_get(make_int_via_current_vm(k))).collect())
        });
        let v_tbl_move = self.make_cfn_val(|args| {
            let a1 = args.first().copied().unwrap_or(Value::nil());
            if !a1.is_table() { return Err(VmError::RuntimeError("table.move: table expected".into())); }
            let f  = int_from_val(args.get(1).copied().unwrap_or(Value::int(1)));
            let e  = int_from_val(args.get(2).copied().unwrap_or(Value::int(0)));
            let t  = int_from_val(args.get(3).copied().unwrap_or(Value::int(1)));
            let a2 = args.get(4).copied().filter(|v| v.is_table()).unwrap_or(a1);
            if e >= f {
                if checked_range_len(f, e).is_none() {
                    return Err(VmError::RuntimeError("table.move: range too large".into()));
                }
                let vals: Vec<Value> = (f..=e)
                    .map(|k| unsafe { (*(a1.as_table().unwrap() as *const Table)).raw_get(make_int_via_current_vm(k)) })
                    .collect();
                let dst = unsafe { table_ref(a2) };
                for (idx, v) in vals.into_iter().enumerate() {
                    let key = make_int_via_current_vm(t + idx as i64);
                    dst.raw_set(key, v);
                }
            }
            Ok(vec![a2])
        });

        {
            let tt = unsafe { &mut *(tbl_table_ptr as *mut Table) };
            let k = self.intern("insert"); tt.raw_set(k, v_tbl_insert);
            let k = self.intern("remove"); tt.raw_set(k, v_tbl_remove);
            let k = self.intern("concat"); tt.raw_set(k, v_tbl_concat);
            let k = self.intern("sort");   tt.raw_set(k, v_tbl_sort);
            let k = self.intern("unpack"); tt.raw_set(k, v_tbl_unpack);
            let k = self.intern("move");   tt.raw_set(k, v_tbl_move);
        }
        let k_table = self.intern("table");
        self.globals.raw_set(k_table, Value::table(tbl_table_ptr));
    }

    pub fn set_global_cfn(&mut self, name: &str, f: impl Fn(&[Value]) -> VmResult<Vec<Value>> + 'static) {
        let k = self.intern(name);
        let v = self.make_cfn_val(f);
        self.globals.raw_set(k, v);
    }

    pub fn make_cfn_val(&mut self, f: impl Fn(&[Value]) -> VmResult<Vec<Value>> + 'static) -> Value {
        let boxed: CFunction = Box::new(f);
        let outer: Box<CFunction> = Box::new(boxed);
        let ptr = Box::into_raw(outer) as *mut u8;
        let tagged = ptr as u64 | (1u64 << 47);
        Value::userdata(tagged as *mut u8)
    }

    pub fn intern_pub(&mut self, s: &str) -> Value { self.intern(s) }

    /// Host-only: bounds how many bytecode instructions a script may execute
    /// before erroring out, resetting the count. 0 means unlimited. There is
    /// deliberately no script-facing way to read or change this — a sandboxed
    /// script shouldn't be able to lift its own leash.
    pub fn set_step_limit(&mut self, limit: u64) {
        self.step_limit = limit;
        self.step_count = 0;
    }

    /// Host-only hard ceiling on live GC-tracked objects (0 = unlimited).
    /// Unlike gc_setstep (which just tunes when a collection is attempted),
    /// exceeding this after a collection is a real error: the script has more
    /// live data than the host is willing to let it hold.
    pub fn set_max_objects(&mut self, limit: usize) {
        self.gc.max_objects = limit;
    }

    pub fn call_cfn(v: Value, args: &[Value]) -> Option<VmResult<Vec<Value>>> {
        get_cfn(v).map(|f| f(args))
    }

    pub fn set_global(&mut self, name: &str, v: Value) {
        let k = self.intern(name);
        self.globals.raw_set(k, v);
    }

    pub fn get_global(&mut self, name: &str) -> Value {
        let k = self.intern(name);
        self.globals.raw_get(k)
    }
}


pub fn clear_print_hook() {
    PRINT_HOOK.with(|h| *h.borrow_mut() = None);
}

pub type CFunction = Box<dyn Fn(&[Value]) -> VmResult<Vec<Value>>>;

fn get_cfn(v: Value) -> Option<&'static dyn Fn(&[Value]) -> VmResult<Vec<Value>>> {
    if !v.is_userdata() { return None; }
    let raw = v.as_userdata().unwrap() as u64;
    if raw & (1u64 << 47) == 0 { return None; }
    let ptr = (raw & !(1u64 << 47)) as *const CFunction;
    Some(unsafe { (*ptr).as_ref() })
}

pub fn get_closure(v: Value) -> Option<*const Proto> {
    if v.is_userdata() {
        let raw = v.as_userdata().unwrap() as u64;
        if raw & (1u64 << 47) != 0 { return None; }
        return Some(raw as *const Proto);
    }
    if v.is_closure() {
        let lc = v.as_closure().unwrap() as *const LuaClosure;
        return Some(unsafe { (*lc).proto });
    }
    None
}

struct CallableProto {
    proto: *const Proto,
    upvals_ptr: *mut Value,
    upvals_len: usize,
}

fn get_proto_callable(v: Value) -> Option<CallableProto> {
    if v.is_userdata() {
        let raw = v.as_userdata().unwrap() as u64;
        if raw & (1u64 << 47) != 0 { return None; }
        return Some(CallableProto { proto: raw as *const Proto, upvals_ptr: std::ptr::null_mut(), upvals_len: 0 });
    }
    if v.is_closure() {
        let lc = v.as_closure().unwrap() as *mut LuaClosure;
        let (ptr, len) = unsafe { ((*lc).upvals.as_mut_ptr(), (*lc).upvals.len()) };
        return Some(CallableProto { proto: unsafe { (*lc).proto }, upvals_ptr: ptr, upvals_len: len });
    }
    None
}

pub fn get_cfn_pub(v: Value) -> Option<&'static dyn Fn(&[Value]) -> VmResult<Vec<Value>>> {
    get_cfn(v)
}


fn coerce_to_concat_str(v: Value) -> String {
    if v.is_string() { unsafe { string_ref(v) }.to_owned() }
    else if v.is_int_like() { v.as_int().unwrap().to_string() }
    else { v.as_float().unwrap().to_string() }
}


fn int_val(v: Value) -> VmResult<i64> {
    v.as_int().or_else(|| v.as_float().map(|f| f as i64))
     .ok_or_else(|| VmError::RuntimeError(format!("integer expected, got {}", v.type_name())))
}

#[derive(Clone, Copy)]
enum Num { Int(i64), Float(f64) }

fn to_number(v: Value) -> VmResult<Num> {
    if v.is_int_like() { return Ok(Num::Int(v.as_int().unwrap())); }
    if v.is_float() { return Ok(Num::Float(v.as_float().unwrap())); }
    Err(VmError::RuntimeError(format!("'for' limit must be a number, got {}", v.type_name())))
}

fn num_add(a: Num, b: Num) -> Value {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => make_int_via_current_vm(x.wrapping_add(y)),
        (Num::Float(x), Num::Float(y)) => Value::float(x + y),
        (Num::Int(x), Num::Float(y)) => Value::float(x as f64 + y),
        (Num::Float(x), Num::Int(y)) => Value::float(x + y as f64),
    }
}

fn num_sub(a: Num, b: Num) -> Value {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => make_int_via_current_vm(x.wrapping_sub(y)),
        (Num::Float(x), Num::Float(y)) => Value::float(x - y),
        (Num::Int(x), Num::Float(y)) => Value::float(x as f64 - y),
        (Num::Float(x), Num::Int(y)) => Value::float(x - y as f64),
    }
}

fn num_le(a: Num, b: Num) -> bool {
    match (a, b) {
        (Num::Int(x), Num::Int(y))     => x <= y,
        (Num::Float(x), Num::Float(y)) => x <= y,
        (Num::Int(x), Num::Float(y))   => (x as f64) <= y,
        (Num::Float(x), Num::Int(y))   => x <= (y as f64),
    }
}

fn num_is_positive(n: Num) -> bool {
    match n { Num::Int(x) => x > 0, Num::Float(x) => x > 0.0 }
}

fn values_equal(a: Value, b: Value) -> bool {
    if a.is_int_like() && b.is_int_like() {
        return a.as_int().unwrap() == b.as_int().unwrap();
    }
    if a.is_int_like() && b.is_float() {
        let n = a.as_int().unwrap();
        let f = b.as_float().unwrap();
        return (n as f64 == f) && (f as i64 == n);
    }
    if a.is_float() && b.is_int_like() {
        let f = a.as_float().unwrap();
        let n = b.as_int().unwrap();
        return (n as f64 == f) && (f as i64 == n);
    }
    if a.is_string() && b.is_string() {
        return unsafe { string_ref(a) } == unsafe { string_ref(b) };
    }
    a.raw_bits() == b.raw_bits()
}

fn value_lt(a: Value, b: Value) -> VmResult<bool> {
    if a.is_int_like() && b.is_int_like() { return Ok(a.as_int().unwrap() < b.as_int().unwrap()); }
    if let (Some(af), Some(bf)) = (a.to_float(), b.to_float()) { return Ok(af < bf); }
    if a.is_string() && b.is_string() {
        return Ok(unsafe { string_ref(a) } < unsafe { string_ref(b) });
    }
    Err(VmError::RuntimeError(format!("attempt to compare {} with {}", a.type_name(), b.type_name())))
}

fn value_le(a: Value, b: Value) -> VmResult<bool> {
    if a.is_int_like() && b.is_int_like() { return Ok(a.as_int().unwrap() <= b.as_int().unwrap()); }
    if let (Some(af), Some(bf)) = (a.to_float(), b.to_float()) { return Ok(af <= bf); }
    if a.is_string() && b.is_string() {
        return Ok(unsafe { string_ref(a) } <= unsafe { string_ref(b) });
    }
    Err(VmError::RuntimeError(format!("attempt to compare {} with {}", a.type_name(), b.type_name())))
}


fn capture_yield_site(co: &mut Coroutine) {
    if let Some(frame) = co.frames.last() {
        if frame.pc > 0 {
            let instr = unsafe { (&(*frame.proto).code)[frame.pc - 1] };
            co.yield_result_base = frame.base + ia(instr) as usize;
            let call_c = ic(instr);
            co.yield_nresults = if call_c == 0 { 255 } else { (call_c - 1) as u8 };
        }
    }
}

fn str_arg<'a>(args: &'a [Value], idx: usize, fn_name: &'static str) -> VmResult<&'a str> {
    match args.get(idx) {
        Some(v) if v.is_string() => Ok(unsafe { string_ref(*v) }),
        _ => Err(VmError::RuntimeError(format!("{fn_name}: string expected"))),
    }
}

fn int_from_val(v: Value) -> i64 {
    v.as_int().or_else(|| v.as_float().map(|f| f as i64)).unwrap_or(0)
}

// i64::MIN..=i64::MAX overflows i64 arithmetic directly, so the length is
// computed in i128; returns None if the (inclusive) range exceeds MAX_ALLOC_LEN.
fn checked_range_len(lo: i64, hi: i64) -> Option<usize> {
    if hi < lo { return Some(0); }
    let len = (hi as i128) - (lo as i128) + 1;
    if len > MAX_ALLOC_LEN as i128 { None } else { Some(len as usize) }
}

fn lua_str_start(len: usize, i: i64) -> usize {
    if i > 0 { ((i - 1) as usize).min(len) }
    else if i < 0 { (len as i64 + i).max(0) as usize }
    else { 0 }
}

fn lua_str_end(len: usize, j: i64) -> usize {
    if j >= 0 { (j as usize).min(len) }
    else { (len as i64 + j + 1).max(0) as usize }
}

fn splitmix64(x: u64) -> u64 {
    let x = x.wrapping_add(0x9e3779b97f4a7c15);
    let x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    let x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

fn pad_str(s: String, width: usize, left_align: bool, zero_pad: bool) -> String {
    if s.len() >= width { return s; }
    let n = width - s.len();
    if left_align {
        let mut r = s;
        for _ in 0..n { r.push(' '); }
        r
    } else if zero_pad {
        // Put zeros after any leading sign character.
        let (sign, rest) = if s.starts_with(['-', '+', ' ']) { (&s[..1], &s[1..]) } else { ("", s.as_str()) };
        format!("{}{}{}", sign, "0".repeat(n), rest)
    } else {
        format!("{}{}", " ".repeat(n), s)
    }
}

fn string_format(fmt: &str, args: &[Value]) -> VmResult<Vec<Value>> {
    let mut out = String::new();
    let bytes = fmt.as_bytes();
    let mut pos = 0usize;
    let mut arg_idx = 0usize;

    while pos < bytes.len() {
        if bytes[pos] != b'%' {
            out.push(bytes[pos] as char);
            pos += 1;
            continue;
        }
        pos += 1;
        if pos >= bytes.len() { break; }
        if bytes[pos] == b'%' { out.push('%'); pos += 1; continue; }

        let mut left = false;
        let mut plus = false;
        let mut zero = false;
        let mut space = false;
        loop {
            match bytes.get(pos) {
                Some(b'-') => { left = true;  pos += 1; }
                Some(b'+') => { plus = true;  pos += 1; }
                Some(b'0') if !left => { zero = true; pos += 1; }
                Some(b' ') => { space = true; pos += 1; }
                _ => break,
            }
        }
        let mut width = 0usize;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            width = (width.saturating_mul(10) + (bytes[pos] - b'0') as usize).min(MAX_ALLOC_LEN);
            pos += 1;
        }
        let mut prec: Option<usize> = None;
        if pos < bytes.len() && bytes[pos] == b'.' {
            pos += 1;
            let mut p = 0usize;
            while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                p = (p.saturating_mul(10) + (bytes[pos] - b'0') as usize).min(MAX_ALLOC_LEN);
                pos += 1;
            }
            prec = Some(p);
        }
        if pos >= bytes.len() { break; }
        let spec = bytes[pos] as char; pos += 1;

        let v = args.get(arg_idx).copied().unwrap_or(Value::nil());
        arg_idx += 1;

        let s = match spec {
            'd' | 'i' => {
                let n = v.as_int().or_else(|| v.as_float().map(|f| f as i64))
                    .ok_or_else(|| VmError::RuntimeError(format!("bad argument #{arg_idx} to 'format' (number expected)")))?;
                let raw = if plus && n >= 0 { format!("+{n}") }
                          else if space && n >= 0 { format!(" {n}") }
                          else { format!("{n}") };
                pad_str(raw, width, left, zero)
            }
            'u' => {
                let n = v.as_int().or_else(|| v.as_float().map(|f| f as i64))
                    .ok_or_else(|| VmError::RuntimeError(format!("bad argument #{arg_idx} to 'format' (number expected)")))?
                    as u64;
                pad_str(format!("{n}"), width, left, zero)
            }
            'x' => {
                let n = v.as_int().or_else(|| v.as_float().map(|f| f as i64))
                    .ok_or_else(|| VmError::RuntimeError(format!("bad argument #{arg_idx} to 'format' (number expected)")))?;
                pad_str(format!("{:x}", n as u64), width, left, zero)
            }
            'X' => {
                let n = v.as_int().or_else(|| v.as_float().map(|f| f as i64))
                    .ok_or_else(|| VmError::RuntimeError(format!("bad argument #{arg_idx} to 'format' (number expected)")))?;
                pad_str(format!("{:X}", n as u64), width, left, zero)
            }
            'f' => {
                let f = v.to_float()
                    .ok_or_else(|| VmError::RuntimeError(format!("bad argument #{arg_idx} to 'format' (number expected)")))?;
                let p = prec.unwrap_or(6);
                let raw = if plus && f >= 0.0 { format!("+{:.prec$}", f, prec = p) }
                          else if space && f >= 0.0 { format!(" {:.prec$}", f, prec = p) }
                          else { format!("{:.prec$}", f, prec = p) };
                pad_str(raw, width, left, zero)
            }
            's' => {
                let raw = if v.is_string() { unsafe { string_ref(v) }.to_owned() } else { format!("{v}") };
                let raw = if let Some(p) = prec { raw.chars().take(p).collect::<String>() } else { raw };
                pad_str(raw, width, left, false)
            }
            'q' => {
                let s = if v.is_string() { unsafe { string_ref(v) }.to_owned() } else { format!("{v}") };
                let mut q = String::from("\"");
                for ch in s.chars() {
                    match ch {
                        '"'  => q.push_str("\\\""),
                        '\\' => q.push_str("\\\\"),
                        '\n' => q.push_str("\\n"),
                        '\r' => q.push_str("\\r"),
                        '\0' => q.push_str("\\0"),
                        _    => q.push(ch),
                    }
                }
                q.push('"'); q
            }
            _ => format!("%{spec}"),
        };
        out.push_str(&s);
    }
    Ok(vec![alloc_string_val(&out)])
}
