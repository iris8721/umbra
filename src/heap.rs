// Every dereference of a heap object outside the collector goes through this
// module. The collector's own walks (mark, sweep, free) are in gc.rs.
//
// Liveness. A heap Value is a bare pointer, so whether it may be dereferenced
// is not something a caller can check locally. The rule is VM-wide: a Value
// the VM touches is reachable from a root (registers, globals, a Ctx, a Ref,
// ...) or was allocated since the last collector step. The collector only
// steps at the interpreter's checkpoints and in gc_collect, so a Value read
// out of a rooted slot stays valid until the next step. A Value belongs to
// the VM that allocated it. Under that rule the accessors are safe functions;
// the one way to break it from outside the crate is a host using a Value
// after its Ctx dropped (embed.rs, rule 3).
//
// Aliasing. The Gc owns every object it registered, so references to GC
// objects borrow the Gc: &Gc gives &Table, &mut Gc gives &mut Table. The
// borrow checker then rejects holding a table across anything that can
// allocate, collect, run a barrier or run script, since all of those need
// &mut Gc or &mut Vm. The collector writes object headers only from &mut Gc
// methods, so it can't touch an object while a reference handed out here is
// live. Two tables can't be mutated at once; callers copy what they need out
// of one before writing the other.
//
// Strings are the first exception. Their bytes never change and live in
// their own buffer, apart from the header the collector writes, so a &str
// stays valid for as long as the string stays rooted, whatever else runs.
// arg_str ties it to the native's argument slice, which stays rooted until
// the native returns.
//
// Natives and protos are the other: the interpreter runs them while it
// lends its &mut Vm out, so their references can't borrow the VM, and the
// collector marks them meanwhile. native() and proto() are unsafe for that
// reason; each states its contract and its call sites say how they meet
// it.

use crate::chunk::{GcProto, Proto};
use crate::gc::{Gc, GcColor, GcPhase};
use crate::value::Value;
use crate::vm::{Coroutine, GcBigInt, LuaClosure, NativeBody, NativeFn, RtBytes, RtString, Table};

impl Gc {
    #[inline(always)]
    pub(crate) fn table(&self, v: Value) -> &Table {
        let p = v.as_table().unwrap() as *const Table;
        // SAFETY: a table Value points at a live Table (liveness rule). A
        // &mut Table only comes from table_mut, which borrows the Gc
        // mutably, so none is live while &self is.
        unsafe { &*p }
    }

    #[inline(always)]
    pub(crate) fn table_mut(&mut self, v: Value) -> &mut Table {
        let p = v.as_table().unwrap() as *mut Table;
        // SAFETY: live by the liveness rule; &mut self rules out every other
        // reference this Gc handed out.
        unsafe { &mut *p }
    }

    /// The metatable of `t`, a table this Gc owns.
    #[inline(always)]
    pub(crate) fn metatable(&self, t: &Table) -> Option<&Table> {
        // SAFETY: a metatable is a live Table: propagate_table marks it
        // through its owner, and set_metatable runs the write barrier.
        t.metatable.map(|p| unsafe { &*p })
    }

    /// `obj`'s metatable entry for `key`, or nil when `obj` isn't a table or
    /// has no metatable.
    #[inline(always)]
    pub(crate) fn meta_get(&self, obj: Value, key: Value) -> Value {
        if !obj.is_table() { return Value::nil(); }
        match self.metatable(self.table(obj)) {
            Some(mt) => mt.raw_get(key),
            None => Value::nil(),
        }
    }

    /// rawset with the incremental write barrier: a black table has already
    /// been scanned this cycle, so storing a white key or value into it
    /// would leave a live object for the sweep. One predictable branch when
    /// idle: registered objects are white between cycles.
    #[inline(always)]
    pub(crate) fn raw_set(&mut self, t: Value, key: Value, val: Value) {
        if self.table(t).gc.color == GcColor::Black && self.phase() != GcPhase::Pause {
            self.barrier_store(key, val);
        }
        self.table_mut(t).set_no_barrier(key, val);
    }

    #[inline(always)]
    pub(crate) fn str(&self, v: Value) -> &str {
        // SAFETY: a string Value points at a live RtString (liveness rule).
        unsafe { str_at(v.as_string().unwrap()) }
    }

    /// The NUL-terminated bytes of a string, for the C API.
    pub(crate) fn str_c_ptr(&self, v: Value) -> *const u8 {
        let p = v.as_string().unwrap() as *const RtString;
        // SAFETY: as str(). The field read doesn't cover the header.
        unsafe { (*p).as_c_ptr() }
    }

    /// Coroutines borrow the Gc like tables, so one can't be held across
    /// running script, which may resume it.
    pub(crate) fn coroutine(&self, v: Value) -> &Coroutine {
        let p = v.as_coroutine().unwrap() as *const Coroutine;
        // SAFETY: as table().
        unsafe { &*p }
    }

    pub(crate) fn coroutine_mut(&mut self, v: Value) -> &mut Coroutine {
        let p = v.as_coroutine().unwrap() as *mut Coroutine;
        // SAFETY: as table_mut().
        unsafe { &mut *p }
    }

    #[inline(always)]
    pub(crate) fn bytes(&self, v: Value) -> &RtBytes {
        let p = v.as_bytes_ptr().unwrap() as *const RtBytes;
        // SAFETY: as table().
        unsafe { &*p }
    }

    #[inline(always)]
    pub(crate) fn bytes_mut(&mut self, v: Value) -> &mut RtBytes {
        let p = v.as_bytes_ptr().unwrap() as *mut RtBytes;
        // SAFETY: as table_mut().
        unsafe { &mut *p }
    }

    /// What a call needs from a script function: its proto and upvalue
    /// array, or None if `v` isn't one. &mut self because the upvalue
    /// pointer is made through the closure's Vec; the frame keeps it.
    #[inline(always)]
    pub(crate) fn callable(&mut self, v: Value) -> Option<CallableProto> {
        if v.is_userdata() {
            if is_native(v) { return None; }
            let proto = v.as_userdata().unwrap() as *const GcProto;
            return Some(CallableProto { proto, upvals_ptr: std::ptr::null_mut(), upvals_len: 0 });
        }
        let c = v.as_closure()? as *mut LuaClosure;
        // SAFETY: live by the liveness rule. Field places only: the &mut
        // to the upvalue Vec is the one live reference into this closure.
        unsafe {
            let upvals = &mut (*c).upvals;
            Some(CallableProto { proto: (*c).proto, upvals_ptr: upvals.as_mut_ptr(), upvals_len: upvals.len() })
        }
    }

    /// The proto of a script function value, or None.
    pub(crate) fn closure_proto(&self, v: Value) -> Option<*const GcProto> {
        if v.is_userdata() {
            return if is_native(v) { None } else { Some(v.as_userdata().unwrap() as *const GcProto) };
        }
        let c = v.as_closure()? as *const LuaClosure;
        // SAFETY: live by the liveness rule; a field read.
        Some(unsafe { (*c).proto })
    }
}

// Bit 47 of a userdata payload marks a native (Value::native); without it
// the payload is the GcProto of an upvalue-less function.
pub(crate) const NATIVE_BIT: u64 = 1 << 47;

#[inline(always)]
pub(crate) fn is_native(v: Value) -> bool {
    v.is_userdata() && v.raw_bits() & NATIVE_BIT != 0
}

/// The function and upvalues of native `v`.
/// SAFETY (callers): `v` is a native and stays rooted for as long as the
/// result is used. The fields never change after allocation and the
/// collector writes only the header, so the result may be held while the
/// native runs script with the VM it is lent.
#[inline(always)]
pub(crate) unsafe fn native<'a>(v: Value) -> (&'a NativeBody, &'a [Value]) {
    debug_assert!(is_native(v));
    let p = v.as_userdata().unwrap() as *const NativeFn;
    // SAFETY: per the caller; field places, so the header isn't covered.
    unsafe { (&(*p).f, &(*p).upvals) }
}

pub(crate) struct CallableProto {
    pub(crate) proto: *const GcProto,
    pub(crate) upvals_ptr: *mut Value,
    pub(crate) upvals_len: usize,
}

/// The i64 inside a boxed integer. `v` must be a bigint.
#[inline(always)]
pub(crate) fn bigint(v: Value) -> i64 {
    let p = v.as_bigint().unwrap() as *const GcBigInt;
    // SAFETY: only Vm::make_int makes bigint Values, each from a live
    // GcBigInt kept alive by the liveness rule. A GcBigInt never changes
    // after allocation, so no &mut to it exists.
    unsafe { (*p).n }
}

/// Runs `f` on the contents of string `v`, for helpers with no Gc at hand.
/// The &str can't outlive the call.
#[inline(always)]
pub(crate) fn with_str<R>(v: Value, f: impl FnOnce(&str) -> R) -> R {
    // SAFETY: a string Value is live by the liveness rule.
    f(unsafe { str_at(v.as_string().unwrap()) })
}

/// String argument `i` of a native, borrowed for as long as the argument
/// slice: the caller roots its arguments until the native returns.
#[inline(always)]
pub(crate) fn arg_str(args: &[Value], i: usize) -> Option<&str> {
    let p = args.get(i)?.as_string()?;
    // SAFETY: a rooted string Value; see the module comment on strings.
    Some(unsafe { str_at(p) })
}

/// The string behind a TableKey::Str, borrowed for as long as the key. The
/// key's table keeps it alive (propagate_table marks key strings); a probe
/// key is made from a live string Value.
#[inline(always)]
pub(crate) fn key_str(p: &*mut u8) -> &str {
    // SAFETY: as above.
    unsafe { str_at(*p) }
}

/// The precomputed hash of a TableKey::Str's string.
#[inline(always)]
pub(crate) fn key_hash(p: *mut u8) -> u64 {
    // SAFETY: as key_str; a field read.
    unsafe { (*(p as *const RtString)).hash }
}

// The contents of the RtString at `p`, through field places so the result
// never covers the header the collector writes.
// SAFETY (callers): `p` points at a live RtString that outlives 'a.
#[inline(always)]
unsafe fn str_at<'a>(p: *mut u8) -> &'a str {
    let p = p as *const RtString;
    // SAFETY: per the caller; bytes[..len] is UTF-8 (RtString's invariant).
    unsafe {
        let bytes = &(*p).bytes;
        std::str::from_utf8_unchecked(bytes.get_unchecked(..(*p).len))
    }
}

/// The code of the proto a frame or function value points at.
/// SAFETY (callers): `p` came from a Frame, a function Value or Vm::exec,
/// and stays reachable while the result is used: a frame's proto is a root
/// (Vm::gc_roots, Gc::mark_coroutine), a function Value's is reached
/// through the function. Nothing mutates a proto but its caches, which are
/// Cells, and the header the collector writes, which the result doesn't
/// cover. So it may be held while the code runs script.
#[inline(always)]
pub(crate) unsafe fn proto<'a>(p: *const GcProto) -> &'a Proto {
    // SAFETY: per the caller; a field place, so the header isn't covered.
    unsafe { &(*p).proto }
}

