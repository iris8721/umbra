#![allow(non_snake_case)] // U is the Lua-style state pointer convention throughout this file

use std::ffi::{c_char, c_double, c_int, CStr};
use crate::vm::{Vm, VmError, VmResult};
use crate::value::Value;

// The host's value stack lives on the Vm (vm.host_stack) so the collector
// sees it as a root; this struct only adds the current C-call frame base.
pub struct UmbraState {
    pub vm: Vm,
    // Index into the stack where the current C function's args begin (Lua-style frame base).
    api_base: usize,
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
        UmbraState { vm: Vm::new(), api_base: 0 }
    }

    fn stack(&self) -> &Vec<Value> { &self.vm.host_stack }
    fn stack_mut(&mut self) -> &mut Vec<Value> { &mut self.vm.host_stack }

    fn abs_idx(&self, idx: c_int) -> Option<usize> {
        let len = self.stack().len();
        if idx > 0 {
            let i = self.api_base + (idx as usize - 1);
            if i < len { Some(i) } else { None }
        } else if idx < 0 {
            let i = len as isize + idx as isize;
            if i >= self.api_base as isize { Some(i as usize) } else { None }
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

unsafe fn cstr<'a>(s: *const c_char) -> Option<std::borrow::Cow<'a, str>> {
    if s.is_null() { None } else { Some(unsafe { CStr::from_ptr(s) }.to_string_lossy()) }
}

#[unsafe(no_mangle)]
pub extern "C" fn umbra_newstate() -> *mut UmbraState {
    Box::into_raw(Box::new(UmbraState::new_inner()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_close(U: *mut UmbraState) {
    if !U.is_null() { unsafe { drop(Box::from_raw(U)); } }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_gettop(U: *const UmbraState) -> c_int {
    let s = unsafe { &*U };
    s.stack().len().saturating_sub(s.api_base) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_settop(U: *mut UmbraState, idx: c_int) {
    let s = unsafe { &mut *U };
    let api_base = s.api_base;
    if idx >= 0 {
        // Clamped: an extreme idx would otherwise force a multi-GB allocation abort.
        let new_len = (api_base + idx as usize).min(api_base + crate::vm::MAX_ALLOC_LEN);
        s.stack_mut().resize(new_len, Value::nil());
    } else {
        let new_len = (s.stack().len() as isize + idx as isize).max(api_base as isize) as usize;
        s.stack_mut().truncate(new_len);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pop(U: *mut UmbraState, n: c_int) {
    if n <= 0 { return; }
    unsafe { umbra_settop(U, -n); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushnil(U: *mut UmbraState) {
    unsafe { (*U).stack_mut().push(Value::nil()); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushnumber(U: *mut UmbraState, n: c_double) {
    unsafe { (*U).stack_mut().push(Value::float(n)); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushinteger(U: *mut UmbraState, n: i64) {
    let state = unsafe { &mut *U };
    let v = state.vm.make_int(n);
    state.stack_mut().push(v);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushboolean(U: *mut UmbraState, b: c_int) {
    unsafe { (*U).stack_mut().push(Value::bool(b != 0)); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushstring(U: *mut UmbraState, s: *const c_char) {
    let state = unsafe { &mut *U };
    let v = match unsafe { cstr(s) } {
        Some(rs) => state.vm.intern_pub(rs.as_ref()),
        None => Value::nil(),
    };
    state.stack_mut().push(v);
}

// Stable FFI contract: 0=nil 1=boolean 2=integer 3=float 4=string 5=table 6=function 7=coroutine
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_type(U: *const UmbraState, idx: c_int) -> c_int {
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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_isnumber(U: *const UmbraState, idx: c_int) -> c_int {
    let v = unsafe { (*U).get(idx) };
    (v.is_int_like() || v.is_float()) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_isstring(U: *const UmbraState, idx: c_int) -> c_int {
    unsafe { (*U).get(idx) }.is_string() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_isnil(U: *const UmbraState, idx: c_int) -> c_int {
    unsafe { (*U).get(idx) }.is_nil() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_isfunction(U: *const UmbraState, idx: c_int) -> c_int {
    let v = unsafe { (*U).get(idx) };
    (v.is_userdata() || v.is_closure()) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_tonumber(U: *const UmbraState, idx: c_int) -> c_double {
    let v = unsafe { (*U).get(idx) };
    if v.is_float() { v.as_float().unwrap() }
    else if v.is_int_like() { v.as_int().unwrap() as c_double }
    else { 0.0 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_tointeger(U: *const UmbraState, idx: c_int) -> i64 {
    let v = unsafe { (*U).get(idx) };
    if v.is_int_like() { v.as_int().unwrap() }
    else if v.is_float() { v.as_float().unwrap() as i64 }
    else { 0 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_toboolean(U: *const UmbraState, idx: c_int) -> c_int {
    unsafe { (*U).get(idx) }.is_truthy() as c_int
}

// Safety: the returned pointer aliases the interned string's heap allocation
// and stays valid while the value is on the API stack (or otherwise reachable).
// RtString's trailing NUL makes this safe as a C string, modulo the usual
// caveat that an embedded NUL in the Umbra string itself would truncate it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_tostring(U: *const UmbraState, idx: c_int) -> *const c_char {
    let v = unsafe { (*U).get(idx) };
    if v.is_string() {
        let ptr = v.as_string().unwrap() as *const crate::vm::RtString;
        unsafe { (*ptr).as_c_ptr() as *const c_char }
    } else {
        std::ptr::null()
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_getglobal(U: *mut UmbraState, name: *const c_char) {
    let state = unsafe { &mut *U };
    let v = match unsafe { cstr(name) } {
        Some(rs) => state.vm.get_global(rs.as_ref()),
        None => Value::nil(),
    };
    state.stack_mut().push(v);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_setglobal(U: *mut UmbraState, name: *const c_char) {
    let state = unsafe { &mut *U };
    // Clamp to the current C frame like settop does: with an empty frame the
    // pop must not steal a slot belonging to the caller's stack.
    let v = if state.stack().len() > state.api_base {
        state.stack_mut().pop().unwrap_or(Value::nil())
    } else {
        Value::nil()
    };
    if let Some(rs) = unsafe { cstr(name) } {
        state.vm.set_global(rs.as_ref(), v);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_dostring(U: *mut UmbraState, src: *const c_char) -> c_int {
    let state = unsafe { &mut *U };
    if state.vm.poisoned {
        return state.push_error("umbra_dostring: VM is poisoned by a previous internal error");
    }
    let rs = match unsafe { cstr(src) } {
        Some(rs) => rs,
        None => return state.push_error("umbra_dostring: null source"),
    };
    // An unwind must never reach this extern "C" boundary uncaught.
    // Inlined run_with_vm so parse/compile failures can report
    // UMBRA_ERR_SYNTAX instead of collapsing into a runtime error.
    let outcome: Result<Result<(), (UmbraStatus, VmError)>, _> =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (block, parse_errs) = crate::parse(rs.as_ref());
            if !parse_errs.is_empty() {
                let msg = parse_errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
                return Err((UmbraStatus::SyntaxError, VmError::RuntimeError(msg)));
            }
            match crate::compile(block, None) {
                Err(e) => Err((UmbraStatus::SyntaxError, VmError::RuntimeError(e.to_string()))),
                Ok(proto) => state.vm.exec_owned(proto)
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
            state.vm.poisoned = true;
            state.push_error(&format!("internal error (panic): {}", crate::vm::panic_message(&*payload)))
        }
    }
}

// Convention: call the function at `-(nargs+1)`, popping it and its args.
// On success pushes `nres` results (-1 = all); on error pushes the error message.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pcall(U: *mut UmbraState, nargs: c_int, nres: c_int) -> c_int {
    let state = unsafe { &mut *U };
    if state.vm.poisoned {
        return state.push_error("umbra_pcall: VM is poisoned by a previous internal error");
    }
    if nargs < 0 {
        return state.push_error("umbra_pcall: negative argument count");
    }
    let nargs = nargs as usize;
    let avail = state.stack().len() - state.api_base;
    if avail < nargs + 1 {
        return state.push_error("umbra_pcall: stack underflow");
    }
    let fn_idx = state.stack().len() - nargs - 1;
    let fn_val = state.stack()[fn_idx];
    let args: Vec<Value> = state.stack_mut().drain(fn_idx..).skip(1).collect();

    // An unwind must never reach this extern "C" boundary uncaught.
    let result: VmResult<Vec<Value>> = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.vm.call_value_isolated(fn_val, &args)
    })) {
        Ok(r) => r,
        Err(payload) => {
            state.vm.poisoned = true;
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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_gc_collect(U: *mut UmbraState) {
    unsafe { (*U).vm.gc_collect(); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_gc_setstep(U: *mut UmbraState, threshold: usize) {
    unsafe { (*U).vm.gc.threshold = threshold; }
}

/// Bounds how many bytecode instructions a script may run before erroring
/// out (0 = unlimited); host-only, resets the count. Call before dostring/
/// pcall-ing a script you don't fully trust to terminate.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_set_step_limit(U: *mut UmbraState, limit: u64) {
    unsafe { (*U).vm.set_step_limit(limit); }
}

/// Host-only hard ceiling on live GC-tracked objects (0 = unlimited); exceeding
/// it after a collection attempt is a real error, unlike umbra_gc_setstep
/// which only tunes when collection is attempted.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_set_max_objects(U: *mut UmbraState, limit: usize) {
    unsafe { (*U).vm.set_max_objects(limit); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_gc_livecount(U: *const UmbraState) -> usize {
    unsafe { (*U).vm.gc.live_count() }
}

// The wrapper gives f its own frame: args are pushed above api_base, and
// whatever f leaves on the stack beyond its declared results is discarded.
// A returned count larger than what f actually pushed is clamped to the frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_register(
    U: *mut UmbraState,
    name: *const c_char,
    f: UmbraCFunction,
) {
    let state = unsafe { &mut *U };
    // Option<fn> is ABI-identical to a raw pointer: a C host passing NULL
    // arrives as None and must not be registered (calling it would jump to 0).
    let Some(f) = f else { return };
    let rs = match unsafe { cstr(name) } {
        Some(rs) => rs.into_owned(),
        None => return,
    };
    let state_ptr = U;
    state.vm.set_global_cfn(&rs, move |args| {
        let s = unsafe { &mut *state_ptr };
        let old_base = s.api_base;
        s.api_base = s.stack().len();
        s.stack_mut().extend_from_slice(args);
        let nret = unsafe { f(state_ptr) };
        let results = if nret > 0 {
            let start = s.stack().len().saturating_sub(nret as usize).max(s.api_base);
            s.stack_mut().drain(start..).collect::<Vec<_>>()
        } else {
            vec![]
        };
        let api_base = s.api_base;
        s.stack_mut().truncate(api_base);
        s.api_base = old_base;
        if nret < 0 {
            Err(VmError::RuntimeError("C function signalled error".into()))
        } else {
            Ok(results)
        }
    });
}
