// The Rust embedding API: Ctx, the handle natives and host code use to
// touch a VM; Ref, a registry-style handle that keeps a value alive across
// calls; host context; and the module loader `require` consults.
//
// Why natives get a Ctx and not a &mut Vm: the interpreter lends its own
// &mut Vm to the native for the duration of the call, so the native's access
// is an ordinary reborrow and nothing aliases it. But a bare &mut Vm could
// be mem::replace'd or mem::swap'd with a fresh Vm::new(), dropping the VM
// out from under the interpreter frame that is still running on it. A Ctx
// can't be built outside this crate, so a native has no second Ctx or Vm to
// swap with, and the field holding the Vm isn't public.
//
// GC rooting rules. A Value is a plain pointer; holding one in Rust does not
// keep its object alive. The collector runs whenever script runs (any call,
// metamethod, require, or gc_collect), never on allocation alone. So:
//
// 1. Every Value a Ctx method returns, and every argument passed to a
//    native, stays alive until that Ctx drops. For a native that is when it
//    returns; for vm.ctx() it is the end of the host's scope.
// 2. A value you need past that point goes in a Ref (make_ref). Refs are
//    roots until release_ref.
// 3. A Value used after its Ctx dropped, without a Ref, may point at freed
//    memory. The type system doesn't catch this; it is the classic embedding
//    footgun, the same one Lua's C API has.
// 4. Natives (Ctx::new_function, new_closure, Vm::make_cfn_val,
//    make_cfn_closure, set_global_cfn) are collected objects like tables,
//    so 1-3 apply to their Values too. A Value the native's Rust closure
//    captures is invisible to the collector; pass it to new_closure as an
//    upvalue instead, which is traced for as long as the native lives.
//
// Rule 1 means a long-lived Ctx keeps everything it ever returned alive. A
// host driving many scripts per frame should take a fresh vm.ctx() per frame
// (or per batch) rather than one for the whole program.

use std::any::{Any, TypeId};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::value::Value;
use crate::vm::{Vm, VmError, VmResult};

/// Access to a VM from host code (Vm::ctx) or from inside a native function.
/// See the rooting rules at the top of embed.rs.
pub struct Ctx<'a> {
    pub(crate) vm: &'a mut Vm,
    // ctx_roots length when this Ctx was made; dropping truncates back.
    root_base: usize,
}

/// A script error as the host sees it.
#[derive(Debug, Clone)]
pub struct ScriptError {
    /// `chunk:line: message` for runtime errors; the string form of the
    /// thrown value for error(v) with a non-string or level-0 value.
    pub message: String,
    /// One `\tchunk:line: in ...` line per frame, innermost first; empty
    /// when no script frame was involved.
    pub traceback: String,
    /// The error object itself, what pcall would have returned. Rooted by
    /// the Ctx that produced the error.
    pub value: Value,
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.traceback.is_empty() { write!(f, "{}", self.message) } else { write!(f, "{}\nstack traceback:\n{}", self.message, self.traceback) }
    }
}

impl std::error::Error for ScriptError {}

// Lets a native `?` a failed call: the error object propagates unchanged,
// like an error passing through a Lua C function.
impl From<ScriptError> for VmError {
    fn from(e: ScriptError) -> Self {
        VmError::Thrown(e.value)
    }
}

/// A value kept alive until released, like a slot in Lua's registry. Not
/// Clone, so a slot can't be released twice. A Ref holds no pointer: one
/// that outlives its VM is inert, and using it with a different VM panics.
/// Dropping a Ref without release_ref keeps its value alive until the VM
/// drops.
#[derive(Debug)]
pub struct Ref {
    slot: u32,
    vm_id: u64,
}

/// What a module loader returns for `require`: the source, and the chunk
/// name errors will cite (e.g. the asset path).
pub struct Module {
    pub source: String,
    pub chunk_name: String,
}

pub(crate) type ModuleLoader = Rc<dyn Fn(&mut Ctx<'_>, &str) -> Result<Module, String>>;

pub(crate) fn next_vm_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Vm {
    /// A Ctx for host code outside any native call.
    pub fn ctx(&mut self) -> Ctx<'_> {
        Ctx::new(self)
    }

    /// Makes `require "a.b"` ask `loader` for the source of module "a.b"
    /// instead of reading a/b.umbra from disk. An Err becomes the require
    /// error. The loader gets the Ctx of the require call, so it can reach
    /// the host (Ctx::host) for its asset store.
    pub fn set_module_loader(&mut self, loader: impl Fn(&mut Ctx<'_>, &str) -> Result<Module, String> + 'static) {
        self.module_loader = Some(Rc::new(loader));
    }
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(vm: &'a mut Vm) -> Self {
        let root_base = vm.ctx_roots.len();
        Ctx { vm, root_base }
    }

    pub(crate) fn root_all(&mut self, vals: &[Value]) {
        self.vm.ctx_roots.extend_from_slice(vals);
    }

    /// Roots a value obtained some other way (Vm::exec's results, a
    /// Value built by hand) until this Ctx drops, like the values Ctx
    /// methods return. Do it before anything that can run script.
    pub fn root(&mut self, v: Value) -> Value {
        self.vm.ctx_roots.push(v);
        v
    }

    // Runs `f`, turning its error into a ScriptError with the traceback
    // the error was raised with. The VM keeps its record of that traceback:
    // a native that passes the error on (`?`) hands it to the frames
    // further out, which extend it instead of starting over.
    fn guard<T>(&mut self, f: impl FnOnce(&mut Vm) -> VmResult<T>) -> Result<T, ScriptError> {
        let serial = self.vm.err_serial;
        let result = f(self.vm);
        result.map_err(|e| {
            let traceback = self.vm.delivered_traceback(serial).unwrap_or_default().to_owned();
            self.error(e, traceback)
        })
    }

    // A runtime error with no position yet (raised before any script ran,
    // like calling a nil value, or by a Ctx method) gets the position of
    // the script that called the native, as the native's own errors do.
    fn error(&mut self, e: VmError, traceback: String) -> ScriptError {
        let (message, value) = match e {
            VmError::Thrown(v) if v.is_string() => (self.vm.gc.str(v).to_owned(), v),
            VmError::Thrown(v) => (format!("{v}"), v),
            other => {
                let message = self.vm.positioned(other.to_string());
                let v = self.vm.intern(&message);
                (message, v)
            }
        };
        self.root(value);
        ScriptError { message, traceback, value }
    }

    fn type_error(&mut self, who: &str, expected: &str, got: Value) -> ScriptError {
        let e = VmError::RuntimeError(format!("{who}: {expected} expected, got {}", got.type_name()));
        self.error(e, String::new())
    }

    /// Lends `host` to every native that runs inside `f`; they reach it with
    /// host::<H>(). The loan ends when with_host returns, even by panic, so
    /// no native can see `host` after that. Nested loans stack; host::<H>()
    /// finds the innermost one of type H.
    pub fn with_host<H: Any, R>(&mut self, host: &mut H, f: impl FnOnce(&mut Ctx<'_>) -> R) -> R {
        // The pointer is the only path to `host` while it is on the stack:
        // with_host holds the &mut for the whole call and never touches it.
        self.vm.hosts.push((TypeId::of::<H>(), host as *mut H as *mut ()));
        let depth = self.vm.hosts.len();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        self.vm.hosts.truncate(depth - 1);
        match result {
            Ok(r) => r,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    /// The innermost host of type H lent by with_host, if any.
    pub fn host<H: Any>(&mut self) -> Option<&mut H> {
        let id = TypeId::of::<H>();
        let &(_, ptr) = self.vm.hosts.iter().rev().find(|(t, _)| *t == id)?;
        // SAFETY: the entry is live, so the with_host that pushed it is still
        // on the stack holding the only other &mut H. The result borrows self, so
        // it is gone before this Ctx can call anything (a nested native, a
        // second host()) that could hand out the same H again.
        Some(unsafe { &mut *(ptr as *mut H) })
    }

    /// An interned string.
    pub fn new_string(&mut self, s: &str) -> Value {
        let v = self.vm.intern(s);
        self.root(v)
    }

    /// An integer; values outside the 48-bit inline range are heap-boxed,
    /// which is why this needs the VM (Value::int alone would truncate).
    pub fn new_int(&mut self, n: i64) -> Value {
        let v = self.vm.make_int(n);
        self.root(v)
    }

    pub fn new_table(&mut self) -> Value {
        let t = self.vm.alloc_table();
        self.root(t)
    }

    /// A native function value, as Vm::make_cfn_val makes, rooted like the
    /// other values a Ctx returns. A Value the Rust closure captures is
    /// invisible to the collector (rule 4); use new_closure for those.
    pub fn new_function(&mut self, f: impl Fn(&mut Ctx<'_>, &[Value]) -> VmResult<Vec<Value>> + 'static) -> Value {
        let v = self.vm.make_cfn_val(f);
        self.root(v)
    }

    /// A native closing over `upvals`, as Vm::make_cfn_closure makes: every
    /// call gets them as its third argument, and the collector traces them
    /// for as long as the native lives. They are fixed at creation; mutable
    /// state goes in a table upvalue. Rooted like new_function.
    pub fn new_closure(&mut self, f: impl Fn(&mut Ctx<'_>, &[Value], &[Value]) -> VmResult<Vec<Value>> + 'static, upvals: &[Value]) -> Value {
        let v = self.vm.make_cfn_closure(upvals, f);
        self.root(v)
    }

    /// The contents of a string value. The borrow keeps the Ctx from running
    /// anything that could collect, so the &str can't dangle.
    pub fn to_str(&self, v: Value) -> Option<&str> {
        if v.is_string() { Some(self.vm.gc.str(v)) } else { None }
    }

    pub fn get_global(&mut self, name: &str) -> Value {
        let v = self.vm.get_global(name);
        self.root(v)
    }

    pub fn set_global(&mut self, name: &str, v: Value) {
        self.vm.set_global(name, v);
    }

    /// `obj[key]` with __index, like the script expression. Strings index
    /// the string library, so methods resolve.
    pub fn get(&mut self, obj: Value, key: Value) -> Result<Value, ScriptError> {
        let v = if obj.is_table() {
            self.guard(|vm| vm.table_index_chain(obj, key))?
        } else if obj.is_string() && self.vm.string_lib.is_table() {
            self.vm.gc.table(self.vm.string_lib).raw_get(key)
        } else {
            let e = VmError::RuntimeError(format!("attempt to index a {} value", obj.type_name()));
            return Err(self.error(e, String::new()));
        };
        Ok(self.root(v))
    }

    /// `obj[key] = val` with __newindex.
    pub fn set(&mut self, obj: Value, key: Value, val: Value) -> Result<(), ScriptError> {
        if !obj.is_table() {
            let e = VmError::RuntimeError(format!("attempt to index a {} value", obj.type_name()));
            return Err(self.error(e, String::new()));
        }
        self.guard(|vm| vm.table_newindex_chain(obj, key, val))
    }

    pub fn get_field(&mut self, obj: Value, name: &str) -> Result<Value, ScriptError> {
        let key = self.new_string(name);
        self.get(obj, key)
    }

    pub fn set_field(&mut self, obj: Value, name: &str, val: Value) -> Result<(), ScriptError> {
        let key = self.new_string(name);
        self.set(obj, key, val)
    }

    /// rawget: no metamethods.
    pub fn raw_get(&mut self, t: Value, key: Value) -> Result<Value, ScriptError> {
        if !t.is_table() {
            return Err(self.type_error("raw_get", "table", t));
        }
        let v = self.vm.gc.table(t).raw_get(key);
        Ok(self.root(v))
    }

    /// rawset: no metamethods. A none key is an error (`index is none`),
    /// as in script.
    pub fn raw_set(&mut self, t: Value, key: Value, val: Value) -> Result<(), ScriptError> {
        if !t.is_table() {
            return Err(self.type_error("raw_set", "table", t));
        }
        self.vm.raw_store(t, key, val).map_err(|e| self.error(e, String::new()))
    }

    /// setmetatable(t, mt); None clears it. Attaching a metatable with __gc
    /// makes `t` finalizable, as in script.
    pub fn set_metatable(&mut self, t: Value, mt: Option<Value>) -> Result<(), ScriptError> {
        if !t.is_table() {
            return Err(self.type_error("set_metatable", "table", t));
        }
        let mt = mt.unwrap_or(Value::nil());
        if !mt.is_table() && !mt.is_nil() {
            return Err(self.type_error("set_metatable", "table", mt));
        }
        self.vm.set_metatable(t, mt);
        Ok(())
    }

    /// The raw metatable of a table (ignores __metatable); None for
    /// non-tables and tables without one.
    pub fn metatable(&mut self, t: Value) -> Option<Value> {
        if !t.is_table() {
            return None;
        }
        let mt = self.vm.gc.table(t).metatable?;
        Some(self.root(Value::table(mt as *mut u8)))
    }

    /// The entry after `key` (none for the first), or None at the end: the
    /// script `next`. Iterating while assigning to existing keys is fine;
    /// adding keys mid-iteration may skip or repeat entries.
    pub fn next(&mut self, t: Value, key: Value) -> Result<Option<(Value, Value)>, ScriptError> {
        if !t.is_table() {
            return Err(self.type_error("next", "table", t));
        }
        Ok(self.vm.table_next(t, key).map(|(k, v)| (self.root(k), self.root(v))))
    }

    /// `#v` without __len: a table's border, or a string's or bytes' length.
    pub fn raw_len(&mut self, v: Value) -> Result<i64, ScriptError> {
        if v.is_table() {
            return Ok(self.vm.gc.table(v).length());
        }
        if v.is_string() {
            return Ok(self.vm.gc.str(v).len() as i64);
        }
        if v.is_bytes() {
            return Ok(self.vm.gc.bytes(v).len as i64);
        }
        Err(self.type_error("raw_len", "table, string or bytes", v))
    }

    /// Calls `f` (a function, a native, or a table with __call) in protected
    /// mode. Step limit, object cap and panic containment apply as for any
    /// script call.
    pub fn call(&mut self, f: Value, args: &[Value]) -> Result<Vec<Value>, ScriptError> {
        let results = self.guard(|vm| vm.call_value_isolated(f, args))?;
        self.root_all(&results);
        Ok(results)
    }

    /// `obj:name(args...)`: looks `name` up through __index and calls it
    /// with `obj` prepended.
    pub fn call_method(&mut self, obj: Value, name: &str, args: &[Value]) -> Result<Vec<Value>, ScriptError> {
        let method = self.get_field(obj, name)?;
        if method.is_nil() {
            let e = VmError::RuntimeError(format!("attempt to call a none value (method '{name}')"));
            return Err(self.error(e, String::new()));
        }
        let mut full = Vec::with_capacity(args.len() + 1);
        full.push(obj);
        full.extend_from_slice(args);
        self.call(method, &full)
    }

    /// Compiles `source` into a function without running it. Runtime errors
    /// from it name `chunk_name` (`scripts/spin.umbra:12: ...`), and so do
    /// syntax errors (`scripts/spin.umbra: line 12: ...`). The function is
    /// a value like any other: its code is freed once nothing reaches it or
    /// a function it made, so reloading a script doesn't accumulate code.
    pub fn load_chunk(&mut self, source: &str, chunk_name: &str) -> Result<Value, ScriptError> {
        // Compiling touches no VM state until it succeeds, apart from the
        // nesting count, so a compiler panic is contained without poisoning
        // the VM once that count is put back.
        let nesting = self.vm.nesting;
        let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.vm.load_proto(source, Some(chunk_name.to_owned()))));
        let e = match compiled {
            Ok(Ok(f)) => return Ok(self.root(f)),
            Ok(Err(msg)) => VmError::RuntimeError(format!("{chunk_name}: {msg}")),
            Err(payload) => {
                self.vm.nesting = nesting;
                VmError::RuntimeError(format!("internal error (panic): {}", crate::vm::panic_message(&*payload)))
            }
        };
        Err(self.error(e, String::new()))
    }

    /// Keeps `v` alive until release_ref.
    pub fn make_ref(&mut self, v: Value) -> Ref {
        let slot = match self.vm.free_refs.pop() {
            Some(slot) => {
                self.vm.refs[slot as usize] = v;
                slot
            }
            None => {
                self.vm.refs.push(v);
                (self.vm.refs.len() - 1) as u32
            }
        };
        Ref { slot, vm_id: self.vm.id }
    }

    pub fn get_ref(&mut self, r: &Ref) -> Value {
        self.check_ref(r);
        let v = self.vm.refs[r.slot as usize];
        self.root(v)
    }

    /// Drops the root; the value is collectable once nothing else holds it.
    pub fn release_ref(&mut self, r: Ref) {
        self.check_ref(&r);
        self.vm.refs[r.slot as usize] = Value::nil();
        self.vm.free_refs.push(r.slot);
    }

    fn check_ref(&self, r: &Ref) {
        assert_eq!(r.vm_id, self.vm.id, "umbra: Ref used with a VM other than the one that made it");
    }
}

impl Drop for Ctx<'_> {
    fn drop(&mut self) {
        self.vm.ctx_roots.truncate(self.root_base);
    }
}
