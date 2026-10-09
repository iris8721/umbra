#![allow(non_snake_case)] // U is the Lua-style state pointer convention throughout this file

use std::ffi::{c_char, c_double, c_int, CStr};
use crate::vm::{Vm, VmError, VmResult};
use crate::value::Value;

// The host's value stack (vm.host_stack) and the current C-call frame base
// (vm.api_base) both live on the Vm, so the state is nothing but a Vm. That
// is what lets umbra_register hand a C function a state pointer derived from
// the native's own &mut Vm (see there).
#[repr(transparent)]
pub struct UmbraState {
    pub vm: Vm,
}

// Callback ABI: receives the state, reads args via umbra_to*, pushes results
// via umbra_push*, returns the number of results.
pub type UmbraCFunction = Option<unsafe extern "C" fn(*mut UmbraState) -> c_int>;

#[repr(C)]
pub enum UmbraStatus {
    Ok = 0,
    RuntimeError = 1,
    SyntaxError = 2,
}

impl UmbraState {
    fn new_inner() -> Self {
        UmbraState { vm: Vm::new() }
    }

    fn stack(&self) -> &Vec<Value> { &self.vm.host_stack }
    fn stack_mut(&mut self) -> &mut Vec<Value> { &mut self.vm.host_stack }

    fn abs_idx(&self, idx: c_int) -> Option<usize> {
        let len = self.stack().len();
        let api_base = self.vm.api_base;
        if idx > 0 {
            let i = api_base + (idx as usize - 1);
            if i < len { Some(i) } else { None }
        } else if idx < 0 {
            let i = len as isize + idx as isize;
            if i >= api_base as isize { Some(i as usize) } else { None }
        } else {
            None
        }
    }

    fn get(&self, idx: c_int) -> Value {
        self.abs_idx(idx).map(|i| self.stack()[i]).unwrap_or(Value::nil())
    }

    fn push_error(&mut self, msg: &str) -> c_int {
        let v = self.vm.intern_pub(msg);
        self.stack_mut().push(v);
        UmbraStatus::RuntimeError as c_int
    }

    fn push_status_error(&mut self, msg: &str, status: UmbraStatus) -> c_int {
        let v = self.vm.intern_pub(msg);
        self.stack_mut().push(v);
        status as c_int
    }

    // A thrown error object keeps its identity for the host (like Lua's
    // lua_pcall leaving the error value on the stack); other errors are
    // pushed as message strings.
    fn push_vm_error(&mut self, e: VmError) -> c_int {
        match e {
            VmError::Thrown(v) => {
                self.stack_mut().push(v);
                UmbraStatus::RuntimeError as c_int
            }
            other => self.push_error(&other.to_string()),
        }
    }
}

// Every entry point takes U on the C API's contract (umbra.h): it came from
// umbra_newstate, hasn't been closed, and one thread at a time uses it.
// Inside a native it is the pointer umbra_register derives from the
// native's own &mut Vm. The SAFETY comments below call this "the U
// contract". A string argument is NULL or NUL-terminated and outlives the
// call ("the string contract").

// SAFETY (callers): the string contract.
unsafe fn cstr<'a>(s: *const c_char) -> Option<std::borrow::Cow<'a, str>> {
    // SAFETY: per the caller.
    if s.is_null() { None } else { Some(unsafe { CStr::from_ptr(s) }.to_string_lossy()) }
}

/// Create a new state with the standard library loaded.
#[unsafe(no_mangle)]
pub extern "C" fn umbra_newstate() -> *mut UmbraState {
    Box::into_raw(Box::new(UmbraState::new_inner()))
}

/// Destroy a state and free everything it owns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_close(U: *mut UmbraState) {
    // SAFETY: the U contract; U came from Box::into_raw in umbra_newstate.
    if !U.is_null() { unsafe { drop(Box::from_raw(U)); } }
}

/// Number of values on the stack in the current C-call frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_gettop(U: *const UmbraState) -> c_int {
    // SAFETY: the U contract.
    let s = unsafe { &*U };
    s.stack().len().saturating_sub(s.vm.api_base) as c_int
}

/// Set the stack top: idx >= 0 grows with nils (clamped), idx < 0 pops.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_settop(U: *mut UmbraState, idx: c_int) {
    // SAFETY: the U contract.
    let s = unsafe { &mut *U };
    let api_base = s.vm.api_base;
    if idx >= 0 {
        // Clamped: an extreme idx would otherwise force a multi-GB allocation abort.
        let new_len = (api_base + idx as usize).min(api_base + crate::vm::MAX_ALLOC_LEN);
        s.stack_mut().resize(new_len, Value::nil());
    } else {
        let new_len = (s.stack().len() as isize + idx as isize).max(api_base as isize) as usize;
        s.stack_mut().truncate(new_len);
    }
}

/// Pop n values from the stack.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pop(U: *mut UmbraState, n: c_int) {
    if n <= 0 { return; }
    // SAFETY: the U contract.
    unsafe { umbra_settop(U, -n); }
}

/// Push nil (the `none` value). Stack: +1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushnil(U: *mut UmbraState) {
    // SAFETY: the U contract.
    unsafe { (*U).stack_mut().push(Value::nil()); }
}

/// Push a float. Stack: +1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushnumber(U: *mut UmbraState, n: c_double) {
    // SAFETY: the U contract.
    unsafe { (*U).stack_mut().push(Value::float(n)); }
}

/// Push an integer (heap-boxed if outside the 48-bit inline range). Stack: +1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushinteger(U: *mut UmbraState, n: i64) {
    // SAFETY: the U contract.
    let state = unsafe { &mut *U };
    let v = state.vm.make_int(n);
    state.stack_mut().push(v);
}

/// Push a boolean (b != 0). Stack: +1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushboolean(U: *mut UmbraState, b: c_int) {
    // SAFETY: the U contract.
    unsafe { (*U).stack_mut().push(Value::bool(b != 0)); }
}

/// Push a NUL-terminated string (interned); NULL pushes nil. Stack: +1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushstring(U: *mut UmbraState, s: *const c_char) {
    // SAFETY: the U contract.
    let state = unsafe { &mut *U };
    // SAFETY: the string contract.
    let v = match unsafe { cstr(s) } {
        Some(rs) => state.vm.intern_pub(rs.as_ref()),
        None => Value::nil(),
    };
    state.stack_mut().push(v);
}

/// Type tag of the value at idx:
/// 0=nil 1=boolean 2=integer 3=float 4=string 5=table 6=function 7=coroutine.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_type(U: *const UmbraState, idx: c_int) -> c_int {
    // SAFETY: the U contract.
    let v = unsafe { (*U).get(idx) };
    if v.is_nil()    { return 0; }
    if v.is_bool()   { return 1; }
    if v.is_int_like() { return 2; }
    if v.is_float()  { return 3; }
    if v.is_string() { return 4; }
    if v.is_table()  { return 5; }
    if v.is_coroutine() { return 7; }
    6
}

/// 1 if the value at idx is an integer or float, else 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_isnumber(U: *const UmbraState, idx: c_int) -> c_int {
    // SAFETY: the U contract.
    let v = unsafe { (*U).get(idx) };
    (v.is_int_like() || v.is_float()) as c_int
}

/// 1 if the value at idx is a string, else 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_isstring(U: *const UmbraState, idx: c_int) -> c_int {
    // SAFETY: the U contract.
    unsafe { (*U).get(idx) }.is_string() as c_int
}

/// 1 if the value at idx is nil, else 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_isnil(U: *const UmbraState, idx: c_int) -> c_int {
    // SAFETY: the U contract.
    unsafe { (*U).get(idx) }.is_nil() as c_int
}

/// 1 if the value at idx is callable (script function or C function), else 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_isfunction(U: *const UmbraState, idx: c_int) -> c_int {
    // SAFETY: the U contract.
    let v = unsafe { (*U).get(idx) };
    (v.is_userdata() || v.is_closure()) as c_int
}

/// Value at idx as a double (integers widen); 0 for non-numbers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_tonumber(U: *const UmbraState, idx: c_int) -> c_double {
    // SAFETY: the U contract.
    let v = unsafe { (*U).get(idx) };
    if v.is_float() { v.as_float().unwrap() }
    else if v.is_int_like() { v.as_int().unwrap() as c_double }
    else { 0.0 }
}

/// Value at idx as an i64 (floats truncate); 0 for non-numbers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_tointeger(U: *const UmbraState, idx: c_int) -> i64 {
    // SAFETY: the U contract.
    let v = unsafe { (*U).get(idx) };
    if v.is_int_like() { v.as_int().unwrap() }
    else if v.is_float() { v.as_float().unwrap() as i64 }
    else { 0 }
}

/// Value at idx as a boolean under umbra truthiness (nil and false are false).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_toboolean(U: *const UmbraState, idx: c_int) -> c_int {
    // SAFETY: the U contract.
    unsafe { (*U).get(idx) }.is_truthy() as c_int
}

// Safety: the returned pointer aliases the interned string's heap allocation
// and stays valid while the value is on the API stack (or otherwise reachable).
// RtString's trailing NUL makes this safe as a C string, modulo the usual
// caveat that an embedded NUL in the Umbra string itself would truncate it.
/// Value at idx as a C string, or NULL if it isn't a string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_tostring(U: *const UmbraState, idx: c_int) -> *const c_char {
    // SAFETY: the U contract.
    let s = unsafe { &*U };
    let v = s.get(idx);
    if v.is_string() {
        s.vm.gc.str_c_ptr(v) as *const c_char
    } else {
        std::ptr::null()
    }
}

/// Push the global named `name` (nil if unset or name is NULL). Stack: +1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_getglobal(U: *mut UmbraState, name: *const c_char) {
    // SAFETY: the U contract.
    let state = unsafe { &mut *U };
    // SAFETY: the string contract.
    let v = match unsafe { cstr(name) } {
        Some(rs) => state.vm.get_global(rs.as_ref()),
        None => Value::nil(),
    };
    state.stack_mut().push(v);
}

/// Pop the top value and store it as the global named `name`. Stack: -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_setglobal(U: *mut UmbraState, name: *const c_char) {
    // SAFETY: the U contract.
    let state = unsafe { &mut *U };
    // Clamp to the current C frame like settop does: with an empty frame the
    // pop must not steal a slot belonging to the caller's stack.
    let v = if state.stack().len() > state.vm.api_base {
        state.stack_mut().pop().unwrap_or(Value::nil())
    } else {
        Value::nil()
    };
    // SAFETY: the string contract.
    if let Some(rs) = unsafe { cstr(name) } {
        state.vm.set_global(rs.as_ref(), v);
    }
}

/// Compile and run a source string; on error pushes the error value and
/// returns UMBRA_ERR_SYNTAX (parse/compile) or UMBRA_ERR_RUNTIME.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_dostring(U: *mut UmbraState, src: *const c_char) -> c_int {
    // SAFETY: the U contract.
    let state = unsafe { &mut *U };
    if state.vm.poisoned {
        return state.push_error("umbra_dostring: VM is poisoned by a previous internal error");
    }
    // SAFETY: the string contract.
    let rs = match unsafe { cstr(src) } {
        Some(rs) => rs,
        None => return state.push_error("umbra_dostring: null source"),
    };
    // An unwind must never reach this extern "C" boundary uncaught.
    // Inlined run_with_vm so parse/compile failures can report
    // UMBRA_ERR_SYNTAX instead of collapsing into a runtime error.
    let nesting = state.vm.nesting;
    let outcome: Result<Result<(), (UmbraStatus, VmError)>, _> =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (block, parse_errs) = crate::parse(rs.as_ref());
            if !parse_errs.is_empty() {
                let msg = parse_errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
                return Err((UmbraStatus::SyntaxError, VmError::RuntimeError(msg)));
            }
            match crate::compile(block, None) {
                Err(e) => Err((UmbraStatus::SyntaxError, VmError::RuntimeError(e.to_string()))),
                Ok(chunk) => state.vm.exec(chunk)
                    .map(|_| ())
                    .map_err(|e| (UmbraStatus::RuntimeError, e)),
            }
        }));
    match outcome {
        Ok(Ok(())) => UmbraStatus::Ok as c_int,
        Ok(Err((status, e))) => match e {
            VmError::Thrown(v) => {
                state.stack_mut().push(v);
                status as c_int
            }
            other => state.push_status_error(&other.to_string(), status),
        },
        Err(payload) => {
            state.vm.poison(nesting);
            state.push_error(&format!("internal error (panic): {}", crate::vm::panic_message(&*payload)))
        }
    }
}

/// Call the function at -(nargs+1), popping it and its args; pushes nres
/// results (-1 = all) on success, or the error value on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pcall(U: *mut UmbraState, nargs: c_int, nres: c_int) -> c_int {
    // SAFETY: the U contract.
    let state = unsafe { &mut *U };
    if state.vm.poisoned {
        return state.push_error("umbra_pcall: VM is poisoned by a previous internal error");
    }
    if nargs < 0 {
        return state.push_error("umbra_pcall: negative argument count");
    }
    let nargs = nargs as usize;
    let avail = state.stack().len() - state.vm.api_base;
    if avail < nargs + 1 {
        return state.push_error("umbra_pcall: stack underflow");
    }
    let fn_idx = state.stack().len() - nargs - 1;
    let fn_val = state.stack()[fn_idx];
    let args: Vec<Value> = state.stack_mut().drain(fn_idx..).skip(1).collect();

    // An unwind must never reach this extern "C" boundary uncaught.
    let nesting = state.vm.nesting;
    let result: VmResult<Vec<Value>> = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.vm.call_value_isolated(fn_val, &args)
    })) {
        Ok(r) => r,
        Err(payload) => {
            state.vm.poison(nesting);
            Err(VmError::RuntimeError(format!("internal error (panic): {}", crate::vm::panic_message(&*payload))))
        }
    };
    match result {
        Ok(results) => {
            let fill = if nres < 0 { results.len() } else { (nres as usize).min(crate::vm::MAX_ALLOC_LEN) };
            for i in 0..fill {
                state.stack_mut().push(if i < results.len() { results[i] } else { Value::nil() });
            }
            UmbraStatus::Ok as c_int
        }
        Err(e) => state.push_vm_error(e),
    }
}

/// Run a full GC collection cycle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_gc_collect(U: *mut UmbraState) {
    // SAFETY: the U contract.
    unsafe { (*U).vm.gc_collect(); }
}

/// Set the object-count threshold that triggers automatic collection.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_gc_setstep(U: *mut UmbraState, threshold: usize) {
    // SAFETY: the U contract.
    unsafe { (*U).vm.gc.threshold = threshold; }
}

/// Bounds how many bytecode instructions a script may run before erroring
/// out (0 = unlimited); host-only, resets the count. Call before dostring/
/// pcall-ing a script you don't fully trust to terminate.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_set_step_limit(U: *mut UmbraState, limit: u64) {
    // SAFETY: the U contract.
    unsafe { (*U).vm.set_step_limit(limit); }
}

/// Host-only hard ceiling on live GC-tracked objects (0 = unlimited); exceeding
/// it after a collection attempt is a real error, unlike umbra_gc_setstep
/// which only tunes when collection is attempted.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_set_max_objects(U: *mut UmbraState, limit: usize) {
    // SAFETY: the U contract.
    unsafe { (*U).vm.set_max_objects(limit); }
}

/// Number of live GC-tracked objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_gc_livecount(U: *const UmbraState) -> usize {
    // SAFETY: the U contract.
    unsafe { (*U).vm.gc.live_count() }
}

/// Register f as a global callable; f gets a fresh frame with args at 1..n
/// and returns the number of results pushed (negative = error).
// The wrapper gives f its own frame: args are pushed above api_base, and
// whatever f leaves on the stack beyond its declared results is discarded.
// A returned count larger than what f actually pushed is clamped to the frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_register(
    U: *mut UmbraState,
    name: *const c_char,
    f: UmbraCFunction,
) {
    // SAFETY: the U contract.
    let state = unsafe { &mut *U };
    // Option<fn> is ABI-identical to a raw pointer: a C host passing NULL
    // arrives as None and must not be registered (calling it would jump to 0).
    let Some(f) = f else { return };
    // SAFETY: the string contract.
    let rs = match unsafe { cstr(name) } {
        Some(rs) => rs.into_owned(),
        None => return,
    };
    // The state pointer f receives is made from the &mut Vm the interpreter
    // lent this native, not captured here at registration: the interpreter
    // holds its own &mut Vm while the native runs, and a pointer from
    // outside that borrow would alias it. UmbraState is a transparent Vm,
    // so the cast is the same object. Every umbra_* call f makes on it is a
    // reborrow of this one, which ends before the native returns.
    state.vm.set_global_cfn(&rs, move |cx, args| {
        // SAFETY: UmbraState is a repr(transparent) Vm, and this is the
        // native's own borrow (see above).
        let s = unsafe { &mut *(&mut *cx.vm as *mut Vm).cast::<UmbraState>() };
        let old_base = s.vm.api_base;
        s.vm.api_base = s.stack().len();
        s.stack_mut().extend_from_slice(args);
        // SAFETY: f is the host's umbra_CFunction, called on the U contract.
        let nret = unsafe { f(s) };
        let results = if nret > 0 {
            let start = s.stack().len().saturating_sub(nret as usize).max(s.vm.api_base);
            s.stack_mut().drain(start..).collect::<Vec<_>>()
        } else {
            vec![]
        };
        let api_base = s.vm.api_base;
        s.stack_mut().truncate(api_base);
        s.vm.api_base = old_base;
        if nret < 0 {
            Err(VmError::RuntimeError("C function signalled error".into()))
        } else {
            Ok(results)
        }
    });
}
