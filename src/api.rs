#![allow(non_snake_case)] // U is the Lua-style state pointer convention throughout this file

use std::ffi::{c_char, c_double, c_int, CStr};
use crate::vm::{Vm, VmError, VmResult, get_closure};
use crate::value::Value;

pub struct UmbraState {
    pub vm: Vm,
    stack: Vec<Value>,
    // Index into `stack` where the current C function's args begin (Lua-style frame base).
    api_base: usize,
}

// Callback ABI: receives the state, reads args via umbra_to*, pushes results
// via umbra_push*, returns the number of results.
pub type UmbraCFunction = unsafe extern "C" fn(*mut UmbraState) -> c_int;

#[repr(C)]
pub enum UmbraStatus {
    Ok = 0,
    RuntimeError = 1,
    SyntaxError = 2,
}

impl UmbraState {
    fn new_inner() -> Self {
        UmbraState {
            vm: Vm::new(),
            stack: Vec::with_capacity(32),
            api_base: 0,
        }
    }

    fn abs_idx(&self, idx: c_int) -> Option<usize> {
        if idx > 0 {
            let i = self.api_base + (idx as usize - 1);
            if i < self.stack.len() { Some(i) } else { None }
        } else if idx < 0 {
            let i = self.stack.len() as isize + idx as isize;
            if i >= self.api_base as isize { Some(i as usize) } else { None }
        } else {
            None
        }
    }

    fn get(&self, idx: c_int) -> Value {
        self.abs_idx(idx).map(|i| self.stack[i]).unwrap_or(Value::nil())
    }
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
    unsafe { ((*U).stack.len() - (*U).api_base) as c_int }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_settop(U: *mut UmbraState, idx: c_int) {
    let s = unsafe { &mut *U };
    if idx >= 0 {
        s.stack.resize(s.api_base + idx as usize, Value::nil());
    } else {
        let new_len = (s.stack.len() as isize + idx as isize).max(s.api_base as isize) as usize;
        s.stack.truncate(new_len);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pop(U: *mut UmbraState, n: c_int) {
    unsafe { umbra_settop(U, -n - 1); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushnil(U: *mut UmbraState) {
    unsafe { (*U).stack.push(Value::nil()); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushnumber(U: *mut UmbraState, n: c_double) {
    unsafe { (*U).stack.push(Value::float(n)); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushinteger(U: *mut UmbraState, n: i64) {
    unsafe { (*U).stack.push(Value::int(n)); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushboolean(U: *mut UmbraState, b: c_int) {
    unsafe { (*U).stack.push(Value::bool(b != 0)); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pushstring(U: *mut UmbraState, s: *const c_char) {
    let state = unsafe { &mut *U };
    let rs = unsafe { CStr::from_ptr(s) }.to_string_lossy();
    let v = state.vm.intern_pub(rs.as_ref());
    state.stack.push(v);
}

// Stable FFI contract: 0=nil 1=boolean 2=integer 3=float 4=string 5=table 6=function
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_type(U: *const UmbraState, idx: c_int) -> c_int {
    let v = unsafe { (*U).get(idx) };
    if v.is_nil()    { return 0; }
    if v.is_bool()   { return 1; }
    if v.is_int()    { return 2; }
    if v.is_float()  { return 3; }
    if v.is_string() { return 4; }
    if v.is_table()  { return 5; }
    6
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_isnumber(U: *const UmbraState, idx: c_int) -> c_int {
    let v = unsafe { (*U).get(idx) };
    (v.is_int() || v.is_float()) as c_int
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
    v.is_userdata() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_tonumber(U: *const UmbraState, idx: c_int) -> c_double {
    let v = unsafe { (*U).get(idx) };
    if v.is_float() { v.as_float().unwrap() }
    else if v.is_int() { v.as_int().unwrap() as c_double }
    else { 0.0 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_tointeger(U: *const UmbraState, idx: c_int) -> i64 {
    let v = unsafe { (*U).get(idx) };
    if v.is_int() { v.as_int().unwrap() }
    else if v.is_float() { v.as_float().unwrap() as i64 }
    else { 0 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_toboolean(U: *const UmbraState, idx: c_int) -> c_int {
    unsafe { (*U).get(idx) }.is_truthy() as c_int
}

// Safety: the returned pointer aliases the interned string's heap allocation
// and is only valid as long as that string stays reachable (i.e. pre-GC).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_tostring(U: *const UmbraState, idx: c_int) -> *const c_char {
    let v = unsafe { (*U).get(idx) };
    if v.is_string() {
        let ptr = v.as_string().unwrap() as *const String;
        unsafe { (*ptr).as_ptr() as *const c_char }
    } else {
        std::ptr::null()
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_getglobal(U: *mut UmbraState, name: *const c_char) {
    let state = unsafe { &mut *U };
    let rs = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    let v = state.vm.get_global(rs.as_ref());
    state.stack.push(v);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_setglobal(U: *mut UmbraState, name: *const c_char) {
    let state = unsafe { &mut *U };
    let rs = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    let v = state.stack.pop().unwrap_or(Value::nil());
    state.vm.set_global(rs.as_ref(), v);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_dostring(U: *mut UmbraState, src: *const c_char) -> c_int {
    let state = unsafe { &mut *U };
    let rs = unsafe { CStr::from_ptr(src) }.to_string_lossy();
    match crate::run_with_vm(rs.as_ref(), &mut state.vm) {
        Ok(()) => UmbraStatus::Ok as c_int,
        Err(e) => {
            let v = state.vm.intern_pub(&e);
            state.stack.push(v);
            UmbraStatus::RuntimeError as c_int
        }
    }
}

// Convention: call the function at `-(nargs+1)`, popping it and its args.
// On success pushes `nres` results (-1 = all); on error pushes the error message.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_pcall(U: *mut UmbraState, nargs: c_int, nres: c_int) -> c_int {
    let state = unsafe { &mut *U };
    let nargs = nargs as usize;
    let stack_top = state.stack.len();
    if stack_top < nargs + 1 {
        let v = state.vm.intern_pub("umbra_pcall: stack underflow");
        state.stack.push(v);
        return UmbraStatus::RuntimeError as c_int;
    }
    let fn_idx = stack_top - nargs - 1;
    let fn_val = state.stack[fn_idx];
    let args: Vec<Value> = state.stack.drain(fn_idx..).skip(1).collect();

    let result: VmResult<Vec<Value>> = call_value(state, fn_val, &args);
    match result {
        Ok(results) => {
            let fill = if nres < 0 { results.len() } else { nres as usize };
            for i in 0..fill {
                state.stack.push(if i < results.len() { results[i] } else { Value::nil() });
            }
            UmbraStatus::Ok as c_int
        }
        Err(e) => {
            let v = state.vm.intern_pub(&e.to_string());
            state.stack.push(v);
            UmbraStatus::RuntimeError as c_int
        }
    }
}

fn call_value(state: &mut UmbraState, fn_val: Value, args: &[Value]) -> VmResult<Vec<Value>> {
    use crate::vm::get_cfn_pub;
    if let Some(cfn) = get_cfn_pub(fn_val) {
        return cfn(args);
    }
    if let Some(proto_ptr) = get_closure(fn_val) {
        return state.vm.exec_call(proto_ptr, args);
    }
    Err(VmError::RuntimeError(format!("attempt to call a {} value", fn_val.type_name())))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_gc_collect(U: *mut UmbraState) {
    unsafe { (*U).vm.gc_collect(); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_gc_setstep(U: *mut UmbraState, threshold: usize) {
    unsafe { (*U).vm.gc.threshold = threshold; }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_gc_livecount(U: *const UmbraState) -> usize {
    unsafe { (*U).vm.gc.live_count() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn umbra_register(
    U: *mut UmbraState,
    name: *const c_char,
    f: UmbraCFunction,
) {
    let state = unsafe { &mut *U };
    let rs = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
    let state_ptr = U;
    state.vm.set_global_cfn(&rs, move |args| {
        let s = unsafe { &mut *state_ptr };
        let old_base = s.api_base;
        s.api_base = s.stack.len();
        for &v in args { s.stack.push(v); }
        let nret = unsafe { f(state_ptr) };
        let results = if nret > 0 {
            let start = s.stack.len().saturating_sub(nret as usize);
            s.stack.drain(start..).collect::<Vec<_>>()
        } else {
            vec![]
        };
        s.stack.truncate(s.api_base);
        s.api_base = old_base;
        if nret < 0 {
            Err(VmError::RuntimeError("C function signalled error".into()))
        } else {
            Ok(results)
        }
    });
}
