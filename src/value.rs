/// NaN-boxed 64-bit value.
///
/// Layout when in NaN space (top 13 bits all set):
///
///   [63..51 = 1111_1111_1111_1]  NaN signature  (13 bits)
///   [50..48]                     type tag        (3 bits)
///   [47..0]                      payload         (48 bits)
///
/// Floats that are not NaN pass through bit-for-bit unchanged.
/// Heap pointers are assumed to fit in 48 bits (a 4-level paging user-space
/// address; bit 47 is used to tag C functions, see Vm::make_cfn_val).
/// Integers outside INLINE_INT_MIN..=INLINE_INT_MAX don't fit the 48-bit
/// payload; those are heap-boxed as a plain i64 under TAG_BIGINT instead
/// (see Vm::make_int), so the full i64 range round-trips correctly.

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Value(u64);

const NAN_BITS:     u64 = 0xFFF8_0000_0000_0000;
const TAG_SHIFT:    u32 = 48;
const PAYLOAD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

const TAG_INT:       u64 = 0;
const TAG_MISC:      u64 = 1;
const TAG_TABLE:     u64 = 2;
const TAG_STRING:    u64 = 3;
const TAG_USERDATA:  u64 = 4;
const TAG_COROUTINE: u64 = 5;
const TAG_CLOSURE:   u64 = 6;
const TAG_BIGINT:    u64 = 7;

// The 48-bit payload can't hold every i64: values outside this range are
// heap-boxed as a plain `Box<i64>` (see Value::bigint) instead of truncated.
pub const INLINE_INT_MIN: i64 = -(1i64 << 47);
pub const INLINE_INT_MAX: i64 = (1i64 << 47) - 1;

const MISC_NIL:   u64 = 0;
const MISC_FALSE: u64 = 1;
const MISC_TRUE:  u64 = 2;

impl Value {
    #[inline(always)]
    fn tagged(tag: u64, payload: u64) -> Self {
        Self(NAN_BITS | (tag << TAG_SHIFT) | (payload & PAYLOAD_MASK))
    }

    #[inline(always)]
    pub fn nil() -> Self { Self::tagged(TAG_MISC, MISC_NIL) }

    #[inline(always)]
    pub fn bool(b: bool) -> Self {
        Self::tagged(TAG_MISC, if b { MISC_TRUE } else { MISC_FALSE })
    }

    /// Fast inline path — only valid for INLINE_INT_MIN..=INLINE_INT_MAX;
    /// callers with an arbitrary i64 must use a GC-aware bigint-boxing
    /// constructor instead (see Vm::make_int) or values silently truncate.
    #[inline(always)]
    pub fn int(n: i64) -> Self {
        Self::tagged(TAG_INT, n as u64)
    }

    #[inline(always)]
    pub fn bigint(ptr: *mut u8) -> Self {
        Self::tagged(TAG_BIGINT, ptr as u64)
    }

    #[inline(always)]
    pub fn float(f: f64) -> Self {
        let bits = f.to_bits();
        // A real NaN's bit pattern would otherwise collide with the tagged-value space.
        if f.is_nan() { Self::nil() } else { Self(bits) }
    }

    #[inline(always)]
    pub fn table(ptr: *mut u8) -> Self {
        Self::tagged(TAG_TABLE, ptr as u64)
    }

    #[inline(always)]
    pub fn string(ptr: *mut u8) -> Self {
        Self::tagged(TAG_STRING, ptr as u64)
    }

    #[inline(always)]
    pub fn userdata(ptr: *mut u8) -> Self {
        Self::tagged(TAG_USERDATA, ptr as u64)
    }

    #[inline(always)]
    pub fn coroutine(ptr: *mut u8) -> Self {
        Self::tagged(TAG_COROUTINE, ptr as u64)
    }

    #[inline(always)]
    pub fn closure(ptr: *mut u8) -> Self {
        Self::tagged(TAG_CLOSURE, ptr as u64)
    }

    #[inline(always)]
    fn is_nan_boxed(self) -> bool {
        (self.0 & NAN_BITS) == NAN_BITS
    }

    #[inline(always)]
    fn tag(self) -> u64 {
        (self.0 >> TAG_SHIFT) & 0x7
    }

    #[inline(always)]
    pub fn is_nil(self)       -> bool { self.is_nan_boxed() && self.tag() == TAG_MISC && (self.0 & PAYLOAD_MASK) == MISC_NIL }
    #[inline(always)]
    pub fn is_bool(self)      -> bool { self.is_nan_boxed() && self.tag() == TAG_MISC && (self.0 & PAYLOAD_MASK) != MISC_NIL }
    #[inline(always)]
    pub fn is_int(self)       -> bool { self.is_nan_boxed() && self.tag() == TAG_INT }
    #[inline(always)]
    pub fn is_bigint(self)    -> bool { self.is_nan_boxed() && self.tag() == TAG_BIGINT }
    /// True for either integer representation — almost always what callers
    /// outside this module actually want ("is this logically an integer"),
    /// as opposed to is_int() which only means "uses the fast inline form".
    #[inline(always)]
    pub fn is_int_like(self)  -> bool { self.is_int() || self.is_bigint() }
    #[inline(always)]
    pub fn is_float(self)     -> bool { !self.is_nan_boxed() }
    #[inline(always)]
    pub fn is_number(self)    -> bool { self.is_int_like() || self.is_float() }
    #[inline(always)]
    pub fn is_table(self)     -> bool { self.is_nan_boxed() && self.tag() == TAG_TABLE }
    #[inline(always)]
    pub fn is_string(self)    -> bool { self.is_nan_boxed() && self.tag() == TAG_STRING }
    #[inline(always)]
    pub fn is_userdata(self)  -> bool { self.is_nan_boxed() && self.tag() == TAG_USERDATA }
    #[inline(always)]
    pub fn is_coroutine(self) -> bool { self.is_nan_boxed() && self.tag() == TAG_COROUTINE }
    #[inline(always)]
    pub fn is_closure(self)   -> bool { self.is_nan_boxed() && self.tag() == TAG_CLOSURE }

    #[inline(always)]
    pub fn as_bool(self) -> Option<bool> {
        if self.is_nil()  { return Some(false); }
        if !self.is_bool() { return None; }
        Some((self.0 & PAYLOAD_MASK) == MISC_TRUE)
    }

    #[inline(always)]
    pub fn is_truthy(self) -> bool {
        if self.is_nil() { return false; }
        if self.is_bool() { return (self.0 & PAYLOAD_MASK) == MISC_TRUE; }
        true
    }

    #[inline(always)]
    pub fn as_int(self) -> Option<i64> {
        if self.is_int() {
            let raw = (self.0 & PAYLOAD_MASK) as i64;
            return Some((raw << 16) >> 16);
        }
        if self.is_bigint() {
            let ptr = (self.0 & PAYLOAD_MASK) as *const i64;
            return Some(unsafe { *ptr });
        }
        None
    }

    #[inline(always)]
    pub fn as_float(self) -> Option<f64> {
        if !self.is_float() { return None; }
        Some(f64::from_bits(self.0))
    }

    #[inline(always)]
    pub fn to_float(self) -> Option<f64> {
        if self.is_float() { return Some(f64::from_bits(self.0)); }
        self.as_int().map(|n| n as f64)
    }

    #[inline(always)]
    pub fn as_bigint(self) -> Option<*mut u8> {
        if !self.is_bigint() { return None; }
        Some((self.0 & PAYLOAD_MASK) as *mut u8)
    }

    #[inline(always)]
    pub fn as_table(self) -> Option<*mut u8> {
        if !self.is_table() { return None; }
        Some((self.0 & PAYLOAD_MASK) as *mut u8)
    }

    #[inline(always)]
    pub fn as_string(self) -> Option<*mut u8> {
        if !self.is_string() { return None; }
        Some((self.0 & PAYLOAD_MASK) as *mut u8)
    }

    #[inline(always)]
    pub fn as_userdata(self) -> Option<*mut u8> {
        if !self.is_userdata() { return None; }
        Some((self.0 & PAYLOAD_MASK) as *mut u8)
    }

    #[inline(always)]
    pub fn as_coroutine(self) -> Option<*mut u8> {
        if !self.is_coroutine() { return None; }
        Some((self.0 & PAYLOAD_MASK) as *mut u8)
    }

    #[inline(always)]
    pub fn as_closure(self) -> Option<*mut u8> {
        if !self.is_closure() { return None; }
        Some((self.0 & PAYLOAD_MASK) as *mut u8)
    }

    #[inline(always)]
    pub fn raw_bits(self) -> u64 { self.0 }

    #[inline(always)]
    pub fn from_raw(bits: u64) -> Self { Self(bits) }

    pub fn type_name(self) -> &'static str {
        if self.is_nil()       { "nil" }
        else if self.is_bool() { "boolean" }
        else if self.is_int_like() { "integer" }
        else if self.is_float(){ "float" }
        else if self.is_string(){ "string" }
        else if self.is_table()  { "table" }
        else if self.is_coroutine() { "coroutine" }
        else if self.is_userdata() || self.is_closure() { "function" }
        else { "unknown" }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self.is_int_like(), other.is_int_like(), self.is_float(), other.is_float()) {
            (true, true, _, _) => self.as_int().unwrap() == other.as_int().unwrap(),
            (true, false, _, true) =>
                cmp_int_float(self.as_int().unwrap(), other.as_float().unwrap()) == Some(Ordering::Equal),
            (false, true, true, _) =>
                cmp_int_float(other.as_int().unwrap(), self.as_float().unwrap()) == Some(Ordering::Equal),
            (false, false, true, true) => self.as_float().unwrap() == other.as_float().unwrap(),
            _ => self.0 == other.0,
        }
    }
}

use std::cmp::Ordering;

// Exact ordering of an i64 against an f64, without rounding the integer
// through f64 first (which would make i64::MAX == 2^63).
pub fn cmp_int_float(i: i64, f: f64) -> Option<Ordering> {
    if f.is_nan() { return None; }
    if f >= 9223372036854775808.0 { return Some(Ordering::Less); }
    if f < -9223372036854775808.0 { return Some(Ordering::Greater); }
    let fl = f.floor() as i64;
    match i.cmp(&fl) {
        Ordering::Equal => Some(if f > fl as f64 { Ordering::Less } else { Ordering::Equal }),
        ord => Some(ord),
    }
}

/// Lua's LUAI_NUMFFORMAT "%.14g" plus the ".0" suffix Lua appends to floats
/// that print without a fraction or exponent (so tostring(3.0) == "3.0").
pub fn lua_float_str(f: f64) -> String {
    if f.is_infinite() { return if f > 0.0 { "inf".into() } else { "-inf".into() }; }
    if f.is_nan() { return "nan".into(); }
    if f == 0.0 { return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() }; }
    // Round to 14 significant digits, then pick fixed vs scientific the way
    // C's %g does: scientific iff the decimal exponent is < -4 or >= 14.
    let e = format!("{:.13e}", f);
    let exp: i32 = e[e.rfind('e').unwrap() + 1..].parse().unwrap();
    let mut s = if (-4..14).contains(&exp) {
        let decimals = (13 - exp).max(0) as usize;
        let mut t = format!("{:.*}", decimals, f);
        if t.contains('.') {
            while t.ends_with('0') { t.pop(); }
            if t.ends_with('.') { t.pop(); }
        }
        t
    } else {
        let t = e;
        let epos = t.rfind('e').unwrap();
        let mut mantissa: String = t[..epos].into();
        while mantissa.ends_with('0') { mantissa.pop(); }
        if mantissa.ends_with('.') { mantissa.pop(); }
        format!("{}e{}{:02}", mantissa, if exp < 0 { "-" } else { "+" }, exp.abs())
    };
    if !s.contains(['.', 'e', 'n']) { s.push_str(".0"); }
    s
}

impl Eq for Value {}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_nil()        { write!(f, "nil") }
        else if self.is_bool()  { write!(f, "{}", (self.0 & PAYLOAD_MASK) == MISC_TRUE) }
        else if self.is_int_like() { write!(f, "{}i", self.as_int().unwrap()) }
        else if self.is_float() { write!(f, "{}f", self.as_float().unwrap()) }
        else if self.is_string()    { write!(f, "string({:x})", self.0 & PAYLOAD_MASK) }
        else if self.is_table()     { write!(f, "table({:x})", self.0 & PAYLOAD_MASK) }
        else if self.is_userdata()  { write!(f, "function({:x})", self.0 & PAYLOAD_MASK) }
        else if self.is_coroutine() { write!(f, "coroutine({:x})", self.0 & PAYLOAD_MASK) }
        else if self.is_closure()   { write!(f, "function({:x})", self.0 & PAYLOAD_MASK) }
        else { write!(f, "Value({:#018x})", self.0) }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_nil()        { write!(f, "nil") }
        else if self.is_bool()  { write!(f, "{}", (self.0 & PAYLOAD_MASK) == MISC_TRUE) }
        else if self.is_int_like() { write!(f, "{}", self.as_int().unwrap()) }
        else if self.is_float() { write!(f, "{}", lua_float_str(self.as_float().unwrap())) }
        else { write!(f, "{}", self.type_name()) }
    }
}
