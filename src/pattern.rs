// Lua-style pattern matching (character classes, sets, quantifiers, anchors,
// captures, %b balanced match, %f frontier, %1-%9 backreferences). Operates
// on raw bytes, matching real Lua's own byte-oriented (not Unicode-aware)
// semantics — %a/%d/etc. only recognize ASCII, same as upstream.

const CAP_UNFINISHED: isize = -1;
const CAP_POSITION: isize = -2;
const MAX_CAPTURES: usize = 32;
const MAX_DEPTH: u32 = 200;

pub enum Capture {
    Str(usize, usize),
    Position(usize),
}

pub struct Match {
    pub start: usize,
    pub end: usize,
    pub captures: Vec<Capture>,
}

fn match_class(c: u8, cl: u8) -> bool {
    let res = match cl.to_ascii_lowercase() {
        b'a' => c.is_ascii_alphabetic(),
        b'd' => c.is_ascii_digit(),
        b'l' => c.is_ascii_lowercase(),
        b'u' => c.is_ascii_uppercase(),
        b's' => c.is_ascii_whitespace(),
        b'w' => c.is_ascii_alphanumeric(),
        b'c' => c.is_ascii_control(),
        b'p' => c.is_ascii_punctuation(),
        b'x' => c.is_ascii_hexdigit(),
        b'g' => c.is_ascii_graphic(),
        _ => return c == cl,
    };
    if cl.is_ascii_uppercase() { !res } else { res }
}

fn classend(pat: &[u8], p: usize) -> Result<usize, String> {
    let mut p = p;
    let c = *pat.get(p).ok_or("malformed pattern")?;
    p += 1;
    match c {
        b'%' => {
            if p >= pat.len() { return Err("malformed pattern (ends with '%')".into()); }
            Ok(p + 1)
        }
        b'[' => {
            if p < pat.len() && pat[p] == b'^' { p += 1; }
            loop {
                if p >= pat.len() { return Err("malformed pattern (missing ']')".into()); }
                let cc = pat[p]; p += 1;
                if cc == b'%' {
                    if p >= pat.len() { return Err("malformed pattern (ends with '%')".into()); }
                    p += 1;
                }
                if p < pat.len() && pat[p] == b']' { break; }
            }
            Ok(p + 1)
        }
        _ => Ok(p),
    }
}

fn match_bracket_class(c: u8, pat: &[u8], open: usize, close_excl: usize) -> bool {
    let mut i = open + 1;
    let end = close_excl - 1;
    let negate = i < end && pat[i] == b'^';
    if negate { i += 1; }
    let mut found = false;
    while i < end {
        if pat[i] == b'%' && i + 1 < end {
            i += 1;
            if match_class(c, pat[i]) { found = true; }
            i += 1;
        } else if i + 2 < end && pat[i + 1] == b'-' {
            if pat[i] <= c && c <= pat[i + 2] { found = true; }
            i += 3;
        } else {
            if pat[i] == c { found = true; }
            i += 1;
        }
    }
    if negate { !found } else { found }
}

fn single_match(subj: &[u8], s: usize, pat: &[u8], p: usize, ep: usize) -> bool {
    if s >= subj.len() { return false; }
    let c = subj[s];
    match pat[p] {
        b'.' => true,
        b'%' => match_class(c, pat[p + 1]),
        b'[' => match_bracket_class(c, pat, p, ep),
        pc => pc == c,
    }
}

struct MatchState<'a> {
    subj: &'a [u8],
    pat: &'a [u8],
    captures: Vec<(usize, isize)>,
    depth: u32,
}

impl<'a> MatchState<'a> {
    fn do_match(&mut self, s: usize, p: usize) -> Result<Option<usize>, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err("pattern too complex".into());
        }
        let result = self.do_match_inner(s, p);
        self.depth -= 1;
        result
    }

    fn do_match_inner(&mut self, mut s: usize, mut p: usize) -> Result<Option<usize>, String> {
        loop {
            if p >= self.pat.len() { return Ok(Some(s)); }
            match self.pat[p] {
                b'(' => {
                    return if p + 1 < self.pat.len() && self.pat[p + 1] == b')' {
                        self.start_capture(s, p + 2, CAP_POSITION)
                    } else {
                        self.start_capture(s, p + 1, CAP_UNFINISHED)
                    };
                }
                b')' => return self.end_capture(s, p + 1),
                b'$' if p + 1 == self.pat.len() => {
                    return Ok(if s == self.subj.len() { Some(s) } else { None });
                }
                b'%' if p + 1 < self.pat.len() && self.pat[p + 1] == b'b' => {
                    match self.match_balance(s, p + 2)? {
                        Some(ns) => { s = ns; p += 4; continue; }
                        None => return Ok(None),
                    }
                }
                b'%' if p + 1 < self.pat.len() && self.pat[p + 1] == b'f' => {
                    let set_start = p + 2;
                    if set_start >= self.pat.len() || self.pat[set_start] != b'[' {
                        return Err("missing '[' after '%f' in pattern".into());
                    }
                    let ep = classend(self.pat, set_start)?;
                    let prev = if s == 0 { 0u8 } else { self.subj[s - 1] };
                    let cur = if s < self.subj.len() { self.subj[s] } else { 0u8 };
                    if !match_bracket_class(prev, self.pat, set_start, ep)
                        && match_bracket_class(cur, self.pat, set_start, ep) {
                        p = ep; continue;
                    }
                    return Ok(None);
                }
                b'%' if p + 1 < self.pat.len() && self.pat[p + 1].is_ascii_digit() => {
                    match self.match_capture(s, self.pat[p + 1])? {
                        Some(ns) => { s = ns; p += 2; continue; }
                        None => return Ok(None),
                    }
                }
                _ => {
                    let ep = classend(self.pat, p)?;
                    let m = single_match(self.subj, s, self.pat, p, ep);
                    let next = if ep < self.pat.len() { self.pat[ep] } else { 0 };
                    if !m {
                        if matches!(next, b'*' | b'?' | b'-') { p = ep + 1; continue; }
                        return Ok(None);
                    }
                    match next {
                        b'?' => {
                            if let Some(r) = self.do_match(s + 1, ep + 1)? { return Ok(Some(r)); }
                            p = ep + 1; continue;
                        }
                        b'+' => return self.max_expand(s + 1, p, ep),
                        b'*' => return self.max_expand(s, p, ep),
                        b'-' => return self.min_expand(s, p, ep),
                        _ => { s += 1; p = ep; continue; }
                    }
                }
            }
        }
    }

    fn start_capture(&mut self, s: usize, p: usize, what: isize) -> Result<Option<usize>, String> {
        if self.captures.len() >= MAX_CAPTURES { return Err("too many captures".into()); }
        self.captures.push((s, what));
        let res = self.do_match(s, p);
        if !matches!(res, Ok(Some(_))) { self.captures.pop(); }
        res
    }

    fn end_capture(&mut self, s: usize, p: usize) -> Result<Option<usize>, String> {
        let idx = self.captures.iter().rposition(|&(_, len)| len == CAP_UNFINISHED)
            .ok_or("invalid pattern capture")?;
        self.captures[idx].1 = (s - self.captures[idx].0) as isize;
        let res = self.do_match(s, p);
        if !matches!(res, Ok(Some(_))) { self.captures[idx].1 = CAP_UNFINISHED; }
        res
    }

    fn match_balance(&self, s: usize, p: usize) -> Result<Option<usize>, String> {
        if p + 1 >= self.pat.len() { return Err("missing arguments to '%b'".into()); }
        if s >= self.subj.len() || self.subj[s] != self.pat[p] { return Ok(None); }
        let (b, e) = (self.pat[p], self.pat[p + 1]);
        let mut cont = 1i32;
        let mut i = s + 1;
        while i < self.subj.len() {
            if self.subj[i] == e {
                cont -= 1;
                if cont == 0 { return Ok(Some(i + 1)); }
            } else if self.subj[i] == b {
                cont += 1;
            }
            i += 1;
        }
        Ok(None)
    }

    fn match_capture(&self, s: usize, digit: u8) -> Result<Option<usize>, String> {
        let idx = digit.wrapping_sub(b'1') as usize;
        let &(cs, clen) = self.captures.get(idx).ok_or("invalid capture index")?;
        if clen == CAP_UNFINISHED { return Err("unfinished capture".into()); }
        // A position capture has no text to re-match; Lua fails the match.
        if clen == CAP_POSITION { return Ok(None); }
        let clen = clen as usize;
        if self.subj.len() >= s + clen && self.subj[cs..cs + clen] == self.subj[s..s + clen] {
            Ok(Some(s + clen))
        } else {
            Ok(None)
        }
    }

    fn max_expand(&mut self, s: usize, p: usize, ep: usize) -> Result<Option<usize>, String> {
        let mut i = 0usize;
        while single_match(self.subj, s + i, self.pat, p, ep) { i += 1; }
        loop {
            if let Some(r) = self.do_match(s + i, ep + 1)? { return Ok(Some(r)); }
            if i == 0 { return Ok(None); }
            i -= 1;
        }
    }

    fn min_expand(&mut self, mut s: usize, p: usize, ep: usize) -> Result<Option<usize>, String> {
        loop {
            if let Some(r) = self.do_match(s, ep + 1)? { return Ok(Some(r)); }
            if single_match(self.subj, s, self.pat, p, ep) { s += 1; } else { return Ok(None); }
        }
    }
}

/// Scans forward from `init` (a byte offset, clamped into range) looking for
/// the first position the pattern matches, honoring a leading '^' as an
/// anchor (only try `init` itself, don't scan further).
pub fn find_from(subj: &[u8], pat: &[u8], init: usize) -> Result<Option<Match>, String> {
    let anchored = pat.first() == Some(&b'^');
    let pat_body = if anchored { &pat[1..] } else { pat };
    let mut s = init.min(subj.len());
    loop {
        let mut ms = MatchState { subj, pat: pat_body, captures: Vec::new(), depth: 0 };
        if let Some(e) = ms.do_match(s, 0)? {
            if ms.captures.iter().any(|&(_, len)| len == CAP_UNFINISHED) {
                return Err("unfinished capture".into());
            }
            let captures = ms.captures.iter().map(|&(cs, clen)| {
                if clen == CAP_POSITION { Capture::Position(cs + 1) }
                else { Capture::Str(cs, cs + clen.max(0) as usize) }
            }).collect();
            return Ok(Some(Match { start: s, end: e, captures }));
        }
        if anchored || s >= subj.len() { return Ok(None); }
        s += 1;
    }
}
