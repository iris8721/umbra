use crate::chunk::{Const, GcProto, Op, Proto, StrConst, ia, ib, ibx, ic, ici, iop, is_rk, isbx, rk_idx};
use crate::embed::{Ctx, ModuleLoader};
use crate::gc::{Gc, GcColor};
use crate::pack;
use crate::pattern;
use std::cell::Cell;
use std::collections::HashMap;

// Where print and io.write go instead of stdout (Vm::new_with_print).
type PrintHook = Box<dyn Fn(&str, bool)>;

// print/io.write go through the VM's hook when one is set, else stdout.
// `newline` is whether a line break follows `line`: print ends its line,
// io.write doesn't.
fn emit_line(vm: &Vm, line: String, newline: bool) {
    if let Some(hook) = &vm.print_hook {
        hook(&line, newline);
        return;
    }
    if newline {
        println!("{line}");
    } else {
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
// Inverse of civil_from_days (Howard Hinnant's days_from_civil). Out-of-range
// months/days normalize like mktime: month 13 rolls into the next year, day 0
// is the last day of the previous month.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y + (m - 1).div_euclid(12);
    let m = (m - 1).rem_euclid(12) + 1;
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + (d - 1);
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

// (year, month, day, yday, weekday-since-Sunday) for an epoch second.
fn civil_fields(epoch_secs: i64) -> (i64, u32, u32, u32, u32) {
    let days = epoch_secs.div_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    let yday = (days - days_from_civil(year, 1, 1) + 1) as u32;
    let wday = (days + 4).rem_euclid(7) as u32; // 1970-01-01 was a Thursday
    (year, month, day, yday, wday)
}

// strftime over the proleptic Gregorian calendar in UTC (Umbra has no
// timezone database; os.date's '!' prefix is accepted but changes nothing).
// Specifiers follow Lua's C99 list, including the E/O modifier characters
// which are no-ops in the C locale. Unknown specifiers are an error, like
// Lua's checkoption.
pub fn format_civil_time(epoch_secs: i64, fmt: &str) -> Result<String, VmError> {
    const WDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let days = epoch_secs.div_euclid(86400);
    let secs_of_day = epoch_secs.rem_euclid(86400);
    let (year, month, day, yday, wday) = civil_fields(epoch_secs);
    let (hour, min, sec) = (secs_of_day / 3600, (secs_of_day / 60) % 60, secs_of_day % 60);
    let hour12 = if hour % 12 == 0 { 12 } else { hour % 12 };
    // Week-of-year: %U counts Sundays, %W counts Mondays (strftime rules).
    let week_u = (yday as i64 + 6 - wday as i64).div_euclid(7);
    let week_w = (yday as i64 + 6 - (wday as i64 + 6) % 7).div_euclid(7);
    // ISO 8601 week date: the ISO year is the year of this week's Thursday.
    let iso_wday = (days + 3).rem_euclid(7) + 1; // Monday = 1
    let thursday = days + (4 - iso_wday);
    let iso_year = civil_from_days(thursday).0;
    let iso_week = (thursday - days_from_civil(iso_year, 1, 1)).div_euclid(7) + 1;
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut spec = chars.next();
        // E/O modifiers select alternative locale representations; the C
        // locale has none, so they reduce to the base specifier.
        if matches!(spec, Some('E') | Some('O')) {
            spec = chars.next();
        }
        match spec {
            Some('Y') => out.push_str(&year.to_string()),
            Some('y') => out.push_str(&format!("{:02}", year.rem_euclid(100))),
            Some('C') => out.push_str(&format!("{:02}", year.div_euclid(100))),
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
            Some('V') => out.push_str(&format!("{iso_week:02}")),
            Some('g') => out.push_str(&format!("{:02}", iso_year.rem_euclid(100))),
            Some('G') => out.push_str(&iso_year.to_string()),
            Some('w') => out.push_str(&wday.to_string()),
            Some('u') => out.push_str(&iso_wday.to_string()),
            Some('c') => out.push_str(&format!(
                "{} {} {:2} {:02}:{:02}:{:02} {}",
                &WDAYS[wday as usize][..3],
                &MONTHS[(month - 1) as usize][..3],
                day,
                hour,
                min,
                sec,
                year
            )),
            Some('x') => out.push_str(&format!("{month:02}/{day:02}/{:02}", year.rem_euclid(100))),
            Some('X') | Some('T') => out.push_str(&format!("{hour:02}:{min:02}:{sec:02}")),
            Some('r') => out.push_str(&format!("{hour12:02}:{min:02}:{sec:02} {}", if hour < 12 { "AM" } else { "PM" })),
            Some('D') => out.push_str(&format!("{month:02}/{day:02}/{:02}", year.rem_euclid(100))),
            Some('F') => out.push_str(&format!("{year}-{month:02}-{day:02}")),
            Some('R') => out.push_str(&format!("{hour:02}:{min:02}")),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('z') => out.push_str("+0000"),
            Some('Z') => out.push_str("GMT"),
            Some('%') => out.push('%'),
            Some(other) => return Err(VmError::RuntimeError(format!("os.date: invalid conversion specifier '%{other}'"))),
            None => return Err(VmError::RuntimeError("os.date: invalid conversion specifier '%'".into())),
        }
    }
    Ok(out)
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
pub struct FxHasher {
    hash: u64,
}

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
        for c in &mut chunks {
            self.add(u64::from_ne_bytes(c.try_into().unwrap()));
        }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            let mut w = [0u8; 8];
            w[..rem.len()].copy_from_slice(rem);
            self.add(u64::from_ne_bytes(w));
        }
    }
    #[inline(always)]
    fn write_u8(&mut self, n: u8) {
        self.add(n as u64);
    }
    #[inline(always)]
    fn write_u32(&mut self, n: u32) {
        self.add(n as u64);
    }
    #[inline(always)]
    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }
    #[inline(always)]
    fn write_i64(&mut self, n: i64) {
        self.add(n as u64);
    }
    #[inline(always)]
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }
    #[inline(always)]
    fn finish(&self) -> u64 {
        self.hash
    }
}

#[derive(Default, Clone, Copy)]
pub struct FxBuild;

impl std::hash::BuildHasher for FxBuild {
    type Hasher = FxHasher;
    #[inline(always)]
    fn build_hasher(&self) -> FxHasher {
        FxHasher::default()
    }
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
    pub(crate) bytes: Box<[u8]>,
}

impl RtString {
    pub fn as_c_ptr(&self) -> *const u8 {
        self.bytes.as_ptr()
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

// Mutable byte buffer: the binary-data counterpart of RtString. Not interned
// (contents change under b[i] = v), no trailing NUL, no cached hash — it is
// never a table key. Indexed 1-based like strings.
#[repr(C)] // gc must stay first: the GC reads the header through the object pointer.
pub struct RtBytes {
    pub gc: crate::gc::GcHeader,
    pub len: usize,
    pub data: Box<[u8]>,
}

fn alloc_bytes_raw(data: Box<[u8]>) -> *mut u8 {
    let rt = RtBytes { gc: crate::gc::GcHeader::new(crate::gc::GcKind::Bytes), len: data.len(), data };
    Box::into_raw(Box::new(rt)) as *mut u8
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
            (TableKey::Int(a), TableKey::Int(b)) => a == b,
            (TableKey::Str(a), TableKey::Str(b)) => a == b || crate::heap::key_str(a) == crate::heap::key_str(b),
            (TableKey::Bool(a), TableKey::Bool(b)) => a == b,
            (TableKey::Ptr(a), TableKey::Ptr(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for TableKey {}

impl std::hash::Hash for TableKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            TableKey::Int(n) => {
                state.write_u8(0);
                state.write_i64(*n);
            }
            TableKey::Str(p) => {
                state.write_u8(1);
                state.write_u64(crate::heap::key_hash(*p));
            }
            TableKey::Bool(b) => {
                state.write_u8(2);
                state.write_u8(*b as u8);
            }
            TableKey::Ptr(p) => {
                state.write_u8(3);
                state.write_u64(*p);
            }
        }
    }
}

impl TableKey {
    fn from_value(v: Value) -> Option<Self> {
        if v.is_nil() {
            return None;
        }
        if v.is_int_like() {
            return Some(TableKey::Int(v.as_int().unwrap()));
        }
        if v.is_float() {
            let f = v.as_float().unwrap();
            if f.fract() == 0.0 && f >= -9223372036854775808.0 && f < 9223372036854775808.0 {
                return Some(TableKey::Int(f as i64));
            }
            return Some(TableKey::Ptr(f.to_bits()));
        }
        if v.is_bool() {
            return Some(TableKey::Bool(v.as_bool().unwrap()));
        }
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
    fn new() -> Self {
        KeyMap { slots: Vec::new(), len: 0, used: 0 }
    }

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
            TableKey::Int(n) => mix(*n as u64),
            TableKey::Str(p) => mix(crate::heap::key_hash(*p)),
            TableKey::Bool(b) => mix(*b as u64),
            TableKey::Ptr(p) => mix(*p),
        }
    }

    #[inline(always)]
    pub fn get(&self, k: &TableKey) -> Option<&Value> {
        if self.slots.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let h = Self::key_hash(k);
        let mut i = (h as usize >> 2) & mask;
        loop {
            // SAFETY: i is masked by len - 1 and len is a power of two.
            let (tag, sk, v) = unsafe { self.slots.get_unchecked(i) };
            if *tag == 0 {
                return None;
            }
            if *tag == h && sk == k {
                return Some(v);
            }
            i = (i + 1) & mask;
        }
    }

    // Slot index where the probe for `k` stops: the key's own slot on a hit,
    // or the first empty slot on a miss. next() uses it to resume iteration
    // without rescanning the whole map.
    fn probe_slot(&self, k: &TableKey) -> Option<usize> {
        if self.slots.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let h = Self::key_hash(k);
        let mut i = (h as usize >> 2) & mask;
        loop {
            // SAFETY: i is masked by len - 1 and len is a power of two.
            let (tag, sk, _) = unsafe { self.slots.get_unchecked(i) };
            if *tag == 0 || (*tag == h && sk == k) {
                return Some(i);
            }
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
                while self.slots[i].0 != 0 {
                    i = (i + 1) & mask;
                }
                self.slots[i] = (tag, k, v);
                self.used += 1;
            }
        }
    }

    pub fn insert(&mut self, k: TableKey, v: Value) {
        if (self.used + 1) * 4 >= self.slots.len().max(1) * 3 {
            self.grow();
        }
        let mask = self.slots.len() - 1;
        let h = Self::key_hash(&k);
        let mut i = (h as usize >> 2) & mask;
        let mut tomb = usize::MAX;
        loop {
            // SAFETY: i is masked by len - 1 and len is a power of two.
            let slot = unsafe { self.slots.get_unchecked_mut(i) };
            if slot.0 == 0 {
                let at = if tomb != usize::MAX { tomb } else { i };
                self.slots[at] = (h, k, v);
                self.len += 1;
                if tomb == usize::MAX {
                    self.used += 1;
                }
                return;
            }
            if slot.0 == h && slot.1 == k {
                slot.2 = v;
                return;
            }
            if slot.0 == 1 && tomb == usize::MAX {
                tomb = i;
            }
            i = (i + 1) & mask;
        }
    }

    pub fn remove(&mut self, k: &TableKey) {
        if self.slots.is_empty() {
            return;
        }
        let mask = self.slots.len() - 1;
        let h = Self::key_hash(k);
        let mut i = (h as usize >> 2) & mask;
        loop {
            // SAFETY: i is masked by len - 1 and len is a power of two.
            let slot = unsafe { self.slots.get_unchecked_mut(i) };
            if slot.0 == 0 {
                return;
            }
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
            return self.hash.get(&TableKey::Str(key.as_string().unwrap())).copied().unwrap_or(Value::nil());
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
            TableKey::Str(p) if crate::heap::key_str(p) == name => Some(*v),
            _ => None,
        })
    }

    // rawset on a table outside the heap (the Vm's globals), with the same
    // write barrier as Gc::raw_set, which heap tables go through. The
    // never-registered globals table is black, so it reaches the
    // out-of-line barrier whenever a cycle is running.
    #[inline(always)]
    pub(crate) fn raw_set(&mut self, gc: &mut Gc, key: Value, val: Value) {
        if self.gc.color == GcColor::Black && gc.phase() != crate::gc::GcPhase::Pause {
            gc.barrier_store(key, val);
        }
        self.set_no_barrier(key, val);
    }

    // The store itself. Callers run the write barrier first: Gc::raw_set or
    // Table::raw_set.
    #[inline(always)]
    pub(crate) fn set_no_barrier(&mut self, key: Value, val: Value) {
        if key.is_int() {
            let i = key.as_int().unwrap();
            if i >= 1 && i <= (self.array.len() + 1) as i64 {
                // The key may already live in the hash part (inserted before
                // the array grew this far); drop it so the key exists in
                // exactly one place and next() can't yield it twice.
                if self.hash.len > 0 {
                    self.hash.remove(&TableKey::Int(i));
                }
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
            if val.is_nil() {
                self.hash.remove(&k);
            } else {
                self.hash.insert(k, val);
            }
        }
    }

    pub fn length(&self) -> i64 {
        if self.array.is_empty() {
            return 0;
        }
        let mut lo = 0usize;
        let mut hi = self.array.len();
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if self.array[mid - 1].is_nil() {
                hi = mid - 1;
            } else {
                lo = mid;
            }
        }
        lo as i64
    }
}

fn alloc_table_raw() -> *mut u8 {
    Box::into_raw(Box::new(Table::new())) as *mut u8
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoStatus {
    Suspended,
    Running,
    Dead,
}

// A coroutine as a heap object, collected once nothing reaches it. While it
// is suspended, `regs`/`frames` hold its own stack; while it runs they hold
// the stack of whoever resumed it (resume swaps them with the Vm's), and the
// Vm's resume chain keeps it rooted. The collector traces either.
#[repr(C)] // gc must stay first: the GC reads the header through the object pointer.
pub struct Coroutine {
    pub gc: crate::gc::GcHeader,
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

// A native function as a heap object (Vm::make_cfn_closure). The collector
// traces `upvals`; they never change after creation, so a running native
// can read them while script it calls runs.
#[repr(C)] // gc must stay first: the GC reads the header through the object pointer.
pub struct NativeFn {
    pub gc: crate::gc::GcHeader,
    pub(crate) upvals: Box<[Value]>,
    pub(crate) f: NativeBody,
}

#[repr(C)] // gc must stay first: the GC reads the header through the object pointer.
pub struct LuaClosure {
    pub gc: crate::gc::GcHeader,
    pub proto: *const GcProto,
    pub upvals: Vec<Value>,
}

pub struct Frame {
    // Rooted while the frame lives: Vm::gc_roots and Gc::mark_coroutine
    // mark it. A tail call or an exec'd chunk may leave nothing else
    // reaching it.
    pub proto: *const GcProto,
    pub pc: usize,
    pub base: usize,
    pub expected_results: u8,
    pub upvals_ptr: *mut Value,
    pub upvals_len: usize,
    pub varargs: Box<[Value]>,
    pub top: usize,
    // The callee value, rooted for the GC. Normal calls leave it nil — the
    // caller's register window already roots the function — but a tail call
    // overwrites that window with the new arguments, so the reused frame
    // must hold the closure itself or its upvalues could be collected.
    pub callable: Value,
    // Set when a tail call reused this frame: the traceback prints
    // "(...tail calls...)" for the frames it replaced.
    pub tail: bool,
}

impl Frame {
    // The running function's proto, borrowed for as long as the frame.
    #[inline(always)]
    pub(crate) fn proto(&self) -> &Proto {
        // SAFETY: push_frame and TailCall set `proto` from a function Value
        // or from exec, and the frame roots it.
        unsafe { crate::heap::proto(self.proto) }
    }

    // Upvalue i of the running closure; nil past the end.
    #[inline(always)]
    pub(crate) fn upval(&self, i: usize) -> Value {
        if i >= self.upvals_len {
            return Value::nil();
        }
        // SAFETY: upvals_ptr/len describe the running closure's upvalue Vec
        // (Gc::callable), which never resizes. The closure stays rooted while
        // its frame runs: by the caller's register, Frame::callable after a
        // tail call, the host stack for isolated calls, a coroutine's fn_val.
        unsafe { *self.upvals_ptr.add(i) }
    }

    #[inline(always)]
    pub(crate) fn set_upval(&mut self, i: usize, v: Value) {
        if i >= self.upvals_len {
            return;
        }
        // SAFETY: as upval.
        unsafe {
            *self.upvals_ptr.add(i) = v;
        }
    }
}

pub struct Vm {
    regs: Vec<Value>,
    frames: Vec<Frame>,
    pub(crate) globals: Table,
    // Bumped on every globals write; GetGlobal callsite caches key on it.
    globals_gen: u64,
    string_cache: FxMap<String, Value>,
    pub top_level_results: Vec<Value>,
    pub(crate) gc: Gc,
    pub string_lib: Value,
    pub bytes_lib: Value,
    // Values the embedding host holds on its API stack; rooted like registers.
    pub host_stack: Vec<Value>,
    // Where the running C function's frame starts in host_stack (Lua's
    // frame base). Lives here rather than in UmbraState so a C callback can
    // be handed the state through the native's own &mut Vm.
    pub(crate) api_base: usize,
    // Values a Ctx has handed out; each Ctx truncates back to its starting
    // length when it drops (see embed.rs for the rooting rules).
    pub(crate) ctx_roots: Vec<Value>,
    // Registry slots behind embed::Ref handles, rooted until released, and
    // the indices of released slots ready for reuse.
    pub(crate) refs: Vec<Value>,
    pub(crate) free_refs: Vec<u32>,
    // Process-unique and never 0, so a Ref can tell whether it belongs to
    // this VM and a proto's caches know whose heap they point into. The
    // address can't serve: a Vm moves, and a new one can reuse a dropped
    // one's address.
    pub(crate) id: u64,
    // Host objects lent by Ctx::with_host, innermost last. Each entry lives
    // exactly as long as the with_host call that pushed it.
    pub(crate) hosts: Vec<(std::any::TypeId, *mut ())>,
    // Host-provided source for `require`; None reads the filesystem.
    pub(crate) module_loader: Option<ModuleLoader>,
    print_hook: Option<PrintHook>,
    // Set after a caught panic; further execution is refused rather than risk
    // UB from continuing on possibly-inconsistent GC/register/frame state.
    pub poisoned: bool,
    // Host-set instruction budget for bounding a runaway script; 0 = unlimited.
    // Not script-settable — only the embedder (via the C API) controls this.
    pub step_limit: u64,
    pub step_count: u64,
    pub loaded_modules: FxMap<String, Value>,
    // The chunks of the modules in loaded_modules, rooted with them: one
    // per module, so they don't add up, and a module's code stays loaded
    // for as long as its value is cached.
    module_chunks: Vec<Value>,
    // Frame stacks parked by run_isolated while a nested isolated call
    // (metamethod, pcall target, __gc) runs; their varargs must stay
    // reachable to the collector.
    saved_frames: Vec<Vec<Frame>>,
    // Registers marked to-be-closed by Op::Tbc, innermost last: (absolute
    // register index, boxed flag). Op::TbcPop drops the newest mark on a
    // normal scope exit; unwind_closes drains the rest when an error
    // discards frames. Kept off Frame so the hot call path stays small.
    tbc: Vec<(usize, bool)>,
    // The coroutines being resumed right now, outermost first; the last one
    // is running. A root: each holds the parked stack of the one before it
    // (the main thread's, for the first), and nothing else need reach it.
    resume_chain: Vec<Value>,
    // Isolated calls nested inside one another right now; see
    // MAX_NESTING.
    pub(crate) nesting: u32,
    // Tracebacks of the errors in flight, one per nesting level at most,
    // innermost last. See record_traceback for their lifecycle.
    pub(crate) err_traces: Vec<ErrTrace>,
    // Serial number of the newest ErrTrace, so Ctx::guard can tell one its
    // own call recorded from one that was already there.
    pub(crate) err_serial: u64,
    // Errors whose xpcall handlers are running, innermost last: the handler
    // runs after the stack unwound, so debug.traceback inside it reports
    // the stack the error was raised on.
    handlers: Vec<Handling>,
    // saved_frames index where the running thread's stack starts: 0 for the
    // main thread; for a coroutine, the parked-stack depth when it was
    // resumed. Segments below it belong to whoever resumed it.
    thread_floor: usize,
    // Modules whose require is still running, innermost last: Lua 5.1's
    // loading sentinel, kept out of loaded_modules so nothing mistakes it
    // for a module's value.
    loading_modules: Vec<String>,
    // math.random's splitmix64 state. Per VM, so VMs sharing a thread
    // don't share a sequence or each other's randomseed.
    rng: u64,
}

// The traceback of an error in flight (Vm::err_traces).
pub(crate) struct ErrTrace {
    // The nesting level the error has reached: the level of the segment
    // that first enriched it, then one less each time it propagates out of
    // an isolated call.
    level: u32,
    // resume_chain depth of the thread whose stack the traceback ends with.
    thread: usize,
    serial: u64,
    traceback: String,
}

// The error an xpcall handler is handling (Vm::handlers).
struct Handling {
    // Segment index (as in Vm::stack_traceback) of the xpcall's caller; the
    // handler's own segments come after it.
    caller_segment: usize,
    thread: usize,
    traceback: String,
}

// Every isolated re-entry (a metamethod, pcall/xpcall, a native calling
// script, require, a table.sort comparator, __gc, a host Ctx call or
// load_chunk, a coroutine resume) runs on the Rust stack below the one that
// made it, and run_inner's frame alone is large. The per-segment frame cap
// can't see across them, so a script recursing through any of them used
// to overflow the Rust stack and abort the process. Like Lua's nCcalls, one
// counter bounds them all; past the limit the call fails with a catchable
// "stack overflow". Lua allows 200; a level here costs more. Measured in
// release: under 3 KB per level, and up to 350 KB for compiling a source
// at the parser's own nesting limit. 150 levels plus that compile come to
// about 800 KB, inside the 1 MB main-thread stack Windows gives. A debug
// build spends about 88 KB per level, so there the limit only holds on a
// thread with a stack of 16 MB or more.
pub(crate) const MAX_NESTING: u32 = 150;

// How a value got its name, for `attempt to ... (kind 'name')` suffixes and
// traceback "in <kind> 'name'" entries.
fn chunk_name(proto: &Proto) -> String {
    match &proto.source {
        Some(name) => name.clone(),
        None => DEFAULT_CHUNK_NAME.to_owned(),
    }
}

// What errors call a chunk compiled without a name.
const DEFAULT_CHUNK_NAME: &str = "[string \"...\"]";

// The last instruction before `pc` that writes `reg` — Lua's findsetreg.
// A write sitting between a forward jump and its target is conditional, so
// it doesn't count; backward jumps only bound the scan.
fn find_set_reg(proto: &Proto, pc: usize, reg: usize) -> Option<usize> {
    let mut set = None;
    let mut jmptarget = 0usize;
    for i in 0..pc.min(proto.code.len()) {
        let instr = proto.code[i];
        let op = Op::from_u8(iop(instr))?;
        let a = ia(instr);
        let writes = match op {
            Op::SelfOp => reg == a || reg == a + 1,
            Op::Call | Op::TailCall => reg >= a,
            Op::Vararg => reg >= a,
            Op::TForCall => reg >= a + 4,
            Op::ForLoop => reg == a || reg == a + 3,
            Op::TForLoop => reg == a + 2,
            Op::SetTable
            | Op::SetField
            | Op::SetList
            | Op::SetGlobal
            | Op::SetUpval
            | Op::Jmp
            | Op::Test
            | Op::Eq
            | Op::Lt
            | Op::Le
            | Op::LtI
            | Op::LeI
            | Op::GtI
            | Op::GeI
            | Op::EqI
            | Op::ForPrep
            | Op::Tbc
            | Op::TbcPop
            | Op::Return => false,
            _ => reg == a,
        };
        if writes {
            set = Some(i);
        }
        if matches!(op, Op::Jmp | Op::ForPrep) {
            let target = (i as i32 + 1 + isbx(instr)) as usize;
            if target <= pc {
                jmptarget = target;
            }
        }
        if i < jmptarget {
            set = None;
        }
    }
    set
}

// Names the value in `reg` at `pc` — Lua's getobjname: a live local wins,
// otherwise the instruction that last wrote the register says where the
// value came from (global, field, method, upvalue, constant, or another
// local via Move). GetTable with a non-string key names the table operand
// instead, which also unboxes captured locals (key 1 into the cell).
fn reg_name(proto: &Proto, pc: usize, reg: usize) -> Option<(&'static str, String)> {
    for lv in &proto.locvars {
        if lv.reg as usize == reg && (lv.start_pc as usize) <= pc && pc < lv.end_pc as usize {
            return Some(("local", lv.name.clone()));
        }
    }
    let set = find_set_reg(proto, pc, reg)?;
    let instr = proto.code[set];
    let const_str = |i: usize| -> Option<&str> {
        match proto.consts.get(i) {
            Some(Const::Str(s)) => Some(s.s.as_str()),
            _ => None,
        }
    };
    match Op::from_u8(iop(instr))? {
        Op::Move => reg_name(proto, set, ib(instr)),
        Op::GetGlobal => const_str(ibx(instr)).map(|s| ("global", s.to_owned())),
        Op::GetField => const_str(ic(instr)).map(|s| ("field", s.to_owned())),
        Op::SelfOp => const_str(ic(instr)).map(|s| ("method", s.to_owned())),
        Op::GetUpval => proto.upvals.get(ib(instr)).map(|u| ("upvalue", u.name.clone())),
        Op::LoadK => Some(("constant", const_str(ibx(instr)).unwrap_or("?").to_owned())),
        Op::GetTable => {
            let k = ic(instr);
            if is_rk(k) {
                if let Some(s) = const_str(rk_idx(k)) {
                    return Some(("field", s.to_owned()));
                }
            }
            reg_name(proto, set, ib(instr))
        }
        _ => None,
    }
}

// ` (kind 'name')` suffix for the value in `reg` at `pc`, or "" when the
// register's provenance can't be named.
fn varinfo(proto: &Proto, pc: usize, reg: usize) -> String {
    match reg_name(proto, pc, reg) {
        Some((kind, name)) => format!(" ({kind} '{name}')"),
        None => String::new(),
    }
}
// Same for an RK operand: registers resolve through reg_name, constants
// report "(constant 'name')" for strings and "(constant '?')" otherwise.
// usize::MAX marks a non-register operand (an immediate literal) that
// shouldn't be named at all.
fn rk_varinfo(proto: &Proto, pc: usize, rk: usize) -> String {
    if rk == usize::MAX {
        return String::new();
    }
    if is_rk(rk) {
        let name = match proto.consts.get(rk_idx(rk)) {
            Some(Const::Str(s)) => s.s.clone(),
            _ => "?".to_owned(),
        };
        format!(" (constant '{name}')")
    } else {
        varinfo(proto, pc, rk)
    }
}

// Names the callee of the call instruction at `pc` in `proto` — used to
// label the callee's frame in tracebacks ("in function 'f'" style).
fn call_site_name(proto: &Proto, pc: usize) -> Option<(&'static str, String)> {
    let instr = *proto.code.get(pc)?;
    match Op::from_u8(iop(instr))? {
        Op::Call | Op::TailCall => reg_name(proto, pc, ia(instr)),
        Op::TForCall => Some(("for iterator", "for iterator".to_owned())),
        _ => None,
    }
}

// True when the message already carries a `source:line:` prefix — either
// from an earlier enrichment or error(msg, 2) — so it isn't prefixed twice.
pub(crate) fn has_position_prefix(msg: &str) -> bool {
    let Some(colon) = msg.find(':') else { return false };
    let rest = &msg[colon + 1..];
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    digits > 0 && rest[digits..].starts_with(':')
}

// Rebuilds a bare "attempt to compare" error with operand names appended.
// Other errors (metamethod failures, already-positioned messages) pass
// through untouched.
fn name_order_err(e: VmError, proto: &Proto, pc: usize, a: Value, b: Value, ra: usize, rb: usize) -> VmError {
    match &e {
        VmError::RuntimeError(m) if m.starts_with("attempt to compare") => VmError::RuntimeError(format!(
            "attempt to compare {}{} with {}{}",
            a.type_name(),
            rk_varinfo(proto, pc, ra),
            b.type_name(),
            rk_varinfo(proto, pc, rb)
        )),
        _ => e,
    }
}

fn build_traceback(frames: &[Frame]) -> String {
    let mut out = Vec::with_capacity(frames.len());
    for (i, f) in frames.iter().enumerate().rev() {
        let proto = f.proto();
        let line = proto.lines.get(f.pc.saturating_sub(1)).copied().unwrap_or(0);
        let src = chunk_name(proto);
        if proto.is_main {
            out.push(format!("\t{src}:{line}: in main chunk"));
            continue;
        }
        // The call site names the function, as in Lua. A segment's first
        // frame has none in the segment (the host, pcall or a metamethod
        // called it), so the name it was declared under stands in.
        let named = if i == 0 {
            None
        } else {
            let caller = &frames[i - 1];
            call_site_name(caller.proto(), caller.pc.saturating_sub(1))
        };
        match named.as_ref().or(proto.decl.as_ref()) {
            Some(("global", name)) => out.push(format!("\t{src}:{line}: in function '{name}'")),
            Some((kind, name)) => out.push(format!("\t{src}:{line}: in {kind} '{name}'")),
            None => out.push(format!("\t{src}:{line}: in function <{src}:{}>", proto.line_defined)),
        }
        if f.tail {
            out.push("\t(...tail calls...)".to_owned());
        }
    }
    out.join("\n")
}

pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        return (*s).to_owned();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "unknown panic".to_owned()
}

impl Drop for Vm {
    fn drop(&mut self) {
        self.gc.free_all();
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
    /// A VM whose print and io.write output goes to `hook` instead of
    /// stdout. The hook gets the text and whether a line break follows it:
    /// true for print, false for io.write, which writes no newline.
    pub fn new_with_print(hook: impl Fn(&str, bool) + 'static) -> Self {
        Self::new_inner(Some(Box::new(hook)))
    }

    fn new_inner(print_hook: Option<PrintHook>) -> Self {
        let mut vm = Vm {
            regs: vec![Value::nil(); 256],
            frames: Vec::with_capacity(200),
            globals: Table::new(),
            globals_gen: 0,
            string_cache: FxMap::default(),
            top_level_results: Vec::new(),
            gc: Gc::new(),
            string_lib: Value::nil(),
            bytes_lib: Value::nil(),
            host_stack: Vec::with_capacity(32),
            api_base: 0,
            ctx_roots: Vec::new(),
            refs: Vec::new(),
            free_refs: Vec::new(),
            id: crate::embed::next_vm_id(),
            hosts: Vec::new(),
            module_loader: None,
            print_hook,
            poisoned: false,
            err_traces: Vec::new(),
            err_serial: 0,
            handlers: Vec::new(),
            thread_floor: 0,
            step_limit: 0,
            step_count: 0,
            loaded_modules: FxMap::default(),
            module_chunks: Vec::new(),
            loading_modules: Vec::new(),
            saved_frames: Vec::new(),
            tbc: Vec::new(),
            resume_chain: Vec::new(),
            nesting: 0,
            rng: 6364136223846793005,
        };
        vm.register_stdlib();
        vm
    }

    pub(crate) fn intern(&mut self, s: &str) -> Value {
        if let Some(&v) = self.string_cache.get(s) {
            return v;
        }
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

    // GC-registered but not interned/deduplicated by content like intern —
    // computed/throwaway strings shouldn't pay a cache lookup+clone every call.
    pub(crate) fn alloc_str(&mut self, s: &str) -> Value {
        let raw = alloc_string_raw(s);
        self.gc.register(raw);
        Value::string(raw)
    }

    pub(crate) fn alloc_bytes(&mut self, data: impl Into<Box<[u8]>>) -> Value {
        let raw = alloc_bytes_raw(data.into());
        self.gc.register(raw);
        Value::bytes(raw)
    }

    fn alloc_bytes_zeroed(&mut self, len: usize) -> Value {
        let raw = alloc_bytes_raw(vec![0u8; len].into_boxed_slice());
        self.gc.register(raw);
        Value::bytes(raw)
    }

    pub(crate) fn alloc_table(&mut self) -> Value {
        let raw = alloc_table_raw();
        self.gc.register(raw);
        Value::table(raw)
    }

    // The entry after `key` in `tbl` (which must be a table), or None at
    // the end. Resumes by position instead of rescanning: an int key inside
    // the array part continues at that index; anything else probes the hash
    // for its slot and continues after it. A deleted or never-present key
    // resumes where it would have sat, which is strictly more useful than
    // Lua's "invalid key" error.
    pub(crate) fn table_next(&mut self, tbl: Value, key: Value) -> Option<(Value, Value)> {
        let t = self.gc.table(tbl);
        let mut ai = 0usize;
        let mut hi = 0usize;
        if !key.is_nil() {
            // Any key not resumed inside the array part comes from the hash
            // part, so the array part is already done.
            ai = t.array.len();
            if key.is_int_like() {
                let i = key.as_int().unwrap();
                if i >= 1 && (i as usize) <= t.array.len() && !t.array[(i - 1) as usize].is_nil() {
                    ai = i as usize;
                } else {
                    hi = t.hash.probe_slot(&TableKey::Int(i)).map(|s| s + 1).unwrap_or(0);
                }
            } else if let Some(tk) = TableKey::from_value(key) {
                hi = t.hash.probe_slot(&tk).map(|s| s + 1).unwrap_or(0);
            }
        }
        for i in ai..t.array.len() {
            let v = t.array[i];
            if !v.is_nil() {
                return Some((Value::int((i + 1) as i64), v));
            }
        }
        let (tk, v) = t.hash.slots.get(hi..)?.iter().find(|(tag, _, _)| *tag > 1).map(|(_, tk, v)| (tk.clone(), *v))?;
        let k = match tk {
            TableKey::Int(n) => self.make_int(n),
            TableKey::Bool(b) => Value::bool(b),
            TableKey::Str(p) => Value::string(p),
            TableKey::Ptr(p) => Value::from_raw(p),
        };
        Some((k, v))
    }

    // setmetatable's effect without its argument checks: `tbl` must be a
    // table and `mt` a table or nil.
    pub(crate) fn set_metatable(&mut self, tbl: Value, mt: Value) {
        self.gc.table_mut(tbl).metatable = mt.as_table().map(|p| p as *mut Table);
        // Write barrier: a black table gaining a white metatable mid-cycle
        // would leave it for the sweep.
        self.gc.barrier_val(mt);
        // __gc is noticed when the metatable is attached (like Lua): record
        // the table as a finalizer candidate for every cycle until it's
        // freed.
        if mt.is_table() && self.gc.table(mt).get_str("__gc").is_some() {
            self.gc.add_fin_candidate(tbl.as_table().unwrap());
        }
    }

    // Compiles a chunk into a callable value, collected like any other
    // function once nothing reaches it: root or store it before anything
    // can collect. The error is the bare parse/compile message.
    pub(crate) fn load_proto(&mut self, src: &str, chunk_name: Option<String>) -> Result<Value, String> {
        // The parser and compiler recurse once per nesting level of the
        // source, so a compile is one more level on the Rust stack.
        let chunk = self
            .nested(|_| {
                let (block, parse_errs) = crate::parse(src);
                if let Some(e) = parse_errs.first() {
                    return Err(VmError::RuntimeError(e.to_string()));
                }
                crate::compile(block, chunk_name).map_err(|e| VmError::RuntimeError(e.to_string()))
            })
            .map_err(|e| e.to_string())?;
        Ok(Value::userdata(self.own_proto(chunk) as *mut u8))
    }

    // Runs `f` one isolated-call level deeper (MAX_NESTING), or fails with
    // "stack overflow" at the limit. A panic unwinding out of `f` skips the
    // decrement; the catch site puts the count back (Vm::poison).
    #[inline]
    pub(crate) fn nested<T>(&mut self, f: impl FnOnce(&mut Vm) -> VmResult<T>) -> VmResult<T> {
        if self.nesting >= MAX_NESTING {
            return Err(VmError::RuntimeError("stack overflow".into()));
        }
        // A call from the host starts with no error in flight: what an
        // earlier one recorded was for the host to read (Ctx::guard).
        if self.nesting == 0 {
            self.err_traces.clear();
        }
        self.nesting += 1;
        let result = f(self);
        self.nesting -= 1;
        if result.is_err() {
            self.deliver_error();
        }
        result
    }

    // An error propagated out of an isolated call: its traceback moves out a
    // level with it, superseding the one an earlier error left there.
    #[cold]
    fn deliver_error(&mut self) {
        let n = self.err_traces.len();
        if n == 0 || self.err_traces[n - 1].level != self.nesting + 1 {
            return;
        }
        self.err_traces[n - 1].level = self.nesting;
        if n >= 2 && self.err_traces[n - 2].level == self.nesting {
            self.err_traces.remove(n - 2);
        }
    }

    // After a caught panic: refuse further work, and restore the nesting
    // count the catching frame entered with, since the unwind skipped the
    // decrements between there and the panic.
    pub(crate) fn poison(&mut self, nesting: u32) {
        self.poisoned = true;
        self.nesting = nesting;
    }

    // Hands a compiled tree to the collector: every proto in it becomes a
    // heap object. Returns the root, which nothing reaches yet; the caller
    // roots it (a frame, a register, a Ctx) before the next checkpoint.
    fn own_proto(&mut self, chunk: crate::compiler::Compiled) -> *const GcProto {
        let root = chunk.into_raw();
        let mut pending = vec![root];
        while let Some(p) = pending.pop() {
            self.gc.register(p as *mut u8);
            // SAFETY: `p` is in the tree into_raw handed over, so live and
            // ours; nothing else refers to it yet.
            pending.extend_from_slice(unsafe { &(*p).proto.protos });
        }
        root
    }

    // resume() swaps a coroutine's regs into self.regs for its run, so the
    // live state at any moment is split between self.regs and the stacks
    // parked in coroutine objects: the resume chain's (rooted below) and
    // every suspended one's (reached like any other object).
    fn gc_roots(&self) -> Vec<Value> {
        // Registers are rooted up to the deepest frame's extent; frame.top can
        // reach past base + max_regs after a multi-value call or `...` spill,
        // and those results are still live until the next instruction consumes
        // them.
        let reg_top = self.frames.iter().map(|f| (f.base + f.proto().max_regs as usize).max(f.top)).max().unwrap_or(0).min(self.regs.len());
        let mut roots: Vec<Value> = self.regs[..reg_top].to_vec();
        // Varargs are copied out of the register window into the frame, so
        // they need their own scan — nothing else references them. The
        // callable is rooted for tail calls, which overwrite the caller's
        // register window (and with it the only other reference). The proto
        // is rooted for the same reason, and because an exec'd chunk is
        // reachable from nothing but its frame.
        for f in self.frames.iter().chain(self.saved_frames.iter().flatten()) {
            roots.extend(f.varargs.iter().copied());
            roots.push(f.callable);
            roots.push(Value::userdata(f.proto as *mut u8));
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
        roots.extend(self.ctx_roots.iter().copied());
        roots.extend(self.refs.iter().copied());
        roots.extend(self.loaded_modules.values().copied());
        roots.extend(self.module_chunks.iter().copied());
        roots.push(self.string_lib);
        roots.push(self.bytes_lib);
        roots.extend(self.resume_chain.iter().copied());
        for &(ptr, gc_fn) in self.gc.pending_finalizers.iter().chain(&self.gc.running_finalizers) {
            roots.push(Value::table(ptr));
            roots.push(gc_fn);
        }
        roots
    }
    // One incremental step, called from 'outer's top when a checkpoint fires.
    // Roots are gathered only for the phases that need them (cycle start and
    // the atomic step) so ordinary propagate/sweep steps stay cheap.
    pub(crate) fn gc_step(&mut self) {
        let roots = if self.gc.needs_roots() { Some(self.gc_roots()) } else { None };
        self.gc.step(roots, &mut self.string_cache, false);
    }

    // collectgarbage("step"): one step's worth of work, as though a granule
    // had been allocated, starting a cycle if none is running. True if the
    // step finished one.
    fn gc_step_once(&mut self) -> bool {
        let running = self.gc.phase() != crate::gc::GcPhase::Pause;
        self.gc.owe_step();
        self.gc_step();
        running && self.gc.phase() == crate::gc::GcPhase::Pause
    }

    // Run the state machine to Pause with an unbounded budget — a
    // stop-the-world collection on demand. Steps taken here are forced, so
    // they don't count toward the max-pause accounting.
    fn gc_full_cycle(&mut self) {
        loop {
            let roots = if self.gc.needs_roots() { Some(self.gc_roots()) } else { None };
            self.gc.step(roots, &mut self.string_cache, true);
            if self.gc.phase() == crate::gc::GcPhase::Pause {
                break;
            }
        }
    }

    // Runs the __gc callbacks the last completed cycle queued. A collection
    // triggered from inside a finalizer (or any nested collect while
    // finalizers are draining) must not drain the queue itself: that would
    // recurse on the Rust stack without bound. The outermost caller drains
    // iteratively instead. The interpreter checks at the top of every outer
    // dispatch round, so the empty case stays inline and the drain doesn't.
    #[inline(always)]
    fn run_pending_finalizers(&mut self) {
        if self.gc.pending_finalizers.is_empty() || !self.gc.running_finalizers.is_empty() {
            return;
        }
        self.drain_finalizers();
    }

    #[inline(never)]
    fn drain_finalizers(&mut self) {
        let batch = std::mem::take(&mut self.gc.pending_finalizers);
        // Kept in running_finalizers (a root) while they run: a nested
        // collection from inside one finalizer must not sweep the others.
        self.gc.running_finalizers.extend(batch);
        // A finalizer's error goes nowhere. Its traceback mustn't stay
        // behind, where a later error at this level would take it for its
        // own; and the tracebacks of errors in flight must survive.
        let traces = std::mem::take(&mut self.err_traces);
        let mut i = 0;
        while i < self.gc.running_finalizers.len() {
            let (ptr, gc_fn) = self.gc.running_finalizers[i];
            let _ = self.call_value_isolated(gc_fn, &[Value::table(ptr)]);
            i += 1;
        }
        self.err_traces = traces;
        self.gc.running_finalizers.clear();
    }

    // Full collection: finish any in-flight cycle, run a fresh one to
    // completion, drain the finalizers it queued, then collect once more so
    // objects the finalizers released are reclaimed too.
    pub fn gc_collect(&mut self) {
        self.gc_full_cycle();
        self.run_pending_finalizers();
        self.gc_full_cycle();
    }

    // Runs close() on every to-be-closed register of the frames an error is
    // about to discard: innermost frame first, innermost local first, each
    // call receiving the pending error as its second argument (Lua 5.4's
    // __close(value, err)). A close that fails replaces the pending error and
    // unwinding continues with the new one. Marks below `floor` belong to
    // frames that survive the unwind (parked by run_isolated) and are left
    // alone. Call while self.frames/self.regs still hold the dying stack.
    fn unwind_closes(&mut self, err: VmError, floor: usize) -> VmError {
        if self.poisoned {
            self.tbc.truncate(floor);
            return err;
        }
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
            if self.poisoned {
                break;
            }
            let mut v = self.regs[reg];
            if boxed && v.is_table() {
                v = self.gc.table(v).raw_get(Value::int(1));
            }
            let close = if v.is_table() {
                let t = self.gc.table(v);
                let raw = t.raw_get(close_key);
                if !raw.is_nil() || t.metatable.is_none() {
                    raw
                } else {
                    match self.table_index_chain(v, close_key) {
                        Ok(r) => r,
                        Err(e) => {
                            replace_err!(e);
                            continue;
                        }
                    }
                }
            } else if v.is_string() {
                if self.string_lib.is_table() { self.gc.table(self.string_lib).raw_get(close_key) } else { Value::nil() }
            } else {
                replace_err!(VmError::RuntimeError(format!("attempt to index a {} value", v.type_name())));
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
        self.frames.iter().map(|f| f.base + f.proto().max_regs as usize).max().map(|top| top + 8).unwrap_or(0)
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
        let result = self.nested(|vm| vm.call_value_isolated_inner(fn_val, args));
        self.host_stack.pop();
        result
    }

    fn call_value_isolated_inner(&mut self, fn_val: Value, args: &[Value]) -> VmResult<Vec<Value>> {
        if crate::heap::is_native(fn_val) {
            let nesting = self.nesting;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.call_native(fn_val, args)));
            return match result {
                Ok(r) => r,
                Err(payload) => {
                    self.poison(nesting);
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
            // Through the rooting entry point: the handler may drop the
            // metatable, and then nothing else holds `mm` while it runs.
            return self.call_value_isolated(mm, &mm_args);
        }
        if let Some(cp) = self.gc.callable(fn_val) {
            self.run_isolated(cp.proto, cp.upvals_ptr, cp.upvals_len, args)
        } else {
            Err(VmError::RuntimeError(format!("attempt to call a {} value", fn_val.type_name())))
        }
    }

    // The scratch base must be computed before frames are saved, since saving
    // swaps `self.frames` out to empty. A yield can't cross this boundary: the
    // isolated frames are discarded on return, so there'd be nothing to resume.
    fn run_isolated(&mut self, proto: *const GcProto, upvals_ptr: *mut Value, upvals_len: usize, args: &[Value]) -> VmResult<Vec<Value>> {
        let base = self.scratch_base();
        // Marks below the floor belong to the parked frames; only the
        // isolated run's own <close> locals unwind here.
        let tbc_floor = self.tbc.len();
        self.saved_frames.push(std::mem::take(&mut self.frames));
        // SAFETY: `proto` came from a function Value or from exec.
        let needed = base + unsafe { crate::heap::proto(proto) }.max_regs as usize + 8;
        if needed > self.regs.len() {
            self.regs.resize(needed + 64, Value::nil());
        }
        for (i, &v) in args.iter().enumerate() {
            self.regs[base + i] = v;
        }
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
                let e = if matches!(e, VmError::Yield(_)) && !self.resume_chain.is_empty() {
                    VmError::RuntimeError("attempt to yield across a C-call boundary".into())
                } else {
                    e
                };
                Err(self.unwind_closes(e, tbc_floor))
            }
        };
        self.frames = self.saved_frames.pop().unwrap();
        run_result.map(|_| results)
    }

    fn new_coroutine(&mut self, fn_val: Value, who: &str) -> VmResult<Value> {
        if self.gc.closure_proto(fn_val).is_none() && !crate::heap::is_native(fn_val) {
            return Err(VmError::RuntimeError(format!("{who}: expected function")));
        }
        let co = Box::new(Coroutine {
            gc: crate::gc::GcHeader::new(crate::gc::GcKind::Coroutine),
            regs: Vec::new(),
            frames: Vec::with_capacity(8),
            status: CoStatus::Suspended,
            fn_val,
            started: false,
            yield_result_base: 0,
            yield_nresults: 0,
            tbc: Vec::new(),
        });
        let ptr = Box::into_raw(co) as *mut u8;
        self.gc.register(ptr);
        Ok(Value::coroutine(ptr))
    }

    // Swaps the coroutine's regs/frames in, runs it until it yields, returns
    // or fails, and swaps them back out. Both a yield and a return report Ok.
    fn resume_coroutine(&mut self, co_val: Value, args: &[Value]) -> VmResult<Vec<Value>> {
        if self.poisoned {
            return Err(VmError::RuntimeError("VM is poisoned by a previous internal error".into()));
        }
        self.nested(|vm| vm.resume_coroutine_inner(co_val, args))
    }

    fn resume_coroutine_inner(&mut self, co_val: Value, args: &[Value]) -> VmResult<Vec<Value>> {
        let nesting = self.nesting;
        let co = self.gc.coroutine_mut(co_val);
        match co.status {
            CoStatus::Dead => return Err(VmError::RuntimeError("cannot resume dead coroutine".into())),
            CoStatus::Running => return Err(VmError::RuntimeError("cannot resume non-suspended coroutine".into())),
            CoStatus::Suspended => {}
        }
        co.status = CoStatus::Running;
        std::mem::swap(&mut self.regs, &mut co.regs);
        std::mem::swap(&mut self.frames, &mut co.frames);
        std::mem::swap(&mut self.tbc, &mut co.tbc);
        let started = std::mem::replace(&mut co.started, true);
        let fn_val = co.fn_val;
        let yield_at = std::mem::take(&mut co.yield_result_base);
        let yield_n = co.yield_nresults;
        // The resumer's stack is parked in the coroutine now, and the
        // coroutine may be reachable from nothing else (a wrapper that was
        // called and dropped, a value only on the stack it just swapped out).
        self.resume_chain.push(co_val);
        // Its tracebacks stop where its own stack starts, like a Lua thread's.
        let floor = std::mem::replace(&mut self.thread_floor, self.saved_frames.len());

        // A host function can't yield (there's no frame to suspend), so it
        // runs to completion inside this one resume; its results become the
        // coroutine's return values and the coroutine ends dead.
        let setup: VmResult<()> = if !started {
            if crate::heap::is_native(fn_val) {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.call_native(fn_val, args))) {
                    Ok(Ok(vals)) => {
                        self.top_level_results = vals;
                        Ok(())
                    }
                    Ok(Err(e)) => Err(e),
                    Err(payload) => {
                        self.poison(nesting);
                        Err(VmError::RuntimeError(format!("internal error (panic): {}", panic_message(&*payload))))
                    }
                }
            } else {
                let cp = self.gc.callable(fn_val).unwrap();
                let base = 1usize;
                // SAFETY: the proto of a function Value.
                let needed = base + unsafe { crate::heap::proto(cp.proto) }.max_regs as usize + 8;
                if self.regs.len() < needed {
                    self.regs.resize(needed + 64, Value::nil());
                }
                for (i, &v) in args.iter().enumerate() {
                    self.regs[base + i] = v;
                }
                self.push_frame(cp.proto, cp.upvals_ptr, cp.upvals_len, base, args.len() as u8, 255)
            }
        } else {
            if yield_at > 0 {
                self.place_results(yield_at, args.to_vec(), yield_n);
            }
            Ok(())
        };
        let run_result = match setup {
            Ok(()) if !self.frames.is_empty() => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_inner())) {
                Ok(r) => r.map_err(|e| self.enrich_error_line(e)),
                Err(payload) => {
                    self.poison(nesting);
                    Err(VmError::RuntimeError(format!("internal error (panic): {}", panic_message(&*payload))))
                }
            },
            Ok(()) => Ok(()),
            // A host-function coroutine that calls yield() can't suspend —
            // there's no frame to park — so report it like any other yield
            // across a C-call boundary.
            Err(VmError::Yield(_)) if self.frames.is_empty() => Err(VmError::RuntimeError("attempt to yield across a C-call boundary".into())),
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
        let co = self.gc.coroutine_mut(co_val);
        std::mem::swap(&mut self.regs, &mut co.regs);
        std::mem::swap(&mut self.frames, &mut co.frames);
        std::mem::swap(&mut self.tbc, &mut co.tbc);
        self.resume_chain.pop();
        self.thread_floor = floor;

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
        // Interning the name costs a hash lookup; skip it when there is no
        // metatable to look in.
        if !obj.is_table() || self.gc.table(obj).metatable.is_none() {
            return Value::nil();
        }
        let key = self.intern(name);
        self.gc.meta_get(obj, key)
    }

    fn get_mm2(&mut self, a: Value, b: Value, name: &str) -> Value {
        let key = self.intern(name);
        let mm = self.gc.meta_get(a, key);
        if !mm.is_nil() {
            return mm;
        }
        self.gc.meta_get(b, key)
    }

    pub(crate) fn table_index_chain(&mut self, tbl: Value, key: Value) -> VmResult<Value> {
        let mut current = tbl;
        for _ in 0..100 {
            if !current.is_table() {
                break;
            }
            let t = self.gc.table(current);
            let raw = t.raw_get(key);
            if !raw.is_nil() || t.metatable.is_none() {
                return Ok(raw);
            }
            let idx_key = self.intern("__index");
            let mm = self.gc.meta_get(current, idx_key);
            if mm.is_nil() {
                return Ok(Value::nil());
            }
            if mm.is_table() {
                current = mm;
            } else {
                let res = self.call_value_isolated(mm, &[current, key])?;
                return Ok(res.into_iter().next().unwrap_or(Value::nil()));
            }
        }
        Err(VmError::RuntimeError("'__index' chain too long; possible loop".into()))
    }

    // A __newindex function sees a none key like any other, as in Lua; only
    // storing it is an error.
    pub(crate) fn table_newindex_chain(&mut self, tbl: Value, key: Value, val: Value) -> VmResult<()> {
        let mut current = tbl;
        for _ in 0..100 {
            if !current.is_table() {
                break;
            }
            let t = self.gc.table(current);
            if !t.raw_get(key).is_nil() || t.metatable.is_none() {
                return self.raw_store(current, key, val);
            }
            let ni_key = self.intern("__newindex");
            let mm = self.gc.meta_get(current, ni_key);
            if mm.is_nil() {
                return self.raw_store(current, key, val);
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

    // rawset for script and host: a none key is an error, as Lua's nil key
    // is, since nothing can be filed under it. NaN keys are none here.
    pub(crate) fn raw_store(&mut self, t: Value, key: Value, val: Value) -> VmResult<()> {
        if key.is_nil() {
            return Err(none_index());
        }
        self.gc.raw_set(t, key, val);
        Ok(())
    }

    fn concat_two(&mut self, left: Value, right: Value) -> VmResult<Value> {
        if (left.is_string() || left.is_number()) && (right.is_string() || right.is_number()) {
            // Build the RtString in place: one allocation, one memcpy per
            // operand, no interning — concat results are throwaway and
            // string identity is content-based everywhere else.
            let ltmp;
            let rtmp;
            let ls: &str = if left.is_string() {
                self.gc.str(left)
            } else {
                ltmp = coerce_num_str(left);
                &ltmp
            };
            let rs: &str = if right.is_string() {
                self.gc.str(right)
            } else {
                rtmp = coerce_num_str(right);
                &rtmp
            };
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
            return Err(VmError::RuntimeError(format!("attempt to concatenate a {} value", bad.type_name())));
        }
        let res = self.call_value_isolated(mm, &[left, right])?;
        Ok(res.into_iter().next().unwrap_or(Value::nil()))
    }

    // The string "a${x}b" makes: tostring() of each part, joined. The
    // parts stay rooted in their registers while a __tostring runs, and
    // its result is copied out at once.
    fn interp_join(&mut self, parts: &[Value]) -> VmResult<Value> {
        let mut buf = Vec::new();
        for &v in parts {
            if v.is_string() {
                buf.extend_from_slice(self.gc.str(v).as_bytes());
            } else {
                buf.extend_from_slice(tostring_value(self, v)?.as_bytes());
            }
        }
        let len = buf.len();
        let raw = alloc_string_owned(buf, len);
        self.gc.register(raw);
        Ok(Value::string(raw))
    }

    fn values_eq_mm(&mut self, a: Value, b: Value) -> VmResult<bool> {
        if a.raw_bits() == b.raw_bits() {
            return Ok(true);
        }
        if a.is_table() && b.is_table() {
            let mm = self.get_mm2(a, b, "__eq");
            if !mm.is_nil() {
                let res = self.call_value_isolated(mm, &[a, b])?;
                return Ok(res.into_iter().next().unwrap_or(Value::nil()).is_truthy());
            }
        }
        Ok(values_equal(&self.gc, a, b))
    }

    fn value_lt_mm(&mut self, a: Value, b: Value) -> VmResult<bool> {
        if (a.is_number() && b.is_number()) || (a.is_string() && b.is_string()) {
            return value_lt(&self.gc, a, b);
        }
        let mm = self.get_mm2(a, b, "__lt");
        if mm.is_nil() {
            return Err(VmError::RuntimeError(format!("attempt to compare {} with {}", a.type_name(), b.type_name())));
        }
        let res = self.call_value_isolated(mm, &[a, b])?;
        Ok(res.into_iter().next().unwrap_or(Value::nil()).is_truthy())
    }

    fn value_le_mm(&mut self, a: Value, b: Value) -> VmResult<bool> {
        if (a.is_number() && b.is_number()) || (a.is_string() && b.is_string()) {
            return value_le(&self.gc, a, b);
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
        Err(VmError::RuntimeError(format!("attempt to compare {} with {}", a.type_name(), b.type_name())))
    }

    #[inline(always)]
    fn resolve_const(&mut self, proto: &Proto, idx: usize) -> Value {
        // String constants dominate (every GetGlobal/LoadK name); test for
        // Str first so the hot case is a compare, not a jump-table hop.
        // SAFETY: constant indices come from the compiler, which only emits
        // indices of constants it added to this proto.
        let c = unsafe { proto.consts.get_unchecked(idx) };
        if let Const::Str(sc) = c {
            let (owner, bits) = sc.cached();
            if bits != 0 && owner == self.id {
                return Value::from_raw(bits);
            }
            return self.intern_const_str(sc);
        }
        match c {
            Const::Nil => Value::nil(),
            Const::Bool(b) => Value::bool(*b),
            Const::Int(n) => self.make_int(*n),
            Const::Float(f) => Value::float(*f),
            Const::Str(_) => unreachable!(),
        }
    }

    // First-touch path for a string constant. The proto marks the string
    // it caches from now on (Gc::propagate_proto), so the two are freed
    // together. Storing it is a write into the proto, which may already
    // be black this cycle, so it takes the barrier.
    #[cold]
    fn intern_const_str(&mut self, sc: &StrConst) -> Value {
        let v = self.intern(&sc.s);
        self.gc.barrier_val(v);
        sc.set_cached(self.id, v.raw_bits());
        v
    }

    /// Runs a compiled chunk and returns its results. The VM keeps the
    /// chunk: functions it defines point into it and may outlive this call.
    /// The results are reachable from nothing once it returns, so root them
    /// (Ctx::root) before running anything else.
    pub fn exec(&mut self, chunk: crate::compiler::Compiled) -> VmResult<Vec<Value>> {
        // Chunks run in an isolated frame too, so a host calling back into
        // the VM from inside a native can't collide with the running script.
        // The frame roots the chunk from its push on.
        let ptr = self.own_proto(chunk);
        self.nested(|vm| vm.run_isolated(ptr, std::ptr::null_mut(), 0, &[])).map_err(thrown_to_runtime)
    }

    #[inline(always)]
    pub(crate) fn push_frame(
        &mut self,
        proto: *const GcProto,
        upvals_ptr: *mut Value,
        upvals_len: usize,
        base: usize,
        nargs: u8,
        expected: u8,
    ) -> VmResult<()> {
        if self.frames.len() >= 200 {
            return Err(VmError::StackOverflow);
        }
        // SAFETY: `proto` came from a function Value or from exec.
        let p = unsafe { crate::heap::proto(proto) };
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
        let f = Frame { proto, pc: 0, base, expected_results: expected, upvals_ptr, upvals_len, varargs, top: base, callable: Value::nil(), tail: false };
        if self.frames.len() == self.frames.capacity() {
            self.frames.reserve(8);
        }
        // SAFETY: the reserve above leaves room for one more element, and
        // the slot at len is written before len covers it.
        unsafe {
            let end = self.frames.as_mut_ptr().add(self.frames.len());
            std::ptr::write(end, f);
            self.frames.set_len(self.frames.len() + 1);
        }
        Ok(())
    }

    // The innermost frame, as a reference that doesn't borrow self: the
    // dispatch loop keeps it across calls that take &mut self.
    // SAFETY (callers): use the result only until the next push, pop, swap
    // or reallocation of self.frames, and touch that frame through nothing
    // else meanwhile. run_inner re-derives it with refresh!() after every
    // operation that can do either.
    #[inline(always)]
    unsafe fn running_frame<'f>(&mut self) -> &'f mut Frame {
        let f = self.frames.last_mut().unwrap() as *mut Frame;
        // SAFETY: per the caller.
        unsafe { &mut *f }
    }

    pub fn run(&mut self) -> VmResult<()> {
        if self.poisoned {
            return Err(VmError::RuntimeError("VM is poisoned by a previous internal error".into()));
        }
        let nesting = self.nesting;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_inner()));
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(self.enrich_error_line(e)),
            Err(payload) => {
                self.poison(nesting);
                Err(VmError::RuntimeError(format!("internal error (panic): {}", panic_message(&*payload))))
            }
        }
    }

    // Prefixes a runtime error with the position it was raised at and
    // records its traceback. Thrown values keep their identity (no prefix),
    // but get a traceback too.
    fn enrich_error_line(&mut self, e: VmError) -> VmError {
        match e {
            VmError::RuntimeError(msg) => {
                self.record_traceback();
                VmError::RuntimeError(self.positioned(msg))
            }
            VmError::Thrown(_) => {
                self.record_traceback();
                e
            }
            other => other,
        }
    }

    // `msg` prefixed with the running script position, unless it already
    // has one or no script is running.
    pub(crate) fn positioned(&self, msg: String) -> String {
        if has_position_prefix(&msg) {
            return msg;
        }
        let Some(frame) = self.frames.last() else { return msg };
        let proto = frame.proto();
        match proto.lines.get(frame.pc.saturating_sub(1)) {
            Some(&line) => format!("{}:{}: {}", chunk_name(proto), line, msg),
            None => msg,
        }
    }

    // An error's traceback is taken once, where the error is first enriched,
    // from the whole stack of the running thread: the segment that raised
    // it and every outer segment parked beneath it, as Lua's traceback runs
    // from the error out to the main chunk. Each time the error propagates
    // out of an isolated call, nested() moves the record out a level, so
    // the enrichment one segment further out finds it at its own level and
    // keeps it instead of overwriting it with a shorter one. An error
    // leaving a coroutine through coroutine.wrap gets the resumer's stack
    // appended. The record is dropped once something handles the error: a
    // native returning normally at that level (pcall, coroutine.resume),
    // xpcall taking it for its handler, the host.
    fn record_traceback(&mut self) {
        let level = self.nesting;
        let thread = self.resume_chain.len();
        // Deeper records belong to errors that were handled.
        while self.err_traces.last().is_some_and(|t| t.level > level) {
            self.err_traces.pop();
        }
        match self.err_traces.last() {
            Some(t) if t.level == level => {
                if t.thread > thread {
                    let outer = self.stack_traceback(self.thread_floor);
                    let t = self.err_traces.last_mut().unwrap();
                    if !outer.is_empty() {
                        t.traceback.push('\n');
                        t.traceback.push_str(&outer);
                    }
                    t.thread = thread;
                }
            }
            _ => {
                let traceback = self.stack_traceback(self.thread_floor);
                self.err_serial += 1;
                self.err_traces.push(ErrTrace { level, thread, serial: self.err_serial, traceback });
            }
        }
    }

    // A native returned normally, so it handled any error that reached its
    // level.
    #[inline(always)]
    fn drop_handled_traces(&mut self) {
        while self.err_traces.last().is_some_and(|t| t.level >= self.nesting) {
            self.err_traces.pop();
        }
    }

    // The traceback of the error an isolated call made at this level just
    // returned, if that error was enriched and recorded after serial `since`.
    pub(crate) fn delivered_traceback(&self, since: u64) -> Option<&str> {
        self.err_traces.last().filter(|t| t.level == self.nesting && t.serial > since).map(|t| t.traceback.as_str())
    }

    // The running thread's stack from segment `from` out to the innermost
    // frame, innermost first. Segment i < saved_frames.len() is
    // saved_frames[i], a stack run_isolated parked; saved_frames.len() is
    // the running one.
    fn stack_traceback(&self, from: usize) -> String {
        if from > self.saved_frames.len() {
            return String::new();
        }
        let segments = std::iter::once(&self.frames).chain(self.saved_frames[from..].iter().rev());
        let parts: Vec<String> = segments.filter(|s| !s.is_empty()).map(|s| build_traceback(s)).collect();
        parts.join("\n")
    }

    pub fn run_inner(&mut self) -> VmResult<()> {
        'outer: loop {
            // GC work runs here, before `frame` becomes a raw pointer into
            // self.frames: a step may finish a cycle and queue __gc
            // finalizers, which push frames of their own when drained.
            if self.gc.should_step() {
                self.gc_step();
            }

            // The hard ceiling can't wait for an incremental cycle to finish:
            // force a full collection, then error if it's still exceeded.
            // >= not >: at the ceiling the allocating instruction never gets
            // to run, so > would spin here instead of reporting.
            if self.gc.max_objects != 0 && self.gc.live_count() >= self.gc.max_objects {
                self.gc_collect();
                if self.gc.live_count() >= self.gc.max_objects {
                    return Err(VmError::RuntimeError("memory limit exceeded".into()));
                }
            }

            if self.step_limit != 0 && self.step_count > self.step_limit {
                return Err(VmError::RuntimeError("instruction budget exceeded".into()));
            }

            self.run_pending_finalizers();
            // The running frame's state, cached in locals for dispatch:
            // `frame` points into self.frames without borrowing self (see
            // running_frame), `proto` is its proto, `regs` the base of
            // self.regs. Anything that can push, pop or swap frames or
            // resize the registers (calls, returns, metamethods, natives) is
            // followed by refresh!() before any of them is touched again.
            let mut frame: &mut Frame;
            let mut proto: &Proto;
            let mut base: usize;
            let mut regs: *mut Value;
            macro_rules! refresh {
                () => {{
                    // SAFETY: the rule above is running_frame's contract.
                    frame = unsafe { self.running_frame() };
                    // SAFETY: a frame's proto outlives the frame.
                    proto = unsafe { crate::heap::proto(frame.proto) };
                    base = frame.base;
                    regs = self.regs.as_mut_ptr();
                }};
            }
            refresh!();

            // Register and constant indices come from the compiler's own
            // bytecode (register allocation is bounded by max_regs, which
            // push_frame sizes the window for), so indexing is unchecked —
            // the bounds checks were a measurable slice of dispatch cost.
            macro_rules! R {
                // SAFETY: base + r < self.regs.len() by the sizing above, and
                // `regs` is current: every resize is followed by refresh!().
                ($r:expr) => {
                    *unsafe { &mut *regs.add(base + $r) }
                };
            }
            macro_rules! RK {
                ($x:expr) => {
                    if is_rk($x) { self.resolve_const(proto, rk_idx($x)) } else { R!($x) }
                };
            }
            // GC/budget checkpoint. Only needed where a loop can spin
            // (backward jumps, calls, returns) or a GC object was just
            // created — checking every instruction was pure dispatch cost.
            macro_rules! ck {
                () => {
                    if self.gc.should_step() || (self.step_limit != 0 && self.step_count > self.step_limit) {
                        continue 'outer;
                    }
                };
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
                            let bad = if !bv.is_number() { ($b, bv) } else { ($c, cv) };
                            return Err(VmError::RuntimeError(format!(
                                "attempt to perform arithmetic on a {} value{}",
                                bad.1.type_name(),
                                rk_varinfo(proto, frame.pc - 1, bad.0)
                            )));
                        }
                        let res = self.call_value_isolated(mm, &[bv, cv])?;
                        self.regs[base + $dst] = res.into_iter().next().unwrap_or(Value::nil());
                        continue 'outer;
                    }
                }};
            }
            // The compiler always emits a Jmp right after a conditional op;
            // consume it inline so a compare+branch costs one dispatch.
            // The consumed Jmp still counts as a step, and a backward target
            // is a loop edge, so it checkpoints like a real Jmp.
            macro_rules! fused_jmp {
                ($taken:expr) => {{
                    // SAFETY: the compiler emits a Jmp after every conditional
                    // op, so frame.pc is still inside the code.
                    let j = unsafe { *proto.code.get_unchecked(frame.pc) };
                    debug_assert!(iop(j) == Op::Jmp as u8);
                    frame.pc += 1;
                    self.step_count += 1;
                    if $taken {
                        frame.pc = (frame.pc as i32 + isbx(j)) as usize;
                        if isbx(j) < 0 {
                            ck!();
                        }
                    }
                }};
            }
            // Shared table-index body for GetTable/GetField/SelfOp: stores
            // the looked-up value in R[dst]. The __index path can swap
            // frames, so it refreshes the cached frame state, and it stores
            // the result before its checkpoint: ck!() resumes at the next
            // instruction, so a step taken first would drop the result,
            // which nothing roots until it is in its register.
            macro_rules! get_table {
                ($dst:expr, $tv:expr, $kv:expr, $treg:expr) => {{
                    let tv = $tv;
                    let kv = $kv;
                    let v = if tv.is_table() {
                        let t = self.gc.table(tv);
                        let raw = t.raw_get(kv);
                        if !raw.is_nil() || t.metatable.is_none() {
                            raw
                        } else {
                            let result = self.table_index_chain(tv, kv)?;
                            refresh!();
                            R!($dst) = result;
                            ck!();
                            continue;
                        }
                    } else if tv.is_string() {
                        if self.string_lib.is_table() { self.gc.table(self.string_lib).raw_get(kv) } else { Value::nil() }
                    } else if tv.is_bytes() {
                        if kv.is_int_like() {
                            let b = self.gc.bytes(tv);
                            let i = kv.as_int().unwrap();
                            if i >= 1 && i <= b.len as i64 { Value::int(b.data[(i - 1) as usize] as i64) } else { Value::nil() }
                        } else if self.bytes_lib.is_table() {
                            self.gc.table(self.bytes_lib).raw_get(kv)
                        } else {
                            Value::nil()
                        }
                    } else {
                        return Err(VmError::RuntimeError(format!("attempt to index a {} value{}", tv.type_name(), varinfo(proto, frame.pc - 1, $treg))));
                    };
                    R!($dst) = v;
                }};
            }
            // `$any_key` is false for SetField, whose key is a constant
            // string, so only SetTable pays for the none-key check.
            macro_rules! set_table {
                ($tv:expr, $kv:expr, $vv:expr, $treg:expr, $any_key:expr) => {{
                    let tv = $tv;
                    let kv = $kv;
                    let vv = $vv;
                    if tv.is_table() {
                        let t = self.gc.table(tv);
                        if t.metatable.is_none() || !t.raw_get(kv).is_nil() {
                            if $any_key && kv.is_nil() {
                                return Err(none_index());
                            }
                            self.gc.raw_set(tv, kv, vv);
                        } else {
                            self.table_newindex_chain(tv, kv, vv)?;
                            refresh!();
                            ck!();
                        }
                    } else if tv.is_bytes() {
                        let b = self.gc.bytes_mut(tv);
                        let i = match kv.as_int() {
                            Some(i) if i >= 1 && i <= b.len as i64 => i,
                            _ => return Err(VmError::RuntimeError("bytes index out of bounds".into())),
                        };
                        let n = match vv.as_int() {
                            Some(n) if (0..=255).contains(&n) => n as u8,
                            _ => return Err(VmError::RuntimeError("bytes value must be an integer 0-255".into())),
                        };
                        b.data[(i - 1) as usize] = n;
                    } else {
                        return Err(VmError::RuntimeError(format!("attempt to index a {} value{}", tv.type_name(), varinfo(proto, frame.pc - 1, $treg))));
                    }
                }};
            }

            // Every compiled Proto ends in Return, so the loop exits through
            // the Return arm — no per-instruction pc bounds check needed.
            loop {
                // Cheap per-instruction counting only; the actual gc_collect()/
                // budget-error handling happens back at 'outer's top, where it's
                // safe to mutate self.frames (frame/proto aren't held past this point).
                self.step_count += 1;

                // SAFETY: every compiled proto ends in Return and jumps
                // target its own code, so pc stays in bounds.
                let instr = unsafe { *proto.code.get_unchecked(frame.pc) };
                frame.pc += 1;
                let a = || ia(instr);
                let b = || ib(instr);
                let c = || ic(instr);
                let bx = || ibx(instr);
                let sbx = || isbx(instr);

                // Op is #[repr(u8)] with contiguous variants 0..=TailCall and the
                // compiler only emits valid opcodes, so decode unchecked.
                debug_assert!(iop(instr) <= Op::TailCall as u8);
                // SAFETY: as the debug_assert says, and Op is repr(u8).
                match unsafe { std::mem::transmute::<u8, Op>(iop(instr)) } {
                    Op::LoadNil => R!(a()) = Value::nil(),
                    Op::LoadBool => {
                        R!(a()) = Value::bool(b() != 0);
                        if c() != 0 {
                            frame.pc += 1;
                        }
                    }
                    Op::LoadInt => R!(a()) = Value::int(sbx() as i64),
                    Op::LoadK => {
                        let v = self.resolve_const(proto, bx());
                        R!(a()) = v;
                    }
                    Op::Move => R!(a()) = R!(b()),

                    Op::Add => arith_op!(a(), b(), c(), |x: i64, y: i64| -> VmResult<i64> { Ok(x.wrapping_add(y)) }, |x: f64, y: f64| x + y, "__add"),
                    Op::Sub => arith_op!(a(), b(), c(), |x: i64, y: i64| -> VmResult<i64> { Ok(x.wrapping_sub(y)) }, |x: f64, y: f64| x - y, "__sub"),
                    // Immediate arith: C is a signed literal, not an RK
                    // operand — the int fast path skips the const resolve.
                    // The metamethod/error path behaves exactly like the
                    // generic op with the literal as the second operand.
                    Op::AddI => {
                        let bv = R!(b());
                        let k = ici(instr);
                        if bv.is_int_like() {
                            let res = self.make_int(bv.as_int().unwrap().wrapping_add(k));
                            R!(a()) = res;
                        } else if bv.is_float() {
                            R!(a()) = Value::float(bv.as_float().unwrap() + k as f64);
                        } else {
                            let cv = Value::int(k);
                            let mm = self.get_mm2(bv, cv, "__add");
                            if mm.is_nil() {
                                return Err(VmError::RuntimeError(format!(
                                    "attempt to perform arithmetic on a {} value{}",
                                    bv.type_name(),
                                    varinfo(proto, frame.pc - 1, b())
                                )));
                            }
                            let res = self.call_value_isolated(mm, &[bv, cv])?;
                            self.regs[base + a()] = res.into_iter().next().unwrap_or(Value::nil());
                            continue 'outer;
                        }
                    }
                    Op::SubI => {
                        let bv = R!(b());
                        let k = ici(instr);
                        if bv.is_int_like() {
                            let res = self.make_int(bv.as_int().unwrap().wrapping_sub(k));
                            R!(a()) = res;
                        } else if bv.is_float() {
                            R!(a()) = Value::float(bv.as_float().unwrap() - k as f64);
                        } else {
                            let cv = Value::int(k);
                            let mm = self.get_mm2(bv, cv, "__sub");
                            if mm.is_nil() {
                                return Err(VmError::RuntimeError(format!(
                                    "attempt to perform arithmetic on a {} value{}",
                                    bv.type_name(),
                                    varinfo(proto, frame.pc - 1, b())
                                )));
                            }
                            let res = self.call_value_isolated(mm, &[bv, cv])?;
                            self.regs[base + a()] = res.into_iter().next().unwrap_or(Value::nil());
                            continue 'outer;
                        }
                    }
                    Op::Mul => arith_op!(a(), b(), c(), |x: i64, y: i64| -> VmResult<i64> { Ok(x.wrapping_mul(y)) }, |x: f64, y: f64| x * y, "__mul"),
                    Op::Div => {
                        let bv = RK!(b());
                        let cv = RK!(c());
                        if bv.is_number() && cv.is_number() {
                            R!(a()) = Value::float(bv.to_float().unwrap() / cv.to_float().unwrap());
                        } else {
                            let mm = self.get_mm2(bv, cv, "__div");
                            if mm.is_nil() {
                                let bad = if !bv.is_number() { (b(), bv) } else { (c(), cv) };
                                return Err(VmError::RuntimeError(format!(
                                    "attempt to perform arithmetic on a {} value{}",
                                    bad.1.type_name(),
                                    rk_varinfo(proto, frame.pc - 1, bad.0)
                                )));
                            }
                            let res = self.call_value_isolated(mm, &[bv, cv])?;
                            self.regs[base + a()] = res.into_iter().next().unwrap_or(Value::nil());
                            continue 'outer;
                        }
                    }
                    Op::IDiv => arith_op!(
                        a(),
                        b(),
                        c(),
                        |x: i64, y: i64| -> VmResult<i64> {
                            if y == 0 { Err(VmError::RuntimeError("attempt to perform 'n//0'".into())) } else { Ok(lua_idiv(x, y)) }
                        },
                        |x: f64, y: f64| (x / y).floor(),
                        "__idiv"
                    ),
                    Op::Mod => arith_op!(
                        a(),
                        b(),
                        c(),
                        |x: i64, y: i64| -> VmResult<i64> {
                            if y == 0 { Err(VmError::RuntimeError("attempt to perform 'n%0'".into())) } else { Ok(lua_mod(x, y)) }
                        },
                        |x: f64, y: f64| x - (x / y).floor() * y,
                        "__mod"
                    ),
                    Op::Pow => {
                        let bv = RK!(b());
                        let cv = RK!(c());
                        if bv.is_number() && cv.is_number() {
                            R!(a()) = Value::float(bv.to_float().unwrap().powf(cv.to_float().unwrap()));
                        } else {
                            let mm = self.get_mm2(bv, cv, "__pow");
                            if mm.is_nil() {
                                let bad = if !bv.is_number() { (b(), bv) } else { (c(), cv) };
                                return Err(VmError::RuntimeError(format!(
                                    "attempt to perform arithmetic on a {} value{}",
                                    bad.1.type_name(),
                                    rk_varinfo(proto, frame.pc - 1, bad.0)
                                )));
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
                                    "attempt to perform arithmetic on a {} value{}",
                                    v.type_name(),
                                    varinfo(proto, frame.pc - 1, b())
                                )));
                            }
                            let res = self.call_value_isolated(mm, &[v, v])?;
                            self.regs[base + a()] = res.into_iter().next().unwrap_or(Value::nil());
                            continue 'outer;
                        }
                    }
                    Op::BAnd => {
                        let bv = int_val(RK!(b()))?;
                        let cv = int_val(RK!(c()))?;
                        let res = self.make_int(bv & cv);
                        R!(a()) = res;
                    }
                    Op::BOr => {
                        let bv = int_val(RK!(b()))?;
                        let cv = int_val(RK!(c()))?;
                        let res = self.make_int(bv | cv);
                        R!(a()) = res;
                    }
                    Op::BXor => {
                        let bv = int_val(RK!(b()))?;
                        let cv = int_val(RK!(c()))?;
                        let res = self.make_int(bv ^ cv);
                        R!(a()) = res;
                    }
                    Op::Shl => {
                        let bv = int_val(RK!(b()))?;
                        let cv = int_val(RK!(c()))?;
                        let res = self.make_int(lua_shl(bv, cv));
                        R!(a()) = res;
                    }
                    Op::Shr => {
                        let bv = int_val(RK!(b()))?;
                        let cv = int_val(RK!(c()))?;
                        let res = self.make_int(lua_shl(bv, cv.wrapping_neg()));
                        R!(a()) = res;
                    }
                    Op::BNot => {
                        let v = int_val(R!(b()))?;
                        let res = self.make_int(!v);
                        R!(a()) = res;
                    }
                    Op::Not => R!(a()) = Value::bool(!R!(b()).is_truthy()),
                    Op::Len => {
                        let v = R!(b());
                        if v.is_string() {
                            let n = self.gc.str(v).len();
                            R!(a()) = Value::int(n as i64);
                        } else if v.is_bytes() {
                            let n = self.gc.bytes(v).len;
                            R!(a()) = Value::int(n as i64);
                        } else if v.is_table() {
                            let t = self.gc.table(v);
                            let (len, has_mt) = (t.length(), t.metatable.is_some());
                            if has_mt {
                                let key = self.intern("__len");
                                let mm = self.gc.meta_get(v, key);
                                if !mm.is_nil() {
                                    let res = self.call_value_isolated(mm, &[v])?;
                                    self.regs[base + a()] = res.into_iter().next().unwrap_or(Value::nil());
                                    continue 'outer;
                                }
                            }
                            R!(a()) = Value::int(len);
                        } else {
                            return Err(VmError::RuntimeError(format!(
                                "attempt to get length of a {} value{}",
                                v.type_name(),
                                varinfo(proto, frame.pc - 1, b())
                            )));
                        }
                    }
                    Op::Concat => {
                        // Snapshot the operand registers: concat_two can run
                        // a __concat metamethod that reallocates self.regs.
                        let lo = b();
                        let mut vals: Vec<Value> = (lo..=c()).map(|i| R!(i)).collect();
                        let mut acc = vals.pop().unwrap_or(Value::nil());
                        // Rightmost operand's register, for error naming.
                        let mut acc_reg = c();
                        for (k, &left) in vals.iter().enumerate().rev() {
                            match self.concat_two(left, acc) {
                                Ok(v) => {
                                    acc = v;
                                    acc_reg = lo + k;
                                }
                                Err(e) => {
                                    let (bad, reg) = if !left.is_string() && !left.is_number() { (left, lo + k) } else { (acc, acc_reg) };
                                    return Err(match e {
                                        VmError::RuntimeError(m) if m.starts_with("attempt to concatenate") => VmError::RuntimeError(format!(
                                            "attempt to concatenate a {} value{}",
                                            bad.type_name(),
                                            varinfo(proto, frame.pc - 1, reg)
                                        )),
                                        other => other,
                                    });
                                }
                            }
                        }
                        self.regs[base + a()] = acc;
                        continue 'outer;
                    }
                    Op::Interp => {
                        // Snapshot, as Concat does: __tostring can
                        // reallocate self.regs.
                        let vals: Vec<Value> = (b()..=c()).map(|i| R!(i)).collect();
                        let s = self.interp_join(&vals)?;
                        self.regs[base + a()] = s;
                        continue 'outer;
                    }

                    // The compares' slow paths can run a metamethod, which may
                    // allocate. They checkpoint only after fused_jmp! has
                    // consumed the Jmp: ck!() resumes at frame.pc, so a step
                    // taken first would run the Jmp unconditionally.
                    Op::Eq => {
                        let bv = RK!(b());
                        let cv = RK!(c());
                        let (eq, slow) = if bv.raw_bits() == cv.raw_bits() {
                            (true, false)
                        } else {
                            let eq = self.values_eq_mm(bv, cv)?;
                            refresh!();
                            (eq, true)
                        };
                        fused_jmp!(eq == (a() != 0));
                        if slow {
                            ck!();
                        }
                    }

                    Op::Lt => {
                        let bv = RK!(b());
                        let cv = RK!(c());
                        // Numbers compare inline (floats can never be NaN here —
                        // Value::float maps NaN to nil); only non-numbers take
                        // the metamethod path, which may swap frames.
                        let (lt, slow) = if bv.is_int() && cv.is_int() {
                            (bv.as_int().unwrap() < cv.as_int().unwrap(), false)
                        } else if bv.is_number() && cv.is_number() {
                            (value_lt(&self.gc, bv, cv)?, false)
                        } else {
                            let lt = self.value_lt_mm(bv, cv);
                            refresh!();
                            (lt.map_err(|e| name_order_err(e, proto, frame.pc - 1, bv, cv, b(), c()))?, true)
                        };
                        fused_jmp!(lt == (a() != 0));
                        if slow {
                            ck!();
                        }
                    }
                    Op::Le => {
                        let bv = RK!(b());
                        let cv = RK!(c());
                        let (le, slow) = if bv.is_int() && cv.is_int() {
                            (bv.as_int().unwrap() <= cv.as_int().unwrap(), false)
                        } else if bv.is_number() && cv.is_number() {
                            (value_le(&self.gc, bv, cv)?, false)
                        } else {
                            let le = self.value_le_mm(bv, cv);
                            refresh!();
                            (le.map_err(|e| name_order_err(e, proto, frame.pc - 1, bv, cv, b(), c()))?, true)
                        };
                        fused_jmp!(le == (a() != 0));
                        if slow {
                            ck!();
                        }
                    }

                    // Immediate compares: R[B] against a signed literal (C).
                    // GtI/GeI evaluate `imm < R[B]` / `imm <= R[B]` — the
                    // compiler already swapped the source operands, so the
                    // metamethod path sees the same argument order the
                    // generic Lt/Le would.
                    Op::LtI => {
                        let bv = R!(b());
                        let k = ici(instr);
                        let (lt, slow) = if bv.is_int() {
                            (bv.as_int().unwrap() < k, false)
                        } else if bv.is_number() {
                            (value_lt(&self.gc, bv, Value::int(k))?, false)
                        } else {
                            let lt = self.value_lt_mm(bv, Value::int(k));
                            refresh!();
                            (lt.map_err(|e| name_order_err(e, proto, frame.pc - 1, bv, Value::int(k), b(), usize::MAX))?, true)
                        };
                        fused_jmp!(lt == (a() != 0));
                        if slow {
                            ck!();
                        }
                    }
                    Op::LeI => {
                        let bv = R!(b());
                        let k = ici(instr);
                        let (le, slow) = if bv.is_int() {
                            (bv.as_int().unwrap() <= k, false)
                        } else if bv.is_number() {
                            (value_le(&self.gc, bv, Value::int(k))?, false)
                        } else {
                            let le = self.value_le_mm(bv, Value::int(k));
                            refresh!();
                            (le.map_err(|e| name_order_err(e, proto, frame.pc - 1, bv, Value::int(k), b(), usize::MAX))?, true)
                        };
                        fused_jmp!(le == (a() != 0));
                        if slow {
                            ck!();
                        }
                    }
                    Op::GtI => {
                        let bv = R!(b());
                        let k = ici(instr);
                        let (gt, slow) = if bv.is_int() {
                            (k < bv.as_int().unwrap(), false)
                        } else if bv.is_number() {
                            (value_lt(&self.gc, Value::int(k), bv)?, false)
                        } else {
                            let gt = self.value_lt_mm(Value::int(k), bv);
                            refresh!();
                            (gt.map_err(|e| name_order_err(e, proto, frame.pc - 1, Value::int(k), bv, usize::MAX, b()))?, true)
                        };
                        fused_jmp!(gt == (a() != 0));
                        if slow {
                            ck!();
                        }
                    }
                    Op::GeI => {
                        let bv = R!(b());
                        let k = ici(instr);
                        let (ge, slow) = if bv.is_int() {
                            (k <= bv.as_int().unwrap(), false)
                        } else if bv.is_number() {
                            (value_le(&self.gc, Value::int(k), bv)?, false)
                        } else {
                            let ge = self.value_le_mm(Value::int(k), bv);
                            refresh!();
                            (ge.map_err(|e| name_order_err(e, proto, frame.pc - 1, Value::int(k), bv, usize::MAX, b()))?, true)
                        };
                        fused_jmp!(ge == (a() != 0));
                        if slow {
                            ck!();
                        }
                    }
                    Op::EqI => {
                        let bv = R!(b());
                        let cv = Value::int(ici(instr));
                        let (eq, slow) = if bv.raw_bits() == cv.raw_bits() {
                            (true, false)
                        } else {
                            let eq = self.values_eq_mm(bv, cv)?;
                            refresh!();
                            (eq, true)
                        };
                        fused_jmp!(eq == (a() != 0));
                        if slow {
                            ck!();
                        }
                    }

                    Op::Test => {
                        fused_jmp!(R!(a()).is_truthy() == (c() != 0));
                    }
                    Op::TestSet => {
                        let bv = R!(b());
                        let taken = bv.is_truthy() == (c() != 0);
                        if taken {
                            R!(a()) = bv;
                        }
                        fused_jmp!(taken);
                    }
                    Op::Jmp => {
                        frame.pc = (frame.pc as i32 + sbx()) as usize;
                        if sbx() < 0 {
                            ck!();
                        }
                    }

                    Op::NewTable => {
                        let ptr = alloc_table_raw();
                        self.gc.register(ptr);
                        R!(a()) = Value::table(ptr);
                        ck!();
                    }
                    Op::GetTable => get_table!(a(), R!(b()), RK!(c()), b()),
                    Op::SetTable => {
                        set_table!(R!(a()), RK!(b()), RK!(c()), a(), true);
                    }
                    Op::GetField => {
                        let kv = self.resolve_const(proto, c());
                        get_table!(a(), R!(b()), kv, b());
                    }
                    Op::SetField => {
                        let kv = self.resolve_const(proto, b());
                        set_table!(R!(a()), kv, RK!(c()), a(), false);
                    }
                    Op::SelfOp => {
                        let tv = R!(b());
                        let kv = self.resolve_const(proto, c());
                        R!(a() + 1) = tv;
                        get_table!(a(), tv, kv, b());
                    }
                    Op::SetList => {
                        let tv = R!(a());
                        let offset = (c() as usize - 1) * 50;
                        let n = if b() == 0 { frame.top.saturating_sub(base + a() + 1) } else { b() };
                        for i in 1..=n {
                            let v = R!(a() + i);
                            self.gc.raw_set(tv, Value::int((offset + i) as i64), v);
                        }
                    }

                    Op::GetGlobal => {
                        let bi = bx();
                        let cached = proto.global_cache.get(bi).map(|c| c.get());
                        if let Some((owner, g, bits)) = cached {
                            if bits != 0 && owner == self.id && g == self.globals_gen {
                                R!(a()) = Value::from_raw(bits);
                                continue;
                            }
                        }
                        let k = self.resolve_const(proto, bi);
                        if !k.is_string() {
                            return Err(VmError::RuntimeError("invalid global name".into()));
                        }
                        let v = self.globals.raw_get(k);
                        if let Some(c) = proto.global_cache.get(bi) {
                            c.set((self.id, self.globals_gen, v.raw_bits()));
                        }
                        R!(a()) = v;
                    }
                    Op::SetGlobal => {
                        let k = self.resolve_const(proto, bx());
                        if !k.is_string() {
                            return Err(VmError::RuntimeError("invalid global name".into()));
                        }
                        let v = R!(a());
                        self.globals.raw_set(&mut self.gc, k, v);
                        self.globals_gen += 1;
                    }

                    Op::Call => {
                        let fn_val = R!(a());
                        let nargs = if b() == 0 { frame.top.saturating_sub(base + a() + 1) as u8 } else { (b() - 1) as u8 };
                        let nresults = if c() == 0 { 255 } else { (c() - 1) as u8 };

                        // Proto-callables (script functions/closures) are the
                        // common case; cfn and __call checks come after.
                        if let Some(cp) = self.gc.callable(fn_val) {
                            self.push_frame(cp.proto, cp.upvals_ptr, cp.upvals_len, base + a() + 1, nargs, nresults)?;
                            // Refresh in place instead of re-entering 'outer;
                            // the checkpoint below covers the GC/budget check.
                            refresh!();
                            ck!();
                            continue;
                        } else if crate::heap::is_native(fn_val) {
                            let args: Vec<Value> = (0..nargs as usize).map(|i| R!(a() + 1 + i)).collect();
                            let results = self.call_native(fn_val, &args)?;
                            // `continue 'outer` re-fetches `frame`/`proto` from self.frames;
                            // a CFunction may have swapped frames (e.g. coroutine.resume),
                            // so the old references must not be touched past this point.
                            self.place_results(base + a(), results, nresults);
                            continue 'outer;
                        } else if fn_val.is_table() {
                            let mm = self.get_mm(fn_val, "__call");
                            if mm.is_nil() {
                                return Err(VmError::RuntimeError(format!("attempt to call a table value{}", varinfo(proto, frame.pc - 1, a()))));
                            }
                            let mut mm_args = Vec::with_capacity(nargs as usize + 1);
                            mm_args.push(fn_val);
                            for i in 0..nargs as usize {
                                mm_args.push(R!(a() + 1 + i));
                            }
                            let results = self.call_value_isolated(mm, &mm_args)?;
                            self.place_results(base + a(), results, nresults);
                            continue 'outer;
                        } else {
                            return Err(VmError::RuntimeError(format!("attempt to call a {} value{}", fn_val.type_name(), varinfo(proto, frame.pc - 1, a()))));
                        }
                    }

                    // `return f(...)`: the callee takes over this frame —
                    // args move down to base, pc/proto/upvals swap, and the
                    // original caller's expected_results stays in force.
                    // The compiler only emits it when no <close> mark is
                    // pending in this frame, so nothing here needs unwinding.
                    Op::TailCall => {
                        let fn_val = R!(a());
                        let nargs = if b() == 0 { frame.top.saturating_sub(base + a() + 1) } else { b() - 1 };

                        if let Some(cp) = self.gc.callable(fn_val) {
                            // SAFETY: the proto of a function Value; protos live
                            // as long as the Vm.
                            let p = unsafe { crate::heap::proto(cp.proto) };
                            let needed = base + (p.max_regs as usize).max(nargs) + 8;
                            if needed > self.regs.len() {
                                self.regs.resize(needed + 64, Value::nil());
                            }
                            self.regs.copy_within(base + a() + 1..base + a() + 1 + nargs, base);
                            regs = self.regs.as_mut_ptr();
                            let varargs: Box<[Value]> = if p.is_vararg && nargs > p.params as usize {
                                self.regs[base + p.params as usize..base + nargs].to_vec().into_boxed_slice()
                            } else {
                                Box::new([])
                            };
                            frame.proto = cp.proto;
                            frame.pc = 0;
                            frame.upvals_ptr = cp.upvals_ptr;
                            frame.upvals_len = cp.upvals_len;
                            frame.varargs = varargs;
                            frame.top = base;
                            frame.callable = fn_val;
                            frame.tail = true;
                            proto = p;
                            ck!();
                            continue;
                        } else if crate::heap::is_native(fn_val) {
                            let args: Vec<Value> = (0..nargs).map(|i| R!(a() + 1 + i)).collect();
                            let expected = frame.expected_results;
                            let base_save = base;
                            let results = self.call_native(fn_val, &args)?;
                            // A yield propagates with the frame still live:
                            // resume() drops the results at base+a() and the
                            // compiler-emitted Return after this op sends
                            // them to the original caller.
                            self.frames.pop();
                            if self.frames.is_empty() {
                                self.top_level_results = results;
                                return Ok(());
                            }
                            self.place_results(base_save - 1, results, expected);
                            continue 'outer;
                        } else if fn_val.is_table() {
                            let mm = self.get_mm(fn_val, "__call");
                            if mm.is_nil() {
                                return Err(VmError::RuntimeError(format!("attempt to call a table value{}", varinfo(proto, frame.pc - 1, a()))));
                            }
                            let mut mm_args = Vec::with_capacity(nargs + 1);
                            mm_args.push(fn_val);
                            for i in 0..nargs {
                                mm_args.push(R!(a() + 1 + i));
                            }
                            let expected = frame.expected_results;
                            let base_save = base;
                            let results = self.call_value_isolated(mm, &mm_args)?;
                            self.frames.pop();
                            if self.frames.is_empty() {
                                self.top_level_results = results;
                                return Ok(());
                            }
                            self.place_results(base_save - 1, results, expected);
                            continue 'outer;
                        } else {
                            return Err(VmError::RuntimeError(format!("attempt to call a {} value{}", fn_val.type_name(), varinfo(proto, frame.pc - 1, a()))));
                        }
                    }

                    Op::Return => {
                        let nv = if b() == 0 { frame.top.saturating_sub(base + a()) } else { b() - 1 };
                        let expected = frame.expected_results;
                        let base_save = base;
                        self.frames.pop();
                        if self.frames.is_empty() {
                            self.top_level_results = self.regs[base_save + a()..base_save + a() + nv].to_vec();
                            return Ok(());
                        }
                        // The frame was pushed at (call site A) + 1, so results land
                        // back on the calling instruction's A register. Source and
                        // destination overlap (dst < src), so move in place.
                        self.place_results_from(base_save - 1, base_save + a(), nv, expected);
                        refresh!();
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
                        let idx = to_number(R!(a()), "initial value")?;
                        let lim = to_number(R!(a() + 1), "limit")?;
                        let step = to_number(R!(a() + 2), "step")?;
                        let in_range = if num_is_positive(step) { num_le(idx, lim) } else { num_le(lim, idx) };
                        if in_range {
                            R!(a() + 3) = R!(a());
                            // Advance the candidate for the next check. Integer
                            // overflow must terminate the loop (Lua semantics),
                            // not wrap the counter back to the other end.
                            let next = num_add(self, idx, step);
                            R!(a()) = next;
                            frame.pc = (frame.pc as i32 + sbx()) as usize;
                            ck!();
                        }
                    }

                    // Frame layout is base=fn, +1 state, +2 control, +3 unused,
                    // +4.. loop vars; a script iterator's frame is pushed at a+5
                    // so its Return lands the results on a+4 like the cfn path.
                    Op::TForCall => {
                        let fn_val = R!(a());
                        let state = R!(a() + 1);
                        let ctrl = R!(a() + 2);
                        let nresults = c() as u8;
                        if crate::heap::is_native(fn_val) {
                            let results = self.call_native(fn_val, &[state, ctrl])?;
                            self.place_results(base + a() + 4, results, nresults);
                            continue 'outer;
                        } else {
                            let cp = self.gc.callable(fn_val).ok_or_else(|| {
                                VmError::RuntimeError(format!("attempt to call a {} value (for iterator 'for iterator')", fn_val.type_name()))
                            })?;
                            let new_base = base + a() + 5;
                            // SAFETY: the proto of a function Value.
                            let needed = new_base + unsafe { crate::heap::proto(cp.proto) }.max_regs as usize + 8;
                            if needed > self.regs.len() {
                                self.regs.resize(needed, Value::nil());
                            }
                            self.regs[new_base] = state;
                            self.regs[new_base + 1] = ctrl;
                            self.push_frame(cp.proto, cp.upvals_ptr, cp.upvals_len, new_base, 2, nresults)?;
                            refresh!();
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
                        let inner = proto.protos[bx()];
                        // SAFETY: a child of the running proto, which marks
                        // it (Gc::propagate_proto) and is rooted by its frame.
                        let inner_proto = unsafe { crate::heap::proto(inner) };
                        let closure_val = if inner_proto.upvals.is_empty() {
                            Value::userdata(inner as *mut u8)
                        } else {
                            let upvals: Vec<Value> = inner_proto
                                .upvals
                                .iter()
                                .map(|desc| if desc.in_stack { R!(desc.idx as usize) } else { frame.upval(desc.idx as usize) })
                                .collect();
                            let lc = Box::new(LuaClosure { gc: crate::gc::GcHeader::new(crate::gc::GcKind::Closure), proto: inner, upvals });
                            let ptr = Box::into_raw(lc) as *mut u8;
                            self.gc.register(ptr);
                            Value::closure(ptr)
                        };
                        R!(a()) = closure_val;
                        ck!();
                    }
                    Op::GetUpval => R!(a()) = frame.upval(b()),
                    Op::SetUpval => {
                        if b() < frame.upvals_len {
                            let v = R!(a());
                            // Write barrier: the owning closure may already be
                            // black this cycle. upvals_ptr borrows the closure's
                            // Vec, so the owner isn't reachable from here —
                            // mark the value unconditionally when a cycle is
                            // live (over-marking only costs retention).
                            self.gc.barrier_val(v);
                            frame.set_upval(b(), v);
                        }
                    }

                    Op::Vararg => {
                        let n = if b() == 0 { frame.varargs.len() } else { b() - 1 };
                        if base + a() + n > self.regs.len() {
                            self.regs.resize(base + a() + n + 64, Value::nil());
                            regs = self.regs.as_mut_ptr();
                        }
                        for i in 0..n {
                            R!(a() + i) = frame.varargs.get(i).copied().unwrap_or(Value::nil());
                        }
                        if b() == 0 {
                            frame.top = base + a() + n;
                        }
                    }

                    Op::Tbc => self.tbc.push((base + a(), b() != 0)),
                    Op::TbcPop => {
                        self.tbc.pop();
                    }
                }
            }
        }
    }
    // Writes a call's results at `at`, nil-padding to `expected` (255 = keep
    // them all and record the new top for a following b=0 Call/Return).
    fn place_results(&mut self, at: usize, results: Vec<Value>, expected: u8) {
        let nr = results.len();
        let fill = if expected == 255 { nr } else { expected as usize };
        if at + fill > self.regs.len() {
            self.regs.resize(at + fill + 64, Value::nil());
        }
        for (i, v) in results.into_iter().take(fill).enumerate() {
            self.regs[at + i] = v;
        }
        for i in nr..fill {
            self.regs[at + i] = Value::nil();
        }
        if expected == 255 {
            if let Some(f) = self.frames.last_mut() {
                f.top = at + nr;
            }
        }
    }

    // Same contract as place_results, but the results already sit in the
    // register window at `src..src+nr` (a returning frame's values), so they
    // move in place instead of round-tripping through a Vec.
    #[inline(always)]
    fn place_results_from(&mut self, at: usize, src: usize, nr: usize, expected: u8) {
        let fill = if expected == 255 { nr } else { expected as usize };
        if at + fill > self.regs.len() {
            self.regs.resize(at + fill + 64, Value::nil());
        }
        let n = nr.min(fill);
        // dst < src always (results land one slot below the callee's base),
        // so a forward copy is safe; a plain loop beats memmove's call
        // overhead for the 0-2 results most returns carry.
        if src != at {
            for i in 0..n {
                self.regs[at + i] = self.regs[src + i];
            }
        }
        for i in n..fill {
            self.regs[at + i] = Value::nil();
        }
        if expected == 255 {
            if let Some(f) = self.frames.last_mut() {
                f.top = at + nr;
            }
        }
    }

    fn register_stdlib(&mut self) {
        self.set_global_cfn("print", |cx, args| {
            let mut parts = Vec::with_capacity(args.len());
            for &v in args {
                parts.push(tostring_value(cx.vm, v)?);
            }
            emit_line(cx.vm, parts.join("\t"), true);
            Ok(vec![])
        });

        self.set_global_cfn("tostring", |cx, args| {
            let v = args.first().copied().ok_or_else(|| VmError::RuntimeError("tostring: value expected".into()))?;
            if v.is_string() {
                return Ok(vec![v]);
            }
            let s = tostring_value(cx.vm, v)?;
            Ok(vec![cx.vm.alloc_str(&s)])
        });

        self.set_global_cfn("tonumber", |cx, args| {
            let v = args.first().copied().ok_or_else(|| VmError::RuntimeError("tonumber: value expected".into()))?;
            // lua_isnoneornil: an absent or nil base means standard
            // conversion; the base itself is checked first either way.
            let base = match args.get(1) {
                None => None,
                Some(&b) if b.is_nil() => None,
                Some(&b) => Some(int_arg(b, "tonumber")?),
            };
            let Some(base) = base else {
                if v.is_number() {
                    return Ok(vec![v]);
                }
                let Some(s) = crate::heap::arg_str(args, 0) else { return Ok(vec![Value::nil()]) };
                return Ok(vec![str_to_number(cx.vm, s).unwrap_or(Value::nil())]);
            };
            // With an explicit base the argument must be a string, like Lua.
            let s = str_arg(args, 0, "tonumber")?;
            if !(2..=36).contains(&base) {
                return Err(VmError::RuntimeError("tonumber: base out of range".into()));
            }
            // b_str2int: leading space skipped, optional sign, then digits
            // only — any trailing junk fails, and overflow wraps modulo 2^64.
            let mut t = s.trim();
            let neg = t.starts_with('-');
            if neg || t.starts_with('+') {
                t = &t[1..];
            }
            if t.is_empty() {
                return Ok(vec![Value::nil()]);
            }
            let mut n: u64 = 0;
            for c in t.bytes() {
                let d = (c as char).to_digit(base as u32);
                match d {
                    Some(d) => n = n.wrapping_mul(base as u64).wrapping_add(d as u64),
                    None => return Ok(vec![Value::nil()]),
                }
            }
            let n = n as i64;
            Ok(vec![cx.vm.make_int(if neg { n.wrapping_neg() } else { n })])
        });

        self.set_global_cfn("type", |cx, args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            // Lua semantics: integers and floats are both "number";
            // math.type distinguishes them.
            let name = if v.is_number() { "number" } else { v.type_name() };
            Ok(vec![cx.vm.alloc_str(name)])
        });

        self.set_global_cfn("assert", |cx, args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if !v.is_truthy() {
                let msg = args.get(1).copied().unwrap_or(Value::nil());
                // Like Lua: a non-string message is thrown as-is so pcall
                // returns the original value; strings get the line prefix.
                if msg.is_nil() {
                    return Err(VmError::RuntimeError("assertion failed!".into()));
                }
                if msg.is_string() {
                    return Err(VmError::RuntimeError(cx.vm.gc.str(msg).to_owned()));
                }
                return Err(VmError::Thrown(msg));
            }
            Ok(args.to_vec())
        });

        self.set_global_cfn("error", |cx, args| {
            let msg = args.first().copied().unwrap_or(Value::nil());
            let level = match args.get(1) {
                None => 1,
                Some(&v) => int_val(v).map_err(|_| VmError::RuntimeError("bad argument #2 to 'error' (integer expected)".into()))?,
            };
            // Non-string messages are thrown as-is at any level (Lua semantics).
            if !msg.is_string() {
                return Err(VmError::Thrown(msg));
            }
            let s = cx.vm.gc.str(msg).to_owned();
            // level 1 is prefixed by enrich_error_line from the innermost
            // frame; level 0 stays bare; level >= 2 attributes the error to an
            // outer frame here instead.
            if level <= 0 {
                return Err(VmError::Thrown(msg));
            }
            if level >= 2 {
                let vm = &mut *cx.vm;
                let prefixed = match vm.frames.len().checked_sub(level as usize).and_then(|i| vm.frames.get(i)) {
                    Some(frame) => {
                        let proto = frame.proto();
                        let line = proto.lines.get(frame.pc.saturating_sub(1)).copied().unwrap_or(0);
                        format!("{}:{}: {}", chunk_name(proto), line, s)
                    }
                    None => s.clone(),
                };
                return Err(VmError::Thrown(vm.intern(&prefixed)));
            }
            Err(VmError::RuntimeError(s))
        });

        // Test-only hook to exercise the panic-catching path in call_value_isolated
        // without needing a real bug; never registered in a non-test build.
        #[cfg(test)]
        self.set_global_cfn("__debug_panic", |_cx, _args| {
            panic!("__debug_panic: deliberate test panic");
        });

        self.set_global_cfn("pcall", |cx, args| {
            let fn_val = args.first().copied().unwrap_or(Value::nil());
            let call_args = if args.len() > 1 { args[1..].to_vec() } else { vec![] };
            let vm = &mut *cx.vm;
            match vm.call_value_isolated(fn_val, &call_args) {
                Ok(results) => {
                    let mut ret = vec![Value::bool(true)];
                    ret.extend(results);
                    Ok(ret)
                }
                Err(VmError::Thrown(v)) => Ok(vec![Value::bool(false), v]),
                Err(e) => {
                    let msg = vm.intern(&e.to_string());
                    Ok(vec![Value::bool(false), msg])
                }
            }
        });

        self.set_global_cfn("xpcall", |cx, args| {
            let fn_val = args.first().copied().unwrap_or(Value::nil());
            let handler = args.get(1).copied().unwrap_or(Value::nil());
            let call_args = if args.len() > 2 { args[2..].to_vec() } else { vec![] };
            let vm = &mut *cx.vm;
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
                        other => vm.intern(&other.to_string()),
                    };
                    // An error raised before any script ran (a nil target)
                    // has no recorded traceback; the stack is this one.
                    let traceback = match vm.delivered_traceback(0) {
                        Some(tb) => tb.to_owned(),
                        None => vm.stack_traceback(vm.thread_floor),
                    };
                    vm.drop_handled_traces();
                    vm.handlers.push(Handling { caller_segment: vm.saved_frames.len(), thread: vm.resume_chain.len(), traceback });
                    // The handler runs even if it panics/errors itself: its own
                    // failure shouldn't be worse than the error it's handling.
                    let handled = vm.call_value_isolated(handler, &[msg]).unwrap_or_else(|_| vec![msg]);
                    vm.handlers.pop();
                    let mut ret = vec![Value::bool(false)];
                    ret.extend(handled);
                    Ok(ret)
                }
            }
        });

        // Source comes from the host's module loader when one is set
        // (Vm::set_module_loader). Without one this is real filesystem
        // access, like io/os; don't expose that to untrusted scripts.
        self.set_global_cfn("require", |cx, args| {
            let name = str_arg(args, 0, "require")?.to_owned();
            if let Some(&cached) = cx.vm.loaded_modules.get(&name) {
                return Ok(vec![cached]);
            }
            // Required again while its own load is still running: a cycle,
            // which would otherwise load each module over and over until
            // the nesting limit.
            if cx.vm.loading_modules.contains(&name) {
                return Err(VmError::RuntimeError(format!("loop or previous error loading module '{name}'")));
            }
            if name.split('.').any(|seg| seg.is_empty() || seg.contains(['/', '\\'])) {
                return Err(VmError::RuntimeError(format!("require: invalid module name '{name}'")));
            }
            // A failed load caches nothing, so a later require (after a hot
            // fix, say) tries again.
            cx.vm.loading_modules.push(name.clone());
            let loaded = load_module(cx, &name);
            cx.vm.loading_modules.pop();
            let (result, chunk_fn) = loaded?;
            cx.vm.loaded_modules.insert(name, result);
            cx.vm.module_chunks.push(chunk_fn);
            Ok(vec![result])
        });

        // Lua's options minus the tuning ones: "collect" (the default) runs
        // a full collection, "count" is the heap in KB as a float, "step"
        // does one incremental step and says whether it finished a cycle.
        self.set_global_cfn("collectgarbage", |cx, args| {
            let opt = match args.first() {
                Some(v) if !v.is_nil() => str_arg(args, 0, "collectgarbage")?,
                _ => "collect",
            };
            match opt {
                "collect" => {
                    cx.vm.gc_collect();
                    Ok(vec![Value::int(0)])
                }
                "count" => Ok(vec![Value::float(cx.vm.gc.heap_bytes() as f64 / 1024.0)]),
                "step" => Ok(vec![Value::bool(cx.vm.gc_step_once())]),
                other => Err(VmError::RuntimeError(format!("collectgarbage: invalid option '{other}'"))),
            }
        });

        // Compiles a source string into a callable chunk; on a syntax error
        // returns none plus the message, Lua-style. The message names the
        // chunk, as its runtime errors will.
        self.set_global_cfn("load", |cx, args| {
            let src = str_arg(args, 0, "load")?;
            let chunkname = crate::heap::arg_str(args, 1).map(str::to_owned);
            let shown = chunkname.clone().unwrap_or_else(|| DEFAULT_CHUNK_NAME.to_owned());
            match cx.vm.load_proto(src, chunkname) {
                Ok(f) => Ok(vec![f]),
                Err(e) => Ok(vec![Value::nil(), cx.vm.intern(&format!("{shown}: {e}"))]),
            }
        });

        {
            let iter_val = self.make_cfn_val(|cx, args| {
                let tbl = args.first().copied().unwrap_or(Value::nil());
                let idx = args.get(1).copied().unwrap_or(Value::nil());
                if !tbl.is_table() {
                    return Ok(vec![Value::nil()]);
                }
                let i = idx.as_int().or_else(|| idx.as_float().map(|f| f as i64)).unwrap_or(0);
                let next_i = i + 1;
                let v = cx.vm.gc.table(tbl).raw_get(Value::int(next_i));
                if v.is_nil() { Ok(vec![Value::nil()]) } else { Ok(vec![Value::int(next_i), v]) }
            });
            let ipairs = self.make_cfn_closure(&[iter_val], |_cx, args, up| {
                let t = args.first().copied().unwrap_or(Value::nil());
                if !t.is_table() {
                    return Err(VmError::RuntimeError("ipairs: expected table".into()));
                }
                Ok(vec![up[0], t, Value::int(0)])
            });
            self.set_global("ipairs", ipairs);
        }

        {
            let next_val = self.make_cfn_val(|cx, args| {
                let tbl = args.first().copied().unwrap_or(Value::nil());
                let key = args.get(1).copied().unwrap_or(Value::nil());
                if !tbl.is_table() {
                    return Err(VmError::RuntimeError("next: expected table".into()));
                }
                Ok(match cx.vm.table_next(tbl, key) {
                    Some((k, v)) => vec![k, v],
                    None => vec![Value::nil()],
                })
            });
            let k_next = self.intern("next");
            self.globals.raw_set(&mut self.gc, k_next, next_val);
            let pairs = self.make_cfn_closure(&[next_val], |_cx, args, up| {
                let t = args.first().copied().unwrap_or(Value::nil());
                if !t.is_table() {
                    return Err(VmError::RuntimeError("pairs: expected table".into()));
                }
                Ok(vec![up[0], t, Value::nil()])
            });
            self.set_global("pairs", pairs);
        }

        self.set_global_cfn("unpack", |cx, args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() {
                return Err(VmError::RuntimeError("unpack: table expected".into()));
            }
            let n = cx.vm.gc.table(t).length();
            let i = match args.get(1) {
                None => 1,
                Some(&v) => int_arg(v, "unpack")?,
            };
            let j = match args.get(2) {
                None => n,
                Some(&v) => int_arg(v, "unpack")?,
            };
            if checked_range_len(i, j).is_none() {
                return Err(VmError::RuntimeError("unpack: range too large".into()));
            }
            Ok((i..=j)
                .map(|k| {
                    let k = cx.vm.make_int(k);
                    cx.vm.gc.table(t).raw_get(k)
                })
                .collect())
        });

        self.set_global_cfn("select", |_cx, args| {
            let sel = args.first().copied().unwrap_or(Value::nil());
            let n = args.len() as i64 - 1;
            if crate::heap::arg_str(args, 0) == Some("#") {
                return Ok(vec![Value::int(n)]);
            }
            let i = sel.as_int().ok_or_else(|| VmError::RuntimeError("select: index expected".into()))?;
            let start = if i < 0 { n + i } else { i - 1 };
            if i == 0 || start < 0 {
                return Err(VmError::RuntimeError("select: index out of range".into()));
            }
            Ok(args.get(start as usize + 1..).map(|s| s.to_vec()).unwrap_or_default())
        });

        self.set_global_cfn("setmetatable", |cx, args| {
            let tbl = args.first().copied().unwrap_or(Value::nil());
            let mt = args.get(1).copied().unwrap_or(Value::nil());
            if !tbl.is_table() {
                return Err(VmError::RuntimeError("setmetatable: first arg must be a table".into()));
            }
            if !mt.is_table() && !mt.is_nil() {
                return Err(VmError::RuntimeError("setmetatable: second arg must be a table or none".into()));
            }
            cx.vm.set_metatable(tbl, mt);
            Ok(vec![tbl])
        });

        self.set_global_cfn("getmetatable", |cx, args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if !v.is_table() {
                return Ok(vec![Value::nil()]);
            }
            let gc = &cx.vm.gc;
            let t = gc.table(v);
            let Some(mt) = gc.metatable(t) else { return Ok(vec![Value::nil()]) };
            if let Some(guard) = mt.get_str("__metatable") {
                return Ok(vec![guard]);
            }
            Ok(vec![Value::table(t.metatable.unwrap() as *mut u8)])
        });

        self.set_global_cfn("rawget", |cx, args| {
            let tbl = args.first().copied().unwrap_or(Value::nil());
            let key = args.get(1).copied().unwrap_or(Value::nil());
            if !tbl.is_table() {
                return Err(VmError::RuntimeError("rawget: expected table".into()));
            }
            Ok(vec![cx.vm.gc.table(tbl).raw_get(key)])
        });

        self.set_global_cfn("rawset", |cx, args| {
            let tbl = args.first().copied().unwrap_or(Value::nil());
            let key = args.get(1).copied().unwrap_or(Value::nil());
            let val = args.get(2).copied().unwrap_or(Value::nil());
            if !tbl.is_table() {
                return Err(VmError::RuntimeError("rawset: expected table".into()));
            }
            cx.vm.raw_store(tbl, key, val)?;
            Ok(vec![tbl])
        });

        self.set_global_cfn("rawequal", |cx, args| {
            let a = args.first().copied().unwrap_or(Value::nil());
            let b = args.get(1).copied().unwrap_or(Value::nil());
            Ok(vec![Value::bool(values_equal(&cx.vm.gc, a, b))])
        });

        self.set_global_cfn("yield", |_cx, args| Err(VmError::Yield(args.to_vec())));

        let co_table_ptr = alloc_table_raw();
        self.gc.register(co_table_ptr);

        let create_val = self.make_cfn_val(|cx, args| {
            let fn_val = args.first().copied().unwrap_or(Value::nil());
            cx.vm.new_coroutine(fn_val, "coroutine.create").map(|co| vec![co])
        });

        let resume_val = self.make_cfn_val(|cx, args| {
            let co_val = args.first().copied().unwrap_or(Value::nil());
            if !co_val.is_coroutine() {
                return Err(VmError::RuntimeError("coroutine.resume: expected coroutine".into()));
            }
            let vm = &mut *cx.vm;
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
        });

        let status_val = self.make_cfn_val(|cx, args| {
            let co_val = args.first().copied().unwrap_or(Value::nil());
            if !co_val.is_coroutine() {
                return Err(VmError::RuntimeError("coroutine.status: expected coroutine".into()));
            }
            let s = match cx.vm.gc.coroutine(co_val).status {
                CoStatus::Suspended => "suspended",
                CoStatus::Running => "running",
                CoStatus::Dead => "dead",
            };
            Ok(vec![cx.vm.alloc_str(s)])
        });

        let wrap_val = self.make_cfn_val(|cx, args| {
            let fn_val = args.first().copied().unwrap_or(Value::nil());
            let co_val = cx.vm.new_coroutine(fn_val, "coroutine.wrap")?;
            let wrapper = cx.vm.make_cfn_closure(&[co_val], |cx, wargs, up| cx.vm.resume_coroutine(up[0], wargs));
            Ok(vec![wrapper])
        });

        let isyieldable_val = self.make_cfn_val(|cx, _args| Ok(vec![Value::bool(!cx.vm.resume_chain.is_empty())]));

        let ct = Value::table(co_table_ptr);
        let k_create = self.intern("create");
        let k_resume = self.intern("resume");
        let k_status = self.intern("status");
        let k_wrap = self.intern("wrap");
        let k_isyieldable = self.intern("isyieldable");
        self.gc.raw_set(ct, k_create, create_val);
        self.gc.raw_set(ct, k_resume, resume_val);
        self.gc.raw_set(ct, k_status, status_val);
        self.gc.raw_set(ct, k_wrap, wrap_val);
        self.gc.raw_set(ct, k_isyieldable, isyieldable_val);

        let co_table_val = Value::table(co_table_ptr);
        let k_coroutine = self.intern("coroutine");
        self.globals.raw_set(&mut self.gc, k_coroutine, co_table_val);

        let str_table_ptr = alloc_table_raw();
        self.gc.register(str_table_ptr);

        let v_str_len = self.make_cfn_val(|_cx, args| Ok(vec![Value::int(str_arg(args, 0, "string.len")?.len() as i64)]));
        let v_str_sub = self.make_cfn_val(|cx, args| {
            let s = str_arg(args, 0, "string.sub")?;
            let len = s.len();
            let i = opt_int_arg(args, 1, "string.sub", 1)?;
            let j = opt_int_arg(args, 2, "string.sub", -1)?;
            let start = lua_str_start(len, i);
            let end = lua_str_end(len, j);
            if start >= end {
                return Ok(vec![cx.vm.alloc_str("")]);
            }
            Ok(vec![cx.vm.alloc_str(&String::from_utf8_lossy(&s.as_bytes()[start..end]))])
        });
        let v_str_rep = self.make_cfn_val(|cx, args| {
            let s = str_arg(args, 0, "string.rep")?;
            let n = int_arg(args.get(1).copied().unwrap_or(Value::nil()), "string.rep")?;
            let sep = match args.get(2) {
                None => String::new(),
                Some(&v) if v.is_string() => cx.vm.gc.str(v).to_owned(),
                Some(&v) if v.is_number() => coerce_to_concat_str(v),
                Some(_) => return Err(VmError::RuntimeError("string.rep: string expected".into())),
            };
            if n <= 0 {
                return Ok(vec![cx.vm.alloc_str("")]);
            }
            let n = n as usize;
            let total = n.saturating_mul(s.len() + sep.len());
            if total > MAX_ALLOC_LEN {
                return Err(VmError::RuntimeError("string.rep: result too large".into()));
            }
            // Write the result in place: a Vec of n &str parts would cost
            // 16 bytes per repeat on top of the join's own buffer.
            let mut buf = Vec::with_capacity(total + 1);
            for i in 0..n {
                if i > 0 {
                    buf.extend_from_slice(sep.as_bytes());
                }
                buf.extend_from_slice(s.as_bytes());
            }
            let len = buf.len();
            let raw = alloc_string_owned(buf, len);
            cx.vm.gc.register(raw);
            Ok(vec![Value::string(raw)])
        });
        let v_str_upper = self.make_cfn_val(|cx, args| {
            // C-locale toupper: ASCII only, like Lua. Byte-wise mapping keeps
            // multibyte UTF-8 sequences untouched.
            let bytes: Vec<u8> = str_arg(args, 0, "string.upper")?.bytes().map(|b| b.to_ascii_uppercase()).collect();
            Ok(vec![cx.vm.alloc_str(&String::from_utf8_lossy(&bytes))])
        });
        let v_str_lower = self.make_cfn_val(|cx, args| {
            let bytes: Vec<u8> = str_arg(args, 0, "string.lower")?.bytes().map(|b| b.to_ascii_lowercase()).collect();
            Ok(vec![cx.vm.alloc_str(&String::from_utf8_lossy(&bytes))])
        });
        let v_str_reverse = self.make_cfn_val(|cx, args| {
            // Byte reversal like Lua; slicing mid-character re-validates.
            let mut bytes: Vec<u8> = str_arg(args, 0, "string.reverse")?.as_bytes().to_vec();
            bytes.reverse();
            Ok(vec![cx.vm.alloc_str(&String::from_utf8_lossy(&bytes))])
        });
        let v_str_byte = self.make_cfn_val(|_cx, args| {
            let s = str_arg(args, 0, "string.byte")?;
            let i = opt_int_arg(args, 1, "string.byte", 1)?;
            let j = opt_int_arg(args, 2, "string.byte", i)?;
            let start = lua_str_start(s.len(), i);
            let end = lua_str_end(s.len(), j).min(s.len());
            if end.saturating_sub(start) > MAX_ALLOC_LEN / 8 {
                return Err(VmError::RuntimeError("string.byte: string slice too long".into()));
            }
            let bytes = s.as_bytes();
            Ok((start..end).map(|k| Value::int(bytes[k] as i64)).collect())
        });
        let v_str_char = self.make_cfn_val(|cx, args| {
            let mut s = String::with_capacity(args.len());
            for &v in args {
                let n = int_arg(v, "string.char")?;
                let b = u8::try_from(n).map_err(|_| VmError::RuntimeError("string.char: value out of range".into()))?;
                s.push(b as char);
            }
            Ok(vec![cx.vm.alloc_str(&s)])
        });
        let v_str_find = self.make_cfn_val(|cx, args| {
            let s = str_arg(args, 0, "string.find")?;
            let pat = str_arg(args, 1, "string.find")?;
            let init = match args.get(2) {
                None => 1,
                Some(&v) => int_arg(v, "string.find")?,
            };
            let plain = args.get(3).map(|&v| v.is_truthy()).unwrap_or(false);
            // Lua: init past len+1 finds nothing (even an empty pattern).
            if init > s.len() as i64 + 1 {
                return Ok(vec![Value::nil()]);
            }
            let start = lua_str_start(s.len(), init);
            if plain {
                let hay = &s.as_bytes()[start..];
                let found = if pat.is_empty() { Some(0) } else { hay.windows(pat.len()).position(|w| w == pat.as_bytes()) };
                return match found {
                    None => Ok(vec![Value::nil()]),
                    Some(pos) => Ok(vec![Value::int((start + pos + 1) as i64), Value::int((start + pos + pat.len()) as i64)]),
                };
            }
            match pattern::find_from(s.as_bytes(), pat.as_bytes(), start) {
                Ok(Some(m)) => {
                    let mut ret = vec![Value::int(m.start as i64 + 1), Value::int(m.end as i64)];
                    ret.extend(pattern_captures(cx.vm, s.as_bytes(), &m));
                    Ok(ret)
                }
                Ok(None) => Ok(vec![Value::nil()]),
                Err(e) => Err(VmError::RuntimeError(format!("string.find: {e}"))),
            }
        });
        let v_str_match = self.make_cfn_val(|cx, args| {
            let s = str_arg(args, 0, "string.match")?;
            let pat = str_arg(args, 1, "string.match")?;
            let init = match args.get(2) {
                None => 1,
                Some(&v) => int_arg(v, "string.match")?,
            };
            if init > s.len() as i64 + 1 {
                return Ok(vec![Value::nil()]);
            }
            let start = lua_str_start(s.len(), init);
            match pattern::find_from(s.as_bytes(), pat.as_bytes(), start) {
                Ok(Some(m)) => {
                    if m.captures.is_empty() {
                        Ok(vec![cx.vm.alloc_str(&String::from_utf8_lossy(&s.as_bytes()[m.start..m.end]))])
                    } else {
                        Ok(pattern_captures(cx.vm, s.as_bytes(), &m))
                    }
                }
                Ok(None) => Ok(vec![Value::nil()]),
                Err(e) => Err(VmError::RuntimeError(format!("string.match: {e}"))),
            }
        });
        let v_str_gmatch = self.make_cfn_val(|cx, args| {
            let s = str_arg(args, 0, "string.gmatch")?.to_owned();
            let pat = str_arg(args, 1, "string.gmatch")?.to_owned();
            let init = match args.get(2) {
                None => 1,
                Some(&v) => int_arg(v, "string.gmatch")?,
            };
            let pos = Cell::new(if init > s.len() as i64 + 1 { usize::MAX } else { lua_str_start(s.len(), init) });
            let iter_val = cx.vm.make_cfn_val(move |cx, _args| {
                let sb = s.as_bytes();
                if pos.get() > sb.len() {
                    return Ok(vec![Value::nil()]);
                }
                match pattern::find_from(sb, pat.as_bytes(), pos.get()) {
                    Ok(Some(m)) => {
                        // Empty match: step by one byte past it so the next
                        // call makes forward progress instead of looping forever.
                        // An anchored pattern only ever matches once.
                        pos.set(if pat.starts_with('^') {
                            sb.len() + 1
                        } else if m.end > m.start {
                            m.end
                        } else {
                            m.end + 1
                        });
                        if m.captures.is_empty() {
                            Ok(vec![cx.vm.alloc_str(&String::from_utf8_lossy(&sb[m.start..m.end]))])
                        } else {
                            Ok(pattern_captures(cx.vm, sb, &m))
                        }
                    }
                    Ok(None) => {
                        pos.set(sb.len() + 1);
                        Ok(vec![Value::nil()])
                    }
                    Err(e) => Err(VmError::RuntimeError(format!("string.gmatch: {e}"))),
                }
            });
            Ok(vec![iter_val])
        });
        let v_str_gsub = self.make_cfn_val(|cx, args| {
            let s = str_arg(args, 0, "string.gsub")?;
            let pat = str_arg(args, 1, "string.gsub")?;
            let repl = args.get(2).copied().unwrap_or(Value::nil());
            let max_n = match args.get(3) {
                None => i64::MAX,
                Some(&v) => int_arg(v, "string.gsub")?,
            };
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
                match apply_gsub_repl(cx.vm, repl, sb, &m, whole)? {
                    Some(bytes) => out.extend_from_slice(&bytes),
                    None => out.extend_from_slice(whole),
                }
                count += 1;
                pos = if m.end > m.start {
                    m.end
                } else {
                    if m.end < sb.len() {
                        out.push(sb[m.end]);
                    }
                    m.end + 1
                };
                if pat.starts_with('^') {
                    break;
                }
            }
            if pos <= sb.len() {
                out.extend_from_slice(&sb[pos..]);
            }
            Ok(vec![cx.vm.alloc_str(&String::from_utf8_lossy(&out)), Value::int(count)])
        });
        let v_str_format = self.make_cfn_val(|cx, args| {
            let fmt = str_arg(args, 0, "string.format")?;
            string_format(cx.vm, fmt, if args.len() > 1 { &args[1..] } else { &[] })
        });
        // Packed output is arbitrary binary, so it comes back as a bytes
        // value — strings stay UTF-8 text. unpack accepts bytes or a string
        // and returns bytes for its string fields.
        let v_str_pack = self.make_cfn_val(|cx, args| {
            let fmt = str_arg(args, 0, "string.pack")?;
            let mut pvals = Vec::new();
            for &v in args.get(1..).unwrap_or(&[]) {
                let pv = if v.is_string() {
                    pack::PackValue::Str(cx.vm.gc.str(v).as_bytes().to_vec())
                } else if v.is_bytes() {
                    pack::PackValue::Str(cx.vm.gc.bytes(v).data.to_vec())
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
            Ok(vec![cx.vm.alloc_bytes(bytes)])
        });
        let v_str_unpack = self.make_cfn_val(|cx, args| {
            let fmt = str_arg(args, 0, "string.unpack")?;
            let init = match args.get(2) {
                None => 1,
                Some(&v) => int_arg(v, "string.unpack")?,
            };
            let gc = &cx.vm.gc;
            let data: &[u8] = match args.get(1) {
                Some(&v) if v.is_string() => gc.str(v).as_bytes(),
                Some(&v) if v.is_bytes() => &gc.bytes(v).data,
                _ => return Err(VmError::RuntimeError("string.unpack: bytes or string expected".into())),
            };
            let (vals, end_pos) = pack::unpack(fmt, data, init).map_err(VmError::RuntimeError)?;
            let mut out: Vec<Value> = vals
                .into_iter()
                .map(|pv| match pv {
                    pack::PackValue::Int(n) => cx.vm.make_int(n),
                    pack::PackValue::Float(f) => Value::float(f),
                    pack::PackValue::Str(s) => cx.vm.alloc_bytes(s),
                })
                .collect();
            out.push(cx.vm.make_int((end_pos + 1) as i64));
            Ok(out)
        });
        let v_str_packsize = self.make_cfn_val(|cx, args| {
            let fmt = str_arg(args, 0, "string.packsize")?;
            let n = pack::packsize(fmt).map_err(VmError::RuntimeError)?;
            Ok(vec![cx.vm.make_int(n as i64)])
        });

        {
            let st = Value::table(str_table_ptr);
            let k = self.intern("len");
            self.gc.raw_set(st, k, v_str_len);
            let k = self.intern("sub");
            self.gc.raw_set(st, k, v_str_sub);
            let k = self.intern("rep");
            self.gc.raw_set(st, k, v_str_rep);
            let k = self.intern("upper");
            self.gc.raw_set(st, k, v_str_upper);
            let k = self.intern("lower");
            self.gc.raw_set(st, k, v_str_lower);
            let k = self.intern("reverse");
            self.gc.raw_set(st, k, v_str_reverse);
            let k = self.intern("byte");
            self.gc.raw_set(st, k, v_str_byte);
            let k = self.intern("char");
            self.gc.raw_set(st, k, v_str_char);
            let k = self.intern("find");
            self.gc.raw_set(st, k, v_str_find);
            let k = self.intern("match");
            self.gc.raw_set(st, k, v_str_match);
            let k = self.intern("gmatch");
            self.gc.raw_set(st, k, v_str_gmatch);
            let k = self.intern("gsub");
            self.gc.raw_set(st, k, v_str_gsub);
            let k = self.intern("format");
            self.gc.raw_set(st, k, v_str_format);
            let k = self.intern("pack");
            self.gc.raw_set(st, k, v_str_pack);
            let k = self.intern("unpack");
            self.gc.raw_set(st, k, v_str_unpack);
            let k = self.intern("packsize");
            self.gc.raw_set(st, k, v_str_packsize);
        }
        let str_table_val = Value::table(str_table_ptr);
        self.string_lib = str_table_val;
        let k_string = self.intern("string");
        self.globals.raw_set(&mut self.gc, k_string, str_table_val);

        let bytes_table_ptr = alloc_table_raw();
        self.gc.register(bytes_table_ptr);

        let v_bytes_new = self.make_cfn_val(|cx, args| {
            let n = match args.first() {
                None => 0,
                Some(&v) => int_arg(v, "bytes.new")?,
            };
            if !(0..=MAX_ALLOC_LEN as i64).contains(&n) {
                return Err(VmError::RuntimeError("bytes.new: invalid size".into()));
            }
            Ok(vec![cx.vm.alloc_bytes_zeroed(n as usize)])
        });
        let v_bytes_from = self.make_cfn_val(|cx, args| match args.first() {
            Some(&v) if v.is_string() => {
                let data: Box<[u8]> = cx.vm.gc.str(v).as_bytes().into();
                Ok(vec![cx.vm.alloc_bytes(data)])
            }
            Some(&v) if v.is_bytes() => {
                let data = cx.vm.gc.bytes(v).data.clone();
                Ok(vec![cx.vm.alloc_bytes(data)])
            }
            _ => Err(VmError::RuntimeError("bytes.from: string expected".into())),
        });
        let v_bytes_tostring = self.make_cfn_val(|cx, args| match args.first() {
            Some(&v) if v.is_bytes() => {
                let b = cx.vm.gc.bytes(v);
                match std::str::from_utf8(&b.data[..b.len]).map(str::to_owned) {
                    Ok(s) => Ok(vec![cx.vm.alloc_str(&s)]),
                    Err(_) => Err(VmError::RuntimeError("bytes.tostring: not valid UTF-8".into())),
                }
            }
            _ => Err(VmError::RuntimeError("bytes.tostring: bytes expected".into())),
        });
        let v_bytes_concat = self.make_cfn_val(|cx, args| {
            let mut out = Vec::new();
            for &v in args {
                if v.is_bytes() {
                    out.extend_from_slice(&cx.vm.gc.bytes(v).data);
                } else if v.is_string() {
                    out.extend_from_slice(cx.vm.gc.str(v).as_bytes());
                } else {
                    return Err(VmError::RuntimeError("bytes.concat: bytes or string expected".into()));
                }
            }
            Ok(vec![cx.vm.alloc_bytes(out)])
        });
        let v_bytes_len = self.make_cfn_val(|cx, args| match args.first() {
            Some(&v) if v.is_bytes() => {
                let n = cx.vm.gc.bytes(v).len;
                Ok(vec![Value::int(n as i64)])
            }
            _ => Err(VmError::RuntimeError("bytes.len: bytes expected".into())),
        });

        {
            let bt = Value::table(bytes_table_ptr);
            let k = self.intern("new");
            self.gc.raw_set(bt, k, v_bytes_new);
            let k = self.intern("from");
            self.gc.raw_set(bt, k, v_bytes_from);
            let k = self.intern("tostring");
            self.gc.raw_set(bt, k, v_bytes_tostring);
            let k = self.intern("concat");
            self.gc.raw_set(bt, k, v_bytes_concat);
            let k = self.intern("len");
            self.gc.raw_set(bt, k, v_bytes_len);
        }
        self.bytes_lib = Value::table(bytes_table_ptr);
        let k_bytes = self.intern("bytes");
        self.globals.raw_set(&mut self.gc, k_bytes, self.bytes_lib);

        let math_table_ptr = alloc_table_raw();
        self.gc.register(math_table_ptr);

        let v_math_floor = self.make_cfn_val(|cx, args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() {
                return Ok(vec![v]);
            }
            let f = num_arg(v, "math.floor")?;
            Ok(vec![float_to_int_or_float(cx.vm, f.floor())])
        });
        let v_math_ceil = self.make_cfn_val(|cx, args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() {
                return Ok(vec![v]);
            }
            let f = num_arg(v, "math.ceil")?;
            Ok(vec![float_to_int_or_float(cx.vm, f.ceil())])
        });
        let v_math_abs = self.make_cfn_val(|cx, args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() {
                return Ok(vec![cx.vm.make_int(v.as_int().unwrap().wrapping_abs())]);
            }
            let f = num_arg(v, "math.abs")?;
            Ok(vec![Value::float(f.abs())])
        });
        let v_math_sqrt = self.make_cfn_val(|_cx, args| {
            let f = num_arg(args.first().copied().unwrap_or(Value::nil()), "math.sqrt")?;
            Ok(vec![Value::float(f.sqrt())])
        });
        let v_math_max = self.make_cfn_val(|cx, args| {
            if args.is_empty() {
                return Err(VmError::RuntimeError("math.max: at least one arg required".into()));
            }
            num_value_arg(cx.vm, args[0], "math.max")?;
            // The winner is the original argument, like Lua's lua_pushvalue —
            // a numeric string stays a string.
            let mut best = args[0];
            for &v in &args[1..] {
                let bv = num_value_arg(cx.vm, best, "math.max")?;
                let vv = num_value_arg(cx.vm, v, "math.max")?;
                if num_cmp(bv, vv).is_some_and(|o| o.is_lt()) {
                    best = v;
                }
            }
            Ok(vec![best])
        });
        let v_math_min = self.make_cfn_val(|cx, args| {
            if args.is_empty() {
                return Err(VmError::RuntimeError("math.min: at least one arg required".into()));
            }
            num_value_arg(cx.vm, args[0], "math.min")?;
            let mut best = args[0];
            for &v in &args[1..] {
                let bv = num_value_arg(cx.vm, best, "math.min")?;
                let vv = num_value_arg(cx.vm, v, "math.min")?;
                if num_cmp(bv, vv).is_some_and(|o| o.is_gt()) {
                    best = v;
                }
            }
            Ok(vec![best])
        });
        let v_math_sin = self.make_cfn_val(|_cx, args| Ok(vec![Value::float(num_arg(args.first().copied().unwrap_or(Value::nil()), "math.sin")?.sin())]));
        let v_math_cos = self.make_cfn_val(|_cx, args| Ok(vec![Value::float(num_arg(args.first().copied().unwrap_or(Value::nil()), "math.cos")?.cos())]));
        let v_math_tan = self.make_cfn_val(|_cx, args| Ok(vec![Value::float(num_arg(args.first().copied().unwrap_or(Value::nil()), "math.tan")?.tan())]));
        let v_math_exp = self.make_cfn_val(|_cx, args| Ok(vec![Value::float(num_arg(args.first().copied().unwrap_or(Value::nil()), "math.exp")?.exp())]));
        let v_math_log = self.make_cfn_val(|_cx, args| {
            let x = num_arg(args.first().copied().unwrap_or(Value::nil()), "math.log")?;
            let result = match args.get(1) {
                Some(&b) if !b.is_nil() => x.log(num_arg(b, "math.log")?),
                _ => x.ln(),
            };
            Ok(vec![Value::float(result)])
        });
        let v_math_modf = self.make_cfn_val(|cx, args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() {
                return Ok(vec![v, Value::float(0.0)]);
            }
            let f = num_arg(v, "math.modf")?;
            // Lua returns the integral part as an integer when it fits.
            Ok(vec![float_to_int_or_float(cx.vm, f.trunc()), Value::float(f.fract())])
        });
        let v_math_type = self.make_cfn_val(|cx, args| {
            let v = args.first().copied().unwrap_or(Value::nil());
            if v.is_int_like() {
                return Ok(vec![cx.vm.alloc_str("integer")]);
            }
            if v.is_float() {
                return Ok(vec![cx.vm.alloc_str("float")]);
            }
            Ok(vec![Value::nil()])
        });
        let v_math_tointeger = self.make_cfn_val(|cx, args| {
            let v = args.first().copied().ok_or_else(|| VmError::RuntimeError("math.tointeger: value expected".into()))?;
            // lua_tointegerx coerces numeric strings too.
            let v = if let Some(s) = crate::heap::arg_str(args, 0) {
                match str_to_number(cx.vm, s) {
                    Some(nv) => nv,
                    None => return Ok(vec![Value::nil()]),
                }
            } else {
                v
            };
            if v.is_int_like() {
                return Ok(vec![v]);
            }
            if v.is_float() {
                let f = v.as_float().unwrap();
                // Bounds first: `f as i64` saturates, and i64::MAX as f64 is
                // exactly 2^63, so the round-trip check alone would wrongly
                // accept 2^63 (and any larger float) as i64::MAX.
                if f >= -9223372036854775808.0 && f < 9223372036854775808.0 {
                    let i = f as i64;
                    if i as f64 == f {
                        return Ok(vec![cx.vm.make_int(i)]);
                    }
                }
            }
            Ok(vec![Value::nil()])
        });
        let v_math_random = self.make_cfn_val(|cx, args| {
            if args.len() > 2 {
                return Err(VmError::RuntimeError("math.random: wrong number of arguments".into()));
            }
            let r = splitmix64(cx.vm.rng);
            cx.vm.rng = r;
            match args.len() {
                0 => Ok(vec![Value::float((r >> 11) as f64 * (1.0_f64 / (1u64 << 53) as f64))]),
                1 => {
                    let m = int_arg(args[0], "math.random")?;
                    // m == 0 means "any bits", like Lua's project.
                    if m == 0 {
                        return Ok(vec![cx.vm.make_int(r as i64)]);
                    }
                    if m < 0 {
                        return Err(VmError::RuntimeError("math.random: interval is empty".into()));
                    }
                    Ok(vec![cx.vm.make_int(1 + (r % m as u64) as i64)])
                }
                _ => {
                    let lo = int_arg(args[0], "math.random")?;
                    let hi = int_arg(args[1], "math.random")?;
                    if lo > hi {
                        return Err(VmError::RuntimeError("math.random: interval is empty".into()));
                    }
                    // The full i64 span can't be a u64 range; Lua treats it
                    // as "any bits" rather than erroring.
                    if lo == i64::MIN && hi == i64::MAX {
                        return Ok(vec![cx.vm.make_int(r as i64)]);
                    }
                    let range = hi as i128 - lo as i128 + 1;
                    Ok(vec![cx.vm.make_int((lo as i128 + (r % range as u64) as i128) as i64)])
                }
            }
        });
        let v_math_randomseed = self.make_cfn_val(|cx, args| {
            let seed = match args.first() {
                None => 0,
                Some(&v) => int_arg(v, "math.randomseed")?,
            };
            cx.vm.rng = seed as u64;
            Ok(vec![])
        });
        let v_math_fmod = self.make_cfn_val(|cx, args| {
            let a = args.first().copied().unwrap_or(Value::nil());
            let b = args.get(1).copied().unwrap_or(Value::nil());
            if a.is_int_like() && b.is_int_like() {
                let (x, y) = (a.as_int().unwrap(), b.as_int().unwrap());
                if y == 0 {
                    return Err(VmError::RuntimeError("math.fmod: zero divisor".into()));
                }
                return Ok(vec![cx.vm.make_int(x.wrapping_rem(y))]);
            }
            let x = num_arg(a, "math.fmod")?;
            let y = num_arg(b, "math.fmod")?;
            Ok(vec![Value::float(x % y)])
        });
        let v_math_ult = self.make_cfn_val(|_cx, args| {
            let a = int_arg(args.first().copied().unwrap_or(Value::nil()), "math.ult")?;
            let b = int_arg(args.get(1).copied().unwrap_or(Value::nil()), "math.ult")?;
            Ok(vec![Value::bool((a as u64) < (b as u64))])
        });

        {
            let mt = Value::table(math_table_ptr);
            let k = self.intern("pi");
            self.gc.raw_set(mt, k, Value::float(std::f64::consts::PI));
            let k = self.intern("huge");
            self.gc.raw_set(mt, k, Value::float(f64::INFINITY));
            let k = self.intern("maxinteger");
            let v = self.make_int(i64::MAX);
            self.gc.raw_set(mt, k, v);
            let k = self.intern("mininteger");
            let v = self.make_int(i64::MIN);
            self.gc.raw_set(mt, k, v);
            let k = self.intern("floor");
            self.gc.raw_set(mt, k, v_math_floor);
            let k = self.intern("ceil");
            self.gc.raw_set(mt, k, v_math_ceil);
            let k = self.intern("abs");
            self.gc.raw_set(mt, k, v_math_abs);
            let k = self.intern("sqrt");
            self.gc.raw_set(mt, k, v_math_sqrt);
            let k = self.intern("max");
            self.gc.raw_set(mt, k, v_math_max);
            let k = self.intern("min");
            self.gc.raw_set(mt, k, v_math_min);
            let k = self.intern("sin");
            self.gc.raw_set(mt, k, v_math_sin);
            let k = self.intern("cos");
            self.gc.raw_set(mt, k, v_math_cos);
            let k = self.intern("tan");
            self.gc.raw_set(mt, k, v_math_tan);
            let k = self.intern("exp");
            self.gc.raw_set(mt, k, v_math_exp);
            let k = self.intern("log");
            self.gc.raw_set(mt, k, v_math_log);
            let k = self.intern("modf");
            self.gc.raw_set(mt, k, v_math_modf);
            let k = self.intern("type");
            self.gc.raw_set(mt, k, v_math_type);
            let k = self.intern("tointeger");
            self.gc.raw_set(mt, k, v_math_tointeger);
            let k = self.intern("random");
            self.gc.raw_set(mt, k, v_math_random);
            let k = self.intern("randomseed");
            self.gc.raw_set(mt, k, v_math_randomseed);
            let k = self.intern("fmod");
            self.gc.raw_set(mt, k, v_math_fmod);
            let k = self.intern("ult");
            self.gc.raw_set(mt, k, v_math_ult);
        }
        let k_math = self.intern("math");
        self.globals.raw_set(&mut self.gc, k_math, Value::table(math_table_ptr));

        let tbl_table_ptr = alloc_table_raw();
        self.gc.register(tbl_table_ptr);

        let v_tbl_insert = self.make_cfn_val(|cx, args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() {
                return Err(VmError::RuntimeError("table.insert: table expected".into()));
            }
            let gc = &mut cx.vm.gc;
            match args.len() {
                2 => {
                    let n = gc.table(t).length() + 1;
                    gc.raw_set(t, Value::int(n), args[1]);
                }
                3 => {
                    let pos = int_arg(args[1], "table.insert")?;
                    let n = gc.table(t).length();
                    if pos < 1 || pos > n + 1 {
                        return Err(VmError::RuntimeError("table.insert: position out of bounds".into()));
                    }
                    for i in (pos..=n).rev() {
                        let elem = gc.table(t).raw_get(Value::int(i));
                        gc.raw_set(t, Value::int(i + 1), elem);
                    }
                    gc.raw_set(t, Value::int(pos), args[2]);
                }
                _ => return Err(VmError::RuntimeError("table.insert: wrong number of args".into())),
            }
            Ok(vec![])
        });
        let v_tbl_remove = self.make_cfn_val(|cx, args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() {
                return Err(VmError::RuntimeError("table.remove: table expected".into()));
            }
            let gc = &mut cx.vm.gc;
            let n = gc.table(t).length();
            let pos = opt_int_arg(args, 1, "table.remove", n)?;
            // Lua: pos == n removes the last element unconditionally; any
            // other position must satisfy 1 <= pos <= n+1.
            if pos != n && (pos < 1 || pos > n + 1) {
                return Err(VmError::RuntimeError("table.remove: position out of bounds".into()));
            }
            if pos < 1 || pos > n {
                return Ok(vec![Value::nil()]);
            }
            let removed = gc.table(t).raw_get(Value::int(pos));
            for i in pos..n {
                let next = gc.table(t).raw_get(Value::int(i + 1));
                gc.raw_set(t, Value::int(i), next);
            }
            gc.table_mut(t).array.truncate((n - 1) as usize);
            Ok(vec![removed])
        });
        let v_tbl_concat = self.make_cfn_val(|cx, args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() {
                return Err(VmError::RuntimeError("table.concat: table expected".into()));
            }
            let sep = match args.get(1) {
                None => String::new(),
                Some(&v) if v.is_nil() => String::new(),
                Some(&v) if v.is_string() => cx.vm.gc.str(v).to_owned(),
                Some(&v) if v.is_number() => coerce_to_concat_str(v),
                Some(_) => return Err(VmError::RuntimeError("table.concat: string expected".into())),
            };
            let n = cx.vm.gc.table(t).length();
            let i = opt_int_arg(args, 2, "table.concat", 1)?;
            let j = opt_int_arg(args, 3, "table.concat", n)?;
            if checked_range_len(i, j).is_none() {
                return Err(VmError::RuntimeError("table.concat: range too large".into()));
            }
            let mut parts: Vec<String> = Vec::new();
            for k in i..=j {
                let k = cx.vm.make_int(k);
                let v = cx.vm.gc.table(t).raw_get(k);
                let s = if v.is_string() {
                    cx.vm.gc.str(v).to_owned()
                } else if v.is_number() {
                    coerce_to_concat_str(v)
                } else {
                    return Err(VmError::RuntimeError("table.concat: invalid value (not string or number)".into()));
                };
                parts.push(s);
            }
            Ok(vec![cx.vm.alloc_str(&parts.join(&sep))])
        });
        // Sorts a copy: the comparator can error or trigger a collection, and
        // the table must keep both its contents and its roots either way.
        let v_tbl_sort = self.make_cfn_val(|cx, args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() {
                return Err(VmError::RuntimeError("table.sort: table expected".into()));
            }
            let comp_val: Option<Value> = args.get(1).copied().filter(|v| !v.is_nil());
            let mut arr = cx.vm.gc.table(t).array.clone();
            // The comparator may remove elements from the table and collect
            // while they sit only in `arr`, so they stay rooted until return.
            cx.root_all(&arr);
            let mut sort_err: Option<VmError> = None;
            arr.sort_by(|a, b| {
                if sort_err.is_some() {
                    return std::cmp::Ordering::Equal;
                }
                let lt = match comp_val {
                    Some(cf) => cx.vm.call_value_isolated(cf, &[*a, *b]).map(|res| res.into_iter().next().unwrap_or(Value::nil()).is_truthy()),
                    None => value_lt(&cx.vm.gc, *a, *b),
                };
                match lt {
                    Ok(true) => std::cmp::Ordering::Less,
                    Ok(false) => std::cmp::Ordering::Greater,
                    Err(e) => {
                        sort_err = Some(e);
                        std::cmp::Ordering::Equal
                    }
                }
            });
            if let Some(e) = sort_err {
                return Err(e);
            }
            // The write-back is a store like any other: the table may have
            // been scanned black while the elements were out of it.
            for &v in &arr {
                cx.vm.gc.barrier_val(v);
            }
            cx.vm.gc.table_mut(t).array = arr;
            Ok(vec![])
        });
        let v_tbl_pack = self.make_cfn_val(|cx, args| {
            let vm = &mut *cx.vm;
            let t = vm.alloc_table();
            for (i, &v) in args.iter().enumerate() {
                let k = vm.make_int(i as i64 + 1);
                vm.gc.raw_set(t, k, v);
            }
            let n_val = vm.make_int(args.len() as i64);
            let n_key = vm.alloc_str("n");
            vm.gc.raw_set(t, n_key, n_val);
            Ok(vec![t])
        });
        let v_tbl_unpack = self.make_cfn_val(|cx, args| {
            let t = args.first().copied().unwrap_or(Value::nil());
            if !t.is_table() {
                return Err(VmError::RuntimeError("table.unpack: table expected".into()));
            }
            let n = cx.vm.gc.table(t).length();
            let i = opt_int_arg(args, 1, "table.unpack", 1)?;
            let j = opt_int_arg(args, 2, "table.unpack", n)?;
            if checked_range_len(i, j).is_none() {
                return Err(VmError::RuntimeError("table.unpack: range too large".into()));
            }
            Ok((i..=j)
                .map(|k| {
                    let k = cx.vm.make_int(k);
                    cx.vm.gc.table(t).raw_get(k)
                })
                .collect())
        });
        let v_tbl_move = self.make_cfn_val(|cx, args| {
            let a1 = args.first().copied().unwrap_or(Value::nil());
            if !a1.is_table() {
                return Err(VmError::RuntimeError("table.move: table expected".into()));
            }
            let f = int_arg(args.get(1).copied().unwrap_or(Value::nil()), "table.move")?;
            let e = int_arg(args.get(2).copied().unwrap_or(Value::nil()), "table.move")?;
            let t = int_arg(args.get(3).copied().unwrap_or(Value::nil()), "table.move")?;
            let a2 = match args.get(4) {
                None => a1,
                Some(&v) if v.is_table() => v,
                Some(_) => return Err(VmError::RuntimeError("table.move: table expected".into())),
            };
            if e >= f {
                if checked_range_len(f, e).is_none() || t.checked_add(e - f).is_none() {
                    return Err(VmError::RuntimeError("table.move: too many elements to move".into()));
                }
                let vals: Vec<Value> = (f..=e)
                    .map(|k| {
                        let k = cx.vm.make_int(k);
                        cx.vm.gc.table(a1).raw_get(k)
                    })
                    .collect();
                for (idx, v) in vals.into_iter().enumerate() {
                    let key = cx.vm.make_int(t + idx as i64);
                    cx.vm.gc.raw_set(a2, key, v);
                }
            }
            Ok(vec![a2])
        });

        {
            let tt = Value::table(tbl_table_ptr);
            let k = self.intern("insert");
            self.gc.raw_set(tt, k, v_tbl_insert);
            let k = self.intern("remove");
            self.gc.raw_set(tt, k, v_tbl_remove);
            let k = self.intern("concat");
            self.gc.raw_set(tt, k, v_tbl_concat);
            let k = self.intern("sort");
            self.gc.raw_set(tt, k, v_tbl_sort);
            let k = self.intern("pack");
            self.gc.raw_set(tt, k, v_tbl_pack);
            let k = self.intern("unpack");
            self.gc.raw_set(tt, k, v_tbl_unpack);
            let k = self.intern("move");
            self.gc.raw_set(tt, k, v_tbl_move);
        }
        let k_table = self.intern("table");
        self.globals.raw_set(&mut self.gc, k_table, Value::table(tbl_table_ptr));

        let io_table_ptr = alloc_table_raw();
        self.gc.register(io_table_ptr);

        let v_io_write = self.make_cfn_val(|cx, args| {
            let mut out = String::new();
            for &v in args {
                if v.is_string() {
                    out.push_str(cx.vm.gc.str(v));
                } else if v.is_number() {
                    out.push_str(&format!("{v}"));
                } else {
                    return Err(VmError::RuntimeError("io.write: string or number expected".into()));
                }
            }
            emit_line(cx.vm, out, false);
            Ok(vec![])
        });
        let v_io_read = self.make_cfn_val(|cx, args| {
            let fmts: Vec<Value> = if args.is_empty() { vec![Value::nil()] } else { args.to_vec() };
            let mut out = Vec::new();
            for fv in fmts {
                let v = read_stdin_format(cx.vm, &fv)?;
                if v.is_nil() {
                    return Ok(vec![Value::nil()]);
                }
                out.push(v);
            }
            Ok(out)
        });
        let v_io_open = self.make_cfn_val(|cx, args| {
            let path = str_arg(args, 0, "io.open")?.to_owned();
            let mode = match args.get(1) {
                None => "r".to_owned(),
                Some(&v) if v.is_string() => cx.vm.gc.str(v).to_owned(),
                Some(_) => return Err(VmError::RuntimeError("io.open: string expected".into())),
            };
            let binary = mode.ends_with('b');
            let mode = mode.trim_end_matches('b');
            let mut opts = std::fs::OpenOptions::new();
            match mode {
                "r" => {
                    opts.read(true);
                }
                "w" => {
                    opts.write(true).create(true).truncate(true);
                }
                "a" => {
                    opts.append(true).create(true);
                }
                "r+" => {
                    opts.read(true).write(true);
                }
                "w+" => {
                    opts.read(true).write(true).create(true).truncate(true);
                }
                "a+" => {
                    opts.read(true).append(true).create(true);
                }
                _ => return Err(VmError::RuntimeError(format!("io.open: invalid mode '{mode}'"))),
            }
            let file = match opts.open(&path) {
                Ok(f) => f,
                Err(e) => return Ok(vec![Value::nil(), cx.vm.alloc_str(&format!("{path}: {e}"))]),
            };
            let file = std::rc::Rc::new(std::cell::RefCell::new(Some(file)));
            let vm = &mut *cx.vm;

            let f = file.clone();
            let v_read = vm.make_cfn_val(move |cx, args| {
                let mut guard = f.borrow_mut();
                let file = guard.as_mut().ok_or_else(|| VmError::RuntimeError("attempt to use a closed file".into()))?;
                let fmts: Vec<Value> = if args.len() <= 1 { vec![Value::nil()] } else { args[1..].to_vec() };
                let mut out = Vec::new();
                for fv in fmts {
                    let v = read_file_format(cx.vm, file, &fv, binary)?;
                    if v.is_nil() {
                        return Ok(vec![Value::nil()]);
                    }
                    out.push(v);
                }
                Ok(out)
            });

            // The handle table exists before the method closures so
            // file:write can return it (Lua returns the file for chaining).
            let handle = vm.alloc_table();

            let f = file.clone();
            let v_write = vm.make_cfn_closure(&[handle], move |cx, args, up| {
                let mut guard = f.borrow_mut();
                let file = guard.as_mut().ok_or_else(|| VmError::RuntimeError("attempt to use a closed file".into()))?;
                use std::io::Write;
                let mut out: Vec<u8> = Vec::new();
                for &v in args.get(1..).unwrap_or(&[]) {
                    if v.is_string() {
                        out.extend_from_slice(cx.vm.gc.str(v).as_bytes());
                    } else if v.is_bytes() {
                        out.extend_from_slice(&cx.vm.gc.bytes(v).data);
                    } else if v.is_number() {
                        out.extend_from_slice(format!("{v}").as_bytes());
                    } else {
                        return Err(VmError::RuntimeError("file:write: string, bytes or number expected".into()));
                    }
                }
                file.write_all(&out).map_err(|e| VmError::RuntimeError(format!("file:write: {e}")))?;
                Ok(vec![up[0]])
            });

            let f = file.clone();
            let v_close = vm.make_cfn_val(move |_cx, _args| {
                f.borrow_mut().take();
                Ok(vec![Value::bool(true)])
            });

            let f = file.clone();
            let v_lines = vm.make_cfn_val(move |cx, _args| {
                let f = f.clone();
                let iter_val = cx.vm.make_cfn_val(move |cx, _args| {
                    let mut guard = f.borrow_mut();
                    let file = guard.as_mut().ok_or_else(|| VmError::RuntimeError("attempt to use a closed file".into()))?;
                    match read_line_from_file(file)? {
                        // Lua closes the file when its lines iterator
                        // reaches EOF.
                        None => {
                            guard.take();
                            Ok(vec![Value::nil()])
                        }
                        Some(mut l) => {
                            while l.last().is_some_and(|&c| c == b'\n' || c == b'\r') {
                                l.pop();
                            }
                            Ok(vec![cx.vm.alloc_str(&String::from_utf8_lossy(&l))])
                        }
                    }
                });
                Ok(vec![iter_val])
            });

            let k = vm.intern("read");
            vm.gc.raw_set(handle, k, v_read);
            let k = vm.intern("write");
            vm.gc.raw_set(handle, k, v_write);
            let k = vm.intern("close");
            vm.gc.raw_set(handle, k, v_close);
            let k = vm.intern("lines");
            vm.gc.raw_set(handle, k, v_lines);
            Ok(vec![handle])
        });
        let v_io_lines = self.make_cfn_val(|cx, args| {
            let path = str_arg(args, 0, "io.lines")?.to_owned();
            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(e) => return Err(VmError::RuntimeError(format!("io.lines: cannot open '{path}': {e}"))),
            };
            let file = std::rc::Rc::new(std::cell::RefCell::new(Some(file)));
            let iter_val = cx.vm.make_cfn_val(move |cx, _args| {
                let mut guard = file.borrow_mut();
                let f = guard.as_mut().ok_or_else(|| VmError::RuntimeError("attempt to use a closed file".into()))?;
                match read_line_from_file(f)? {
                    None => {
                        guard.take();
                        Ok(vec![Value::nil()])
                    }
                    Some(mut l) => {
                        while l.last().is_some_and(|&c| c == b'\n' || c == b'\r') {
                            l.pop();
                        }
                        Ok(vec![cx.vm.alloc_str(&String::from_utf8_lossy(&l))])
                    }
                }
            });
            Ok(vec![iter_val])
        });
        let v_io_close = self.make_cfn_val(|cx, args| {
            let h = args.first().copied().unwrap_or(Value::nil());
            if !h.is_table() {
                return Err(VmError::RuntimeError("io.close: file expected".into()));
            }
            let key = cx.vm.alloc_str("close");
            let close = cx.vm.gc.table(h).raw_get(key);
            cx.vm.call_value_isolated(close, &[h])
        });
        {
            let iot = Value::table(io_table_ptr);
            let k = self.intern("write");
            self.gc.raw_set(iot, k, v_io_write);
            let k = self.intern("read");
            self.gc.raw_set(iot, k, v_io_read);
            let k = self.intern("open");
            self.gc.raw_set(iot, k, v_io_open);
            let k = self.intern("lines");
            self.gc.raw_set(iot, k, v_io_lines);
            let k = self.intern("close");
            self.gc.raw_set(iot, k, v_io_close);
        }
        let k_io = self.intern("io");
        self.globals.raw_set(&mut self.gc, k_io, Value::table(io_table_ptr));

        let os_table_ptr = alloc_table_raw();
        self.gc.register(os_table_ptr);

        let v_os_time = self.make_cfn_val(|cx, args| {
            // lua_isnoneornil: no argument (or nil) means the current time.
            if let Some(&t) = args.first().filter(|v| !v.is_nil()) {
                if !t.is_table() {
                    return Err(VmError::RuntimeError("os.time: table expected".into()));
                }
                let vm = &mut *cx.vm;
                let mut get = |name: &str| {
                    let k = vm.intern(name);
                    vm.gc.table(t).raw_get(k)
                };
                let mut field = |name: &str, who: &str| -> VmResult<i64> {
                    let v = get(name);
                    if v.is_nil() {
                        return Err(VmError::RuntimeError(format!("{who}: field '{name}' missing in date table")));
                    }
                    int_arg(v, who)
                };
                let year = field("year", "os.time")?;
                let month = field("month", "os.time")?;
                let day = field("day", "os.time")?;
                let hour = if get("hour").is_nil() { 12 } else { int_arg(get("hour"), "os.time")? };
                let min = if get("min").is_nil() { 0 } else { int_arg(get("min"), "os.time")? };
                let sec = if get("sec").is_nil() { 0 } else { int_arg(get("sec"), "os.time")? };
                // isdst is read but ignored: Umbra's clock is UTC, which has
                // no DST, so mktime would not shift the result either.
                let _isdst = get("isdst").is_truthy();
                let days = days_from_civil(year, month, day);
                let Some(secs) = days.checked_mul(86400).and_then(|d| d.checked_add(hour * 3600 + min * 60 + sec)) else {
                    return Err(VmError::RuntimeError("os.time: time result cannot be represented".into()));
                };
                // mktime writes the normalized fields back into the table.
                let (ny, nm, nd, yday, wday) = civil_fields(secs);
                let sod = secs.rem_euclid(86400);
                let mut set = |k: &str, v: i64| {
                    let key = vm.intern(k);
                    let val = vm.make_int(v);
                    vm.gc.raw_set(t, key, val);
                };
                set("year", ny);
                set("month", nm as i64);
                set("day", nd as i64);
                set("hour", sod / 3600);
                set("min", (sod / 60) % 60);
                set("sec", sod % 60);
                set("wday", wday as i64 + 1);
                set("yday", yday as i64);
                let k = vm.intern("isdst");
                vm.gc.raw_set(t, k, Value::bool(false));
                return Ok(vec![vm.make_int(secs)]);
            }
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
            Ok(vec![cx.vm.make_int(secs)])
        });
        let v_os_clock = self.make_cfn_val(|_cx, _args| {
            let start = PROCESS_START.get_or_init(std::time::Instant::now);
            Ok(vec![Value::float(start.elapsed().as_secs_f64())])
        });
        let v_os_getenv = self.make_cfn_val(|cx, args| {
            let name = str_arg(args, 0, "os.getenv")?;
            match std::env::var(name) {
                Ok(v) => Ok(vec![cx.vm.alloc_str(&v)]),
                Err(_) => Ok(vec![Value::nil()]),
            }
        });
        let v_os_date = self.make_cfn_val(|cx, args| {
            let mut fmt = match args.first() {
                None => "%c".to_owned(),
                Some(&v) if v.is_nil() => "%c".to_owned(),
                Some(&v) if v.is_string() => cx.vm.gc.str(v).to_owned(),
                // luaL_optlstring coerces numbers to strings.
                Some(&v) if v.is_number() => coerce_to_concat_str(v),
                Some(_) => return Err(VmError::RuntimeError("os.date: string expected".into())),
            };
            // '!' selects UTC; Umbra only has UTC, so it's a no-op marker.
            if let Some(rest) = fmt.strip_prefix('!') {
                fmt = rest.to_owned();
            }
            let secs = match args.get(1) {
                None => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
                Some(&v) if v.is_nil() => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
                Some(&v) => int_arg(v, "os.date")?,
            };
            if fmt == "*t" {
                let vm = &mut *cx.vm;
                let tv = vm.alloc_table();
                let (year, month, day, yday, wday) = civil_fields(secs);
                let sod = secs.rem_euclid(86400);
                let mut set = |k: &str, v: i64| {
                    let key = vm.intern(k);
                    let val = vm.make_int(v);
                    vm.gc.raw_set(tv, key, val);
                };
                set("year", year);
                set("month", month as i64);
                set("day", day as i64);
                set("hour", sod / 3600);
                set("min", (sod / 60) % 60);
                set("sec", sod % 60);
                set("wday", wday as i64 + 1);
                set("yday", yday as i64);
                let k = vm.intern("isdst");
                vm.gc.raw_set(tv, k, Value::bool(false));
                return Ok(vec![tv]);
            }
            Ok(vec![cx.vm.alloc_str(&format_civil_time(secs, &fmt)?)])
        });
        {
            let ot = Value::table(os_table_ptr);
            let k = self.intern("time");
            self.gc.raw_set(ot, k, v_os_time);
            let k = self.intern("clock");
            self.gc.raw_set(ot, k, v_os_clock);
            let k = self.intern("getenv");
            self.gc.raw_set(ot, k, v_os_getenv);
            let k = self.intern("date");
            self.gc.raw_set(ot, k, v_os_date);
        }
        let k_os = self.intern("os");
        self.globals.raw_set(&mut self.gc, k_os, Value::table(os_table_ptr));

        let utf8_table_ptr = alloc_table_raw();
        self.gc.register(utf8_table_ptr);

        let v_utf8_char = self.make_cfn_val(|cx, args| {
            let mut s = String::new();
            for &v in args {
                let n = int_arg(v, "utf8.char")?;
                let cp = u32::try_from(n).ok().and_then(char::from_u32).ok_or_else(|| VmError::RuntimeError("utf8.char: value out of range".into()))?;
                s.push(cp);
            }
            Ok(vec![cx.vm.alloc_str(&s)])
        });
        let v_utf8_len = self.make_cfn_val(|cx, args| {
            let s = str_arg(args, 0, "utf8.len")?;
            let i = opt_int_arg(args, 1, "utf8.len", 1)?;
            let j = opt_int_arg(args, 2, "utf8.len", -1)?;
            // u_posrelat + bounds checks, like Lua: i in [1, len+1], j <= len.
            let posi = u_posrelat(i, s.len());
            let posj = u_posrelat(j, s.len());
            if !(1..=s.len() as i64 + 1).contains(&posi) {
                return Err(VmError::RuntimeError("utf8.len: initial position out of bounds".into()));
            }
            if posj - 1 >= s.len() as i64 {
                return Err(VmError::RuntimeError("utf8.len: final position out of bounds".into()));
            }
            // Counts characters that START in [i, j]; a mid-character j is
            // fine, but a mid-character i is an invalid position.
            if posi > posj {
                return Ok(vec![Value::int(0)]);
            }
            let (start, end) = ((posi - 1) as usize, (posj - 1) as usize);
            if !s.is_char_boundary(start) {
                return Ok(vec![Value::nil(), Value::int(start as i64 + 1)]);
            }
            let n = s[start..].char_indices().take_while(|(off, _)| start + off <= end).count();
            Ok(vec![cx.vm.make_int(n as i64)])
        });
        let v_utf8_codepoint = self.make_cfn_val(|cx, args| {
            let s = str_arg(args, 0, "utf8.codepoint")?;
            let i = opt_int_arg(args, 1, "utf8.codepoint", 1)?;
            let j = match args.get(2) {
                None => i,
                Some(&v) if v.is_nil() => i,
                Some(&v) => int_arg(v, "utf8.codepoint")?,
            };
            let posi = u_posrelat(i, s.len());
            let pose = u_posrelat(j, s.len());
            if posi < 1 {
                return Err(VmError::RuntimeError("utf8.codepoint: initial position out of bounds".into()));
            }
            if pose > s.len() as i64 {
                return Err(VmError::RuntimeError("utf8.codepoint: final position out of bounds".into()));
            }
            if posi > pose {
                return Ok(vec![]);
            }
            let start = (posi - 1) as usize;
            // Characters that start in [posi, pose]; pose may sit mid-character.
            if !s.is_char_boundary(start) {
                return Err(VmError::RuntimeError("utf8.codepoint: invalid byte position".into()));
            }
            let mut result = Vec::new();
            for (off, ch) in s[start..].char_indices() {
                if start + off >= pose as usize {
                    break;
                }
                result.push(cx.vm.make_int(ch as i64));
            }
            Ok(result)
        });
        let v_utf8_offset = self.make_cfn_val(|cx, args| {
            let s = str_arg(args, 0, "utf8.offset")?;
            let n = int_arg(args.get(1).copied().unwrap_or(Value::nil()), "utf8.offset")?;
            let default_i = if n >= 0 { 1 } else { s.len() as i64 + 1 };
            let i = opt_int_arg(args, 2, "utf8.offset", default_i)?;
            let posi = u_posrelat(i, s.len());
            if !(1..=s.len() as i64 + 1).contains(&posi) {
                return Err(VmError::RuntimeError("utf8.offset: position out of bounds".into()));
            }
            let mut posi = (posi - 1) as usize;
            let mut n = n;
            if n == 0 {
                // Beginning of the byte sequence containing position i.
                while posi > 0 && !s.is_char_boundary(posi) {
                    posi -= 1;
                }
            } else {
                if posi < s.len() && !s.is_char_boundary(posi) {
                    return Err(VmError::RuntimeError("utf8.offset: initial position is a continuation byte".into()));
                }
                if n < 0 {
                    while n < 0 && posi > 0 {
                        posi -= 1;
                        while posi > 0 && !s.is_char_boundary(posi) {
                            posi -= 1;
                        }
                        n += 1;
                    }
                } else {
                    n -= 1;
                    while n > 0 && posi < s.len() {
                        posi += 1;
                        while posi < s.len() && !s.is_char_boundary(posi) {
                            posi += 1;
                        }
                        n -= 1;
                    }
                }
            }
            if n == 0 { Ok(vec![cx.vm.make_int(posi as i64 + 1)]) } else { Ok(vec![Value::nil()]) }
        });
        let v_utf8_codes = self.make_cfn_val(|cx, args| {
            let s = str_arg(args, 0, "utf8.codes")?.to_owned();
            let iter_val = cx.vm.make_cfn_val(move |cx, cargs| {
                let prev = match cargs.get(1) {
                    None => 0,
                    Some(&v) => int_arg(v, "utf8.codes")?,
                };
                // The control value is the 1-based start of the previous
                // character; skip its continuation bytes like iter_aux.
                let next_byte = if prev <= 0 {
                    0usize
                } else {
                    let mut p = prev as usize;
                    while p < s.len() && !s.is_char_boundary(p) {
                        p += 1;
                    }
                    p
                };
                match s.get(next_byte..).and_then(|rest| rest.chars().next()) {
                    Some(ch) => Ok(vec![cx.vm.make_int(next_byte as i64 + 1), cx.vm.make_int(ch as i64)]),
                    None => Ok(vec![Value::nil()]),
                }
            });
            Ok(vec![iter_val, args.first().copied().unwrap_or(Value::nil()), Value::int(0)])
        });
        {
            let ut = Value::table(utf8_table_ptr);
            let k = self.intern("char");
            self.gc.raw_set(ut, k, v_utf8_char);
            let k = self.intern("len");
            self.gc.raw_set(ut, k, v_utf8_len);
            let k = self.intern("codepoint");
            self.gc.raw_set(ut, k, v_utf8_codepoint);
            let k = self.intern("codes");
            self.gc.raw_set(ut, k, v_utf8_codes);
            let k = self.intern("offset");
            self.gc.raw_set(ut, k, v_utf8_offset);
        }
        let k_utf8 = self.intern("utf8");
        self.globals.raw_set(&mut self.gc, k_utf8, Value::table(utf8_table_ptr));

        let debug_table_ptr = alloc_table_raw();
        self.gc.register(debug_table_ptr);
        let v_debug_traceback = self.make_cfn_val(|cx, args| {
            let msg = crate::heap::arg_str(args, 0).map(str::to_owned);
            let vm = &mut *cx.vm;
            let tb = match vm.handlers.last() {
                // Inside an xpcall handler: the handler's own frames, then
                // the stack the error was raised on, which the handler would
                // run on top of in Lua.
                Some(h) if h.thread == vm.resume_chain.len() => {
                    let own = vm.stack_traceback(h.caller_segment + 1);
                    if own.is_empty() { h.traceback.clone() } else { format!("{own}\n{}", h.traceback) }
                }
                _ => vm.stack_traceback(vm.thread_floor),
            };
            let full = match &msg {
                Some(m) => format!("{m}\nstack traceback:\n{tb}"),
                None => format!("stack traceback:\n{tb}"),
            };
            Ok(vec![vm.alloc_str(&full)])
        });
        {
            let dt = Value::table(debug_table_ptr);
            let k = self.intern("traceback");
            self.gc.raw_set(dt, k, v_debug_traceback);
        }
        let k_debug = self.intern("debug");
        self.globals.raw_set(&mut self.gc, k_debug, Value::table(debug_table_ptr));
    }

    /// Registers a native function as the global `name`. See CFunction for
    /// what the native receives.
    pub fn set_global_cfn(&mut self, name: &str, f: impl Fn(&mut Ctx<'_>, &[Value]) -> VmResult<Vec<Value>> + 'static) {
        let k = self.intern(name);
        let v = self.make_cfn_val(f);
        self.globals.raw_set(&mut self.gc, k, v);
        self.globals_gen += 1;
    }

    /// Wraps a native function in a callable value without binding it to a
    /// name, e.g. to store it in a table. The value is a collected object
    /// like a table: it lives while script, a Ctx or a Ref can reach it, so
    /// root or store it before running script. A Value the closure captures
    /// is invisible to the collector; pass those to make_cfn_closure.
    pub fn make_cfn_val(&mut self, f: impl Fn(&mut Ctx<'_>, &[Value]) -> VmResult<Vec<Value>> + 'static) -> Value {
        self.make_cfn_closure(&[], move |cx, args, _| f(cx, args))
    }

    /// A native closing over `upvals`: the collector traces them for as long
    /// as the native lives, and every call gets them as its third argument.
    /// They are fixed at creation; mutable state goes in a table upvalue.
    pub fn make_cfn_closure(&mut self, upvals: &[Value], f: impl Fn(&mut Ctx<'_>, &[Value], &[Value]) -> VmResult<Vec<Value>> + 'static) -> Value {
        let native = Box::new(NativeFn { gc: crate::gc::GcHeader::new(crate::gc::GcKind::Native), upvals: upvals.into(), f: Box::new(f) });
        let ptr = Box::into_raw(native) as *mut u8;
        self.gc.register(ptr);
        Value::native(ptr)
    }

    // Calls native `f` with its arguments and itself rooted until it
    // returns: the args may come from a plain slice (a host call), and the
    // native's closure state lives in its heap object.
    fn call_native(&mut self, f: Value, args: &[Value]) -> VmResult<Vec<Value>> {
        let mut cx = Ctx::new(self);
        cx.root(f);
        cx.root_all(args);
        // SAFETY: `f` is a native, rooted until cx drops after the call.
        let (body, upvals) = unsafe { crate::heap::native(f) };
        let result = body(&mut cx, args, upvals);
        if result.is_ok() {
            cx.vm.drop_handled_traces();
        }
        result
    }

    pub fn intern_pub(&mut self, s: &str) -> Value {
        self.intern(s)
    }

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

    pub fn set_global(&mut self, name: &str, v: Value) {
        let k = self.intern(name);
        self.globals.raw_set(&mut self.gc, k, v);
        self.globals_gen += 1;
    }

    pub fn get_global(&mut self, name: &str) -> Value {
        let k = self.intern(name);
        self.globals.raw_get(k)
    }
}

/// A native function. It receives the calling VM through `cx` (allocate,
/// call back into script, reach the host via Ctx::host) and its arguments,
/// which stay alive until it returns.
pub type CFunction = Box<dyn Fn(&mut Ctx<'_>, &[Value]) -> VmResult<Vec<Value>>>;

// What a native object holds: a closure that also gets its upvalues.
pub(crate) type NativeBody = Box<dyn Fn(&mut Ctx<'_>, &[Value], &[Value]) -> VmResult<Vec<Value>>>;

// Lua's "index is nil", for a store under a none key.
fn none_index() -> VmError {
    VmError::RuntimeError("index is none".into())
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

// At the top-level boundary (exec, require) a thrown value that escaped
// every pcall becomes a plain runtime error carrying its display form —
// there is no script-side handler left to receive the object.
fn thrown_to_runtime(e: VmError) -> VmError {
    match e {
        VmError::Thrown(v) if v.is_string() => VmError::RuntimeError(crate::heap::with_str(v, str::to_owned)),
        VmError::Thrown(v) => VmError::RuntimeError(format!("{v}")),
        other => other,
    }
}

// require's work for a module not yet loaded: fetch its source, compile it
// and run it. Returns the module's value and its chunk.
fn load_module(cx: &mut Ctx<'_>, name: &str) -> VmResult<(Value, Value)> {
    // Cloned out of the Vm so the loader can be handed the Ctx (and
    // through it the host) while it runs.
    let (src, chunk) = match cx.vm.module_loader.clone() {
        Some(loader) => {
            let m = loader(cx, name).map_err(|e| VmError::RuntimeError(format!("require: {e}")))?;
            (m.source, m.chunk_name)
        }
        None => {
            let path = format!("{}.umbra", name.replace('.', "/"));
            let src = std::fs::read_to_string(&path).map_err(|e| VmError::RuntimeError(format!("require: cannot open '{path}': {e}")))?;
            (src, path)
        }
    };
    let vm = &mut *cx.vm;
    let chunk_fn = vm.load_proto(&src, Some(chunk.clone())).map_err(|e| VmError::RuntimeError(format!("require: {chunk}: {e}")))?;
    let results = vm.call_value_isolated(chunk_fn, &[]).map_err(thrown_to_runtime)?;
    Ok((results.first().copied().unwrap_or(Value::bool(true)), chunk_fn))
}

fn pattern_captures(vm: &mut Vm, subj: &[u8], m: &pattern::Match) -> Vec<Value> {
    m.captures
        .iter()
        .map(|c| match c {
            pattern::Capture::Position(p) => vm.make_int(*p as i64),
            pattern::Capture::Str(a, b) => vm.alloc_str(&String::from_utf8_lossy(&subj[*a..*b])),
        })
        .collect()
}

fn apply_gsub_repl(vm: &mut Vm, repl: Value, subj: &[u8], m: &pattern::Match, whole: &[u8]) -> VmResult<Option<Vec<u8>>> {
    let cap_bytes = |i: usize| -> Vec<u8> {
        if m.captures.is_empty() {
            return whole.to_vec();
        }
        match &m.captures[i] {
            pattern::Capture::Str(a, b) => subj[*a..*b].to_vec(),
            pattern::Capture::Position(p) => p.to_string().into_bytes(),
        }
    };
    if repl.is_string() {
        let rb = vm.gc.str(repl).as_bytes().to_vec();
        let mut result = Vec::new();
        let mut i = 0;
        while i < rb.len() {
            if rb[i] == b'%' && i + 1 == rb.len() {
                return Err(VmError::RuntimeError("invalid use of '%' in replacement string".into()));
            }
            if rb[i] == b'%' && i + 1 < rb.len() {
                let c = rb[i + 1];
                if c == b'%' {
                    result.push(b'%');
                } else if c == b'0' {
                    result.extend_from_slice(whole);
                } else if c.is_ascii_digit() {
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
            vm.alloc_str(&String::from_utf8_lossy(whole))
        } else {
            match &m.captures[0] {
                pattern::Capture::Str(a, b) => vm.alloc_str(&String::from_utf8_lossy(&subj[*a..*b])),
                pattern::Capture::Position(p) => vm.make_int(*p as i64),
            }
        };
        let v = vm.gc.table(repl).raw_get(key);
        return gsub_result_value(v);
    }
    if crate::heap::is_native(repl) || vm.gc.closure_proto(repl).is_some() {
        let call_args: Vec<Value> = if m.captures.is_empty() { vec![vm.alloc_str(&String::from_utf8_lossy(whole))] } else { pattern_captures(vm, subj, m) };
        let result = vm.call_value_isolated(repl, &call_args)?;
        let v = result.into_iter().next().unwrap_or(Value::nil());
        return gsub_result_value(v);
    }
    Err(VmError::RuntimeError("bad argument to 'gsub' (string/function/table expected)".into()))
}

fn gsub_result_value(v: Value) -> VmResult<Option<Vec<u8>>> {
    if !v.is_truthy() {
        return Ok(None);
    }
    if v.is_string() {
        return Ok(Some(crate::heap::with_str(v, |s| s.as_bytes().to_vec())));
    }
    if v.is_number() {
        return Ok(Some(format!("{v}").into_bytes()));
    }
    Err(VmError::RuntimeError("invalid replacement value (a table/function must return a string/number/false/none)".into()))
}

fn coerce_to_concat_str(v: Value) -> String {
    if v.is_string() { crate::heap::with_str(v, str::to_owned) } else { coerce_num_str(v) }
}

// Number half of coerce_to_concat_str; concat_two borrows string operands
// instead of copying them, so only numbers need a scratch String.
fn coerce_num_str(v: Value) -> String {
    if v.is_int_like() { v.as_int().unwrap().to_string() } else { crate::value::lua_float_str(v.as_float().unwrap()) }
}

// __tostring-aware stringification shared by print/tostring/string.format.
// Tables without __tostring, functions, coroutines and bytes print their
// type and identity (`table: 0x...`), as in Lua.
fn tostring_value(vm: &mut Vm, v: Value) -> VmResult<String> {
    if v.is_string() {
        return Ok(vm.gc.str(v).to_owned());
    }
    if v.is_table() {
        let mm = vm.get_mm(v, "__tostring");
        if !mm.is_nil() {
            let sv = vm.call_value_isolated(mm, &[v])?.into_iter().next().unwrap_or(Value::nil());
            // luaL_tolstring: __tostring must return a string or a number.
            if sv.is_string() {
                return Ok(vm.gc.str(sv).to_owned());
            }
            if sv.is_number() {
                return Ok(format!("{sv}"));
            }
            return Err(VmError::RuntimeError("'__tostring' must return a string".into()));
        }
    }
    Ok(format!("{v}"))
}

fn float_to_int_or_float(vm: &mut Vm, f: f64) -> Value {
    if f >= -9223372036854775808.0 && f < 9223372036854775808.0 { vm.make_int(f as i64) } else { Value::float(f) }
}

fn int_val(v: Value) -> VmResult<i64> {
    if let Some(n) = v.as_int() {
        return Ok(n);
    }
    if let Some(f) = v.as_float() {
        if f.fract() == 0.0 && f >= -9223372036854775808.0 && f < 9223372036854775808.0 {
            return Ok(f as i64);
        }
        return Err(VmError::RuntimeError("number has no integer representation".into()));
    }
    Err(VmError::RuntimeError(format!("integer expected, got {}", v.type_name())))
}

// Lua's lua_stringtonumber: full-string parse of a decimal or hex numeral
// (leading/trailing whitespace allowed, hex ints wrap, 'inf'/'nan' rejected
// like l_str2d's 'n' check). Returns None on any trailing junk.
// Parsing allocates nothing, so callers that only want the i64/f64 (the
// argument checkers) don't need the VM; str_to_number boxes the result.
fn parse_number(s: &str) -> Option<Num> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let body = body.strip_prefix('+').unwrap_or(body);
    if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        if !hex.is_empty() && hex.bytes().all(|b| (b as char).is_ascii_hexdigit()) {
            let mut n: u64 = 0;
            for c in hex.bytes() {
                n = n.wrapping_mul(16).wrapping_add((c as char).to_digit(16).unwrap() as u64);
            }
            let n = n as i64;
            return Some(Num::Int(if neg { n.wrapping_neg() } else { n }));
        }
        if let Ok(f) = crate::lexer::parse_hex_float(body) {
            return Some(Num::Float(if neg { -f } else { f }));
        }
        return None;
    }
    // l_str2d rejects strings whose first '.', 'x'/'X', or 'n'/'N' is an
    // 'n'/'N' — that's how 'inf'/'nan' are excluded from strtod's grammar.
    if s.bytes().find(|b| matches!(b, b'.' | b'x' | b'X' | b'n' | b'N')).is_some_and(|b| b == b'n' || b == b'N') {
        return None;
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(Num::Int(n));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Some(Num::Float(f));
    }
    None
}

fn str_to_number(vm: &mut Vm, s: &str) -> Option<Value> {
    Some(match parse_number(s)? {
        Num::Int(n) => vm.make_int(n),
        Num::Float(f) => Value::float(f),
    })
}

// Strict integer argument for stdlib calls, matching Lua's luaL_checkinteger:
// integral floats and numeric strings convert, anything else is an error
// (int_from_val silently truncated non-integral floats and defaulted
// non-numbers to 0).
fn int_arg(v: Value, who: &str) -> VmResult<i64> {
    let v = if v.is_string() {
        match crate::heap::with_str(v, parse_number) {
            Some(Num::Int(n)) => return Ok(n),
            Some(Num::Float(f)) => Value::float(f),
            None => return Err(VmError::RuntimeError(format!("{who}: integer expected, got string"))),
        }
    } else {
        v
    };
    int_val(v).map_err(|e| match e {
        VmError::RuntimeError(m) => VmError::RuntimeError(format!("{who}: {m}")),
        other => other,
    })
}

// Optional integer argument: nil means "use the default", like luaL_optinteger.
fn opt_int_arg(args: &[Value], idx: usize, who: &'static str, default: i64) -> VmResult<i64> {
    match args.get(idx) {
        None => Ok(default),
        Some(&v) if v.is_nil() => Ok(default),
        Some(&v) => int_arg(v, who),
    }
}

// Float argument for stdlib calls, matching luaL_checknumber: numbers and
// numeric strings convert (as a float even for integral strings).
fn num_arg(v: Value, who: &str) -> VmResult<f64> {
    if let Some(f) = v.to_float() {
        return Ok(f);
    }
    if v.is_string() {
        match crate::heap::with_str(v, parse_number) {
            Some(Num::Int(n)) => return Ok(n as f64),
            Some(Num::Float(f)) => return Ok(f),
            None => {}
        }
    }
    Err(VmError::RuntimeError(format!("{who}: number expected, got {}", v.type_name())))
}
// Same coercion as num_arg but keeps the Value (int stays int) — for
// comparisons that must not go through f64, like math.max/min.
fn num_value_arg(vm: &mut Vm, v: Value, who: &str) -> VmResult<Value> {
    if v.is_number() {
        return Ok(v);
    }
    if v.is_string() {
        match crate::heap::with_str(v, parse_number) {
            Some(Num::Int(n)) => return Ok(vm.make_int(n)),
            Some(Num::Float(f)) => return Ok(Value::float(f)),
            None => {}
        }
    }
    Err(VmError::RuntimeError(format!("{who}: number expected, got {}", v.type_name())))
}

// Lua shift semantics: negative counts shift the other way, |n| >= 64 gives 0.
fn lua_shl(x: i64, n: i64) -> i64 {
    if n <= -64 || n >= 64 {
        0
    } else if n >= 0 {
        ((x as u64) << n) as i64
    } else {
        ((x as u64) >> -n) as i64
    }
}

#[derive(Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}

fn to_number(v: Value, what: &str) -> VmResult<Num> {
    if v.is_int_like() {
        return Ok(Num::Int(v.as_int().unwrap()));
    }
    if v.is_float() {
        return Ok(Num::Float(v.as_float().unwrap()));
    }
    Err(VmError::RuntimeError(format!("'for' {what} must be a number, got {}", v.type_name())))
}

fn num_add(vm: &mut Vm, a: Num, b: Num) -> Value {
    match (a, b) {
        // Overflow pushes the candidate past every limit as +/-inf, so the
        // loop terminates instead of wrapping the counter (Lua semantics).
        (Num::Int(x), Num::Int(y)) => match x.checked_add(y) {
            Some(n) => vm.make_int(n),
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
        (Num::Int(x), Num::Int(y)) => x <= y,
        (Num::Float(x), Num::Float(y)) => x <= y,
        // Exact int/float compare: `x as f64` would round e.g. i64::MAX up to
        // 2^63 and wrongly include it in a `<= 2^63` range.
        (Num::Int(x), Num::Float(y)) => cmp_int_float(x, y).is_some_and(|o| o.is_le()),
        (Num::Float(x), Num::Int(y)) => cmp_int_float(y, x).is_some_and(|o| o.is_ge()),
    }
}

fn num_is_positive(n: Num) -> bool {
    match n {
        Num::Int(x) => x > 0,
        Num::Float(x) => x > 0.0,
    }
}

fn values_equal(gc: &Gc, a: Value, b: Value) -> bool {
    if a.is_string() && b.is_string() {
        return gc.str(a) == gc.str(b);
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

fn value_lt(gc: &Gc, a: Value, b: Value) -> VmResult<bool> {
    if let Some(ord) = num_cmp(a, b) {
        return Ok(ord.is_lt());
    }
    if a.is_string() && b.is_string() {
        return Ok(gc.str(a) < gc.str(b));
    }
    Err(VmError::RuntimeError(format!("attempt to compare {} with {}", a.type_name(), b.type_name())))
}

fn value_le(gc: &Gc, a: Value, b: Value) -> VmResult<bool> {
    if let Some(ord) = num_cmp(a, b) {
        return Ok(ord.is_le());
    }
    if a.is_string() && b.is_string() {
        return Ok(gc.str(a) <= gc.str(b));
    }
    Err(VmError::RuntimeError(format!("attempt to compare {} with {}", a.type_name(), b.type_name())))
}

fn capture_yield_site(co: &mut Coroutine) {
    if let Some(frame) = co.frames.last() {
        if frame.pc > 0 {
            let instr = frame.proto().code[frame.pc - 1];
            co.yield_result_base = frame.base + ia(instr) as usize;
            let call_c = ic(instr);
            co.yield_nresults = if call_c == 0 { 255 } else { (call_c - 1) as u8 };
        }
    }
}

// No BufReader: the same File is shared (via Rc<RefCell<>>) between a file
// handle's read/write closures, and buffering reads would desync an
// interleaved read/write sequence on "r+"/"w+"/"a+" handles.
fn read_line_from_file(file: &mut std::fs::File) -> VmResult<Option<Vec<u8>>> {
    use std::io::Read;
    let mut bytes: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match file.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                bytes.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(e) => return Err(VmError::RuntimeError(format!("file:read: {e}"))),
        }
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    Ok(Some(bytes))
}

// Lua's str_to_number on a maximal prefix: returns (value, bytes consumed
// from the start of `s`, leading whitespace included). Accepts decimal
// ints/floats, hex ints (wrapping) and hex floats, inf/nan.
fn str_to_number_prefix(vm: &mut Vm, s: &str) -> Option<(Value, usize)> {
    let ws = s.len() - s.trim_start().len();
    let t = s.trim_start();
    // Try every prefix length, longest first — cheap because numeric prefixes
    // are short; stops at the first parseable one.
    for end in (1..=t.len()).rev() {
        if !t.is_char_boundary(end) {
            continue;
        }
        let cand = &t[..end];
        let (neg, body) = match cand.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, cand),
        };
        let body = body.strip_prefix('+').unwrap_or(body);
        if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            if !hex.is_empty() && hex.bytes().all(|b| (b as char).is_ascii_hexdigit()) {
                let mut n: u64 = 0;
                for c in hex.bytes() {
                    n = n.wrapping_mul(16).wrapping_add((c as char).to_digit(16).unwrap() as u64);
                }
                let n = n as i64;
                return Some((vm.make_int(if neg { n.wrapping_neg() } else { n }), ws + end));
            }
            if let Ok(f) = crate::lexer::parse_hex_float(body) {
                return Some((Value::float(if neg { -f } else { f }), ws + end));
            }
            continue;
        }
        if let Ok(n) = cand.parse::<i64>() {
            return Some((vm.make_int(n), ws + end));
        }
        if let Ok(f) = cand.parse::<f64>() {
            return Some((Value::float(f), ws + end));
        }
    }
    None
}

// Reads a Lua "n" format from a seekable file: skip whitespace, take the
// longest valid numeric prefix, then seek back the unread tail.
fn read_number_from_file(vm: &mut Vm, file: &mut std::fs::File) -> VmResult<Value> {
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
                if !numeric && !still_ws {
                    break;
                }
            }
            Err(e) => return Err(VmError::RuntimeError(format!("file:read: {e}"))),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let (v, used) = match str_to_number_prefix(vm, &text) {
        Some(r) => r,
        None => (Value::nil(), 0),
    };
    let unread = buf.len() - used;
    if unread > 0 {
        let _ = file.seek(std::io::SeekFrom::Current(-(unread as i64)));
    }
    Ok(v)
}
// u_posrelat from lutf8lib: 1-based position, negative counts back from the
// end; too-far-negative clamps to 0 (which callers then reject or treat as
// an empty range).
fn u_posrelat(pos: i64, len: usize) -> i64 {
    if pos >= 0 {
        pos
    } else if pos.wrapping_neg() as u64 > len as u64 {
        0
    } else {
        len as i64 + pos + 1
    }
}

fn read_file_format(vm: &mut Vm, file: &mut std::fs::File, fv: &Value, binary: bool) -> VmResult<Value> {
    use std::io::{Read, Seek};
    if fv.is_number() {
        let n = int_arg(*fv, "file:read")?;
        if n < 0 {
            return Err(VmError::RuntimeError("file:read: invalid length".into()));
        }
        if n == 0 {
            // Probe for EOF: empty string if more data, nil at EOF.
            let mut b = [0u8; 1];
            return match file.read(&mut b) {
                Ok(0) => Ok(Value::nil()),
                Ok(_) => {
                    let _ = file.seek(std::io::SeekFrom::Current(-1));
                    Ok(if binary { vm.alloc_bytes_zeroed(0) } else { vm.alloc_str("") })
                }
                Err(e) => Err(VmError::RuntimeError(format!("file:read: {e}"))),
            };
        }
        let mut buf = vec![0u8; n as usize];
        let got = file.read(&mut buf).map_err(|e| VmError::RuntimeError(format!("file:read: {e}")))?;
        if got == 0 {
            return Ok(Value::nil());
        }
        buf.truncate(got);
        return Ok(if binary { vm.alloc_bytes(buf) } else { vm.alloc_str(&String::from_utf8_lossy(&buf)) });
    }
    let fmt = if fv.is_string() {
        vm.gc.str(*fv).trim_start_matches('*').to_owned()
    } else if fv.is_nil() {
        "l".to_owned()
    } else {
        return Err(VmError::RuntimeError("file:read: invalid format".into()));
    };
    match fmt.as_str() {
        "a" => {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).map_err(|e| VmError::RuntimeError(format!("file:read: {e}")))?;
            Ok(if binary { vm.alloc_bytes(buf) } else { vm.alloc_str(&String::from_utf8_lossy(&buf)) })
        }
        "n" => read_number_from_file(vm, file),
        "l" => match read_line_from_file(file)? {
            None => Ok(Value::nil()),
            Some(mut l) => {
                while l.last().is_some_and(|&c| c == b'\n' || c == b'\r') {
                    l.pop();
                }
                Ok(if binary { vm.alloc_bytes(l) } else { vm.alloc_str(&String::from_utf8_lossy(&l)) })
            }
        },
        "L" => match read_line_from_file(file)? {
            None => Ok(Value::nil()),
            Some(l) => Ok(if binary { vm.alloc_bytes(l) } else { vm.alloc_str(&String::from_utf8_lossy(&l)) }),
        },
        _ => Err(VmError::RuntimeError(format!("file:read: invalid format '{fmt}'"))),
    }
}

fn read_stdin_format(vm: &mut Vm, fv: &Value) -> VmResult<Value> {
    use std::io::Read;
    if fv.is_number() {
        let n = int_arg(*fv, "io.read")?;
        if n < 0 {
            return Err(VmError::RuntimeError("io.read: invalid length".into()));
        }
        let mut buf = vec![0u8; n as usize];
        let got = std::io::stdin().read(&mut buf).map_err(|e| VmError::RuntimeError(format!("io.read: {e}")))?;
        if got == 0 && n > 0 {
            return Ok(Value::nil());
        }
        buf.truncate(got);
        return Ok(vm.alloc_str(&String::from_utf8_lossy(&buf)));
    }
    let fmt = if fv.is_string() {
        vm.gc.str(*fv).trim_start_matches('*').to_owned()
    } else if fv.is_nil() {
        "l".to_owned()
    } else {
        return Err(VmError::RuntimeError("io.read: invalid format".into()));
    };
    match fmt.as_str() {
        "a" => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).map_err(|e| VmError::RuntimeError(format!("io.read: {e}")))?;
            Ok(vm.alloc_str(&buf))
        }
        "n" => {
            // stdin isn't seekable, so the longest numeric prefix of the line
            // is taken and the rest of the line is consumed.
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).map_err(|e| VmError::RuntimeError(format!("io.read: {e}")))?;
            match str_to_number_prefix(vm, &line) {
                Some((v, _)) => Ok(v),
                None => Ok(Value::nil()),
            }
        }
        "l" | "L" => {
            let mut line = String::new();
            let n = std::io::stdin().read_line(&mut line).map_err(|e| VmError::RuntimeError(format!("io.read: {e}")))?;
            if n == 0 {
                return Ok(Value::nil());
            }
            if fmt == "l" {
                while line.ends_with('\n') || line.ends_with('\r') {
                    line.pop();
                }
            }
            Ok(vm.alloc_str(&line))
        }
        _ => Err(VmError::RuntimeError(format!("io.read: invalid format '{fmt}'"))),
    }
}

fn str_arg<'a>(args: &'a [Value], idx: usize, fn_name: &'static str) -> VmResult<&'a str> {
    crate::heap::arg_str(args, idx).ok_or_else(|| VmError::RuntimeError(format!("{fn_name}: string expected")))
}

// i64::MIN..=i64::MAX overflows i64 arithmetic directly, so the length is
// computed in i128; returns None if the (inclusive) range exceeds MAX_ALLOC_LEN.
fn checked_range_len(lo: i64, hi: i64) -> Option<usize> {
    if hi < lo {
        return Some(0);
    }
    let len = (hi as i128) - (lo as i128) + 1;
    if len > MAX_ALLOC_LEN as i128 { None } else { Some(len as usize) }
}

fn lua_str_start(len: usize, i: i64) -> usize {
    if i > 0 {
        ((i - 1) as usize).min(len)
    } else if i < 0 {
        (len as i64 + i).max(0) as usize
    } else {
        0
    }
}

fn lua_str_end(len: usize, j: i64) -> usize {
    if j >= 0 { (j as usize).min(len) } else { (len as i64 + j + 1).max(0) as usize }
}

fn splitmix64(x: u64) -> u64 {
    let x = x.wrapping_add(0x9e3779b97f4a7c15);
    let x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    let x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

fn pad_str(s: String, width: usize, left_align: bool, zero_pad: bool) -> String {
    if s.len() >= width {
        return s;
    }
    let n = width - s.len();
    if left_align {
        let mut r = s;
        for _ in 0..n {
            r.push(' ');
        }
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
    if f.is_nan() {
        return if upper { "NAN".into() } else { "nan".into() };
    }
    if f.is_infinite() {
        return if upper { "INF".into() } else { "inf".into() };
    }
    let t = format!("{:.*e}", prec, f);
    let epos = t.rfind('e').unwrap();
    let ex: i32 = t[epos + 1..].parse().unwrap();
    let s = format!("{}e{}{:02}", &t[..epos], if ex < 0 { "-" } else { "+" }, ex.abs());
    if upper { s.to_uppercase() } else { s }
}

// C-style %g: shortest of %e/%f at the given significant-digit count, with
// trailing zeros stripped.
fn fmt_g(f: f64, prec: usize, upper: bool, alt: bool) -> String {
    if f.is_nan() {
        return if upper { "NAN".into() } else { "nan".into() };
    }
    if f.is_infinite() {
        return if upper { "INF".into() } else { "inf".into() };
    }
    let p = if prec == 0 { 1 } else { prec };
    let e = format!("{:.*e}", p - 1, f);
    let exp: i32 = e[e.rfind('e').unwrap() + 1..].parse().unwrap();
    let s = if exp < -4 || exp >= p as i32 {
        let t = format!("{:.*e}", p - 1, f);
        let epos = t.rfind('e').unwrap();
        let ex: i32 = t[epos + 1..].parse().unwrap();
        let mut m: String = t[..epos].into();
        // '#' keeps trailing zeros (and the point) instead of stripping.
        if !alt && m.contains('.') {
            while m.ends_with('0') {
                m.pop();
            }
            if m.ends_with('.') {
                m.pop();
            }
        }
        format!("{}e{}{:02}", m, if ex < 0 { "-" } else { "+" }, ex.abs())
    } else {
        let decimals = (p as i32 - 1 - exp).max(0) as usize;
        let mut t = format!("{:.*}", decimals, f);
        if !alt && t.contains('.') {
            while t.ends_with('0') {
                t.pop();
            }
            if t.ends_with('.') {
                t.pop();
            }
        }
        t
    };
    if upper { s.to_uppercase() } else { s }
}

// C-style %a hex float on |f| (caller handles the sign).
fn fmt_a(f: f64, prec: Option<usize>, upper: bool) -> String {
    if f.is_nan() {
        return if upper { "NAN".into() } else { "nan".into() };
    }
    if f.is_infinite() {
        return if upper { "INF".into() } else { "inf".into() };
    }
    if f == 0.0 {
        return "0x0p+0".into();
    }
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
            for _ in 0..13 {
                ds.push((m >> 48) & 0xf);
                m <<= 4;
            }
            while ds.last() == Some(&0) {
                ds.pop();
            }
            for d in ds {
                digits.push(char::from_digit(d as u32, 16).unwrap());
            }
        }
        Some(p) => {
            if p == 0 {
                // glibc rounds the whole significand (leading digit included)
                // without renormalizing: %.0a of 1.5 is 0x2p+0, not 0x1p+1.
                if frac >= 1u64 << 51 {
                    lead += 1;
                }
            } else if p < 13 {
                // Round the 52-bit mantissa to p hex digits (half up).
                let drop = 52 - p as u32 * 4;
                frac += 1u64 << (drop - 1);
                if frac >= 1u64 << 52 {
                    frac = 0;
                    if lead == 0 {
                        lead = 1;
                    } else {
                        e2 += 1;
                    }
                }
                frac >>= drop;
                for k in (0..p).rev() {
                    digits.push(char::from_digit(((frac >> (k * 4)) & 0xf) as u32, 16).unwrap());
                }
            } else {
                let mut m = frac;
                for _ in 0..13 {
                    digits.push(char::from_digit(((m >> 48) & 0xf) as u32, 16).unwrap());
                    m <<= 4;
                }
                for _ in 13..p {
                    digits.push('0');
                }
            }
        }
    }
    let s = if digits.is_empty() { format!("0x{lead}p{e2:+}") } else { format!("0x{lead}.{digits}p{e2:+}") };
    if upper { s.to_uppercase() } else { s }
}

fn string_format(vm: &mut Vm, fmt: &str, args: &[Value]) -> VmResult<Vec<Value>> {
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
        if bytes[pos] == b'%' {
            out.push('%');
            pos += 1;
            continue;
        }

        // Lua checks the argument exists before parsing the specifier.
        arg_idx += 1;
        let v = args.get(arg_idx - 1).copied().ok_or_else(|| VmError::RuntimeError(format!("bad argument #{arg_idx} to 'format' (value expected)")))?;

        let mut left = false;
        let mut plus = false;
        let mut zero = false;
        let mut space = false;
        let mut alt = false;
        loop {
            match bytes.get(pos) {
                Some(b'-') => {
                    left = true;
                    pos += 1;
                }
                Some(b'+') => {
                    plus = true;
                    pos += 1;
                }
                Some(b'0') if !left => {
                    zero = true;
                    pos += 1;
                }
                Some(b' ') => {
                    space = true;
                    pos += 1;
                }
                Some(b'#') => {
                    alt = true;
                    pos += 1;
                }
                _ => break,
            }
        }
        // Lua's get2digits: width and precision are at most two digits.
        let mut width = 0usize;
        for _ in 0..2 {
            if pos < bytes.len() && bytes[pos].is_ascii_digit() {
                width = width * 10 + (bytes[pos] - b'0') as usize;
                pos += 1;
            }
        }
        if pos < bytes.len() && bytes[pos].is_ascii_digit() {
            return Err(VmError::RuntimeError("invalid conversion specification to 'format'".into()));
        }
        let mut prec: Option<usize> = None;
        if pos < bytes.len() && bytes[pos] == b'.' {
            pos += 1;
            let mut p = 0usize;
            for _ in 0..2 {
                if pos < bytes.len() && bytes[pos].is_ascii_digit() {
                    p = p * 10 + (bytes[pos] - b'0') as usize;
                    pos += 1;
                }
            }
            if pos < bytes.len() && bytes[pos].is_ascii_digit() {
                return Err(VmError::RuntimeError("invalid conversion specification to 'format'".into()));
            }
            prec = Some(p);
        }
        if pos >= bytes.len() {
            return Err(VmError::RuntimeError("invalid option '%' to 'format'".into()));
        }
        let spec = bytes[pos] as char;
        pos += 1;

        let num_err = |m: &str| VmError::RuntimeError(format!("bad argument #{arg_idx} to 'format' ({m})"));
        let conv_err = || VmError::RuntimeError("invalid conversion specification to 'format'".into());
        // checkformat: each conversion allows a subset of the flags.
        let check_flags = |allowed: &str, allow_prec: bool| -> VmResult<()> {
            for (on, f) in [(left, '-'), (plus, '+'), (zero, '0'), (space, ' '), (alt, '#')] {
                if on && !allowed.contains(f) {
                    return Err(conv_err());
                }
            }
            if prec.is_some() && !allow_prec {
                return Err(conv_err());
            }
            Ok(())
        };
        let fmt_int = |v: Value| -> VmResult<i64> {
            let v = if v.is_string() {
                match crate::heap::with_str(v, parse_number).ok_or_else(|| num_err("number expected"))? {
                    Num::Int(n) => return Ok(n),
                    Num::Float(f) => Value::float(f),
                }
            } else {
                v
            };
            if let Some(n) = v.as_int() {
                return Ok(n);
            }
            if let Some(f) = v.as_float() {
                if f.fract() == 0.0 && f >= -9223372036854775808.0 && f < 9223372036854775808.0 {
                    return Ok(f as i64);
                }
                return Err(num_err("number has no integer representation"));
            }
            Err(num_err("number expected"))
        };
        let fmt_num = |v: Value| -> VmResult<f64> {
            if let Some(f) = v.to_float() {
                return Ok(f);
            }
            if v.is_string() {
                match crate::heap::with_str(v, parse_number) {
                    Some(Num::Int(n)) => return Ok(n as f64),
                    Some(Num::Float(f)) => return Ok(f),
                    None => {}
                }
            }
            Err(num_err("number expected"))
        };
        // Sign + zero-padding shared by the float conversions.
        let float_body = |f: f64, body: String| -> String {
            let sign = if f.is_sign_negative() {
                "-"
            } else if plus {
                "+"
            } else if space {
                " "
            } else {
                ""
            };
            format!("{sign}{body}")
        };

        let s = match spec {
            'd' | 'i' => {
                let n = fmt_int(v)?;
                check_flags("-+0 ", true)?;
                let mut raw = n.unsigned_abs().to_string();
                // Precision for integers means minimum digits (zero-filled),
                // disables the '0' flag, and %.0d of zero prints nothing.
                if let Some(p) = prec {
                    if p == 0 && n == 0 {
                        raw.clear();
                    }
                    while raw.len() < p {
                        raw.insert(0, '0');
                    }
                }
                let signed = if n < 0 {
                    format!("-{raw}")
                } else if plus {
                    format!("+{raw}")
                } else if space {
                    format!(" {raw}")
                } else {
                    raw
                };
                pad_str(signed, width, left, zero && prec.is_none())
            }
            'u' => {
                let n = fmt_int(v)? as u64;
                check_flags("-0", true)?;
                let mut raw = n.to_string();
                if let Some(p) = prec {
                    if p == 0 && n == 0 {
                        raw.clear();
                    }
                    while raw.len() < p {
                        raw.insert(0, '0');
                    }
                }
                pad_str(raw, width, left, zero && prec.is_none())
            }
            'x' | 'X' | 'o' => {
                let n = fmt_int(v)? as u64;
                check_flags("-#0", true)?;
                let mut raw = match spec {
                    'x' => format!("{n:x}"),
                    'X' => format!("{n:X}"),
                    _ => format!("{n:o}"),
                };
                if let Some(p) = prec {
                    if p == 0 && n == 0 {
                        raw.clear();
                    }
                    while raw.len() < p {
                        raw.insert(0, '0');
                    }
                }
                // For octal, '#' forces a leading zero.
                if alt && spec == 'o' && !raw.starts_with('0') {
                    raw = format!("0{raw}");
                }
                let prefix = if alt && n != 0 {
                    match spec {
                        'x' => "0x",
                        'X' => "0X",
                        _ => "",
                    }
                } else {
                    ""
                };
                // Zero-padding goes after the base prefix: %#08x → 0x0000ff.
                if zero && prec.is_none() && !left && raw.len() + prefix.len() < width {
                    let zeros = width - prefix.len() - raw.len();
                    raw = format!("{}{raw}", "0".repeat(zeros));
                }
                pad_str(format!("{prefix}{raw}"), width, left, zero && prec.is_none() && prefix.is_empty())
            }
            'c' => {
                let n = fmt_int(v)?;
                check_flags("-", false)?;
                // C's %c takes the int mod 256; bytes >= 0x80 become the
                // corresponding UTF-8 char (documented UTF-8 difference).
                pad_str((n as u8 as char).to_string(), width, left, false)
            }
            'f' => {
                let f = fmt_num(v)?;
                check_flags("-+#0 ", true)?;
                let mut body = format!("{:.*}", prec.unwrap_or(6), f.abs());
                if alt && !body.contains('.') {
                    body.push('.');
                }
                pad_str(float_body(f, body), width, left, zero && f.is_finite())
            }
            'e' | 'E' => {
                let f = fmt_num(v)?;
                check_flags("-+#0 ", true)?;
                let mut body = fmt_e(f.abs(), prec.unwrap_or(6), spec == 'E');
                if alt {
                    let epos = body.find(['e', 'E']).unwrap_or(body.len());
                    if !body[..epos].contains('.') {
                        body.insert(epos, '.');
                    }
                }
                pad_str(float_body(f, body), width, left, zero && f.is_finite())
            }
            'g' | 'G' => {
                let f = fmt_num(v)?;
                check_flags("-+#0 ", true)?;
                let body = fmt_g(f.abs(), prec.unwrap_or(6), spec == 'G', alt);
                pad_str(float_body(f, body), width, left, zero && f.is_finite())
            }
            'a' | 'A' => {
                let f = fmt_num(v)?;
                check_flags("-+#0 ", true)?;
                let mut body = fmt_a(f.abs(), prec, spec == 'A');
                if alt && !body.contains('.') {
                    let ppos = body.find(['p', 'P']).unwrap_or(body.len());
                    body.insert(ppos, '.');
                }
                pad_str(float_body(f, body), width, left, zero && f.is_finite())
            }
            's' => {
                let raw = tostring_value(vm, v)?;
                let has_mods = left || plus || zero || space || alt || width > 0 || prec.is_some();
                if has_mods {
                    if raw.contains('\0') {
                        return Err(num_err("string contains zeros"));
                    }
                    check_flags("-", true)?;
                }
                // Precision counts bytes like C; cut at a char boundary so
                // the result stays valid UTF-8.
                let raw = if let Some(p) = prec {
                    let mut cut = p.min(raw.len());
                    while !raw.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    raw[..cut].to_owned()
                } else {
                    raw
                };
                pad_str(raw, width, left, false)
            }
            'q' => {
                if left || plus || zero || space || alt || width > 0 || prec.is_some() {
                    return Err(VmError::RuntimeError("specifier '%q' cannot have modifiers".into()));
                }
                if v.is_int_like() {
                    let n = v.as_int().unwrap();
                    // LUA_MININTEGER can't be negated for decimal printing;
                    // Lua emits it as hex.
                    if n == i64::MIN { "0x8000000000000000".into() } else { n.to_string() }
                } else if v.is_float() {
                    let f = v.as_float().unwrap();
                    // Lua quotes floats as %a hex so they re-read exactly.
                    if f.is_infinite() {
                        if f > 0.0 { "1e9999".into() } else { "-1e9999".into() }
                    } else if f.is_nan() {
                        "(0/0)".into()
                    } else {
                        let body = fmt_a(f.abs(), None, false);
                        if f.is_sign_negative() { format!("-{body}") } else { body }
                    }
                } else if v.is_string() {
                    let sb = vm.gc.str(v).as_bytes();
                    let mut q: Vec<u8> = Vec::with_capacity(sb.len() + 2);
                    q.push(b'"');
                    for (i, &b) in sb.iter().enumerate() {
                        match b {
                            b'"' | b'\\' | b'\n' => {
                                q.push(b'\\');
                                q.push(b);
                            }
                            _ if b < 0x20 || b == 0x7f => {
                                // \ddd only when a digit follows (else \d).
                                if i + 1 < sb.len() && sb[i + 1].is_ascii_digit() {
                                    q.extend_from_slice(format!("\\{b:03}").as_bytes());
                                } else {
                                    q.extend_from_slice(format!("\\{b}").as_bytes());
                                }
                            }
                            _ => q.push(b),
                        }
                    }
                    q.push(b'"');
                    // Valid UTF-8 in, ASCII escapes out: still valid UTF-8.
                    String::from_utf8(q).unwrap()
                } else if v.is_nil() || v.is_bool() {
                    format!("{v}")
                } else {
                    return Err(num_err("value has no literal form"));
                }
            }
            'p' => {
                check_flags("-", false)?;
                match v.identity() {
                    Some(addr) => pad_str(format!("{addr:#x}"), width, left, false),
                    None => pad_str("(null)".into(), width, left, false),
                }
            }
            _ => return Err(VmError::RuntimeError(format!("invalid option '%{spec}' to 'format'"))),
        };
        out.push_str(&s);
    }
    Ok(vec![vm.alloc_str(&out)])
}
