// Lua 5.4 string.pack/string.unpack: the full format language — fixed-width
// and native-size integers, floats, fixed-length/length-prefixed/zero-
// terminated strings, padding, endianness ('<', '>', '=') and alignment
// ('!n', 'Xop'). Native sizes match Lua on a 64-bit build: int 4, long/
// lua_Integer/size_t 8, float 4, double/lua_Number 8, native alignment 8.

// Widest integer size the format accepts ('i16', 's16', '!16').
const MAXINTSIZE: usize = 16;
// offsetof(struct { char c; union { maxalign_t } u; }, u) on x86-64.
const NATIVE_ALIGN: usize = 8;
const NATIVE_LITTLE: bool = true;

#[derive(Debug, Clone)]
pub enum PackValue {
    Int(i64),
    Float(f64),
    Str(Vec<u8>),
}

fn as_int(v: &PackValue) -> Result<i64, String> {
    match v {
        PackValue::Int(n) => Ok(*n),
        PackValue::Float(f) if f.fract() == 0.0
            && *f >= -9223372036854775808.0 && *f < 9223372036854775808.0 => Ok(*f as i64),
        PackValue::Float(_) => Err("number has no integer representation".to_string()),
        PackValue::Str(_) => Err("number expected".to_string()),
    }
}

fn as_float(v: &PackValue) -> Result<f64, String> {
    match v {
        PackValue::Int(n) => Ok(*n as f64),
        PackValue::Float(f) => Ok(*f),
        PackValue::Str(_) => Err("number expected".to_string()),
    }
}

fn as_bytes(v: &PackValue) -> Result<&[u8], String> {
    match v {
        PackValue::Str(s) => Ok(s),
        _ => Err("string expected".to_string()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Opt {
    Int,      // b h l j i[n]
    Uint,     // B H L J T I[n]
    F32,      // f
    F64,      // d n
    Chars,    // c[n]
    LenStr,   // s[n]
    ZStr,     // z
    Pad,      // x
    PadAlign, // X<op>
    Nop,      // space, < > = !
}

struct Header {
    little: bool,
    maxalign: usize,
}

// getnum: digits or the default; saturates so an overflowing numeral fails
// the caller's range check instead of wrapping.
fn read_num(fmt: &[u8], i: &mut usize, default: usize) -> usize {
    let start = *i;
    let mut n: usize = 0;
    while *i < fmt.len() && fmt[*i].is_ascii_digit() {
        n = n.saturating_mul(10).saturating_add((fmt[*i] - b'0') as usize);
        *i += 1;
    }
    if *i == start { default } else { n }
}

// getnumlimit: sizes for i/I/s/! must be in 1..=MAXINTSIZE.
fn read_num_limit(fmt: &[u8], i: &mut usize, default: usize, who: &str) -> Result<usize, String> {
    let n = read_num(fmt, i, default);
    if n == 0 || n > MAXINTSIZE {
        return Err(format!("{who}: integral size ({n}) out of limits [1,{MAXINTSIZE}]"));
    }
    Ok(n)
}

fn getoption(h: &mut Header, fmt: &[u8], i: &mut usize, who: &str) -> Result<(Opt, usize), String> {
    let c = fmt[*i];
    *i += 1;
    let (opt, size) = match c {
        b'b' => (Opt::Int, 1),
        b'B' => (Opt::Uint, 1),
        b'h' => (Opt::Int, 2),
        b'H' => (Opt::Uint, 2),
        b'l' => (Opt::Int, 8),
        b'L' => (Opt::Uint, 8),
        b'j' => (Opt::Int, 8),
        b'J' => (Opt::Uint, 8),
        b'T' => (Opt::Uint, 8),
        b'f' => (Opt::F32, 4),
        b'n' | b'd' => (Opt::F64, 8),
        b'i' => (Opt::Int, read_num_limit(fmt, i, 4, who)?),
        b'I' => (Opt::Uint, read_num_limit(fmt, i, 4, who)?),
        b's' => (Opt::LenStr, read_num_limit(fmt, i, 8, who)?),
        b'c' => {
            let start = *i;
            let n = read_num(fmt, i, 0);
            if *i == start {
                return Err(format!("{who}: missing size for format option 'c'"));
            }
            (Opt::Chars, n)
        }
        b'z' => (Opt::ZStr, 0),
        b'x' => (Opt::Pad, 1),
        b'X' => (Opt::PadAlign, 0),
        b' ' => (Opt::Nop, 0),
        b'<' => { h.little = true; (Opt::Nop, 0) }
        b'>' => { h.little = false; (Opt::Nop, 0) }
        b'=' => { h.little = NATIVE_LITTLE; (Opt::Nop, 0) }
        b'!' => { h.maxalign = read_num_limit(fmt, i, NATIVE_ALIGN, who)?; (Opt::Nop, 0) }
        _ => return Err(format!("{who}: invalid format option '{}'", c as char)),
    };
    Ok((opt, size))
}

// getdetails: the option, its size, and the padding needed to align it.
// Alignment follows size, capped at maxalign; 'X' takes its alignment from
// the option that follows it (which is consumed but packs/skips nothing).
fn getdetails(h: &mut Header, fmt: &[u8], i: &mut usize, totalsize: usize, who: &str)
    -> Result<(Opt, usize, usize), String>
{
    let (opt, size) = getoption(h, fmt, i, who)?;
    let mut align = size;
    if opt == Opt::PadAlign {
        if *i >= fmt.len() {
            return Err(format!("{who}: invalid next option for option 'X'"));
        }
        let (next, next_size) = getoption(h, fmt, i, who)?;
        if next == Opt::Chars || next_size == 0 {
            return Err(format!("{who}: invalid next option for option 'X'"));
        }
        align = next_size;
    }
    let ntoalign = if align <= 1 || opt == Opt::Chars {
        0
    } else {
        if align > h.maxalign { align = h.maxalign; }
        if align & (align - 1) != 0 {
            return Err(format!("{who}: format asks for alignment not power of 2"));
        }
        (align - (totalsize & (align - 1))) & (align - 1)
    };
    Ok((opt, size, ntoalign))
}

// packint: low 8 bytes of n in the chosen endianness; bytes past 8 are the
// sign extension for negative signed values, zero otherwise.
fn push_int(out: &mut Vec<u8>, n: i64, size: usize, little: bool, neg: bool) {
    let b = n.to_le_bytes();
    let at = |k: usize| if k < 8 { b[k] } else if neg { 0xFF } else { 0 };
    if little {
        for k in 0..size { out.push(at(k)); }
    } else {
        for k in (0..size).rev() { out.push(at(k)); }
    }
}

pub fn pack(fmt: &str, args: &[PackValue]) -> Result<Vec<u8>, String> {
    const WHO: &str = "string.pack";
    let f = fmt.as_bytes();
    let mut h = Header { little: NATIVE_LITTLE, maxalign: 1 };
    let mut i = 0;
    let mut arg_i = 0;
    let mut out = Vec::new();

    macro_rules! next_arg {
        () => {{
            let v = args.get(arg_i).ok_or_else(|| format!("{WHO}: too few arguments"))?;
            arg_i += 1;
            v
        }};
    }

    while i < f.len() {
        let (opt, size, ntoalign) = getdetails(&mut h, f, &mut i, out.len(), WHO)?;
        out.resize(out.len() + ntoalign, 0);
        match opt {
            Opt::Int => {
                let n = as_int(next_arg!()).map_err(|e| format!("{WHO}: {e}"))?;
                if size < 8 {
                    let lim = 1i64 << (size * 8 - 1);
                    if n < -lim || n >= lim {
                        return Err(format!("{WHO}: integer overflow"));
                    }
                }
                push_int(&mut out, n, size, h.little, n < 0);
            }
            Opt::Uint => {
                let n = as_int(next_arg!()).map_err(|e| format!("{WHO}: {e}"))?;
                if size < 8 && (n as u64) >= (1u64 << (size * 8)) {
                    return Err(format!("{WHO}: unsigned overflow"));
                }
                push_int(&mut out, n, size, h.little, false);
            }
            Opt::F32 => {
                let v = as_float(next_arg!()).map_err(|e| format!("{WHO}: {e}"))? as f32;
                out.extend_from_slice(&if h.little { v.to_le_bytes() } else { v.to_be_bytes() });
            }
            Opt::F64 => {
                let v = as_float(next_arg!()).map_err(|e| format!("{WHO}: {e}"))?;
                out.extend_from_slice(&if h.little { v.to_le_bytes() } else { v.to_be_bytes() });
            }
            Opt::Chars => {
                if size > crate::vm::MAX_ALLOC_LEN {
                    return Err(format!("{WHO}: 'c{size}' too large"));
                }
                let s = as_bytes(next_arg!()).map_err(|e| format!("{WHO}: {e}"))?;
                if s.len() > size {
                    return Err(format!("{WHO}: string longer than given size"));
                }
                out.extend_from_slice(s);
                out.resize(out.len() + (size - s.len()), 0);
            }
            Opt::LenStr => {
                let s = as_bytes(next_arg!()).map_err(|e| format!("{WHO}: {e}"))?;
                if size < 8 && s.len() as u64 >= (1u64 << (size * 8)) {
                    return Err(format!("{WHO}: string length does not fit in given size"));
                }
                push_int(&mut out, s.len() as i64, size, h.little, false);
                out.extend_from_slice(s);
            }
            Opt::ZStr => {
                let s = as_bytes(next_arg!()).map_err(|e| format!("{WHO}: {e}"))?;
                if s.contains(&0) {
                    return Err(format!("{WHO}: string contains zeros"));
                }
                out.extend_from_slice(s);
                out.push(0);
            }
            Opt::Pad => out.push(0),
            Opt::PadAlign | Opt::Nop => {}
        }
    }
    Ok(out)
}

// unpackint: low min(size,8) bytes become the value; a signed value under 8
// bytes sign-extends, and bytes past 8 must all match the sign extension.
fn read_int(data: &[u8], pos: usize, size: usize, signed: bool, little: bool) -> Result<i64, String> {
    let bs = &data[pos..pos + size];
    let limit = size.min(8);
    let mut res: u64 = 0;
    for k in (0..limit).rev() {
        res <<= 8;
        res |= bs[if little { k } else { size - 1 - k }] as u64;
    }
    if size < 8 {
        if signed {
            let mask = 1u64 << (size * 8 - 1);
            res = (res ^ mask).wrapping_sub(mask);
        }
    } else if size > 8 {
        let ext = if !signed || (res as i64) >= 0 { 0x00 } else { 0xFF };
        for k in 8..size {
            if bs[if little { k } else { size - 1 - k }] != ext {
                return Err(format!("string.unpack: {size}-byte integer does not fit into Lua Integer"));
            }
        }
    }
    Ok(res as i64)
}

// posrelatI: 1-based position; 0 and positions before the start clip to 1.
fn pos_relat(init: i64, len: usize) -> i64 {
    if init > 0 { init }
    else if init == 0 || -init > len as i64 { 1 }
    else { len as i64 + init + 1 }
}

/// Returns the unpacked values and the byte position just past the last one
/// consumed (0-based; the caller adds 1 for the 1-based result).
pub fn unpack(fmt: &str, data: &[u8], start: i64) -> Result<(Vec<PackValue>, usize), String> {
    const WHO: &str = "string.unpack";
    let pos0 = pos_relat(start, data.len());
    if pos0 > data.len() as i64 + 1 {
        return Err(format!("{WHO}: initial position out of string"));
    }
    let f = fmt.as_bytes();
    let mut h = Header { little: NATIVE_LITTLE, maxalign: 1 };
    let mut i = 0;
    let mut pos = (pos0 - 1) as usize;
    let mut results = Vec::new();

    while i < f.len() {
        let (opt, size, ntoalign) = getdetails(&mut h, f, &mut i, pos, WHO)?;
        if ntoalign + size > data.len() - pos {
            return Err(format!("{WHO}: data string too short"));
        }
        pos += ntoalign;
        match opt {
            Opt::Int => results.push(PackValue::Int(read_int(data, pos, size, true, h.little)?)),
            Opt::Uint => results.push(PackValue::Int(read_int(data, pos, size, false, h.little)?)),
            Opt::F32 => {
                let arr: [u8; 4] = data[pos..pos + 4].try_into().unwrap();
                let v = if h.little { f32::from_le_bytes(arr) } else { f32::from_be_bytes(arr) };
                results.push(PackValue::Float(v as f64));
            }
            Opt::F64 => {
                let arr: [u8; 8] = data[pos..pos + 8].try_into().unwrap();
                let v = if h.little { f64::from_le_bytes(arr) } else { f64::from_be_bytes(arr) };
                results.push(PackValue::Float(v));
            }
            Opt::Chars => results.push(PackValue::Str(data[pos..pos + size].to_vec())),
            Opt::LenStr => {
                let len = read_int(data, pos, size, false, h.little)? as usize;
                if len > data.len() - pos - size {
                    return Err(format!("{WHO}: data string too short"));
                }
                results.push(PackValue::Str(data[pos + size..pos + size + len].to_vec()));
                pos += len;
            }
            Opt::ZStr => {
                let end = data[pos..].iter().position(|&b| b == 0)
                    .map(|p| pos + p)
                    .ok_or_else(|| format!("{WHO}: unfinished string for format 'z'"))?;
                results.push(PackValue::Str(data[pos..end].to_vec()));
                pos += end - pos + 1;
            }
            Opt::Pad | Opt::PadAlign | Opt::Nop => {}
        }
        pos += size;
    }
    Ok((results, pos))
}

/// string.packsize: total bytes a format packs, or an error for the
/// variable-length options ('s', 'z').
pub fn packsize(fmt: &str) -> Result<usize, String> {
    const WHO: &str = "string.packsize";
    let f = fmt.as_bytes();
    let mut h = Header { little: NATIVE_LITTLE, maxalign: 1 };
    let mut i = 0;
    let mut total = 0usize;
    while i < f.len() {
        let (opt, size, ntoalign) = getdetails(&mut h, f, &mut i, total, WHO)?;
        if opt == Opt::LenStr || opt == Opt::ZStr {
            return Err(format!("{WHO}: variable-length format"));
        }
        total = total.checked_add(ntoalign + size)
            .ok_or_else(|| format!("{WHO}: format result too large"))?;
    }
    Ok(total)
}
