// Lua-style string.pack/string.unpack: a subset of Lua 5.3/5.4's format
// language covering the formats scripts actually reach for (fixed-width
// integers, floats, fixed and length-prefixed strings, padding, endianness).
// Not covered: alignment ('!' is parsed and ignored) and platform-native
// size/endianness detection (native '=' is treated as little-endian).

#[derive(Debug, Clone)]
pub enum PackValue {
    Int(i64),
    Float(f64),
    Str(Vec<u8>),
}

fn as_int(v: &PackValue) -> Result<i64, String> {
    match v {
        PackValue::Int(n) => Ok(*n),
        PackValue::Float(f) => Ok(*f as i64),
        PackValue::Str(_) => Err("string.pack: number expected".to_string()),
    }
}

fn as_float(v: &PackValue) -> Result<f64, String> {
    match v {
        PackValue::Int(n) => Ok(*n as f64),
        PackValue::Float(f) => Ok(*f),
        PackValue::Str(_) => Err("string.pack: number expected".to_string()),
    }
}

fn as_bytes(v: &PackValue) -> Result<&[u8], String> {
    match v {
        PackValue::Str(s) => Ok(s),
        _ => Err("string.pack: string expected".to_string()),
    }
}

fn read_size(fmt: &[u8], i: &mut usize, default: usize) -> usize {
    let start = *i;
    while *i < fmt.len() && fmt[*i].is_ascii_digit() { *i += 1; }
    if *i == start { default } else {
        std::str::from_utf8(&fmt[start..*i]).unwrap().parse().unwrap_or(default)
    }
}

fn read_required_size(fmt: &[u8], i: &mut usize, opt: char) -> Result<usize, String> {
    let start = *i;
    while *i < fmt.len() && fmt[*i].is_ascii_digit() { *i += 1; }
    if *i == start { return Err(format!("string.pack: missing size for '{opt}'")); }
    std::str::from_utf8(&fmt[start..*i]).unwrap().parse()
        .map_err(|_| format!("string.pack: bad size for '{opt}'"))
}

fn push_int_bytes(out: &mut Vec<u8>, n: i64, size: usize, little: bool) -> Result<(), String> {
    if size == 0 || size > 8 { return Err(format!("string.pack: unsupported integer size {size}")); }
    let full = n.to_le_bytes();
    if little {
        out.extend_from_slice(&full[..size]);
    } else {
        out.extend(full[..size].iter().rev());
    }
    Ok(())
}

pub fn pack(fmt: &str, args: &[PackValue]) -> Result<Vec<u8>, String> {
    let f = fmt.as_bytes();
    let mut i = 0;
    let mut little = true;
    let mut arg_i = 0;
    let mut out = Vec::new();

    macro_rules! next_arg {
        () => {{
            let v = args.get(arg_i).ok_or_else(|| "string.pack: not enough arguments".to_string())?;
            arg_i += 1;
            v
        }};
    }

    while i < f.len() {
        let c = f[i]; i += 1;
        match c {
            b' ' => {}
            b'<' => little = true,
            b'>' => little = false,
            b'=' => little = true,
            b'!' => { while i < f.len() && f[i].is_ascii_digit() { i += 1; } }
            b'b' | b'B' => push_int_bytes(&mut out, as_int(next_arg!())?, 1, little)?,
            b'h' | b'H' => push_int_bytes(&mut out, as_int(next_arg!())?, 2, little)?,
            b'i' | b'I' => {
                let size = read_size(f, &mut i, 4);
                push_int_bytes(&mut out, as_int(next_arg!())?, size, little)?;
            }
            b'l' | b'L' => push_int_bytes(&mut out, as_int(next_arg!())?, 8, little)?,
            b'f' => {
                let v = as_float(next_arg!())? as f32;
                out.extend_from_slice(&if little { v.to_le_bytes() } else { v.to_be_bytes() });
            }
            b'd' => {
                let v = as_float(next_arg!())?;
                out.extend_from_slice(&if little { v.to_le_bytes() } else { v.to_be_bytes() });
            }
            b'c' => {
                let n = read_required_size(f, &mut i, 'c')?;
                let s = as_bytes(next_arg!())?;
                if s.len() > n { return Err(format!("string.pack: string longer than 'c{n}'")); }
                out.extend_from_slice(s);
                out.resize(out.len() + (n - s.len()), 0);
            }
            b's' => {
                let size = read_size(f, &mut i, 8);
                let s = as_bytes(next_arg!())?.to_vec();
                push_int_bytes(&mut out, s.len() as i64, size, little)?;
                out.extend_from_slice(&s);
            }
            b'x' => out.push(0),
            _ => return Err(format!("string.pack: invalid format option '{}'", c as char)),
        }
    }
    Ok(out)
}

fn read_bytes<'a>(data: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8], String> {
    if pos.checked_add(n).is_none_or(|end| end > data.len()) {
        return Err("string.unpack: data string too short".to_string());
    }
    let s = &data[*pos..*pos + n];
    *pos += n;
    Ok(s)
}

fn read_int(data: &[u8], pos: &mut usize, size: usize, signed: bool, little: bool) -> Result<i64, String> {
    if size == 0 || size > 8 { return Err(format!("string.unpack: unsupported integer size {size}")); }
    let bs = read_bytes(data, pos, size)?;
    let mut buf = [0u8; 8];
    let sign_byte;
    if little {
        buf[..size].copy_from_slice(bs);
        sign_byte = bs[size - 1];
    } else {
        for (idx, &b) in bs.iter().enumerate() { buf[size - 1 - idx] = b; }
        sign_byte = bs[0];
    }
    if signed && size < 8 && (sign_byte & 0x80) != 0 {
        for b in &mut buf[size..] { *b = 0xFF; }
    }
    Ok(i64::from_le_bytes(buf))
}

/// Returns the unpacked values and the byte position just past the last one
/// consumed (0-based, matching `start`).
pub fn unpack(fmt: &str, data: &[u8], start: usize) -> Result<(Vec<PackValue>, usize), String> {
    let f = fmt.as_bytes();
    let mut i = 0;
    let mut little = true;
    let mut pos = start;
    let mut results = Vec::new();

    while i < f.len() {
        let c = f[i]; i += 1;
        match c {
            b' ' => {}
            b'<' => little = true,
            b'>' => little = false,
            b'=' => little = true,
            b'!' => { while i < f.len() && f[i].is_ascii_digit() { i += 1; } }
            b'b' => results.push(PackValue::Int(read_int(data, &mut pos, 1, true, little)?)),
            b'B' => results.push(PackValue::Int(read_int(data, &mut pos, 1, false, little)?)),
            b'h' => results.push(PackValue::Int(read_int(data, &mut pos, 2, true, little)?)),
            b'H' => results.push(PackValue::Int(read_int(data, &mut pos, 2, false, little)?)),
            b'i' => {
                let size = read_size(f, &mut i, 4);
                results.push(PackValue::Int(read_int(data, &mut pos, size, true, little)?));
            }
            b'I' => {
                let size = read_size(f, &mut i, 4);
                results.push(PackValue::Int(read_int(data, &mut pos, size, false, little)?));
            }
            b'l' => results.push(PackValue::Int(read_int(data, &mut pos, 8, true, little)?)),
            b'L' => results.push(PackValue::Int(read_int(data, &mut pos, 8, false, little)?)),
            b'f' => {
                let bs = read_bytes(data, &mut pos, 4)?;
                let arr: [u8; 4] = bs.try_into().unwrap();
                let v = if little { f32::from_le_bytes(arr) } else { f32::from_be_bytes(arr) };
                results.push(PackValue::Float(v as f64));
            }
            b'd' => {
                let bs = read_bytes(data, &mut pos, 8)?;
                let arr: [u8; 8] = bs.try_into().unwrap();
                let v = if little { f64::from_le_bytes(arr) } else { f64::from_be_bytes(arr) };
                results.push(PackValue::Float(v));
            }
            b'c' => {
                let n = read_required_size(f, &mut i, 'c')?;
                results.push(PackValue::Str(read_bytes(data, &mut pos, n)?.to_vec()));
            }
            b's' => {
                let size = read_size(f, &mut i, 8);
                let len = read_int(data, &mut pos, size, false, little)? as usize;
                results.push(PackValue::Str(read_bytes(data, &mut pos, len)?.to_vec()));
            }
            b'x' => { read_bytes(data, &mut pos, 1)?; }
            _ => return Err(format!("string.unpack: invalid format option '{}'", c as char)),
        }
    }
    Ok((results, pos))
}
