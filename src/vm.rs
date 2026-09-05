use std::cell::Cell;
use std::collections::HashMap;
use crate::chunk::{Const, Op, Proto, StrConst, ia, ib, ic, ibx, isbx, iop, is_rk, rk_idx};
use crate::gc::Gc;
use crate::pack;
use crate::pattern;

thread_local! {
    static CURRENT_VM: Cell<*mut Vm> = const { Cell::new(std::ptr::null_mut()) };
    static RNG: Cell<u64> = const { Cell::new(6364136223846793005) };
}

fn with_current_vm<T>(f: impl FnOnce(&mut Vm) -> T) -> Option<T> {
    CURRENT_VM.with(|c| {
        let ptr = c.get();
        if ptr.is_null() { None } else { Some(f(unsafe { &mut *ptr })) }
    })
}

// print/io.write go through the VM's hook when one is set, else stdout.
fn emit_line(line: String, newline: bool) {
    let handled = with_current_vm(|vm| {
        match &vm.print_hook {
            Some(hook) => { hook(line.clone()); true }
            None => false,
        }
    }).unwrap_or(false);
    if handled { return; }
    if newline { println!("{line}"); } else {
        use std::io::Write;
        print!("{line}");
        let _ = std::io::stdout().flush();
    }
}

static PROCESS_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

// Days-since-epoch -> (year, month, day), Howard Hinnant's well-known
// civil_from_days algorithm (proleptic Gregorian, valid for all i64 inputs).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// Inverse of civil_from_days (Howard Hinnant's days_from_civil).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + (d - 1) as u64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

// (year, month, day, yday, weekday-since-Sunday) for an epoch second.
fn civil_fields(epoch_secs: i64) -> (i64, u32, u32, u32, u32) {
    let days = epoch_secs.div_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    let yday = (days - days_from_civil(year, 1, 1) + 1) as u32;
    let wday = (days + 4).rem_euclid(7) as u32; // 1970-01-01 was a Thursday
    (year, month, day, yday, wday)
}

pub fn format_civil_time(epoch_secs: i64, fmt: &str) -> String {
    const WDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June",
                                "July", "August", "September", "October", "November", "December"];
    let secs_of_day = epoch_secs.rem_euclid(86400);
    let (year, month, day, yday, wday) = civil_fields(epoch_secs);
    let (hour, min, sec) = (secs_of_day / 3600, (secs_of_day / 60) % 60, secs_of_day % 60);
    let hour12 = if hour % 12 == 0 { 12 } else { hour % 12 };
    // Week-of-year: %U counts Sundays, %W counts Mondays (strftime rules).
    let week_u = (yday as i64 + 6 - wday as i64).div_euclid(7);
    let week_w = (yday as i64 + 6 - (wday as i64 + 6) % 7).div_euclid(7);
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' { out.push(c); continue; }
        match chars.next() {
            Some('Y') => out.push_str(&year.to_string()),
            Some('y') => out.push_str(&format!("{:02}", year.rem_euclid(100))),
            Some('m') => out.push_str(&format!("{month:02}")),
            Some('d') => out.push_str(&format!("{day:02}")),
            Some('e') => out.push_str(&format!("{day:2}")),
            Some('H') => out.push_str(&format!("{hour:02}")),
            Some('I') => out.push_str(&format!("{hour12:02}")),
            Some('M') => out.push_str(&format!("{min:02}")),
            Some('S') => out.push_str(&format!("{sec:02}")),
            Some('p') => out.push_str(if hour < 12 { "AM" } else { "PM" }),
            Some('a') => out.push_str(&WDAYS[wday as usize][..3]),
            Some('A') => out.push_str(WDAYS[wday as usize]),
            Some('b') | Some('h') => out.push_str(&MONTHS[(month - 1) as usize][..3]),
            Some('B') => out.push_str(MONTHS[(month - 1) as usize]),
            Some('j') => out.push_str(&format!("{yday:03}")),
            Some('U') => out.push_str(&format!("{week_u:02}")),
            Some('W') => out.push_str(&format!("{week_w:02}")),
            Some('w') => out.push_str(&wday.to_string()),
            Some('c') => out.push_str(&format!("{} {} {:02} {:02}:{:02}:{:02} {}",
                &WDAYS[wday as usize][..3], &MONTHS[(month - 1) as usize][..3],
                day, hour, min, sec, year)),
            Some('x') => out.push_str(&format!("{month:02}/{day:02}/{:02}", year.rem_euclid(100))),
            Some('X') | Some('T') => out.push_str(&format!("{hour:02}:{min:02}:{sec:02}")),
            Some('D') => out.push_str(&format!("{month:02}/{day:02}/{:02}", year.rem_euclid(100))),
            Some('F') => out.push_str(&format!("{year}-{month:02}-{day:02}")),
            Some('R') => out.push_str(&format!("{hour:02}:{min:02}")),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('Z') => out.push_str("UTC"),
            Some('%') => out.push('%'),
            Some(other) => { out.push('%'); out.push(other); }
            None => out.push('%'),
        }
    }
    out
}

use crate::value::Value;

// Caps any single allocation a script can request (string.rep/format, table
// range ops) — an allocator failure on an oversized request aborts the
// process unconditionally and can't be caught, unlike a normal panic.
pub(crate) const MAX_ALLOC_LEN: usize = 64 * 1024 * 1024;
// Multiply-rotate hasher for the VM's internal maps; SipHash's DoS resistance
// buys nothing for script tables and intern caches, and it dominated the
// profile on global/table lookups.
#[derive(Default, Clone, Copy)]
pub struct FxHasher { hash: u64 }

impl FxHasher {
    #[inline(always)]
    fn add(&mut self, w: u64) {
        self.hash = (self.hash.rotate_left(5) ^ w).wrapping_mul(0x517cc1b727220a95);
    }
}

impl std::hash::Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks { self.add(u64::from_ne_bytes(c.try_into().unwrap())); }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            let mut w = [0u8; 8];
            w[..rem.len()].copy_from_slice(rem);
            self.add(u64::from_ne_bytes(w));
        }
    }
    #[inline(always)] fn write_u8(&mut self, n: u8)       { self.add(n as u64); }
    #[inline(always)] fn write_u32(&mut self, n: u32)     { self.add(n as u64); }
    #[inline(always)] fn write_u64(&mut self, n: u64)     { self.add(n); }
    #[inline(always)] fn write_i64(&mut self, n: i64)     { self.add(n as u64); }
    #[inline(always)] fn write_usize(&mut self, n: usize) { self.add(n as u64); }
    #[inline(always)] fn finish(&self) -> u64 { self.hash }
}

#[derive(Default, Clone, Copy)]
pub struct FxBuild;

impl std::hash::BuildHasher for FxBuild {
    type Hasher = FxHasher;
    #[inline(always)]
    fn build_hasher(&self) -> FxHasher { FxHasher::default() }
}

pub type FxMap<K, V> = HashMap<K, V, FxBuild>;

fn fx_str_hash_bytes(b: &[u8]) -> u64 {
    let mut h = FxHasher::default();
    std::hash::Hasher::write(&mut h, b);
    std::hash::Hasher::finish(&h)
}


// bytes[..len] is valid UTF-8; bytes[len] == 0. The trailing NUL makes
// as_c_ptr() safe to hand to a C host expecting a NUL-terminated string
// (a plain Rust String's buffer has no such guarantee). `hash` is the
// FxHash of the bytes, precomputed so table-key hashing is O(1).
#[repr(C)] // gc must stay first: the GC reads the header through the object pointer.
pub struct RtString {
    pub gc: crate::gc::GcHeader,
    pub len: usize,
    pub hash: u64,
    bytes: Box<[u8]>,
}

impl RtString {
    pub fn as_c_ptr(&self) -> *const u8 { self.bytes.as_ptr() }
    fn as_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.bytes[..self.len]) }
    }
}

fn alloc_string_raw(s: &str) -> *mut u8 {
    alloc_string_bytes(s.as_bytes())
}

// Builds an RtString from bytes the caller already owns (or can write
// directly), avoiding the extra copy alloc_string_raw would make. The
// caller guarantees the bytes are valid UTF-8; a trailing NUL is appended.
fn alloc_string_bytes(bytes: &[u8]) -> *mut u8 {
    let mut buf = Vec::with_capacity(bytes.len() + 1);
    buf.extend_from_slice(bytes);
    buf.push(0);
    let rt = RtString { gc: crate::gc::GcHeader::new(crate::gc::GcKind::Str), len: bytes.len(), hash: fx_str_hash_bytes(bytes), bytes: buf.into_boxed_slice() };
    Box::into_raw(Box::new(rt)) as *mut u8
}

// Same as alloc_string_bytes but takes ownership of a buffer that already
// has spare capacity for the NUL — the concat fast path writes into one
// of these directly so a `t .. x` chain is a single memcpy per operand.
fn alloc_string_owned(mut buf: Vec<u8>, len: usize) -> *mut u8 {
    buf.truncate(len);
    buf.push(0);
    let rt = RtString { gc: crate::gc::GcHeader::new(crate::gc::GcKind::Str), len, hash: fx_str_hash_bytes(&buf[..len]), bytes: buf.into_boxed_slice() };
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
        vm.gc.register(raw);
        Value::string(raw)
    })
}

// Boxed i64 for values that don't fit the inline int range; the gc header
// keeps it on the same heap list as every other object.
#[repr(C)]
pub struct GcBigInt {
    pub gc: crate::gc::GcHeader,
    pub n: i64,
}

fn alloc_bigint_raw(n: i64) -> *mut u8 {
    Box::into_raw(Box::new(GcBigInt { gc: crate::gc::GcHeader::new(crate::gc::GcKind::BigInt), n })) as *mut u8
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

#[repr(C)] // gc must stay first: the GC reads the header through the object pointer.
pub struct Table {
    pub gc: crate::gc::GcHeader,
    pub array: Vec<Value>,
    pub hash: KeyMap,
    pub metatable: Option<*mut Table>,
}

// Str carries the interned string's pointer, not an owned String: lookups
// hash the precomputed RtString::hash and compare contents only on a probe
// hit, so a string-keyed raw_get no longer allocates or rehashes bytes.
// Keys keep their strings alive via propagate_table marking.
#[derive(Debug, Clone)]
pub enum TableKey {
    Int(i64),
    Str(*mut u8),
    Bool(bool),
    Ptr(u64),
}

impl PartialEq for TableKey {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (TableKey::Int(a), TableKey::Int(b))   => a == b,
            (TableKey::Str(a), TableKey::Str(b))   =>
                a == b || unsafe { (*a as *const RtString).as_ref().unwrap().as_str()
                    == (*b as *const RtString).as_ref().unwrap().as_str() },
            (TableKey::Bool(a), TableKey::Bool(b)) => a == b,
            (TableKey::Ptr(a), TableKey::Ptr(b))   => a == b,
            _ => false,
        }
    }
}

impl Eq for TableKey {}

impl std::hash::Hash for TableKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            TableKey::Int(n)  => { state.write_u8(0); state.write_i64(*n); }
            TableKey::Str(p)  => { state.write_u8(1); state.write_u64(unsafe { (*(*p as *const RtString)).hash }); }
            TableKey::Bool(b) => { state.write_u8(2); state.write_u8(*b as u8); }
            TableKey::Ptr(p)  => { state.write_u8(3); state.write_u64(*p); }
        }
    }
}

impl TableKey {
    fn from_value(v: Value) -> Option<Self> {
        if v.is_nil() { return None; }
        if v.is_int_like() { return Some(TableKey::Int(v.as_int().unwrap())); }
        if v.is_float() {
            let f = v.as_float().unwrap();
            if f.fract() == 0.0 && f >= -9223372036854775808.0 && f < 9223372036854775808.0 {
                return Some(TableKey::Int(f as i64));
            }
            return Some(TableKey::Ptr(f.to_bits()));
        }
        if v.is_bool() { return Some(TableKey::Bool(v.as_bool().unwrap())); }
        if v.is_string() {
            return Some(TableKey::Str(v.as_string().unwrap()));
        }
        Some(TableKey::Ptr(v.raw_bits()))
    }
}

// Open-addressed map for the non-array part of a Table. std HashMap::get
// never inlines, and every GetGlobal/GetTable pays that call plus a probe —
// this keeps the whole lookup in the dispatch loop. Slot tags: 0 = empty,
// 1 = tombstone, otherwise (hash | 2) so a hit compares one u64 before the key.
pub struct KeyMap {
    slots: Vec<(u64, TableKey, Value)>,
    len: usize,
    used: usize,
}

impl KeyMap {
    fn new() -> Self { KeyMap { slots: Vec::new(), len: 0, used: 0 } }

    // Heap bytes held by the slot array, for the GC's byte-paced trigger.
    pub fn capacity_bytes(&self) -> usize {
        self.slots.capacity() * std::mem::size_of::<(u64, TableKey, Value)>()
    }

    #[inline(always)]
    fn key_hash(k: &TableKey) -> u64 {
        // Probes index on the LOW bits of this hash, but multiplication only
        // spreads entropy upward: float/pointer keys (mantissa low bits are
        // zero) would otherwise all land in the same few slots. Fold the
        // product's high bits back down so every key type scatters.
        let mix = |h: u64| {
            let h = h.wrapping_mul(0x9E3779B97F4A7C15);
            (h ^ (h >> 32)) | 2
        };
        match k {
            TableKey::Int(n)  => mix(*n as u64),
            TableKey::Str(p)  => mix(unsafe { (*(*p as *const RtString)).hash }),
            TableKey::Bool(b) => mix(*b as u64),
            TableKey::Ptr(p)  => mix(*p),
        }
    }

    #[inline(always)]
    pub fn get(&self, k: &TableKey) -> Option<&Value> {
        if self.slots.is_empty() { return None; }
        let mask = self.slots.len() - 1;
        let h = Self::key_hash(k);
        let mut i = (h as usize >> 2) & mask;
        loop {
            let (tag, sk, v) = unsafe { self.slots.get_unchecked(i) };
            if *tag == 0 { return None; }
            if *tag == h && sk == k { return Some(v); }
            i = (i + 1) & mask;
        }
    }

    // Slot index where the probe for `k` stops: the key's own slot on a hit,
    // or the first empty slot on a miss. next() uses it to resume iteration
    // without rescanning the whole map.
    fn probe_slot(&self, k: &TableKey) -> Option<usize> {
        if self.slots.is_empty() { return None; }
        let mask = self.slots.len() - 1;
        let h = Self::key_hash(k);
        let mut i = (h as usize >> 2) & mask;
        loop {
            let (tag, sk, _) = unsafe { self.slots.get_unchecked(i) };
            if *tag == 0 || (*tag == h && sk == k) { return Some(i); }
            i = (i + 1) & mask;
        }
    }

    fn grow(&mut self) {
        let cap = (self.slots.len() * 2).max(8);
        let mut slots = Vec::new();
        slots.resize(cap, (0u64, TableKey::Bool(false), Value::nil()));
        let old = std::mem::replace(&mut self.slots, slots);
        self.used = 0;
        for (tag, k, v) in old {
            if tag > 1 {
                let mask = self.slots.len() - 1;
                let mut i = (tag as usize >> 2) & mask;
                while self.slots[i].0 != 0 { i = (i + 1) & mask; }
                self.slots[i] = (tag, k, v);
                self.used += 1;
            }
        }
    }

    pub fn insert(&mut self, k: TableKey, v: Value) {
        if (self.used + 1) * 4 >= self.slots.len().max(1) * 3 { self.grow(); }
        let mask = self.slots.len() - 1;
        let h = Self::key_hash(&k);
        let mut i = (h as usize >> 2) & mask;
        let mut tomb = usize::MAX;
        loop {
            let slot = unsafe { self.slots.get_unchecked_mut(i) };
            if slot.0 == 0 {
                let at = if tomb != usize::MAX { tomb } else { i };
                self.slots[at] = (h, k, v);
                self.len += 1;
                if tomb == usize::MAX { self.used += 1; }
                return;
            }
            if slot.0 == h && slot.1 == k {
                slot.2 = v;
                return;
            }
            if slot.0 == 1 && tomb == usize::MAX { tomb = i; }
            i = (i + 1) & mask;
        }
    }

    pub fn remove(&mut self, k: &TableKey) {
        if self.slots.is_empty() { return; }
        let mask = self.slots.len() - 1;
        let h = Self::key_hash(k);
        let mut i = (h as usize >> 2) & mask;
        loop {
            let slot = unsafe { self.slots.get_unchecked_mut(i) };
            if slot.0 == 0 { return; }
            if slot.0 == h && slot.1 == *k {
                slot.0 = 1;
                self.len -= 1;
                return;
            }
            i = (i + 1) & mask;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&TableKey, &Value)> {
        self.slots.iter().filter(|s| s.0 > 1).map(|s| (&s.1, &s.2))
    }

    pub fn retain(&mut self, mut f: impl FnMut(&TableKey, &mut Value) -> bool) {
        for s in self.slots.iter_mut() {
            if s.0 > 1 && !f(&s.1, &mut s.2) {
                s.0 = 1;
                self.len -= 1;
            }
        }
    }
}

impl Table {
    pub fn new() -> Self {
        Table { gc: crate::gc::GcHeader::new(crate::gc::GcKind::Table), array: Vec::new(), hash: KeyMap::new(), metatable: None }
    }
    #[inline(always)]
    pub fn raw_get(&self, key: Value) -> Value {
        if key.is_int() {
            let i = key.as_int().unwrap();
            if i >= 1 && (i as usize) <= self.array.len() {
                return self.array[(i - 1) as usize];
            }
        }
        // String keys dominate; skip the full from_value cascade for them.
        if key.is_string() {
            return self.hash.get(&TableKey::Str(key.as_string().unwrap()))
                .copied().unwrap_or(Value::nil());
        }
        if let Some(k) = TableKey::from_value(key) {
            return self.hash.get(&k).copied().unwrap_or(Value::nil());
        }
        Value::nil()
    }

    // String lookup without a Value at hand (GC metatable checks); metatables
    // are tiny, so a linear scan beats fabricating a key.
    pub fn get_str(&self, name: &str) -> Option<Value> {
        self.hash.iter().find_map(|(k, v)| match k {
            TableKey::Str(p) if unsafe { (*(*p as *const RtString)).as_str() == name } => Some(*v),
            _ => None,
        })
    }

    pub fn raw_set(&mut self, key: Value, val: Value) {
        if key.is_int() {
            let i = key.as_int().unwrap();
            if i >= 1 && i <= (self.array.len() + 1) as i64 {
                // The key may already live in the hash part (inserted before
                // the array grew this far); drop it so the key exists in
                // exactly one place and next() can't yield it twice.
                if self.hash.len > 0 { self.hash.remove(&TableKey::Int(i)); }
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
    // The coroutine's pending <close> marks; swapped in and out of Vm::tbc
    // with its regs/frames on resume.
    pub tbc: Vec<(usize, bool)>,
}

#[repr(C)] // gc must stay first: the GC reads the header through the object pointer.
pub struct LuaClosure {
    pub gc: crate::gc::GcHeader,
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
    string_cache: FxMap<String, Value>,
    // Interned string constants resolved from live Protos; rooted for the
    // VM's lifetime so the per-constant cache in StrConst can never dangle.
    const_strings: Vec<Value>,
    pub top_level_results: Vec<Value>,
    pub gc: Gc,
    pub coroutines: Vec<*mut Coroutine>,
    pub owned_protos: Vec<Box<crate::chunk::Proto>>,
    cfns: Vec<*mut CFunction>,
    pub string_lib: Value,
    // Values the embedding host holds on its API stack; rooted like registers.
    pub host_stack: Vec<Value>,
    print_hook: Option<Box<dyn Fn(String)>>,
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
    pub loaded_modules: FxMap<String, Value>,
    // Frame stacks parked by run_isolated while a nested isolated call
    // (metamethod, pcall target, __gc) runs; their varargs must stay
    // reachable to the collector.
    saved_frames: Vec<Vec<Frame>>,
    // Registers marked to-be-closed by Op::Tbc, innermost last: (absolute
    // register index, boxed flag). Op::TbcPop drops the newest mark on a
    // normal scope exit; unwind_closes drains the rest when an error
    // discards frames. Kept off Frame so the hot call path stays small.
    tbc: Vec<(usize, bool)>,
    coroutine_depth: usize,
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
        CURRENT_VM.with(|c| if c.get() == self as *mut Vm { c.set(std::ptr::null_mut()) });
        for &ptr in &self.coroutines {
            unsafe { drop(Box::from_raw(ptr)); }
        }
        self.gc.free_all();
        for &ptr in &self.cfns {
            unsafe { drop(Box::from_raw(ptr)); }
        }
    }
}

#[derive(Debug)]
pub enum VmError {
    RuntimeError(String),
    StackOverflow,
    Yield(Vec<Value>),
    // A value thrown by error()/assert(): propagated to pcall/xpcall/resume
    // as-is (like Lua's error object), never line-prefixed by enrichment.
    Thrown(Value),
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmError::RuntimeError(s) => write!(f, "{s}"),
            VmError::StackOverflow => write!(f, "stack overflow"),
            VmError::Yield(_) => write!(f, "attempt to yield from outside a coroutine"),
            VmError::Thrown(v) => write!(f, "{v}"),
        }
    }
}

pub type VmResult<T> = Result<T, VmError>;

impl Vm {
    pub fn new() -> Self {
        Self::new_inner(None)
    }
    pub fn new_with_print(hook: impl Fn(String) + 'static) -> Self {
        Self::new_inner(Some(Box::new(hook)))
    }

    fn new_inner(print_hook: Option<Box<dyn Fn(String)>>) -> Self {
        let mut vm = Vm {
            regs: vec![Value::nil(); 256],
            frames: Vec::with_capacity(200),
            globals: Table::new(),
            string_cache: FxMap::default(),
            const_strings: Vec::new(),
            top_level_results: Vec::new(),
            gc: Gc::new(),
            coroutines: Vec::new(),
            owned_protos: Vec::new(),
            cfns: Vec::new(),
            string_lib: Value::nil(),
            host_stack: Vec::with_capacity(32),
            print_hook,
            poisoned: false,
            last_traceback: None,
            step_limit: 0,
            step_count: 0,
            loaded_modules: FxMap::default(),
            saved_frames: Vec::new(),
            tbc: Vec::new(),
            coroutine_depth: 0,
        };
        vm.register_stdlib();
        vm
    }


    fn intern(&mut self, s: &str) -> Value {
        if let Some(&v) = self.string_cache.get(s) { return v; }
        let ptr = alloc_string_raw(s);
        self.gc.register(ptr);
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
        self.gc.register(ptr);
        Value::bigint(ptr)
    }

    // resume() swaps a coroutine's regs into self.regs for its run, so the live
    // state at any moment is split between self.regs and every other coroutine's
    // parked regs — missing the latter frees values a suspended coroutine still holds.

    fn gc_roots(&self) -> Vec<Value> {
        // Registers are rooted up to the deepest frame's extent; frame.top can
        // reach past base + max_regs after a multi-value call or `...` spill,
        // and those results are still live until the next instruction consumes
        // them.
        let reg_top = self.frames.iter().map(|f| {
            (f.base + unsafe { &*f.proto }.max_regs as usize).max(f.top)
        }).max().unwrap_or(0).min(self.regs.len());
        let mut roots: Vec<Value> = self.regs[..reg_top].to_vec();
        // Varargs are copied out of the register window into the frame, so
        // they need their own scan — nothing else references them.
        for f in self.frames.iter().chain(self.saved_frames.iter().flatten()) {
            roots.extend(f.varargs.iter().copied());
        }
        roots.extend(self.globals.array.iter().copied());
        // globals isn't a GC object, so propagate_table never sees it: root
        // its hash keys (strings keep the key's pointer alive) and values.
        for (k, v) in self.globals.hash.iter() {
            match k {
                TableKey::Str(p) => roots.push(Value::string(*p)),
                TableKey::Ptr(bits) => roots.push(Value::from_raw(*bits)),
                _ => {}
            }
            roots.push(*v);
        }
        if let Some(mt) = self.globals.metatable {
            roots.push(Value::table(mt as *mut u8));
        }
        roots.extend(self.host_stack.iter().copied());
        roots.extend(self.const_strings.iter().copied());
        roots.extend(self.loaded_modules.values().copied());
        roots.push(self.string_lib);
        for &ptr in &self.coroutines {
            let co = unsafe { &*ptr };
            if co.status == CoStatus::Dead { continue; }
            roots.extend(co.regs.iter().copied());
            roots.push(co.fn_val);
            for f in &co.frames {
                roots.extend(f.varargs.iter().copied());
            }
        }
        for &(ptr, gc_fn) in self.gc.pending_finalizers.iter().chain(&self.gc.running_finalizers) {
            roots.push(Value::table(ptr as *mut u8));
            roots.push(gc_fn);
        }
        roots
    }

    pub fn gc_collect(&mut self) {
        let roots = self.gc_roots();
        self.gc.collect(roots.into_iter(), &mut self.string_cache);

        // A collection triggered from inside a finalizer (or any nested
        // collect while finalizers are draining) must not run the finalizers
        // it just queued: that would recurse gc_collect on the Rust stack
        // without bound. They stay pending and the outermost collect drains
        // them iteratively below.
        if !self.gc.running_finalizers.is_empty() { return; }
        let batch = std::mem::take(&mut self.gc.pending_finalizers);
        if batch.is_empty() { return; }
        // Kept in running_finalizers (a root) while they run: a nested
        // collection from inside one finalizer must not sweep the others.
        self.gc.running_finalizers.extend(batch);
        let mut i = 0;
        while i < self.gc.running_finalizers.len() {
            let (ptr, gc_fn) = self.gc.running_finalizers[i];
            let _ = self.call_value_isolated(gc_fn, &[Value::table(ptr as *mut u8)]);
            i += 1;
        }
        self.gc.running_finalizers.clear();
        let roots = self.gc_roots();
        self.gc.collect(roots.into_iter(), &mut self.string_cache);
    }


    // Runs close() on every to-be-closed register of the frames an error is
    // about to discard: innermost frame first, innermost local first, each
    // call receiving the pending error as its second argument (Lua 5.4's
    // __close(value, err)). A close that fails replaces the pending error and
    // unwinding continues with the new one. Marks below `floor` belong to
    // frames that survive the unwind (parked by run_isolated) and are left
    // alone. Call while self.frames/self.regs still hold the dying stack.
    fn unwind_closes(&mut self, err: VmError, floor: usize) -> VmError {
        if self.poisoned { self.tbc.truncate(floor); return err; }
        let mut err = err;
        // The error object is handed to each close call; a close that fails
        // replaces it (and the pending error) for the rest of the unwind.
        let mut err_val = match err {
            VmError::Thrown(v) => v,
            ref other => self.intern(&other.to_string()),
        };
        let close_key = self.intern("close");
        // The close calls can collect; the error object and key must stay
        // reachable across them.
        self.host_stack.push(err_val);
        self.host_stack.push(close_key);
        macro_rules! replace_err {
            ($e:expr) => {{
                err = $e;
                err_val = match err {
                    VmError::Thrown(v) => v,
                    ref other => self.intern(&other.to_string()),
                };
                let n = self.host_stack.len();
                self.host_stack[n - 2] = err_val;
            }};
        }
        while self.tbc.len() > floor {
            let (reg, boxed) = self.tbc.pop().unwrap();
            if self.poisoned { break; }
            let mut v = self.regs[reg];
            if boxed && v.is_table() {
                v = unsafe { &*(v.as_table().unwrap() as *const Table) }.raw_get(Value::int(1));
            }
            let close = if v.is_table() {
                let t = unsafe { &*(v.as_table().unwrap() as *const Table) };
                let raw = t.raw_get(close_key);
                if !raw.is_nil() || t.metatable.is_none() {
                    raw
                } else {
                    match self.table_index_chain(v, close_key) {
                        Ok(r) => r,
                        Err(e) => { replace_err!(e); continue; }
                    }
                }
            } else if v.is_string() {
                if self.string_lib.is_table() {
                    unsafe { &*(self.string_lib.as_table().unwrap() as *const Table) }.raw_get(close_key)
                } else {
                    Value::nil()
                }
            } else {
                replace_err!(VmError::RuntimeError(
                    format!("attempt to index a {} value", v.type_name())));
                continue;
            };
            if let Err(e) = self.call_value_isolated(close, &[v, err_val]) {
                replace_err!(e);
            }
        }
        self.tbc.truncate(floor);
        self.host_stack.pop();
        self.host_stack.pop();
        err
    }
    // First register above every live frame; scratch space for calls that
    // must not clobber the running function's registers.
    fn scratch_base(&self) -> usize {
        self.frames.iter().map(|f| f.base + unsafe { &*f.proto }.max_regs as usize)
            .max().map(|top| top + 8).unwrap_or(0)
    }

    // Runs `fn_val` in an isolated frame above the active call stack, so a
    // metamethod invocation mid-instruction can't clobber the caller's
    // registers.
    pub fn call_value_isolated(&mut self, fn_val: Value, args: &[Value]) -> VmResult<Vec<Value>> {
        if self.poisoned {
            return Err(VmError::RuntimeError("VM is poisoned by a previous internal error".into()));
        }
        // The frame stores raw pointers into the closure's upvalue vector, not
        // the closure itself, and callers (e.g. umbra_pcall) may hold fn_val
        // nowhere the collector can see — root it for the duration of the call.
        self.host_stack.push(fn_val);
        let result = self.call_value_isolated_inner(fn_val, args);
        self.host_stack.pop();
        result
    }

    fn call_value_isolated_inner(&mut self, fn_val: Value, args: &[Value]) -> VmResult<Vec<Value>> {
        if let Some(cfn) = get_cfn(fn_val) {
            // Same save/restore as run(): the cfn may reenter the API on a
            // different state and leave CURRENT_VM pointing at it.
            let prev_vm = CURRENT_VM.with(|c| c.replace(self as *mut Vm));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cfn(args)));
            CURRENT_VM.with(|c| c.set(prev_vm));
            return match result {
                Ok(r) => r,
                Err(payload) => {
                    self.poisoned = true;
                    Err(VmError::RuntimeError(format!("internal error (panic): {}", panic_message(&*payload))))
                }
            };
        }
        if fn_val.is_table() {
            // Op::Call honors __call on tables; pcall/metamethod dispatch must too.
            let mm = self.get_mm(fn_val, "__call");
            if mm.is_nil() {
                return Err(VmError::RuntimeError("attempt to call a table value".into()));
            }
            let mut mm_args = Vec::with_capacity(args.len() + 1);
            mm_args.push(fn_val);
            mm_args.extend_from_slice(args);
            return self.call_value_isolated_inner(mm, &mm_args);
        }
        if let Some(cp) = get_proto_callable(fn_val) {
            self.run_isolated(cp.proto, cp.upvals_ptr, cp.upvals_len, args)
        } else {
            Err(VmError::RuntimeError(format!("attempt to call a {} value", fn_val.type_name())))
        }
    }

    // The scratch base must be computed before frames are saved, since saving
    // swaps `self.frames` out to empty. A yield can't cross this boundary: the
    // isolated frames are discarded on return, so there'd be nothing to resume.
    fn run_isolated(&mut self, proto: *const Proto, upvals_ptr: *mut Value, upvals_len: usize, args: &[Value]) -> VmResult<Vec<Value>> {
        let base = self.scratch_base();
        // Marks below the floor belong to the parked frames; only the
        // isolated run's own <close> locals unwind here.
        let tbc_floor = self.tbc.len();
        self.saved_frames.push(std::mem::take(&mut self.frames));
        let needed = base + unsafe { &*proto }.max_regs as usize + 8;
        if needed > self.regs.len() { self.regs.resize(needed + 64, Value::nil()); }
        for (i, &v) in args.iter().enumerate() { self.regs[base + i] = v; }
        if let Err(e) = self.push_frame(proto, upvals_ptr, upvals_len, base, args.len() as u8, 255) {
            self.frames = self.saved_frames.pop().unwrap();
            return Err(e);
        }
        let run_result = self.run();
        let results = std::mem::take(&mut self.top_level_results);
        let run_result = match run_result {
            Ok(()) => Ok(()),
            Err(e) => {
                // The isolated frames are about to be dropped: run their
                // pending <close> locals first, while self.frames/self.regs
                // still hold them.
                let e = if matches!(e, VmError::Yield(_)) && self.coroutine_depth > 0 {
                    VmError::RuntimeError("attempt to yield across a C-call boundary".into())
                } else { e };
                Err(self.unwind_closes(e, tbc_floor))
            }
        };
        self.frames = self.saved_frames.pop().unwrap();
        run_result.map(|_| results)
    }

    fn new_coroutine(&mut self, fn_val: Value, who: &str) -> VmResult<Value> {
        if get_closure(fn_val).is_none() && get_cfn(fn_val).is_none() {
            return Err(VmError::RuntimeError(format!("{who}: expected function")));
        }
        let co = Box::new(Coroutine {
            regs: Vec::new(),
            frames: Vec::with_capacity(8),
            status: CoStatus::Suspended,
            fn_val,
            started: false,
            yield_result_base: 0,
            yield_nresults: 0,
            tbc: Vec::new(),
        });
        let ptr = Box::into_raw(co);
        self.coroutines.push(ptr);
        Ok(Value::coroutine(ptr as *mut u8))
    }

    // Swaps the coroutine's regs/frames in, runs it until it yields, returns
    // or fails, and swaps them back out. Both a yield and a return report Ok.
    fn resume_coroutine(&mut self, co_val: Value, args: &[Value]) -> VmResult<Vec<Value>> {
        if self.poisoned {
            return Err(VmError::RuntimeError("VM is poisoned by a previous internal error".into()));
        }
        let co = unsafe { &mut *(co_val.as_coroutine().unwrap() as *mut Coroutine) };
        match co.status {
            CoStatus::Dead => return Err(VmError::RuntimeError("cannot resume dead coroutine".into())),
            CoStatus::Running => return Err(VmError::RuntimeError("cannot resume non-suspended coroutine".into())),
            CoStatus::Suspended => {}
        }
        co.status = CoStatus::Running;
        std::mem::swap(&mut self.regs, &mut co.regs);
        std::mem::swap(&mut self.frames, &mut co.frames);
        std::mem::swap(&mut self.tbc, &mut co.tbc);
        self.coroutine_depth += 1;

        // A host function can't yield (there's no frame to suspend), so it
        // runs to completion inside this one resume; its results become the
        // coroutine's return values and the coroutine ends dead.
        let setup: VmResult<()> = if !co.started {
            co.started = true;
            if let Some(cfn) = get_cfn(co.fn_val) {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cfn(args))) {
                    Ok(Ok(vals)) => { self.top_level_results = vals; Ok(()) }
                    Ok(Err(e)) => Err(e),
                    Err(payload) => {
                        self.poisoned = true;
                        Err(VmError::RuntimeError(format!("internal error (panic): {}", panic_message(&*payload))))
                    }
                }
            } else {
                let cp = get_proto_callable(co.fn_val).unwrap();
                let base = 1usize;
                let needed = base + unsafe { &*cp.proto }.max_regs as usize + 8;
                if self.regs.len() < needed { self.regs.resize(needed + 64, Value::nil()); }
                for (i, &v) in args.iter().enumerate() { self.regs[base + i] = v; }
                self.push_frame(cp.proto, cp.upvals_ptr, cp.upvals_len, base, args.len() as u8, 255)
            }
        } else {
            if co.yield_result_base > 0 {
                self.place_results(co.yield_result_base, args.to_vec(), co.yield_nresults);
                co.yield_result_base = 0;
            }
            Ok(())
        };
        let run_result = match setup {
            Ok(()) if !self.frames.is_empty() =>
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_inner())) {
                    Ok(r) => r.map_err(|e| self.enrich_error_line(e)),
                    Err(payload) => {
                        self.poisoned = true;
                        Err(VmError::RuntimeError(format!("internal error (panic): {}", panic_message(&*payload))))
                    }
                },
            Ok(()) => Ok(()),
            // A host-function coroutine that calls yield() can't suspend —
            // there's no frame to park — so report it like any other yield
            // across a C-call boundary.
            Err(VmError::Yield(_)) if self.frames.is_empty() =>
                Err(VmError::RuntimeError("attempt to yield across a C-call boundary".into())),
            Err(e) => Err(e),
        };
        let co_results = std::mem::take(&mut self.top_level_results);
        // A dead coroutine's frames are dropped below; run their pending
        // <close> locals while they're still swapped in.
        let run_result = match run_result {
            Err(e @ VmError::Yield(_)) => Err(e),
            Err(e) => Err(self.unwind_closes(e, 0)),
            ok => ok,
        };
        self.coroutine_depth -= 1;
        std::mem::swap(&mut self.regs, &mut co.regs);
        std::mem::swap(&mut self.frames, &mut co.frames);
        std::mem::swap(&mut self.tbc, &mut co.tbc);

        match run_result {
            Err(VmError::Yield(values)) => {
                co.status = CoStatus::Suspended;
                capture_yield_site(co);
                Ok(values)
            }
            other => {
                co.status = CoStatus::Dead;
                co.regs = Vec::new();
                co.frames = Vec::new();
                other.map(|_| co_results)
            }
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
            // Build the RtString in place: one allocation, one memcpy per
            // operand, no interning — concat results are throwaway and
            // string identity is content-based everywhere else.
            let ltmp;
            let rtmp;
            let ls: &str = if left.is_string() { unsafe { string_ref(left) } } else { ltmp = coerce_num_str(left); &ltmp };
            let rs: &str = if right.is_string() { unsafe { string_ref(right) } } else { rtmp = coerce_num_str(right); &rtmp };
            let mut buf = Vec::with_capacity(ls.len() + rs.len() + 1);
            buf.extend_from_slice(ls.as_bytes());
            buf.extend_from_slice(rs.as_bytes());
            let raw = alloc_string_owned(buf, ls.len() + rs.len());
            self.gc.register(raw);
            return Ok(Value::string(raw));
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

    #[inline(always)]
    fn resolve_const(&mut self, proto: &Proto, idx: usize) -> Value {
        // String constants dominate (every GetGlobal/LoadK name); test for
        // Str first so the hot case is a compare, not a jump-table hop.
        let c = unsafe { proto.consts.get_unchecked(idx) };
        if let Const::Str(sc) = c {
            let (owner, bits) = sc.cached();
            if bits != 0 && owner == self as *const Vm as usize {
                return Value::from_raw(bits);
            }
            return self.intern_const_str(sc);
        }
        match c {
            Const::Nil       => Value::nil(),
            Const::Bool(b)   => Value::bool(*b),
            Const::Int(n)    => self.make_int(*n),
            Const::Float(f)  => Value::float(*f),
            Const::Str(_)    => unreachable!(),
        }
    }

    // First-touch path for a string constant; interned constants are rooted
    // for the VM's lifetime (const_strings): a borrowed Proto (exec) can
    // outlive a collection, and a collected-then-reused address would
    // resurrect the cache pointing at an unrelated string.
    #[cold]
    fn intern_const_str(&mut self, sc: &StrConst) -> Value {
        let v = self.intern(&sc.s);
        sc.set_cached(self as *const Vm as usize, v.raw_bits());
        self.const_strings.push(v);
        v
    }

    // Chunks run in an isolated frame too, so a host calling back into the VM
    // from inside a registered C function can't collide with the running script.
    pub fn exec(&mut self, proto: &Proto) -> VmResult<()> {
        self.run_isolated(proto, std::ptr::null_mut(), 0, &[])
            .map(|_| ()).map_err(thrown_to_runtime)
    }

    // Proto must be owned here (not borrowed) so raw closure pointers into it
    // stay valid after the caller's source/AST goes out of scope.
    pub fn exec_owned(&mut self, proto: crate::chunk::Proto) -> VmResult<Vec<Value>> {
        let boxed = Box::new(proto);
        let ptr: *const Proto = &*boxed;
        self.owned_protos.push(boxed);
        self.run_isolated(ptr, std::ptr::null_mut(), 0, &[])
            .map_err(thrown_to_runtime)
    }

    #[inline(always)]
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
        // Manual push: Vec::push_mut's grow path never inlines and dominated
        // the call profile; capacity is preallocated to the 200-frame limit.
        let f = Frame { proto, pc: 0, base, expected_results: expected, upvals_ptr, upvals_len, varargs, top: base };
        if self.frames.len() == self.frames.capacity() {
            self.frames.reserve(8);
        }
        unsafe {
            let end = self.frames.as_mut_ptr().add(self.frames.len());
            std::ptr::write(end, f);
            self.frames.set_len(self.frames.len() + 1);
        }
        Ok(())
    }

    pub fn run(&mut self) -> VmResult<()> {
        if self.poisoned {
            return Err(VmError::RuntimeError("VM is poisoned by a previous internal error".into()));
        }
        // Save/restore: a host C function may reenter the API on a *different*
        // state (umbra_dostring/umbra_pcall), and leaving CURRENT_VM pointing
        // at that other VM would register this VM's later allocations on the
        // wrong collector.
        let prev_vm = CURRENT_VM.with(|c| c.replace(self as *mut Vm));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_inner()));
        CURRENT_VM.with(|c| c.set(prev_vm));
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(self.enrich_error_line(e)),
            Err(payload) => {
                self.poisoned = true;
                Err(VmError::RuntimeError(format!("internal error (panic): {}", panic_message(&*payload))))
            }
        }
    }

    fn enrich_error_line(&mut self, e: VmError) -> VmError {
        match &e {
            VmError::RuntimeError(msg) => {
                if !msg.starts_with("line ") {
                    self.last_traceback = Some(build_traceback(&self.frames));
                    if let Some(frame) = self.frames.last() {
                        let proto = unsafe { &*frame.proto };
                        let pc = frame.pc.saturating_sub(1);
                        if let Some(&line) = proto.lines.get(pc) {
                            let at = match &proto.source {
                                Some(name) => format!("line {line} ({name})"),
                                None => format!("line {line}"),
                            };
                            return VmError::RuntimeError(format!("{at}: {msg}"));
                        }
                    }
                }
            }
            // Thrown values keep their identity (no line prefix), but the
            // traceback is still captured for debug.traceback.
            VmError::Thrown(_) => {
                self.last_traceback = Some(build_traceback(&self.frames));
            }
            _ => {}
        }
        e
    }

    // R!/RK! expand to unchecked accesses; a few call sites already sit inside
    // unsafe blocks, which would warn without this.
    #[allow(unused_unsafe)]
    pub fn run_inner(&mut self) -> VmResult<()> {
        'outer: loop {
            // GC runs here, before `frame` becomes a raw pointer into self.frames:
            // gc_collect may run __gc finalizers that push frames of their own.
            if self.gc.should_collect() { self.gc_collect(); }

            // >= not >: at the ceiling the allocating instruction never gets to
            // run, so > would spin here instead of reporting.
            if self.gc.max_objects != 0 && self.gc.live_count() >= self.gc.max_objects {
                return Err(VmError::RuntimeError("memory limit exceeded".into()));
            }

            if self.step_limit != 0 && self.step_count > self.step_limit {
                return Err(VmError::RuntimeError("instruction budget exceeded".into()));
            }

            let frame = self.frames.last_mut().unwrap() as *mut Frame;
            let mut frame = unsafe { &mut *frame };
            let mut proto = unsafe { &*frame.proto };
            let mut base = frame.base;
            // Cached regs base pointer: self.regs only moves on resize, and
            // every path that can resize refreshes this before continuing.
            let mut regs = self.regs.as_mut_ptr();

            // Register and constant indices come from the compiler's own
            // bytecode (register allocation is bounded by max_regs, which
            // push_frame sizes the window for), so indexing is unchecked —
            // the bounds checks were a measurable slice of dispatch cost.
            macro_rules! R {
                ($r:expr) => { *unsafe { &mut *regs.add(base + $r) } }
            }
            macro_rules! RK {
                ($x:expr) => {
                    if is_rk($x) { self.resolve_const(proto, rk_idx($x)) }
                    else { unsafe { *regs.add(base + $x) } }
                }
            }
            // GC/budget checkpoint. Only needed where a loop can spin
            // (backward jumps, calls, returns) or a GC object was just
            // created — checking every instruction was pure dispatch cost.
            macro_rules! ck {
                () => {
                    if self.gc.should_collect() || (self.step_limit != 0 && self.step_count > self.step_limit) {
                        continue 'outer;
                    }
                }
            }
            macro_rules! arith_op {
                ($dst:expr, $b:expr, $c:expr, $int_op:expr, $float_op:expr, $mm_name:expr) => {{
                    let bv = RK!($b);
                    let cv = RK!($c);
                    if bv.is_number() && cv.is_number() {
                        let res = if bv.is_int_like() && cv.is_int_like() {
                            self.make_int($int_op(bv.as_int().unwrap(), cv.as_int().unwrap())?)
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
            // The compiler always emits a Jmp right after a conditional op;
            // consume it inline so a compare+branch costs one dispatch.
            // The consumed Jmp still counts as a step, and a backward target
            // is a loop edge, so it checkpoints like a real Jmp.
            macro_rules! fused_jmp {
                ($taken:expr) => {{
                    let j = unsafe { *proto.code.get_unchecked(frame.pc) };
                    debug_assert!(iop(j) == Op::Jmp as u8);
                    frame.pc += 1;
                    self.step_count += 1;
                    if $taken {
                        frame.pc = (frame.pc as i32 + isbx(j)) as usize;
                        if isbx(j) < 0 { ck!(); }
                    }
                }}
            }

            // Every compiled Proto ends in Return, so the loop exits through
            // the Return arm — no per-instruction pc bounds check needed.
            loop {

                // Cheap per-instruction counting only; the actual gc_collect()/
                // budget-error handling happens back at 'outer's top, where it's
                // safe to mutate self.frames (frame/proto aren't held past this point).
                self.step_count += 1;


                let instr = unsafe { *proto.code.get_unchecked(frame.pc) };
                frame.pc += 1;
                let a   = || ia(instr);
                let b   = || ib(instr);
                let c   = || ic(instr);
                let bx  = || ibx(instr);
                let sbx = || isbx(instr);

                // Op is #[repr(u8)] with contiguous variants 0..=TbcPop and the
                // compiler only emits valid opcodes, so decode unchecked.
                debug_assert!(iop(instr) <= Op::TbcPop as u8);
                match unsafe { std::mem::transmute::<u8, Op>(iop(instr)) } {
                    Op::LoadNil  => R!(a()) = Value::nil(),
                    Op::LoadBool => {
                        R!(a()) = Value::bool(b() != 0);
                        if c() != 0 { frame.pc += 1; }
                    }
                    Op::LoadInt  => R!(a()) = Value::int(sbx() as i64),
                    Op::LoadK    => {
                        let v = self.resolve_const(proto, bx());
                        unsafe { *regs.add(base + a()) = v; }
                    }
                    Op::Move     => R!(a()) = R!(b()),

                    Op::Add  => arith_op!(a(), b(), c(), |x: i64, y: i64| -> VmResult<i64> { Ok(x.wrapping_add(y)) }, |x: f64, y: f64| x + y, "__add"),
                    Op::Sub  => arith_op!(a(), b(), c(), |x: i64, y: i64| -> VmResult<i64> { Ok(x.wrapping_sub(y)) }, |x: f64, y: f64| x - y, "__sub"),
                    Op::Mul  => arith_op!(a(), b(), c(), |x: i64, y: i64| -> VmResult<i64> { Ok(x.wrapping_mul(y)) }, |x: f64, y: f64| x * y, "__mul"),
                    Op::Div  => {
                        let bv = RK!(b()); let cv = RK!(c());
                        if bv.is_number() && cv.is_number() {
                            R!(a()) = Value::float(bv.to_float().unwrap() / cv.to_float().unwrap());
                        } else {
                            let mm = self.get_mm2(bv, cv, "__div");
                            if mm.is_nil() {
                                let bad = if !bv.is_number() { bv } else { cv };
                                return Err(VmError::RuntimeError(format!(
                                    "attempt to perform arithmetic on a {} value", bad.type_name())));
                            }
                            let res = self.call_value_isolated(mm, &[bv, cv])?;
                            self.regs[base + a()] = res.into_iter().next().unwrap_or(Value::nil());
                            continue 'outer;
                        }
                    }
                    Op::IDiv => arith_op!(a(), b(), c(),
                        |x: i64, y: i64| -> VmResult<i64> {
                            if y == 0 { Err(VmError::RuntimeError("attempt to perform 'n//0'".into())) } else { Ok(lua_idiv(x, y)) }
                        },
                        |x: f64, y: f64| (x / y).floor(), "__idiv"),
                    Op::Mod  => arith_op!(a(), b(), c(),
                        |x: i64, y: i64| -> VmResult<i64> {
                            if y == 0 { Err(VmError::RuntimeError("attempt to perform 'n%0'".into())) } else { Ok(lua_mod(x, y)) }
                        },
                        |x: f64, y: f64| x - (x / y).floor() * y, "__mod"),
                    Op::Pow  => {
                        let bv = RK!(b()); let cv = RK!(c());
                        if bv.is_number() && cv.is_number() {
                            R!(a()) = Value::float(bv.to_float().unwrap().powf(cv.to_float().unwrap()));
                        } else {
                            let mm = self.get_mm2(bv, cv, "__pow");
                            if mm.is_nil() {
                                let bad = if !bv.is_number() { bv } else { cv };
                                return Err(VmError::RuntimeError(format!(
                                    "attempt to perform arithmetic on a {} value", bad.type_name())));
                            }
                            let res = self.call_value_isolated(mm, &[bv, cv])?;
                            self.regs[base + a()] = res.into_iter().next().unwrap_or(Value::nil());
                            continue 'outer;
                        }
                    }
                    Op::Unm => {
                        let v = R!(b());
                        if v.is_int_like() {
                            let n = self.make_int(v.as_int().unwrap().wrapping_neg());
                            R!(a()) = n;
                        } else if v.is_float() {
                            R!(a()) = Value::float(-v.as_float().unwrap());
                        } else {
                            let mm = self.get_mm(v, "__unm");
                            if mm.is_nil() {
                                return Err(VmError::RuntimeError(format!(
                                    "attempt to perform arithmetic on a {} value", v.type_name())));
                            }
                            let res = self.call_value_isolated(mm, &[v, v])?;
                            self.regs[base + a()] = res.into_iter().next().unwrap_or(Value::nil());
                            continue 'outer;
                        }
                    }
                    Op::BAnd => {
                        let bv = int_val(RK!(b()))?; let cv = int_val(RK!(c()))?;
                        let res = self.make_int(bv & cv);
                        R!(a()) = res;
                    }
                    Op::BOr  => {
                        let bv = int_val(RK!(b()))?; let cv = int_val(RK!(c()))?;
                        let res = self.make_int(bv | cv);
                        R!(a()) = res;
                    }
                    Op::BXor => {
                        let bv = int_val(RK!(b()))?; let cv = int_val(RK!(c()))?;
                        let res = self.make_int(bv ^ cv);
                        R!(a()) = res;
                    }
                    Op::Shl  => {
                        let bv = int_val(RK!(b()))?; let cv = int_val(RK!(c()))?;
                        let res = self.make_int(lua_shl(bv, cv));
                        R!(a()) = res;
                    }
                    Op::Shr  => {
                        let bv = int_val(RK!(b()))?; let cv = int_val(RK!(c()))?;
                        let res = self.make_int(lua_shl(bv, cv.wrapping_neg()));
                        R!(a()) = res;
                    }
                    Op::BNot => {
                        let v = int_val(R!(b()))?;
                        let res = self.make_int(!v);
                        R!(a()) = res;
                    }
                    Op::Not  => R!(a()) = Value::bool(!R!(b()).is_truthy()),
                    Op::Len  => {
                        let v = R!(b());
                        if v.is_string() {
                            let s = unsafe { string_ref(v) };
                            R!(a()) = Value::int(s.len() as i64);
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
                                    self.regs[base + a()] = res.into_iter().next().unwrap_or(Value::nil());
                                    continue 'outer;
                                }
                            }
                            unsafe { *regs.add(base + a()) = Value::int(len); }
                        } else {
                            return Err(VmError::RuntimeError(format!("attempt to get length of a {} value", v.type_name())));
                        }
                    }
                    Op::Concat => {
                        let mut vals: Vec<Value> = (b()..=c()).map(|i| unsafe { *regs.add(base + i) }).collect();
                        let mut acc = vals.pop().unwrap_or(Value::nil());
                        for &left in vals.iter().rev() {
                            acc = self.concat_two(left, acc)?;
                        }
                        self.regs[base + a()] = acc;
                        continue 'outer;
                    }

                    Op::Eq => {
                        let bv = RK!(b()); let cv = RK!(c());
                        let eq = if bv.raw_bits() == cv.raw_bits() {
                            true
                        } else {
                            let eq = self.values_eq_mm(bv, cv)?;
                            frame = unsafe { &mut *(self.frames.last_mut().unwrap() as *mut Frame) };
                            proto = unsafe { &*frame.proto };
                            base = frame.base;
                            regs = self.regs.as_mut_ptr();
                            ck!();
                            eq
                        };
                        fused_jmp!(eq == (a() != 0));
                    }

                    Op::Lt => {
                        let bv = RK!(b()); let cv = RK!(c());
                        // Numbers compare inline (floats can never be NaN here —
                        // Value::float maps NaN to nil); only non-numbers take
                        // the metamethod path, which may swap frames.
                        let lt = if bv.is_int() && cv.is_int() {
                            bv.as_int().unwrap() < cv.as_int().unwrap()
                        } else if bv.is_number() && cv.is_number() {
                            value_lt(bv, cv)?
                        } else {
                            let lt = self.value_lt_mm(bv, cv)?;
                            frame = unsafe { &mut *(self.frames.last_mut().unwrap() as *mut Frame) };
                            proto = unsafe { &*frame.proto };
                            base = frame.base;
                            regs = self.regs.as_mut_ptr();
                            ck!();
                            lt
                        };
                        fused_jmp!(lt == (a() != 0));
                    }
                    Op::Le => {
                        let bv = RK!(b()); let cv = RK!(c());
                        let le = if bv.is_int() && cv.is_int() {
                            bv.as_int().unwrap() <= cv.as_int().unwrap()
                        } else if bv.is_number() && cv.is_number() {
                            value_le(bv, cv)?
                        } else {
                            let le = self.value_le_mm(bv, cv)?;
                            frame = unsafe { &mut *(self.frames.last_mut().unwrap() as *mut Frame) };
                            proto = unsafe { &*frame.proto };
                            base = frame.base;
                            regs = self.regs.as_mut_ptr();
                            ck!();
                            le
                        };
                        fused_jmp!(le == (a() != 0));
                    }

                    Op::Test => {
                        fused_jmp!(R!(a()).is_truthy() == (c() != 0));
                    }
                    Op::TestSet => {
                        let bv = R!(b());
                        let taken = bv.is_truthy() == (c() != 0);
                        if taken { R!(a()) = bv; }
                        fused_jmp!(taken);
                    }
                    Op::Jmp => {
                        frame.pc = (frame.pc as i32 + sbx()) as usize;
                        if sbx() < 0 { ck!(); }
                    }

                    Op::NewTable => {
                        let ptr = alloc_table_raw();
                        self.gc.register(ptr);
                        unsafe { *regs.add(base + a()) = Value::table(ptr); }
                        ck!();
                    }
                    Op::GetTable => {
                        let tv = R!(b());
                        let kv = RK!(c());
                        if tv.is_table() {
                            let t = unsafe { &*(tv.as_table().unwrap() as *const Table) };
                            let raw = t.raw_get(kv);
                            if !raw.is_nil() || t.metatable.is_none() {
                                R!(a()) = raw;
                                continue;
                            }
                            let result = self.table_index_chain(tv, kv)?;
                            self.regs[base + a()] = result;
                            frame = unsafe { &mut *(self.frames.last_mut().unwrap() as *mut Frame) };
                            proto = unsafe { &*frame.proto };
                            base = frame.base;
                            regs = self.regs.as_mut_ptr();
                            ck!();
                            continue;
                        }
                        if tv.is_string() {
                            let result = if self.string_lib.is_table() {
                                unsafe { &*(self.string_lib.as_table().unwrap() as *const Table) }.raw_get(kv)
                            } else {
                                Value::nil()
                            };
                            R!(a()) = result;
                            continue;
                        }
                        return Err(VmError::RuntimeError(format!("attempt to index a {} value", tv.type_name())));
                    }
                    Op::SetTable => {
                        let tv = R!(a());
                        let kv = RK!(b());
                        let vv = RK!(c());
                        if tv.is_table() {
                            let t = unsafe { &*(tv.as_table().unwrap() as *const Table) };
                            if t.metatable.is_none() || !t.raw_get(kv).is_nil() {
                                unsafe { table_ref(tv) }.raw_set(kv, vv);
                                continue;
                            }
                            self.table_newindex_chain(tv, kv, vv)?;
                            frame = unsafe { &mut *(self.frames.last_mut().unwrap() as *mut Frame) };
                            proto = unsafe { &*frame.proto };
                            base = frame.base;
                            regs = self.regs.as_mut_ptr();
                            ck!();
                            continue;
                        }
                        return Err(VmError::RuntimeError(format!("attempt to index a {} value", tv.type_name())));
                    }
                    Op::SetList => {
                        let tv = R!(a());
                        let t = unsafe { table_ref(tv) };
                        let offset = (c() as usize - 1) * 50;
                        let n = if b() == 0 { frame.top.saturating_sub(base + a() + 1) } else { b() };
                        for i in 1..=n {
                            let v = R!(a() + i);
                            t.raw_set(Value::int((offset + i) as i64), v);
                        }
                    }

                    Op::GetGlobal => {
                        let k = self.resolve_const(proto, bx());
                        if !k.is_string() {
                            return Err(VmError::RuntimeError("invalid global name".into()));
                        }
                        let v = self.globals.raw_get(k);
                        unsafe { *regs.add(base + a()) = v; }
                    }
                    Op::SetGlobal => {
                        let k = self.resolve_const(proto, bx());
                        if !k.is_string() {
                            return Err(VmError::RuntimeError("invalid global name".into()));
                        }
                        let v = unsafe { *regs.add(base + a()) };
                        self.globals.raw_set(k, v);
                    }

                    Op::Call => {
                        let fn_val = R!(a());
                        let nargs = if b() == 0 { frame.top.saturating_sub(base + a() + 1) as u8 } else { (b() - 1) as u8 };
                        let nresults = if c() == 0 { 255 } else { (c() - 1) as u8 };

                        // Proto-callables (script functions/closures) are the
                        // common case; cfn and __call checks come after.
                        if let Some(cp) = get_proto_callable(fn_val) {
                            self.push_frame(cp.proto, cp.upvals_ptr, cp.upvals_len, base + a() + 1, nargs, nresults)?;
                            // Refresh in place instead of re-entering 'outer;
                            // the checkpoint below covers the GC/budget check.
                            frame = unsafe { &mut *(self.frames.last_mut().unwrap() as *mut Frame) };
                            proto = unsafe { &*frame.proto };
                            base = frame.base;
                            regs = self.regs.as_mut_ptr();
                            ck!();
                            continue;
                        } else if let Some(cfn) = get_cfn(fn_val) {
                            let args_base = base + a() + 1;
                            let args: Vec<Value> = (0..nargs as usize)
                                .map(|i| unsafe { *regs.add(args_base + i) })
                                .collect();
                            let results = cfn(&args)?;
                            // `continue 'outer` re-fetches `frame`/`proto` from self.frames;
                            // a CFunction may have swapped frames (e.g. coroutine.resume),
                            // so the old references must not be touched past this point.
                            self.place_results(base + a(), results, nresults);
                            continue 'outer;
                        } else if fn_val.is_table() {
                            let mm = self.get_mm(fn_val, "__call");
                            if mm.is_nil() {
                                return Err(VmError::RuntimeError("attempt to call a table value".into()));
                            }
                            let args_base = base + a() + 1;
                            let mut mm_args = Vec::with_capacity(nargs as usize + 1);
                            mm_args.push(fn_val);
                            for i in 0..nargs as usize { mm_args.push(unsafe { *regs.add(args_base + i) }); }
                            let results = self.call_value_isolated(mm, &mm_args)?;
                            self.place_results(base + a(), results, nresults);
                            continue 'outer;
                        } else {
                            return Err(VmError::RuntimeError(format!("attempt to call a {}", fn_val.type_name())));
                        }
                    }

                    Op::Return => {
                        let nv = if b() == 0 { frame.top.saturating_sub(base + a()) } else { b() - 1 };
                        let expected = frame.expected_results;
                        let base_save = base;
                        self.frames.pop();
                        if self.frames.is_empty() {
                            self.top_level_results =
                                self.regs[base_save + a()..base_save + a() + nv].to_vec();
                            return Ok(());
                        }
                        // The frame was pushed at (call site A) + 1, so results land
                        // back on the calling instruction's A register. Source and
                        // destination overlap (dst < src), so move in place.
                        self.place_results_from(base_save - 1, base_save + a(), nv, expected);
                        frame = unsafe { &mut *(self.frames.last_mut().unwrap() as *mut Frame) };
                        proto = unsafe { &*frame.proto };
                        base = frame.base;
                        regs = self.regs.as_mut_ptr();
                        ck!();
                        continue;
                    }

                    Op::ForPrep => {
                        // R!(a) keeps the initial value; ForLoop treats it as
                        // the next candidate index. No init-step subtraction:
                        // an i64 wrap there (e.g. init=mininteger, step=1)
                        // would corrupt the first iteration.
                        to_number(R!(a()), "initial value")?;
                        to_number(R!(a() + 1), "limit")?;
                        let step = to_number(R!(a() + 2), "step")?;
                        if matches!(step, Num::Int(0)) || matches!(step, Num::Float(f) if f == 0.0) {
                            return Err(VmError::RuntimeError("'for' step is zero".into()));
                        }
                        frame.pc = (frame.pc as i32 + sbx()) as usize;
                    }
                    Op::ForLoop => {
                        let idx  = to_number(R!(a()), "initial value")?;
                        let lim  = to_number(R!(a() + 1), "limit")?;
                        let step = to_number(R!(a() + 2), "step")?;
                        let in_range = if num_is_positive(step) {
                            num_le(idx, lim)
                        } else {
                            num_le(lim, idx)
                        };
                        if in_range {
                            R!(a() + 3) = R!(a());
                            // Advance the candidate for the next check. Integer
                            // overflow must terminate the loop (Lua semantics),
                            // not wrap the counter back to the other end.
                            R!(a()) = num_add(idx, step);
                            frame.pc = (frame.pc as i32 + sbx()) as usize;
                            ck!();
                        }
                    }

                    // Frame layout is base=fn, +1 state, +2 control, +3 unused,
                    // +4.. loop vars; a script iterator's frame is pushed at a+5
                    // so its Return lands the results on a+4 like the cfn path.
                    Op::TForCall => {
                        let fn_val = R!(a());
                        let state  = R!(a() + 1);
                        let ctrl   = R!(a() + 2);
                        let nresults = c() as u8;
                        if let Some(cfn) = get_cfn(fn_val) {
                            let results = cfn(&[state, ctrl])?;
                            self.place_results(base + a() + 4, results, nresults);
                            continue 'outer;
                        } else {
                            let cp = get_proto_callable(fn_val)
                                .ok_or_else(|| VmError::RuntimeError("attempt to call non-function in for-in".into()))?;
                            let new_base = base + a() + 5;
                            let needed = new_base + unsafe { &*cp.proto }.max_regs as usize + 8;
                            if needed > self.regs.len() { self.regs.resize(needed, Value::nil()); }
                            self.regs[new_base] = state;
                            self.regs[new_base + 1] = ctrl;
                            self.push_frame(cp.proto, cp.upvals_ptr, cp.upvals_len, new_base, 2, nresults)?;
                            frame = unsafe { &mut *(self.frames.last_mut().unwrap() as *mut Frame) };
                            proto = unsafe { &*frame.proto };
                            base = frame.base;
                            regs = self.regs.as_mut_ptr();
                            ck!();
                            continue;
                        }
                    }
                    Op::TForLoop => {
                        if !R!(a() + 4).is_nil() {
                            R!(a() + 2) = R!(a() + 4);
                            frame.pc = (frame.pc as i32 + sbx()) as usize;
                            ck!();
                        }
                    }

                    Op::Closure => {
                        let inner_proto = unsafe { proto.protos.get_unchecked(bx()) };
                        let closure_val = if inner_proto.upvals.is_empty() {
                            Value::userdata(inner_proto as *const Proto as *mut u8)
                        } else {
                            let upvals: Vec<Value> = inner_proto.upvals.iter().map(|desc| {
                                if desc.in_stack {
                                    unsafe { *regs.add(base + desc.idx as usize) }
                                } else if (desc.idx as usize) < frame.upvals_len {
                                    unsafe { *frame.upvals_ptr.add(desc.idx as usize) }
                                } else {
                                    Value::nil()
                                }
                            }).collect();
                            let lc = Box::new(LuaClosure {
                                gc: crate::gc::GcHeader::new(crate::gc::GcKind::Closure),
                                proto: inner_proto as *const Proto,
                                upvals,
                            });
                            let ptr = Box::into_raw(lc) as *mut u8;
                            self.gc.register(ptr);
                            Value::closure(ptr)
                        };
                        unsafe { *regs.add(base + a()) = closure_val; }
                        ck!();
                    }
                    Op::GetUpval => {
                        let v = if b() < frame.upvals_len {
                            unsafe { *frame.upvals_ptr.add(b()) }
                        } else {
                            Value::nil()
                        };
                        R!(a()) = v;
                    }
                    Op::SetUpval => {
                        if b() < frame.upvals_len {
                            unsafe { *frame.upvals_ptr.add(b()) = R!(a()); }
                        }
                    }

                    Op::Vararg => {
                        let n = if b() == 0 { frame.varargs.len() } else { b() - 1 };
                        if base + a() + n > self.regs.len() { self.regs.resize(base + a() + n + 64, Value::nil()); regs = self.regs.as_mut_ptr(); }
                        for i in 0..n {
                            R!(a() + i) = frame.varargs.get(i).copied().unwrap_or(Value::nil());
                        }
                        if b() == 0 { frame.top = base + a() + n; }
                    }

                    Op::Tbc    => self.tbc.push((base + a(), b() != 0)),
                    Op::TbcPop => { self.tbc.pop(); }
                }
            }
        }
    }
    // Writes a call's results at `at`, nil-padding to `expected` (255 = keep
    // them all and record the new top for a following b=0 Call/Return).
    fn place_results(&mut self, at: usize, results: Vec<Value>, expected: u8) {
        let nr = results.len();
        let fill = if expected == 255 { nr } else { expected as usize };
        if at + fill > self.regs.len() { self.regs.resize(at + fill + 64, Value::nil()); }
        for (i, v) in results.into_iter().take(fill).enumerate() { self.regs[at + i] = v; }
        for i in nr..fill { self.regs[at + i] = Value::nil(); }
        if expected == 255 {
            if let Some(f) = self.frames.last_mut() { f.top = at + nr; }
        }
    }

    // Same contract as place_results, but the results already sit in the
    // register window at `src..src+nr` (a returning frame's values), so they
    // move in place instead of round-tripping through a Vec.
    #[inline(always)]
    fn place_results_from(&mut self, at: usize, src: usize, nr: usize, expected: u8) {
        let fill = if expected == 255 { nr } else { expected as usize };
        if at + fill > self.regs.len() { self.regs.resize(at + fill + 64, Value::nil()); }
        let n = nr.min(fill);
        // dst < src always (results land one slot below the callee's base),
        // so a forward copy is safe; a plain loop beats memmove's call
        // overhead for the 0-2 results most returns carry.
        if src != at {
            for i in 0..n { self.regs[at + i] = self.regs[src + i]; }
        }
        for i in n..fill { self.regs[at + i] = Value::nil(); }
        if expected == 255 {
            if let Some(f) = self.frames.last_mut() { f.top = at + nr; }
        }
    }

    fn register_stdlib(&mut self) {
        self.set_global_cfn("print", |args| {
            let mut parts = Vec::with_capacity(args.len());
            for &v in args { parts.push(tostring_value(v)?); }
            emit_line(parts.join("\t"), true);
            Ok(vec![])
        });

        self.set_global_cfn("tostring", |args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_string() { return Ok(vec![v]); }
            Ok(vec![alloc_string_val(&tostring_value(v)?)])
        });

        self.set_global_cfn("tonumber", |args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            let base = match args.get(1) {
                None => 10,
                Some(&b) => int_arg(b, "tonumber")?,
            };
            if base == 10 && (v.is_int_like() || v.is_float()) { return Ok(vec![v]); }
            // With an explicit base the argument must be a string, like Lua.
            if !v.is_string() {
                return if base == 10 { Ok(vec![Value::nil()]) }
                       else { Err(VmError::RuntimeError("tonumber: string expected".into())) };
            }
            let s = unsafe { string_ref(v) };
            if base != 10 {
                if !(2..=36).contains(&base) {
                    return Err(VmError::RuntimeError("tonumber: base out of range".into()));
                }
                // Lua: leading space skipped, optional sign, optional 0x for
                // base 16, then digits only — any trailing junk fails, and
                // overflow wraps modulo 2^64.
                let mut t = s.trim();
                let neg = t.starts_with('-');
                if neg || t.starts_with('+') { t = &t[1..]; }
                if base == 16 { t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t); }
                if t.is_empty() { return Ok(vec![Value::nil()]); }
                let mut n: u64 = 0;
                for c in t.bytes() {
                    let d = (c as char).to_digit(base as u32);
                    match d { Some(d) => n = n.wrapping_mul(base as u64).wrapping_add(d as u64),
                              None => return Ok(vec![Value::nil()]) }
                }
                let n = n as i64;
                return Ok(vec![make_int_via_current_vm(if neg { n.wrapping_neg() } else { n })]);
            }
            let s = s.trim();
            let (neg, body) = match s.strip_prefix('-') { Some(r) => (true, r), None => (false, s) };
            if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
                // Integer hex wraps modulo 2^64 like the lexer; failing that,
                // a hex float (0xA.8p1) is still a number.
                if !hex.is_empty() && hex.bytes().all(|b| (b as char).is_ascii_hexdigit()) {
                    let mut n: u64 = 0;
                    for c in hex.bytes() { n = n.wrapping_mul(16).wrapping_add((c as char).to_digit(16).unwrap() as u64); }
                    let n = n as i64;
                    return Ok(vec![make_int_via_current_vm(if neg { n.wrapping_neg() } else { n })]);
                }
                if let Ok(f) = crate::lexer::parse_hex_float(body) {
                    return Ok(vec![Value::float(if neg { -f } else { f })]);
                }
                return Ok(vec![Value::nil()]);
            }
            if let Ok(n) = s.parse::<i64>() { return Ok(vec![make_int_via_current_vm(n)]); }
            if let Ok(f) = s.parse::<f64>() { return Ok(vec![Value::float(f)]); }
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
                // Like Lua: a non-string message is thrown as-is so pcall
                // returns the original value; strings get the line prefix.
                if msg.is_nil() {
                    return Err(VmError::RuntimeError("assertion failed!".into()));
                }
                if msg.is_string() {
                    return Err(VmError::RuntimeError(unsafe { string_ref(msg) }.to_owned()));
                }
                return Err(VmError::Thrown(msg));
            }
            Ok(args.to_vec())
        });

        self.set_global_cfn("error", |args| {
            let msg = args.first().copied().unwrap_or(Value::nil());
            let level = match args.get(1) {
                None => 1,
                Some(&v) => int_val(v).map_err(|_| {
                    VmError::RuntimeError("bad argument #2 to 'error' (integer expected)".into())
                })?,
            };
            // Non-string messages are thrown as-is at any level (Lua semantics).
            if !msg.is_string() {
                return Err(VmError::Thrown(msg));
            }
            let s = unsafe { string_ref(msg) }.to_owned();
            // level 1 is prefixed by enrich_error_line from the innermost
            // frame; level 0 stays bare; level >= 2 attributes the error to an
            // outer frame here instead.
            if level <= 0 {
                return Err(VmError::Thrown(msg));
            }
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
                return Err(VmError::Thrown(with_current_vm(|vm| vm.intern(&prefixed))
                    .unwrap_or_else(|| alloc_string_val(&prefixed))));
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
                    Err(VmError::Thrown(v)) => Ok(vec![Value::bool(false), v]),
                    Err(e) => {
                        let msg = vm.intern_pub(&e.to_string());
                        Ok(vec![Value::bool(false), msg])
                    }
                }
            })
        });

        self.set_global_cfn("xpcall", |args| {
            let fn_val = args.first().copied().unwrap_or(Value::nil());
            let handler = args.get(1).copied().unwrap_or(Value::nil());
            let call_args = if args.len() > 2 { args[2..].to_vec() } else { vec![] };
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
                        // Thrown values reach the handler as the original
                        // object; internal errors are stringified as before.
                        let msg = match e {
                            VmError::Thrown(v) => v,
                            other => vm.intern_pub(&other.to_string()),
                        };
                        // The handler runs even if it panics/errors itself: its own
                        // failure shouldn't be worse than the error it's handling.
                        let handled = vm.call_value_isolated(handler, &[msg])
                            .unwrap_or_else(|_| vec![msg]);
                        let mut ret = vec![Value::bool(false)];
                        ret.extend(handled);
                        Ok(ret)
                    }
                }
            })
        });

        // Real filesystem access, like io/os; don't expose to untrusted scripts.
        self.set_global_cfn("require", |args| {
            let name = str_arg(args, 0, "require")?.to_owned();
            CURRENT_VM.with(|c| {
                let vm_ptr = c.get();
                if vm_ptr.is_null() { return Err(VmError::RuntimeError("no VM context".into())); }
                let vm = unsafe { &mut *vm_ptr };
                if let Some(&cached) = vm.loaded_modules.get(&name) {
                    return Ok(vec![cached]);
                }
                if name.split('.').any(|seg| seg.is_empty() || seg.contains(['/', '\\'])) {
                    return Err(VmError::RuntimeError(format!("require: invalid module name '{name}'")));
                }
                let path = format!("{}.umbra", name.replace('.', "/"));
                let src = std::fs::read_to_string(&path)
                    .map_err(|e| VmError::RuntimeError(format!("require: cannot open '{path}': {e}")))?;
                let (block, parse_errs) = crate::parse(&src);
                if let Some(e) = parse_errs.first() {
                    return Err(VmError::RuntimeError(format!("require: {path}: {e}")));
                }
                let proto = crate::compile(block, Some(path.clone()))
                    .map_err(|e| VmError::RuntimeError(format!("require: {path}: {e}")))?;
                let results = vm.exec_owned(proto)?;
                let result = results.first().copied().unwrap_or(Value::bool(true));
                vm.loaded_modules.insert(name, result);
                Ok(vec![result])
            })
        });

        // Compiles a source string into a callable chunk; on a syntax error
        // returns none plus the message, Lua-style. The chunk's Proto is
        // owned by the VM (owned_protos) so the returned value stays valid.
        self.set_global_cfn("load", |args| {
            let src = str_arg(args, 0, "load")?.to_owned();
            let chunkname = match args.get(1) {
                Some(v) if v.is_string() => Some(unsafe { string_ref(*v) }.to_owned()),
                _ => None,
            };
            CURRENT_VM.with(|c| {
                let vm_ptr = c.get();
                if vm_ptr.is_null() { return Err(VmError::RuntimeError("no VM context".into())); }
                let vm = unsafe { &mut *vm_ptr };
                let (block, parse_errs) = crate::parse(&src);
                if let Some(e) = parse_errs.first() {
                    return Ok(vec![Value::nil(), vm.intern(&e.to_string())]);
                }
                match crate::compile(block, chunkname) {
                    Err(e) => Ok(vec![Value::nil(), vm.intern(&e.to_string())]),
                    Ok(proto) => {
                        let boxed = Box::new(proto);
                        let ptr: *const crate::chunk::Proto = &*boxed;
                        vm.owned_protos.push(boxed);
                        Ok(vec![Value::userdata(ptr as *mut u8)])
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
                // Resume by position instead of rescanning: an int key inside
                // the array part continues at that index; anything else probes
                // the hash for its slot and continues after it. A deleted or
                // never-present key resumes where it would have sat, which is
                // strictly more useful than Lua's "invalid key" error.
                let mut ai = 0usize;
                let mut hi = 0usize;
                if !key.is_nil() {
                    if key.is_int_like() {
                        let i = key.as_int().unwrap();
                        if i >= 1 && (i as usize) <= t.array.len()
                            && !t.array[(i - 1) as usize].is_nil() {
                            ai = i as usize;
                        } else {
                            hi = t.hash.probe_slot(&TableKey::Int(i))
                                .map(|s| s + 1).unwrap_or(0);
                        }
                    } else if let Some(tk) = TableKey::from_value(key) {
                        hi = t.hash.probe_slot(&tk).map(|s| s + 1).unwrap_or(0);
                    }
                }
                for i in ai..t.array.len() {
                    let v = t.array[i];
                    if !v.is_nil() { return Ok(vec![Value::int((i + 1) as i64), v]); }
                }
                for s in hi..t.hash.slots.len() {
                    let (tag, tk, v) = &t.hash.slots[s];
                    if *tag <= 1 { continue; }
                    let k = match tk {
                        TableKey::Int(n)  => make_int_via_current_vm(*n),
                        TableKey::Bool(b) => Value::bool(*b),
                        TableKey::Str(p)  => Value::string(*p),
                        TableKey::Ptr(p)  => Value::from_raw(*p),
                    };
                    return Ok(vec![k, *v]);
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
            if !t.is_table() { return Err(VmError::RuntimeError("unpack: table expected".into())); }
            let table = unsafe { table_ref(t) };
            let n = table.length();
            let i = match args.get(1) { None => 1, Some(&v) => int_arg(v, "unpack")? };
            let j = match args.get(2) { None => n, Some(&v) => int_arg(v, "unpack")? };
            if checked_range_len(i, j).is_none() {
                return Err(VmError::RuntimeError("unpack: range too large".into()));
            }
            Ok((i..=j).map(|k| table.raw_get(make_int_via_current_vm(k))).collect())
        });

        self.set_global_cfn("select", |args| {
            let sel = args.first().copied().unwrap_or(Value::nil());
            let n = args.len() as i64 - 1;
            if sel.is_string() && unsafe { string_ref(sel) } == "#" {
                return Ok(vec![Value::int(n)]);
            }
            let i = sel.as_int().ok_or_else(|| VmError::RuntimeError("select: index expected".into()))?;
            let start = if i < 0 { n + i } else { i - 1 };
            if i == 0 || start < 0 {
                return Err(VmError::RuntimeError("select: index out of range".into()));
            }
            Ok(args.get(start as usize + 1..).map(|s| s.to_vec()).unwrap_or_default())
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
            if let Some(guard) = mt.get_str("__metatable") {
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
            Ok(vec![Value::bool(values_equal(a, b))])
        });

        self.set_global_cfn("yield", |args| {
            Err(VmError::Yield(args.to_vec()))
        });

        let co_table_ptr = alloc_table_raw();
        self.gc.register(co_table_ptr);

        let create_val = self.make_cfn_val(|args| {
            let fn_val = args.first().copied().unwrap_or(Value::nil());
            with_current_vm(|vm| vm.new_coroutine(fn_val, "coroutine.create"))
                .unwrap_or_else(|| Err(VmError::RuntimeError("no VM context".into())))
                .map(|co| vec![co])
        });

        let resume_val = self.make_cfn_val(|args| {
            let co_val = args.first().copied().unwrap_or(Value::nil());
            if !co_val.is_coroutine() {
                return Err(VmError::RuntimeError("coroutine.resume: expected coroutine".into()));
            }
            with_current_vm(|vm| {
                match vm.resume_coroutine(co_val, &args[1..]) {
                    Ok(vals) => {
                        let mut ret = vec![Value::bool(true)];
                        ret.extend(vals);
                        Ok(ret)
                    }
                    Err(e) if vm.poisoned => Err(e),
                    Err(VmError::Thrown(v)) => Ok(vec![Value::bool(false), v]),
                    Err(e) => {
                        let msg = vm.intern(&e.to_string());
                        Ok(vec![Value::bool(false), msg])
                    }
                }
            }).unwrap_or_else(|| Err(VmError::RuntimeError("no VM context".into())))
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
            with_current_vm(|vm| {
                let co_val = vm.new_coroutine(fn_val, "coroutine.wrap")?;
                let wrapper = vm.make_cfn_val(move |wargs| {
                    with_current_vm(|vm| vm.resume_coroutine(co_val, wargs))
                        .unwrap_or_else(|| Err(VmError::RuntimeError("no VM context".into())))
                });
                Ok(vec![wrapper])
            }).unwrap_or_else(|| Err(VmError::RuntimeError("no VM context".into())))
        });

        let isyieldable_val = self.make_cfn_val(|_args| {
            Ok(vec![Value::bool(with_current_vm(|vm| vm.coroutine_depth > 0).unwrap_or(false))])
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
        self.gc.register(str_table_ptr);

        let v_str_len = self.make_cfn_val(|args| {
            Ok(vec![Value::int(str_arg(args, 0, "string.len")?.len() as i64)])
        });
        let v_str_sub = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "string.sub")?;
            let len = s.len();
            let i = match args.get(1) { None => 1, Some(&v) => int_arg(v, "string.sub")? };
            let j = match args.get(2) { None => -1, Some(&v) => int_arg(v, "string.sub")? };
            let start = lua_str_start(len, i);
            let end   = lua_str_end(len, j);
            if start >= end { return Ok(vec![alloc_string_val("")]); }
            Ok(vec![alloc_string_val(&String::from_utf8_lossy(&s.as_bytes()[start..end]))])
        });
        let v_str_rep = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "string.rep")?;
            let n = match args.get(1) { None => 0, Some(&v) => int_arg(v, "string.rep")? };
            let sep = match args.get(2) {
                None => String::new(),
                Some(&v) if v.is_string() => unsafe { string_ref(v) }.to_owned(),
                Some(&v) if v.is_number() => coerce_to_concat_str(v),
                Some(_) => return Err(VmError::RuntimeError("string.rep: string expected".into())),
            };
            if n <= 0 { return Ok(vec![alloc_string_val("")]); }
            let n = n as usize;
            let total = n.saturating_mul(s.len() + sep.len());
            if total > MAX_ALLOC_LEN {
                return Err(VmError::RuntimeError("string.rep: result too large".into()));
            }
            // Write the result in place: a Vec of n &str parts would cost
            // 16 bytes per repeat on top of the join's own buffer.
            let mut buf = Vec::with_capacity(total + 1);
            for i in 0..n {
                if i > 0 { buf.extend_from_slice(sep.as_bytes()); }
                buf.extend_from_slice(s.as_bytes());
            }
            let len = buf.len();
            let raw = alloc_string_owned(buf, len);
            CURRENT_VM.with(|c| {
                let ptr = c.get();
                if !ptr.is_null() { unsafe { &mut *ptr }.gc.register(raw); }
            });
            Ok(vec![Value::string(raw)])
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
            let i = match args.get(1) { None => 1, Some(&v) => int_arg(v, "string.byte")? };
            let j = match args.get(2) { None => i, Some(&v) => int_arg(v, "string.byte")? };
            let start = lua_str_start(s.len(), i);
            let end   = lua_str_end(s.len(), j).min(s.len());
            if end.saturating_sub(start) > MAX_ALLOC_LEN / 8 {
                return Err(VmError::RuntimeError("string.byte: string slice too long".into()));
            }
            let bytes = s.as_bytes();
            Ok((start..end).map(|k| Value::int(bytes[k] as i64)).collect())
        });
        let v_str_char = self.make_cfn_val(|args| {
            let mut s = String::with_capacity(args.len());
            for &v in args {
                let n = int_arg(v, "string.char")?;
                let b = u8::try_from(n).map_err(|_| VmError::RuntimeError("string.char: value out of range".into()))?;
                s.push(b as char);
            }
            Ok(vec![alloc_string_val(&s)])
        });
        let v_str_find = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "string.find")?;
            let pat = str_arg(args, 1, "string.find")?;
            let init = match args.get(2) { None => 1, Some(&v) => int_arg(v, "string.find")? };
            let plain = args.get(3).map(|&v| v.is_truthy()).unwrap_or(false);
            // Lua: init past len+1 finds nothing (even an empty pattern).
            if init > s.len() as i64 + 1 { return Ok(vec![Value::nil()]); }
            let start = lua_str_start(s.len(), init);
            if plain {
                let hay = &s.as_bytes()[start..];
                let found = if pat.is_empty() { Some(0) } else {
                    hay.windows(pat.len()).position(|w| w == pat.as_bytes())
                };
                return match found {
                    None => Ok(vec![Value::nil()]),
                    Some(pos) => Ok(vec![
                        Value::int((start + pos + 1) as i64),
                        Value::int((start + pos + pat.len()) as i64),
                    ]),
                };
            }
            match pattern::find_from(s.as_bytes(), pat.as_bytes(), start) {
                Ok(Some(m)) => {
                    let mut ret = vec![Value::int(m.start as i64 + 1), Value::int(m.end as i64)];
                    ret.extend(pattern_captures(s.as_bytes(), &m));
                    Ok(ret)
                }
                Ok(None) => Ok(vec![Value::nil()]),
                Err(e) => Err(VmError::RuntimeError(format!("string.find: {e}"))),
            }
        });
        let v_str_match = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "string.match")?;
            let pat = str_arg(args, 1, "string.match")?;
            let init = match args.get(2) { None => 1, Some(&v) => int_arg(v, "string.match")? };
            if init > s.len() as i64 + 1 { return Ok(vec![Value::nil()]); }
            let start = lua_str_start(s.len(), init);
            match pattern::find_from(s.as_bytes(), pat.as_bytes(), start) {
                Ok(Some(m)) => {
                    if m.captures.is_empty() {
                        Ok(vec![alloc_string_val(&String::from_utf8_lossy(&s.as_bytes()[m.start..m.end]))])
                    } else {
                        Ok(pattern_captures(s.as_bytes(), &m))
                    }
                }
                Ok(None) => Ok(vec![Value::nil()]),
                Err(e) => Err(VmError::RuntimeError(format!("string.match: {e}"))),
            }
        });
        let v_str_gmatch = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "string.gmatch")?.to_owned();
            let pat = str_arg(args, 1, "string.gmatch")?.to_owned();
            let init = match args.get(2) { None => 1, Some(&v) => int_arg(v, "string.gmatch")? };
            let pos = Cell::new(if init > s.len() as i64 + 1 { usize::MAX }
                                else { lua_str_start(s.len(), init) });
            let iter_val = CURRENT_VM.with(|c| {
                let vm_ptr = c.get();
                if vm_ptr.is_null() { return Value::nil(); }
                unsafe { &mut *vm_ptr }.make_cfn_val(move |_args| {
                    let sb = s.as_bytes();
                    if pos.get() > sb.len() { return Ok(vec![Value::nil()]); }
                    match pattern::find_from(sb, pat.as_bytes(), pos.get()) {
                        Ok(Some(m)) => {
                            // Empty match: step by one byte past it so the next
                            // call makes forward progress instead of looping forever.
                            // An anchored pattern only ever matches once.
                            pos.set(if pat.starts_with('^') { sb.len() + 1 }
                                    else if m.end > m.start { m.end } else { m.end + 1 });
                            if m.captures.is_empty() {
                                Ok(vec![alloc_string_val(&String::from_utf8_lossy(&sb[m.start..m.end]))])
                            } else {
                                Ok(pattern_captures(sb, &m))
                            }
                        }
                        Ok(None) => { pos.set(sb.len() + 1); Ok(vec![Value::nil()]) }
                        Err(e) => Err(VmError::RuntimeError(format!("string.gmatch: {e}"))),
                    }
                })
            });
            Ok(vec![iter_val])
        });
        let v_str_gsub = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "string.gsub")?;
            let pat = str_arg(args, 1, "string.gsub")?;
            let repl = args.get(2).copied().unwrap_or(Value::nil());
            let max_n = match args.get(3) { None => i64::MAX, Some(&v) => int_arg(v, "string.gsub")? };
            let sb = s.as_bytes();
            let mut out: Vec<u8> = Vec::new();
            let mut pos = 0usize;
            let mut count: i64 = 0;
            while count < max_n && pos <= sb.len() {
                let m = match pattern::find_from(sb, pat.as_bytes(), pos) {
                    Ok(Some(m)) => m,
                    Ok(None) => break,
                    Err(e) => return Err(VmError::RuntimeError(format!("string.gsub: {e}"))),
                };
                out.extend_from_slice(&sb[pos..m.start]);
                let whole = &sb[m.start..m.end];
                match apply_gsub_repl(repl, sb, &m, whole)? {
                    Some(bytes) => out.extend_from_slice(&bytes),
                    None => out.extend_from_slice(whole),
                }
                count += 1;
                pos = if m.end > m.start {
                    m.end
                } else {
                    if m.end < sb.len() { out.push(sb[m.end]); }
                    m.end + 1
                };
                if pat.starts_with('^') { break; }
            }
            if pos <= sb.len() { out.extend_from_slice(&sb[pos..]); }
            Ok(vec![alloc_string_val(&String::from_utf8_lossy(&out)), Value::int(count)])
        });
        let v_str_format = self.make_cfn_val(|args| {
            let fmt = str_arg(args, 0, "string.format")?;
            string_format(fmt, if args.len() > 1 { &args[1..] } else { &[] })
        });
        // Strings must be valid UTF-8 (string_ref uses from_utf8_unchecked), so
        // the packed blob is hex-encoded rather than raw bytes, unlike Lua.
        let v_str_pack = self.make_cfn_val(|args| {
            let fmt = str_arg(args, 0, "string.pack")?;
            let mut pvals = Vec::new();
            for &v in args.get(1..).unwrap_or(&[]) {
                let pv = if v.is_string() {
                    pack::PackValue::Str(unsafe { string_ref(v) }.as_bytes().to_vec())
                } else if v.is_int_like() {
                    pack::PackValue::Int(v.as_int().unwrap())
                } else if v.is_float() {
                    pack::PackValue::Float(v.as_float().unwrap())
                } else {
                    return Err(VmError::RuntimeError("string.pack: unsupported argument type".into()));
                };
                pvals.push(pv);
            }
            let bytes = pack::pack(fmt, &pvals).map_err(VmError::RuntimeError)?;
            Ok(vec![alloc_string_val(&bytes_to_hex(&bytes))])
        });
        let v_str_unpack = self.make_cfn_val(|args| {
            let fmt = str_arg(args, 0, "string.unpack")?;
            let hex = str_arg(args, 1, "string.unpack")?;
            let bytes = hex_to_bytes(hex)
                .ok_or_else(|| VmError::RuntimeError("string.unpack: invalid packed data".into()))?;
            // Lua's posrelatI: negative counts back from the end; 0 or a
            // position past len+1 is an error, not a silent clamp.
            let init = match args.get(2) { None => 1, Some(&v) => int_arg(v, "string.unpack")? };
            let start = if init > 0 { init }
                        else if init != 0 && -init <= bytes.len() as i64 { bytes.len() as i64 + init + 1 }
                        else { 0 };
            if start < 1 || start > bytes.len() as i64 + 1 {
                return Err(VmError::RuntimeError("string.unpack: initial position out of bounds".into()));
            }
            let start = (start - 1) as usize;
            let (vals, end_pos) = pack::unpack(fmt, &bytes, start).map_err(VmError::RuntimeError)?;
            let mut out: Vec<Value> = vals.into_iter().map(|pv| match pv {
                pack::PackValue::Int(n) => make_int_via_current_vm(n),
                pack::PackValue::Float(f) => Value::float(f),
                pack::PackValue::Str(s) => alloc_string_val(&String::from_utf8_lossy(&s)),
            }).collect();
            out.push(make_int_via_current_vm((end_pos + 1) as i64));
            Ok(out)
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
            let k = self.intern("match");   st.raw_set(k, v_str_match);
            let k = self.intern("gmatch");  st.raw_set(k, v_str_gmatch);
            let k = self.intern("gsub");    st.raw_set(k, v_str_gsub);
            let k = self.intern("format");  st.raw_set(k, v_str_format);
            let k = self.intern("pack");    st.raw_set(k, v_str_pack);
            let k = self.intern("unpack");  st.raw_set(k, v_str_unpack);
        }
        let str_table_val = Value::table(str_table_ptr);
        self.string_lib = str_table_val;
        let k_string = self.intern("string");
        self.globals.raw_set(k_string, str_table_val);

        let math_table_ptr = alloc_table_raw();
        self.gc.register(math_table_ptr);

        let v_math_floor = self.make_cfn_val(|args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() { return Ok(vec![v]); }
            let f = v.as_float().ok_or_else(|| VmError::RuntimeError("math.floor: number expected".into()))?;
            Ok(vec![float_to_int_or_float(f.floor())])
        });
        let v_math_ceil = self.make_cfn_val(|args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() { return Ok(vec![v]); }
            let f = v.as_float().ok_or_else(|| VmError::RuntimeError("math.ceil: number expected".into()))?;
            Ok(vec![float_to_int_or_float(f.ceil())])
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
                // Bounds first: `f as i64` saturates, and i64::MAX as f64 is
                // exactly 2^63, so the round-trip check alone would wrongly
                // accept 2^63 (and any larger float) as i64::MAX.
                if f >= -9223372036854775808.0 && f < 9223372036854775808.0 {
                    let i = f as i64;
                    if i as f64 == f { return Ok(vec![make_int_via_current_vm(i)]); }
                }
            }
            Ok(vec![Value::nil()])
        });
        let v_math_random = self.make_cfn_val(|args| {
            if args.len() > 2 {
                return Err(VmError::RuntimeError("math.random: wrong number of arguments".into()));
            }
            let r = RNG.with(|c| { let n = splitmix64(c.get()); c.set(n); n });
            match args.len() {
                0 => Ok(vec![Value::float((r >> 11) as f64 * (1.0_f64 / (1u64 << 53) as f64))]),
                1 => {
                    let m = int_arg(args[0], "math.random")?;
                    if m < 1 { return Err(VmError::RuntimeError("math.random: interval is empty".into())); }
                    Ok(vec![make_int_via_current_vm(1 + (r % m as u64) as i64)])
                }
                _ => {
                    let lo = int_arg(args[0], "math.random")?;
                    let hi = int_arg(args[1], "math.random")?;
                    if lo > hi { return Err(VmError::RuntimeError("math.random: interval is empty".into())); }
                    // The full i64 span can't be a u64 range; Lua treats it
                    // as "any bits" rather than erroring.
                    if lo == i64::MIN && hi == i64::MAX {
                        return Ok(vec![make_int_via_current_vm(r as i64)]);
                    }
                    let range = hi as i128 - lo as i128 + 1;
                    Ok(vec![make_int_via_current_vm((lo as i128 + (r % range as u64) as i128) as i64)])
                }
            }
        });
        let v_math_randomseed = self.make_cfn_val(|args| {
            let seed = match args.first() { None => 0, Some(&v) => int_arg(v, "math.randomseed")? };
            RNG.with(|c| c.set(seed as u64));
            Ok(vec![])
        });
        let v_math_fmod = self.make_cfn_val(|args| {
            let a = args.first().copied().unwrap_or(Value::nil());
            let b = args.get(1).copied().unwrap_or(Value::nil());
            if a.is_int_like() && b.is_int_like() {
                let (x, y) = (a.as_int().unwrap(), b.as_int().unwrap());
                if y == 0 { return Err(VmError::RuntimeError("math.fmod: zero divisor".into())); }
                return Ok(vec![make_int_via_current_vm(x.wrapping_rem(y))]);
            }
            let x = a.to_float().ok_or_else(|| VmError::RuntimeError("math.fmod: number expected".into()))?;
            let y = b.to_float().ok_or_else(|| VmError::RuntimeError("math.fmod: number expected".into()))?;
            Ok(vec![Value::float(x % y)])
        });
        let v_math_ult = self.make_cfn_val(|args| {
            let a = int_arg(args.first().copied().unwrap_or(Value::nil()), "math.ult")?;
            let b = int_arg(args.get(1).copied().unwrap_or(Value::nil()), "math.ult")?;
            Ok(vec![Value::bool((a as u64) < (b as u64))])
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
            let k = self.intern("fmod");        mt.raw_set(k, v_math_fmod);
            let k = self.intern("ult");         mt.raw_set(k, v_math_ult);
        }
        let k_math = self.intern("math");
        self.globals.raw_set(k_math, Value::table(math_table_ptr));

        let tbl_table_ptr = alloc_table_raw();
        self.gc.register(tbl_table_ptr);

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
                    let pos = int_arg(args[1], "table.insert")?;
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
            let pos = match args.get(1) { None => n, Some(&v) => int_arg(v, "table.remove")? };
            if n == 0 || pos < 1 || pos > n { return Ok(vec![Value::nil()]); }
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
            let sep = match args.get(1) {
                None => String::new(),
                Some(&v) if v.is_nil() => String::new(),
                Some(&v) if v.is_string() => unsafe { string_ref(v) }.to_owned(),
                Some(&v) if v.is_number() => coerce_to_concat_str(v),
                Some(_) => return Err(VmError::RuntimeError("table.concat: string expected".into())),
            };
            let n = tbl.length();
            let i = match args.get(2) { None => 1, Some(&v) => int_arg(v, "table.concat")? };
            let j = match args.get(3) { None => n, Some(&v) => int_arg(v, "table.concat")? };
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
        // Sorts a copy: the comparator can error or trigger a collection, and
        // the table must keep both its contents and its roots either way.
        let v_tbl_sort = self.make_cfn_val(|args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() { return Err(VmError::RuntimeError("table.sort: table expected".into())); }
            let comp_val: Option<Value> = args.get(1).copied().filter(|v| !v.is_nil());
            let mut arr = unsafe { table_ref(t) }.array.clone();
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
                    }).and_then(|lt| {
                        // Lua's "invalid order function": a comparator that
                        // claims both a<b and b<a can't drive a sort.
                        if !lt { return Ok(false); }
                        CURRENT_VM.with(|c| {
                            let vm_ptr = c.get();
                            if vm_ptr.is_null() { return Ok(false); }
                            let vm = unsafe { &mut *vm_ptr };
                            match vm.call_value_isolated(cf, &[*b, *a]) {
                                Ok(res) => {
                                    if res.into_iter().next().unwrap_or(Value::nil()).is_truthy() {
                                        Err(VmError::RuntimeError("invalid order function for sorting".into()))
                                    } else {
                                        Ok(true)
                                    }
                                }
                                Err(e) => Err(e),
                            }
                        })
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
        let v_tbl_pack = self.make_cfn_val(|args| {
            let ptr = alloc_table_raw();
            CURRENT_VM.with(|c| {
                let vm_ptr = c.get();
                if !vm_ptr.is_null() { unsafe { &mut *vm_ptr }.gc.register(ptr); }
            });
            let tbl = unsafe { &mut *(ptr as *mut Table) };
            for (i, &v) in args.iter().enumerate() {
                tbl.raw_set(make_int_via_current_vm(i as i64 + 1), v);
            }
            let n_val = make_int_via_current_vm(args.len() as i64);
            tbl.raw_set(alloc_string_val("n"), n_val);
            Ok(vec![Value::table(ptr)])
        });
        let v_tbl_unpack = self.make_cfn_val(|args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() { return Err(VmError::RuntimeError("table.unpack: table expected".into())); }
            let tbl = unsafe { &*(t.as_table().unwrap() as *const Table) };
            let n = tbl.length();
            let i = match args.get(1) { None => 1, Some(&v) => int_arg(v, "table.unpack")? };
            let j = match args.get(2) { None => n, Some(&v) => int_arg(v, "table.unpack")? };
            if checked_range_len(i, j).is_none() {
                return Err(VmError::RuntimeError("table.unpack: range too large".into()));
            }
            Ok((i..=j).map(|k| tbl.raw_get(make_int_via_current_vm(k))).collect())
        });
        let v_tbl_move = self.make_cfn_val(|args| {
            let a1 = args.first().copied().unwrap_or(Value::nil());
            if !a1.is_table() { return Err(VmError::RuntimeError("table.move: table expected".into())); }
            let f  = int_arg(args.get(1).copied().unwrap_or(Value::nil()), "table.move")?;
            let e  = int_arg(args.get(2).copied().unwrap_or(Value::nil()), "table.move")?;
            let t  = int_arg(args.get(3).copied().unwrap_or(Value::nil()), "table.move")?;
            let a2 = match args.get(4) {
                None => a1,
                Some(&v) if v.is_table() => v,
                Some(_) => return Err(VmError::RuntimeError("table.move: table expected".into())),
            };
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
            let k = self.intern("pack");   tt.raw_set(k, v_tbl_pack);
            let k = self.intern("unpack"); tt.raw_set(k, v_tbl_unpack);
            let k = self.intern("move");   tt.raw_set(k, v_tbl_move);
        }
        let k_table = self.intern("table");
        self.globals.raw_set(k_table, Value::table(tbl_table_ptr));

        let io_table_ptr = alloc_table_raw();
        self.gc.register(io_table_ptr);

        let v_io_write = self.make_cfn_val(|args| {
            let mut out = String::new();
            for &v in args {
                if v.is_string() { out.push_str(unsafe { string_ref(v) }); }
                else if v.is_number() { out.push_str(&format!("{v}")); }
                else { return Err(VmError::RuntimeError("io.write: string or number expected".into())); }
            }
            emit_line(out, false);
            Ok(vec![])
        });
        let v_io_read = self.make_cfn_val(|args| {
            let fmts: Vec<Value> = if args.is_empty() { vec![Value::nil()] } else { args.to_vec() };
            let mut out = Vec::new();
            for fv in fmts {
                let v = read_stdin_format(&fv)?;
                if v.is_nil() { return Ok(vec![Value::nil()]); }
                out.push(v);
            }
            Ok(out)
        });
        let v_io_open = self.make_cfn_val(|args| {
            let path = str_arg(args, 0, "io.open")?.to_owned();
            let mode = match args.get(1) {
                None => "r".to_owned(),
                Some(&v) if v.is_string() => unsafe { string_ref(v) }.to_owned(),
                Some(_) => return Err(VmError::RuntimeError("io.open: string expected".into())),
            };
            let mode = mode.trim_end_matches('b');
            let mut opts = std::fs::OpenOptions::new();
            match mode {
                "r"  => { opts.read(true); }
                "w"  => { opts.write(true).create(true).truncate(true); }
                "a"  => { opts.append(true).create(true); }
                "r+" => { opts.read(true).write(true); }
                "w+" => { opts.read(true).write(true).create(true).truncate(true); }
                "a+" => { opts.read(true).append(true).create(true); }
                _ => return Err(VmError::RuntimeError(format!("io.open: invalid mode '{mode}'"))),
            }
            let file = match opts.open(&path) {
                Ok(f) => f,
                Err(e) => return Ok(vec![Value::nil(), alloc_string_val(&format!("{path}: {e}"))]),
            };
            let file = std::rc::Rc::new(std::cell::RefCell::new(Some(file)));

            let handle_val = CURRENT_VM.with(|c| -> Value {
                let vm_ptr = c.get();
                if vm_ptr.is_null() { return Value::nil(); }
                let vm = unsafe { &mut *vm_ptr };

                let f = file.clone();
                let v_read = vm.make_cfn_val(move |args| {
                    let mut guard = f.borrow_mut();
                    let file = guard.as_mut()
                        .ok_or_else(|| VmError::RuntimeError("attempt to use a closed file".into()))?;
                    let fmts: Vec<Value> = if args.len() <= 1 { vec![Value::nil()] } else { args[1..].to_vec() };
                    let mut out = Vec::new();
                    for fv in fmts {
                        let v = read_file_format(file, &fv)?;
                        if v.is_nil() { return Ok(vec![Value::nil()]); }
                        out.push(v);
                    }
                    Ok(out)
                });

                // The handle table exists before the method closures so
                // file:write can return it (Lua returns the file for chaining).
                let ft_ptr = alloc_table_raw();
                vm.gc.register(ft_ptr);
                let handle = Value::table(ft_ptr);

                let f = file.clone();
                let v_write = vm.make_cfn_val(move |args| {
                    let mut guard = f.borrow_mut();
                    let file = guard.as_mut()
                        .ok_or_else(|| VmError::RuntimeError("attempt to use a closed file".into()))?;
                    use std::io::Write;
                    let mut out = String::new();
                    for &v in args.get(1..).unwrap_or(&[]) {
                        if v.is_string() { out.push_str(unsafe { string_ref(v) }); }
                        else if v.is_number() { out.push_str(&format!("{v}")); }
                        else { return Err(VmError::RuntimeError("file:write: string or number expected".into())); }
                    }
                    file.write_all(out.as_bytes())
                        .map_err(|e| VmError::RuntimeError(format!("file:write: {e}")))?;
                    Ok(vec![handle])
                });

                let f = file.clone();
                let v_close = vm.make_cfn_val(move |_args| {
                    f.borrow_mut().take();
                    Ok(vec![Value::bool(true)])
                });

                let f = file.clone();
                let v_lines = vm.make_cfn_val(move |_args| {
                    let f = f.clone();
                    let iter_val = CURRENT_VM.with(|c| {
                        let vm_ptr = c.get();
                        if vm_ptr.is_null() { return Value::nil(); }
                        unsafe { &mut *vm_ptr }.make_cfn_val(move |_args| {
                            let mut guard = f.borrow_mut();
                            let file = guard.as_mut()
                                .ok_or_else(|| VmError::RuntimeError("attempt to use a closed file".into()))?;
                            match read_line_from_file(file)? {
                                // Lua closes the file when its lines iterator
                                // reaches EOF.
                                None => { guard.take(); Ok(vec![Value::nil()]) }
                                Some(mut l) => {
                                    while l.ends_with('\n') || l.ends_with('\r') { l.pop(); }
                                    Ok(vec![alloc_string_val(&l)])
                                }
                            }
                        })
                    });
                    Ok(vec![iter_val])
                });

                let ft = unsafe { &mut *(ft_ptr as *mut Table) };
                let k = vm.intern("read");  ft.raw_set(k, v_read);
                let k = vm.intern("write"); ft.raw_set(k, v_write);
                let k = vm.intern("close"); ft.raw_set(k, v_close);
                let k = vm.intern("lines"); ft.raw_set(k, v_lines);
                handle
            });
            Ok(vec![handle_val])
        });
        let v_io_lines = self.make_cfn_val(|args| {
            let path = str_arg(args, 0, "io.lines")?.to_owned();
            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(e) => return Err(VmError::RuntimeError(format!("io.lines: cannot open '{path}': {e}"))),
            };
            let file = std::rc::Rc::new(std::cell::RefCell::new(Some(file)));
            let iter_val = CURRENT_VM.with(|c| {
                let vm_ptr = c.get();
                if vm_ptr.is_null() { return Value::nil(); }
                unsafe { &mut *vm_ptr }.make_cfn_val(move |_args| {
                    let mut guard = file.borrow_mut();
                    let f = guard.as_mut()
                        .ok_or_else(|| VmError::RuntimeError("attempt to use a closed file".into()))?;
                    match read_line_from_file(f)? {
                        None => { guard.take(); Ok(vec![Value::nil()]) }
                        Some(mut l) => {
                            while l.ends_with('\n') || l.ends_with('\r') { l.pop(); }
                            Ok(vec![alloc_string_val(&l)])
                        }
                    }
                })
            });
            Ok(vec![iter_val])
        });
        let v_io_close = self.make_cfn_val(|args| {
            let h = args.first().copied().unwrap_or(Value::nil());
            if !h.is_table() { return Err(VmError::RuntimeError("io.close: file expected".into())); }
            let close = unsafe { table_ref(h) }.raw_get(alloc_string_val("close"));
            CURRENT_VM.with(|c| {
                let vm_ptr = c.get();
                if vm_ptr.is_null() { return Err(VmError::RuntimeError("no VM context".into())); }
                unsafe { &mut *vm_ptr }.call_value_isolated(close, &[h])
            })
        });
        {
            let iot = unsafe { &mut *(io_table_ptr as *mut Table) };
            let k = self.intern("write"); iot.raw_set(k, v_io_write);
            let k = self.intern("read");  iot.raw_set(k, v_io_read);
            let k = self.intern("open");  iot.raw_set(k, v_io_open);
            let k = self.intern("lines"); iot.raw_set(k, v_io_lines);
            let k = self.intern("close"); iot.raw_set(k, v_io_close);
        }
        let k_io = self.intern("io");
        self.globals.raw_set(k_io, Value::table(io_table_ptr));

        let os_table_ptr = alloc_table_raw();
        self.gc.register(os_table_ptr);

        let v_os_time = self.make_cfn_val(|args| {
            if let Some(&t) = args.first() {
                if !t.is_table() {
                    return Err(VmError::RuntimeError("os.time: table expected".into()));
                }
                let tbl = unsafe { &*(t.as_table().unwrap() as *const Table) };
                let get = |name: &str| tbl.raw_get(alloc_string_val(name));
                let field = |name: &str, who: &str| -> VmResult<i64> {
                    let v = get(name);
                    if v.is_nil() {
                        return Err(VmError::RuntimeError(format!("{who}: field '{name}' missing in date table")));
                    }
                    int_arg(v, who)
                };
                let year  = field("year", "os.time")?;
                let month = field("month", "os.time")?;
                let day   = field("day", "os.time")?;
                let hour   = if get("hour").is_nil() { 12 } else { int_arg(get("hour"), "os.time")? };
                let min    = if get("min").is_nil() { 0 } else { int_arg(get("min"), "os.time")? };
                let sec    = if get("sec").is_nil() { 0 } else { int_arg(get("sec"), "os.time")? };
                let isdst  = get("isdst").is_truthy();
                return Ok(vec![make_int_via_current_vm(days_from_civil(year, month, day) * 86400
                    + hour * 3600 + min * 60 + sec - if isdst { 3600 } else { 0 })]);
            }
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64).unwrap_or(0);
            Ok(vec![make_int_via_current_vm(secs)])
        });
        let v_os_clock = self.make_cfn_val(|_args| {
            let start = PROCESS_START.get_or_init(std::time::Instant::now);
            Ok(vec![Value::float(start.elapsed().as_secs_f64())])
        });
        let v_os_getenv = self.make_cfn_val(|args| {
            let name = str_arg(args, 0, "os.getenv")?;
            match std::env::var(name) {
                Ok(v) => Ok(vec![alloc_string_val(&v)]),
                Err(_) => Ok(vec![Value::nil()]),
            }
        });
        let v_os_date = self.make_cfn_val(|args| {
            let mut fmt = match args.first() {
                None => "%c".to_owned(),
                Some(&v) if v.is_string() => unsafe { string_ref(v) }.to_owned(),
                Some(_) => return Err(VmError::RuntimeError("os.date: string expected".into())),
            };
            // '!' selects UTC; Umbra only has UTC, so it's a no-op marker.
            if let Some(rest) = fmt.strip_prefix('!') { fmt = rest.to_owned(); }
            let secs = match args.get(1) {
                None => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64).unwrap_or(0),
                Some(&v) => int_arg(v, "os.date")?,
            };
            if fmt == "*t" {
                return Ok(vec![CURRENT_VM.with(|c| {
                    let vm_ptr = c.get();
                    if vm_ptr.is_null() { return Value::nil(); }
                    let vm = unsafe { &mut *vm_ptr };
                    let ptr = alloc_table_raw();
                    vm.gc.register(ptr);
                    let t = unsafe { &mut *(ptr as *mut Table) };
                    let (year, month, day, yday, wday) = civil_fields(secs);
                    let sod = secs.rem_euclid(86400);
                    let mut set = |t: &mut Table, k: &str, v: i64| {
                        t.raw_set(vm.intern(k), make_int_via_current_vm(v));
                    };
                    set(t, "year", year);
                    set(t, "month", month as i64);
                    set(t, "day", day as i64);
                    set(t, "hour", sod / 3600);
                    set(t, "min", (sod / 60) % 60);
                    set(t, "sec", sod % 60);
                    set(t, "wday", wday as i64 + 1);
                    set(t, "yday", yday as i64);
                    t.raw_set(vm.intern("isdst"), Value::bool(false));
                    Value::table(ptr)
                })]);
            }
            Ok(vec![alloc_string_val(&format_civil_time(secs, &fmt))])
        });
        {
            let ot = unsafe { &mut *(os_table_ptr as *mut Table) };
            let k = self.intern("time");   ot.raw_set(k, v_os_time);
            let k = self.intern("clock");  ot.raw_set(k, v_os_clock);
            let k = self.intern("getenv"); ot.raw_set(k, v_os_getenv);
            let k = self.intern("date");   ot.raw_set(k, v_os_date);
        }
        let k_os = self.intern("os");
        self.globals.raw_set(k_os, Value::table(os_table_ptr));

        let utf8_table_ptr = alloc_table_raw();
        self.gc.register(utf8_table_ptr);

        let v_utf8_char = self.make_cfn_val(|args| {
            let mut s = String::new();
            for &v in args {
                let n = int_arg(v, "utf8.char")?;
                let cp = u32::try_from(n).ok().and_then(char::from_u32)
                    .ok_or_else(|| VmError::RuntimeError("utf8.char: value out of range".into()))?;
                s.push(cp);
            }
            Ok(vec![alloc_string_val(&s)])
        });
        let v_utf8_len = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "utf8.len")?;
            let i = match args.get(1) { None => 1, Some(&v) => int_arg(v, "utf8.len")? };
            let j = match args.get(2) { None => -1, Some(&v) => int_arg(v, "utf8.len")? };
            let start = lua_str_start(s.len(), i);
            let end = lua_str_end(s.len(), j);
            // An empty range is 0 characters, not an error.
            if start > end { return Ok(vec![Value::int(0)]); }
            if !s.is_char_boundary(start) || !s.is_char_boundary(end) {
                return Ok(vec![Value::nil(), Value::int(start as i64 + 1)]);
            }
            Ok(vec![make_int_via_current_vm(s[start..end].chars().count() as i64)])
        });
        let v_utf8_codepoint = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "utf8.codepoint")?;
            let i = match args.get(1) { None => 1, Some(&v) => int_arg(v, "utf8.codepoint")? };
            let j = match args.get(2) { None => i, Some(&v) => int_arg(v, "utf8.codepoint")? };
            let start = lua_str_start(s.len(), i);
            // i/j are start-of-character byte positions, not a byte range end —
            // a multi-byte char's last byte can't be an exact "j" on its own.
            let last_start = lua_str_start(s.len(), j);
            if start > last_start { return Ok(vec![]); }
            if start > s.len() || !s.is_char_boundary(start) {
                return Err(VmError::RuntimeError("utf8.codepoint: invalid byte position".into()));
            }
            let mut result = Vec::new();
            for (off, ch) in s[start..].char_indices() {
                if start + off > last_start { break; }
                result.push(make_int_via_current_vm(ch as i64));
            }
            Ok(result)
        });
        let v_utf8_codes = self.make_cfn_val(|args| {
            let s = str_arg(args, 0, "utf8.codes")?.to_owned();
            let iter_val = CURRENT_VM.with(|c| {
                let vm_ptr = c.get();
                if vm_ptr.is_null() { return Value::nil(); }
                unsafe { &mut *vm_ptr }.make_cfn_val(move |cargs| {
                    let prev = match cargs.get(1) { None => 0, Some(&v) => int_arg(v, "utf8.codes")? };
                    let next_byte = if prev <= 0 { 0usize } else {
                        let p = (prev - 1) as usize;
                        if p >= s.len() || !s.is_char_boundary(p) {
                            return Err(VmError::RuntimeError("utf8.codes: invalid byte position".into()));
                        }
                        p + s[p..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)
                    };
                    match s.get(next_byte..).and_then(|rest| rest.chars().next()) {
                        Some(ch) => Ok(vec![
                            make_int_via_current_vm(next_byte as i64 + 1),
                            make_int_via_current_vm(ch as i64),
                        ]),
                        None => Ok(vec![Value::nil()]),
                    }
                })
            });
            Ok(vec![iter_val, args.first().copied().unwrap_or(Value::nil()), Value::int(0)])
        });
        {
            let ut = unsafe { &mut *(utf8_table_ptr as *mut Table) };
            let k = self.intern("char");      ut.raw_set(k, v_utf8_char);
            let k = self.intern("len");       ut.raw_set(k, v_utf8_len);
            let k = self.intern("codepoint"); ut.raw_set(k, v_utf8_codepoint);
            let k = self.intern("codes");     ut.raw_set(k, v_utf8_codes);
        }
        let k_utf8 = self.intern("utf8");
        self.globals.raw_set(k_utf8, Value::table(utf8_table_ptr));

        let debug_table_ptr = alloc_table_raw();
        self.gc.register(debug_table_ptr);
        let v_debug_traceback = self.make_cfn_val(|args| {
            let msg = args.first().filter(|v| v.is_string())
                .map(|&v| unsafe { string_ref(v) }.to_owned());
            let full = CURRENT_VM.with(|c| {
                let vm_ptr = c.get();
                let tb = if vm_ptr.is_null() {
                    String::new()
                } else {
                    let vm = unsafe { &mut *vm_ptr };
                    vm.last_traceback.clone().unwrap_or_else(|| build_traceback(&vm.frames))
                };
                match &msg {
                    Some(m) => format!("{m}\nstack traceback:\n{tb}"),
                    None => format!("stack traceback:\n{tb}"),
                }
            });
            Ok(vec![alloc_string_val(&full)])
        });
        {
            let dt = unsafe { &mut *(debug_table_ptr as *mut Table) };
            let k = self.intern("traceback"); dt.raw_set(k, v_debug_traceback);
        }
        let k_debug = self.intern("debug");
        self.globals.raw_set(k_debug, Value::table(debug_table_ptr));
    }

    pub fn set_global_cfn(&mut self, name: &str, f: impl Fn(&[Value]) -> VmResult<Vec<Value>> + 'static) {
        let k = self.intern(name);
        let v = self.make_cfn_val(f);
        self.globals.raw_set(k, v);
    }

    // Not GC-tracked (a cfn box lives until the VM is dropped); bit 47 of the
    // payload marks it as a cfn rather than a bare Proto pointer.
    pub fn make_cfn_val(&mut self, f: impl Fn(&[Value]) -> VmResult<Vec<Value>> + 'static) -> Value {
        let boxed: CFunction = Box::new(f);
        let outer: Box<CFunction> = Box::new(boxed);
        let ptr = Box::into_raw(outer);
        self.cfns.push(ptr);
        let tagged = ptr as u64 | (1u64 << 47);
        Value::userdata(tagged as *mut u8)
    }

    pub fn intern_pub(&mut self, s: &str) -> Value { self.intern(s) }

    /// Host-only instruction budget (0 = unlimited); resets the count.
    pub fn set_step_limit(&mut self, limit: u64) {
        self.step_limit = limit;
        self.step_count = 0;
    }

    /// Host-only hard ceiling on live GC-tracked objects (0 = unlimited);
    /// still exceeding it after a collection is an error.
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

// Lua floor division/modulo on integers: the result rounds toward -inf and
// the remainder takes the divisor's sign (unlike Rust's div_euclid, which
// keeps the remainder non-negative). wrapping_* keeps i64::MIN // -1 and
// i64::MIN % -1 panic-free (MIN and 0 respectively, like Lua's wrap).
fn lua_idiv(x: i64, y: i64) -> i64 {
    let q = x.wrapping_div(y);
    let r = x.wrapping_rem(y);
    if r != 0 && (r < 0) != (y < 0) { q - 1 } else { q }
}

fn lua_mod(x: i64, y: i64) -> i64 {
    let r = x.wrapping_rem(y);
    if r != 0 && (r < 0) != (y < 0) { r + y } else { r }
}

// At the top-level boundary (exec/exec_owned) a thrown value that escaped
// every pcall becomes a plain runtime error carrying its display form —
// there is no script-side handler left to receive the object.
fn thrown_to_runtime(e: VmError) -> VmError {
    match e {
        VmError::Thrown(v) if v.is_string() =>
            VmError::RuntimeError(unsafe { string_ref(v) }.to_owned()),
        VmError::Thrown(v) => VmError::RuntimeError(format!("{v}")),
        other => other,
    }
}


fn pattern_captures(subj: &[u8], m: &pattern::Match) -> Vec<Value> {
    m.captures.iter().map(|c| match c {
        pattern::Capture::Position(p) => make_int_via_current_vm(*p as i64),
        pattern::Capture::Str(a, b) => alloc_string_val(&String::from_utf8_lossy(&subj[*a..*b])),
    }).collect()
}

fn apply_gsub_repl(repl: Value, subj: &[u8], m: &pattern::Match, whole: &[u8]) -> VmResult<Option<Vec<u8>>> {
    let cap_bytes = |i: usize| -> Vec<u8> {
        if m.captures.is_empty() { return whole.to_vec(); }
        match &m.captures[i] {
            pattern::Capture::Str(a, b) => subj[*a..*b].to_vec(),
            pattern::Capture::Position(p) => p.to_string().into_bytes(),
        }
    };
    if repl.is_string() {
        let rb = unsafe { string_ref(repl) }.as_bytes().to_vec();
        let mut result = Vec::new();
        let mut i = 0;
        while i < rb.len() {
            if rb[i] == b'%' && i + 1 == rb.len() {
                return Err(VmError::RuntimeError("invalid use of '%' in replacement string".into()));
            }
            if rb[i] == b'%' && i + 1 < rb.len() {
                let c = rb[i + 1];
                if c == b'%' { result.push(b'%'); }
                else if c == b'0' { result.extend_from_slice(whole); }
                else if c.is_ascii_digit() {
                    let idx = (c - b'1') as usize;
                    if m.captures.is_empty() {
                        if idx != 0 {
                            return Err(VmError::RuntimeError("invalid capture index in replacement string".into()));
                        }
                        result.extend_from_slice(whole);
                    } else {
                        if idx >= m.captures.len() {
                            return Err(VmError::RuntimeError("invalid capture index in replacement string".into()));
                        }
                        result.extend_from_slice(&cap_bytes(idx));
                    }
                } else {
                    return Err(VmError::RuntimeError("invalid use of '%' in replacement string".into()));
                }
                i += 2;
            } else {
                result.push(rb[i]);
                i += 1;
            }
        }
        return Ok(Some(result));
    }
    if repl.is_table() {
        let key = if m.captures.is_empty() {
            alloc_string_val(&String::from_utf8_lossy(whole))
        } else {
            match &m.captures[0] {
                pattern::Capture::Str(a, b) => alloc_string_val(&String::from_utf8_lossy(&subj[*a..*b])),
                pattern::Capture::Position(p) => make_int_via_current_vm(*p as i64),
            }
        };
        let v = unsafe { table_ref(repl) }.raw_get(key);
        return gsub_result_value(v);
    }
    if get_cfn(repl).is_some() || get_proto_callable(repl).is_some() {
        let call_args: Vec<Value> = if m.captures.is_empty() {
            vec![alloc_string_val(&String::from_utf8_lossy(whole))]
        } else {
            pattern_captures(subj, m)
        };
        let result = CURRENT_VM.with(|c| {
            let vm_ptr = c.get();
            if vm_ptr.is_null() { return Err(VmError::RuntimeError("no VM context".into())); }
            unsafe { &mut *vm_ptr }.call_value_isolated(repl, &call_args)
        })?;
        let v = result.into_iter().next().unwrap_or(Value::nil());
        return gsub_result_value(v);
    }
    Err(VmError::RuntimeError("bad argument to 'gsub' (string/function/table expected)".into()))
}

fn gsub_result_value(v: Value) -> VmResult<Option<Vec<u8>>> {
    if !v.is_truthy() { return Ok(None); }
    if v.is_string() { return Ok(Some(unsafe { string_ref(v) }.as_bytes().to_vec())); }
    if v.is_number() { return Ok(Some(format!("{v}").into_bytes())); }
    Err(VmError::RuntimeError("invalid replacement value (a table/function must return a string/number/false/nil)".into()))
}

fn coerce_to_concat_str(v: Value) -> String {
    if v.is_string() { unsafe { string_ref(v) }.to_owned() }
    else { coerce_num_str(v) }
}

// Number half of coerce_to_concat_str; concat_two borrows string operands
// instead of copying them, so only numbers need a scratch String.
fn coerce_num_str(v: Value) -> String {
    if v.is_int_like() { v.as_int().unwrap().to_string() }
    else { crate::value::lua_float_str(v.as_float().unwrap()) }
}

// __tostring-aware stringification shared by print/tostring/string.format.
fn tostring_value(v: Value) -> VmResult<String> {
    if v.is_string() { return Ok(unsafe { string_ref(v) }.to_owned()); }
    if v.is_table() {
        let via_mm = with_current_vm(|vm| -> VmResult<Option<String>> {
            let mm = vm.get_mm(v, "__tostring");
            if mm.is_nil() { return Ok(None); }
            let sv = vm.call_value_isolated(mm, &[v])?.into_iter().next().unwrap_or(Value::nil());
            Ok(Some(if sv.is_string() { unsafe { string_ref(sv) }.to_owned() } else { format!("{sv}") }))
        });
        if let Some(s) = via_mm.transpose()?.flatten() { return Ok(s); }
    }
    Ok(format!("{v}"))
}

fn float_to_int_or_float(f: f64) -> Value {
    if f >= -9223372036854775808.0 && f < 9223372036854775808.0 {
        make_int_via_current_vm(f as i64)
    } else {
        Value::float(f)
    }
}

fn int_val(v: Value) -> VmResult<i64> {
    if let Some(n) = v.as_int() { return Ok(n); }
    if let Some(f) = v.as_float() {
        if f.fract() == 0.0 && f >= -9223372036854775808.0 && f < 9223372036854775808.0 {
            return Ok(f as i64);
        }
        return Err(VmError::RuntimeError("number has no integer representation".into()));
    }
    Err(VmError::RuntimeError(format!("integer expected, got {}", v.type_name())))
}

// Strict integer argument for stdlib calls, matching Lua's luaL_checkinteger:
// integral floats convert, anything else is an error (int_from_val silently
// truncated non-integral floats and defaulted non-numbers to 0).
fn int_arg(v: Value, who: &str) -> VmResult<i64> {
    int_val(v).map_err(|e| match e {
        VmError::RuntimeError(m) => VmError::RuntimeError(format!("{who}: {m}")),
        other => other,
    })
}

// Lua shift semantics: negative counts shift the other way, |n| >= 64 gives 0.
fn lua_shl(x: i64, n: i64) -> i64 {
    if n <= -64 || n >= 64 { 0 }
    else if n >= 0 { ((x as u64) << n) as i64 }
    else { ((x as u64) >> -n) as i64 }
}

#[derive(Clone, Copy)]
enum Num { Int(i64), Float(f64) }

fn to_number(v: Value, what: &str) -> VmResult<Num> {
    if v.is_int_like() { return Ok(Num::Int(v.as_int().unwrap())); }
    if v.is_float() { return Ok(Num::Float(v.as_float().unwrap())); }
    Err(VmError::RuntimeError(format!("'for' {what} must be a number, got {}", v.type_name())))
}

fn num_add(a: Num, b: Num) -> Value {
    match (a, b) {
        // Overflow pushes the candidate past every limit as +/-inf, so the
        // loop terminates instead of wrapping the counter (Lua semantics).
        (Num::Int(x), Num::Int(y)) => match x.checked_add(y) {
            Some(n) => make_int_via_current_vm(n),
            None => Value::float(if y >= 0 { f64::INFINITY } else { f64::NEG_INFINITY }),
        },
        (Num::Float(x), Num::Float(y)) => Value::float(x + y),
        (Num::Int(x), Num::Float(y)) => Value::float(x as f64 + y),
        (Num::Float(x), Num::Int(y)) => Value::float(x + y as f64),
    }
}

fn num_le(a: Num, b: Num) -> bool {
    use crate::value::cmp_int_float;
    match (a, b) {
        (Num::Int(x), Num::Int(y))     => x <= y,
        (Num::Float(x), Num::Float(y)) => x <= y,
        // Exact int/float compare: `x as f64` would round e.g. i64::MAX up to
        // 2^63 and wrongly include it in a `<= 2^63` range.
        (Num::Int(x), Num::Float(y))   => cmp_int_float(x, y).is_some_and(|o| o.is_le()),
        (Num::Float(x), Num::Int(y))   => cmp_int_float(y, x).is_some_and(|o| o.is_ge()),
    }
}

fn num_is_positive(n: Num) -> bool {
    match n { Num::Int(x) => x > 0, Num::Float(x) => x > 0.0 }
}

fn values_equal(a: Value, b: Value) -> bool {
    if a.is_string() && b.is_string() {
        return unsafe { string_ref(a) } == unsafe { string_ref(b) };
    }
    a == b
}

fn num_cmp(a: Value, b: Value) -> Option<std::cmp::Ordering> {
    use crate::value::cmp_int_float;
    match (a.as_int(), b.as_int()) {
        (Some(x), Some(y)) => Some(x.cmp(&y)),
        (Some(x), None) => cmp_int_float(x, b.as_float()?),
        (None, Some(y)) => cmp_int_float(y, a.as_float()?).map(|o| o.reverse()),
        (None, None) => a.as_float()?.partial_cmp(&b.as_float()?),
    }
}

fn value_lt(a: Value, b: Value) -> VmResult<bool> {
    if let Some(ord) = num_cmp(a, b) { return Ok(ord.is_lt()); }
    if a.is_string() && b.is_string() {
        return Ok(unsafe { string_ref(a) } < unsafe { string_ref(b) });
    }
    Err(VmError::RuntimeError(format!("attempt to compare {} with {}", a.type_name(), b.type_name())))
}

fn value_le(a: Value, b: Value) -> VmResult<bool> {
    if let Some(ord) = num_cmp(a, b) { return Ok(ord.is_le()); }
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

// No BufReader: the same File is shared (via Rc<RefCell<>>) between a file
// handle's read/write closures, and buffering reads would desync an
// interleaved read/write sequence on "r+"/"w+"/"a+" handles.
fn read_line_from_file(file: &mut std::fs::File) -> VmResult<Option<String>> {
    use std::io::Read;
    let mut bytes: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match file.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                bytes.push(byte[0]);
                if byte[0] == b'\n' { break; }
            }
            Err(e) => return Err(VmError::RuntimeError(format!("file:read: {e}"))),
        }
    }
    if bytes.is_empty() { return Ok(None); }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

// Lua's str_to_number on a maximal prefix: returns (value, bytes consumed
// from the start of `s`, leading whitespace included). Accepts decimal
// ints/floats, hex ints (wrapping) and hex floats, inf/nan.
fn str_to_number_prefix(s: &str) -> Option<(Value, usize)> {
    let ws = s.len() - s.trim_start().len();
    let t = s.trim_start();
    // Try every prefix length, longest first — cheap because numeric prefixes
    // are short; stops at the first parseable one.
    for end in (1..=t.len()).rev() {
        if !t.is_char_boundary(end) { continue; }
        let cand = &t[..end];
        let (neg, body) = match cand.strip_prefix('-') { Some(r) => (true, r), None => (false, cand) };
        let body = body.strip_prefix('+').unwrap_or(body);
        if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            if !hex.is_empty() && hex.bytes().all(|b| (b as char).is_ascii_hexdigit()) {
                let mut n: u64 = 0;
                for c in hex.bytes() { n = n.wrapping_mul(16).wrapping_add((c as char).to_digit(16).unwrap() as u64); }
                let n = n as i64;
                return Some((make_int_via_current_vm(if neg { n.wrapping_neg() } else { n }), ws + end));
            }
            if let Ok(f) = crate::lexer::parse_hex_float(body) {
                return Some((Value::float(if neg { -f } else { f }), ws + end));
            }
            continue;
        }
        if let Ok(n) = cand.parse::<i64>() {
            return Some((make_int_via_current_vm(n), ws + end));
        }
        if let Ok(f) = cand.parse::<f64>() {
            return Some((Value::float(f), ws + end));
        }
    }
    None
}

// Reads a Lua "n" format from a seekable file: skip whitespace, take the
// longest valid numeric prefix, then seek back the unread tail.
fn read_number_from_file(file: &mut std::fs::File) -> VmResult<Value> {
    use std::io::{Read, Seek};
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match file.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                let b = byte[0];
                buf.push(b);
                let numeric = b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-' | b'_');
                let still_ws = buf.iter().all(|c| c.is_ascii_whitespace());
                if !numeric && !still_ws { break; }
            }
            Err(e) => return Err(VmError::RuntimeError(format!("file:read: {e}"))),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let (v, used) = match str_to_number_prefix(&text) {
        Some(r) => r,
        None => (Value::nil(), 0),
    };
    let unread = buf.len() - used;
    if unread > 0 {
        let _ = file.seek(std::io::SeekFrom::Current(-(unread as i64)));
    }
    Ok(v)
}

fn read_file_format(file: &mut std::fs::File, fv: &Value) -> VmResult<Value> {
    use std::io::{Read, Seek};
    if fv.is_number() {
        let n = int_arg(*fv, "file:read")?;
        if n < 0 { return Err(VmError::RuntimeError("file:read: invalid length".into())); }
        if n == 0 {
            // Probe for EOF: empty string if more data, nil at EOF.
            let mut b = [0u8; 1];
            return match file.read(&mut b) {
                Ok(0) => Ok(Value::nil()),
                Ok(_) => {
                    let _ = file.seek(std::io::SeekFrom::Current(-1));
                    Ok(alloc_string_val(""))
                }
                Err(e) => Err(VmError::RuntimeError(format!("file:read: {e}"))),
            };
        }
        let mut buf = vec![0u8; n as usize];
        let got = file.read(&mut buf).map_err(|e| VmError::RuntimeError(format!("file:read: {e}")))?;
        if got == 0 { return Ok(Value::nil()); }
        buf.truncate(got);
        return Ok(alloc_string_val(&String::from_utf8_lossy(&buf)));
    }
    let fmt = if fv.is_string() {
        unsafe { string_ref(*fv) }.trim_start_matches('*').to_owned()
    } else if fv.is_nil() {
        "l".to_owned()
    } else {
        return Err(VmError::RuntimeError("file:read: invalid format".into()));
    };
    match fmt.as_str() {
        "a" => {
            let mut buf = String::new();
            file.read_to_string(&mut buf)
                .map_err(|e| VmError::RuntimeError(format!("file:read: {e}")))?;
            Ok(alloc_string_val(&buf))
        }
        "n" => read_number_from_file(file),
        "l" => match read_line_from_file(file)? {
            None => Ok(Value::nil()),
            Some(mut l) => {
                while l.ends_with('\n') || l.ends_with('\r') { l.pop(); }
                Ok(alloc_string_val(&l))
            }
        },
        "L" => match read_line_from_file(file)? {
            None => Ok(Value::nil()),
            Some(l) => Ok(alloc_string_val(&l)),
        },
        _ => Err(VmError::RuntimeError(format!("file:read: invalid format '{fmt}'"))),
    }
}

fn read_stdin_format(fv: &Value) -> VmResult<Value> {
    use std::io::Read;
    if fv.is_number() {
        let n = int_arg(*fv, "io.read")?;
        if n < 0 { return Err(VmError::RuntimeError("io.read: invalid length".into())); }
        let mut buf = vec![0u8; n as usize];
        let got = std::io::stdin().read(&mut buf)
            .map_err(|e| VmError::RuntimeError(format!("io.read: {e}")))?;
        if got == 0 && n > 0 { return Ok(Value::nil()); }
        buf.truncate(got);
        return Ok(alloc_string_val(&String::from_utf8_lossy(&buf)));
    }
    let fmt = if fv.is_string() {
        unsafe { string_ref(*fv) }.trim_start_matches('*').to_owned()
    } else if fv.is_nil() {
        "l".to_owned()
    } else {
        return Err(VmError::RuntimeError("io.read: invalid format".into()));
    };
    match fmt.as_str() {
        "a" => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)
                .map_err(|e| VmError::RuntimeError(format!("io.read: {e}")))?;
            Ok(alloc_string_val(&buf))
        }
        "n" => {
            // stdin isn't seekable, so the longest numeric prefix of the line
            // is taken and the rest of the line is consumed.
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)
                .map_err(|e| VmError::RuntimeError(format!("io.read: {e}")))?;
            match str_to_number_prefix(&line) {
                Some((v, _)) => Ok(v),
                None => Ok(Value::nil()),
            }
        }
        "l" | "L" => {
            let mut line = String::new();
            let n = std::io::stdin().read_line(&mut line)
                .map_err(|e| VmError::RuntimeError(format!("io.read: {e}")))?;
            if n == 0 { return Ok(Value::nil()); }
            if fmt == "l" { while line.ends_with('\n') || line.ends_with('\r') { line.pop(); } }
            Ok(alloc_string_val(&line))
        }
        _ => Err(VmError::RuntimeError(format!("io.read: invalid format '{fmt}'"))),
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes { s.push_str(&format!("{b:02x}")); }
    s
}

fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) { return None; }
    let sb = s.as_bytes();
    let mut out = Vec::with_capacity(sb.len() / 2);
    for chunk in sb.chunks(2) {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

fn str_arg<'a>(args: &'a [Value], idx: usize, fn_name: &'static str) -> VmResult<&'a str> {
    match args.get(idx) {
        Some(v) if v.is_string() => Ok(unsafe { string_ref(*v) }),
        _ => Err(VmError::RuntimeError(format!("{fn_name}: string expected"))),
    }
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

// C-style %e: Rust's {:e} prints "1.23e4"; C wants "1.23e+04".
fn fmt_e(f: f64, prec: usize, upper: bool) -> String {
    if f.is_nan() { return if upper { "NAN".into() } else { "nan".into() }; }
    if f.is_infinite() { return if upper { "INF".into() } else { "inf".into() }; }
    let t = format!("{:.*e}", prec, f);
    let epos = t.rfind('e').unwrap();
    let ex: i32 = t[epos + 1..].parse().unwrap();
    let s = format!("{}e{}{:02}", &t[..epos], if ex < 0 { "-" } else { "+" }, ex.abs());
    if upper { s.to_uppercase() } else { s }
}

// C-style %g: shortest of %e/%f at the given significant-digit count, with
// trailing zeros stripped.
fn fmt_g(f: f64, prec: usize, upper: bool) -> String {
    if f.is_nan() { return if upper { "NAN".into() } else { "nan".into() }; }
    if f.is_infinite() { return if upper { "INF".into() } else { "inf".into() }; }
    let p = if prec == 0 { 1 } else { prec };
    let e = format!("{:.*e}", p - 1, f);
    let exp: i32 = e[e.rfind('e').unwrap() + 1..].parse().unwrap();
    let s = if exp < -4 || exp >= p as i32 {
        let t = format!("{:.*e}", p - 1, f);
        let epos = t.rfind('e').unwrap();
        let ex: i32 = t[epos + 1..].parse().unwrap();
        let mut m: String = t[..epos].into();
        if m.contains('.') {
            while m.ends_with('0') { m.pop(); }
            if m.ends_with('.') { m.pop(); }
        }
        format!("{}e{}{:02}", m, if ex < 0 { "-" } else { "+" }, ex.abs())
    } else {
        let decimals = (p as i32 - 1 - exp).max(0) as usize;
        let mut t = format!("{:.*}", decimals, f);
        if t.contains('.') {
            while t.ends_with('0') { t.pop(); }
            if t.ends_with('.') { t.pop(); }
        }
        t
    };
    if upper { s.to_uppercase() } else { s }
}

// C-style %a hex float on |f| (caller handles the sign).
fn fmt_a(f: f64, prec: Option<usize>, upper: bool) -> String {
    if f.is_nan() { return if upper { "NAN".into() } else { "nan".into() }; }
    if f.is_infinite() { return if upper { "INF".into() } else { "inf".into() }; }
    if f == 0.0 { return "0x0p+0".into(); }
    let bits = f.abs().to_bits();
    let raw_exp = ((bits >> 52) & 0x7ff) as i64;
    let mut frac = bits & ((1u64 << 52) - 1);
    let (mut lead, mut e2) = if raw_exp == 0 { (0u64, -1022i64) } else { (1u64, raw_exp - 1023) };
    let mut digits = String::new();
    match prec {
        None => {
            // Minimal form: emit all 13 nibbles, then drop trailing zeros.
            let mut ds = Vec::new();
            let mut m = frac;
            for _ in 0..13 { ds.push((m >> 48) & 0xf); m <<= 4; }
            while ds.last() == Some(&0) { ds.pop(); }
            for d in ds { digits.push(char::from_digit(d as u32, 16).unwrap()); }
        }
        Some(p) => {
            if p < 13 {
                // Round the 52-bit mantissa to p hex digits (half up).
                let drop = 52 - p as u32 * 4;
                frac += 1u64 << (drop - 1);
                if frac >= 1u64 << 52 {
                    frac = 0;
                    if lead == 0 { lead = 1; } else { e2 += 1; }
                }
                frac >>= drop;
                for k in (0..p).rev() {
                    digits.push(char::from_digit(((frac >> (k * 4)) & 0xf) as u32, 16).unwrap());
                }
            } else {
                let mut m = frac;
                for _ in 0..13 { digits.push(char::from_digit(((m >> 48) & 0xf) as u32, 16).unwrap()); m <<= 4; }
                for _ in 13..p { digits.push('0'); }
            }
        }
    }
    let s = if digits.is_empty() { format!("0x{lead}p{e2:+}") }
            else { format!("0x{lead}.{digits}p{e2:+}") };
    if upper { s.to_uppercase() } else { s }
}

fn string_format(fmt: &str, args: &[Value]) -> VmResult<Vec<Value>> {
    let mut out = String::new();
    let bytes = fmt.as_bytes();
    let mut pos = 0usize;
    let mut arg_idx = 0usize;

    while pos < bytes.len() {
        if bytes[pos] != b'%' {
            let run_end = bytes[pos..].iter().position(|&b| b == b'%').map(|p| pos + p).unwrap_or(bytes.len());
            out.push_str(&fmt[pos..run_end]);
            pos = run_end;
            continue;
        }
        pos += 1;
        if pos >= bytes.len() {
            return Err(VmError::RuntimeError("invalid option '%' to 'format'".into()));
        }
        if bytes[pos] == b'%' { out.push('%'); pos += 1; continue; }

        let mut left = false;
        let mut plus = false;
        let mut zero = false;
        let mut space = false;
        let mut alt = false;
        loop {
            match bytes.get(pos) {
                Some(b'-') => { left = true;  pos += 1; }
                Some(b'+') => { plus = true;  pos += 1; }
                Some(b'0') if !left => { zero = true; pos += 1; }
                Some(b' ') => { space = true; pos += 1; }
                Some(b'#') => { alt = true; pos += 1; }
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
        if pos >= bytes.len() {
            return Err(VmError::RuntimeError("invalid option '%' to 'format'".into()));
        }
        let spec = bytes[pos] as char; pos += 1;

        arg_idx += 1;
        let v = args.get(arg_idx - 1).copied().ok_or_else(|| VmError::RuntimeError(
            format!("bad argument #{arg_idx} to 'format' (value expected)")))?;
        let num_err = |m: &str| VmError::RuntimeError(format!("bad argument #{arg_idx} to 'format' ({m})"));
        let fmt_int = |v: Value| -> VmResult<i64> {
            if let Some(n) = v.as_int() { return Ok(n); }
            if let Some(f) = v.as_float() {
                if f.fract() == 0.0 && f >= -9223372036854775808.0 && f < 9223372036854775808.0 {
                    return Ok(f as i64);
                }
                return Err(num_err("number has no integer representation"));
            }
            Err(num_err("number expected"))
        };
        let fmt_num = |v: Value| -> VmResult<f64> {
            v.to_float().ok_or_else(|| num_err("number expected"))
        };
        // Sign + zero-padding shared by the float conversions.
        let float_body = |f: f64, body: String| -> String {
            let sign = if f.is_sign_negative() { "-" }
                       else if plus { "+" } else if space { " " } else { "" };
            format!("{sign}{body}")
        };

        let s = match spec {
            'd' | 'i' => {
                let n = fmt_int(v)?;
                let mut raw = n.unsigned_abs().to_string();
                // Precision for integers means minimum digits (zero-filled),
                // disables the '0' flag, and %.0d of zero prints nothing.
                if let Some(p) = prec {
                    if p == 0 && n == 0 { raw.clear(); }
                    while raw.len() < p { raw.insert(0, '0'); }
                }
                let signed = if n < 0 { format!("-{raw}") }
                             else if plus { format!("+{raw}") }
                             else if space { format!(" {raw}") }
                             else { raw };
                pad_str(signed, width, left, zero && prec.is_none())
            }
            'u' => {
                let n = fmt_int(v)? as u64;
                let mut raw = n.to_string();
                if let Some(p) = prec {
                    if p == 0 && n == 0 { raw.clear(); }
                    while raw.len() < p { raw.insert(0, '0'); }
                }
                pad_str(raw, width, left, zero && prec.is_none())
            }
            'x' | 'X' | 'o' => {
                let n = fmt_int(v)? as u64;
                let mut raw = match spec { 'x' => format!("{n:x}"), 'X' => format!("{n:X}"), _ => format!("{n:o}") };
                if let Some(p) = prec {
                    if p == 0 && n == 0 { raw.clear(); }
                    while raw.len() < p { raw.insert(0, '0'); }
                }
                // For octal, '#' forces a leading zero even at precision 0.
                if alt && spec == 'o' && raw.is_empty() { raw = "0".into(); }
                let prefix = if alt && n != 0 {
                    match spec { 'x' => "0x", 'X' => "0X", _ => "" }
                } else { "" };
                // Zero-padding goes after the base prefix: %#08x → 0x0000ff.
                if zero && prec.is_none() && !left && raw.len() + prefix.len() < width {
                    let zeros = width - prefix.len() - raw.len();
                    raw = format!("{}{raw}", "0".repeat(zeros));
                }
                pad_str(format!("{prefix}{raw}"), width, left, zero && prec.is_none() && prefix.is_empty())
            }
            'c' => {
                let n = fmt_int(v)?;
                let ch = u8::try_from(n).map(|b| b as char)
                    .map_err(|_| num_err("char out of range"))?;
                pad_str(ch.to_string(), width, left, false)
            }
            'f' | 'F' => {
                let f = fmt_num(v)?;
                let body = format!("{:.*}", prec.unwrap_or(6), f.abs());
                pad_str(float_body(f, body), width, left, zero && f.is_finite())
            }
            'e' | 'E' => {
                let f = fmt_num(v)?;
                let body = fmt_e(f.abs(), prec.unwrap_or(6), spec == 'E');
                pad_str(float_body(f, body), width, left, zero && f.is_finite())
            }
            'g' | 'G' => {
                let f = fmt_num(v)?;
                let body = fmt_g(f.abs(), prec.unwrap_or(6), spec == 'G');
                pad_str(float_body(f, body), width, left, zero && f.is_finite())
            }
            'a' | 'A' => {
                let f = fmt_num(v)?;
                let body = fmt_a(f.abs(), prec, spec == 'A');
                pad_str(float_body(f, body), width, left, zero && f.is_finite())
            }
            's' => {
                let raw = tostring_value(v)?;
                let raw = if let Some(p) = prec { raw.chars().take(p).collect::<String>() } else { raw };
                pad_str(raw, width, left, false)
            }
            'q' => {
                if v.is_int_like() {
                    v.as_int().unwrap().to_string()
                } else if v.is_float() {
                    // {:?} prints the shortest round-trip form with a
                    // mandatory fraction/exponent, so it re-reads as a float.
                    format!("{:?}", v.as_float().unwrap())
                } else if v.is_string() {
                    let s = unsafe { string_ref(v) };
                    let mut q = String::from("\"");
                    for ch in s.chars() {
                        match ch {
                            '"'  => q.push_str("\\\""),
                            '\\' => q.push_str("\\\\"),
                            '\n' => q.push_str("\\n"),
                            '\r' => q.push_str("\\r"),
                            c if (c as u32) < 0x20 || (c as u32) == 0x7f =>
                                q.push_str(&format!("\\{:03}", c as u32)),
                            _    => q.push(ch),
                        }
                    }
                    q.push('"'); q
                } else {
                    return Err(num_err("value has no literal form"));
                }
            }
            _ => return Err(VmError::RuntimeError(format!("invalid option '%{spec}' to 'format'"))),
        };
        out.push_str(&s);
    }
    Ok(vec![alloc_string_val(&out)])
}
