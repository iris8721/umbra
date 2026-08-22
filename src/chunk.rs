use std::cell::Cell;

// ABC format:  [ op:8 | A:8 | B:8  | C:8  ]
// ABx format:  [ op:8 | A:8 | Bx:16       ]  (Bx unsigned)
// AsBx format: [ op:8 | A:8 | sBx:16      ]  (sBx = raw - BIAS)

pub const BIAS: i32 = 32767;

#[inline(always)] pub fn iop(i: u32)   -> u8    { (i & 0xFF) as u8 }
#[inline(always)] pub fn ia(i: u32)    -> usize { ((i >> 8)  & 0xFF) as usize }
#[inline(always)] pub fn ib(i: u32)    -> usize { ((i >> 16) & 0xFF) as usize }
#[inline(always)] pub fn ic(i: u32)    -> usize { ((i >> 24) & 0xFF) as usize }
#[inline(always)] pub fn ibx(i: u32)   -> usize { ((i >> 16) & 0xFFFF) as usize }
#[inline(always)] pub fn isbx(i: u32)  -> i32   { ((i >> 16) as u16 as i32) - BIAS }

#[inline(always)]
pub fn enc_abc(op: Op, a: u8, b: u8, c: u8) -> u32 {
    op as u32 | ((a as u32) << 8) | ((b as u32) << 16) | ((c as u32) << 24)
}
#[inline(always)]
pub fn enc_abx(op: Op, a: u8, bx: u16) -> u32 {
    op as u32 | ((a as u32) << 8) | ((bx as u32) << 16)
}
#[inline(always)]
pub fn enc_asbx(op: Op, a: u8, sbx: i32) -> u32 {
    enc_abx(op, a, (sbx + BIAS) as u16)
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    LoadNil,
    LoadBool,
    LoadInt,
    LoadK,
    Move,

    Add, Sub, Mul, Div, IDiv, Mod, Pow,
    Unm,
    BAnd, BOr, BXor, Shl, Shr,
    BNot,
    Concat,
    Len,
    Not,

    Eq, Lt, Le,

    Test,
    TestSet,

    Jmp,

    NewTable,
    GetTable,
    SetTable,
    SetList,

    GetGlobal,
    SetGlobal,

    Call,
    Return,

    ForPrep,
    ForLoop,

    TForCall,
    TForLoop,

    Closure,
    Vararg,

    GetUpval,
    SetUpval,

    // Tbc marks register A as to-be-closed (B != 0 when it holds an upvalue
    // box); TbcPop unmarks the most recent mark. The marks let an error
    // unwinding through a frame run close() on the locals it tears down.
    Tbc,
    TbcPop,
}

impl Op {
    #[inline(always)]
    pub fn from_u8(b: u8) -> Option<Self> {
        if b <= Op::TbcPop as u8 {
            Some(unsafe { std::mem::transmute(b) })
        } else {
            None
        }
    }
}

pub const RK_BIT: usize = 0x80;
#[inline(always)] pub fn is_rk(x: usize) -> bool { x & RK_BIT != 0 }
#[inline(always)] pub fn rk_idx(x: usize) -> usize { x & !RK_BIT }

#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(StrConst),
}

// The interned Value is filled in by the first VM that resolves the constant;
// the owner tag keeps a Proto shared across VMs from handing back a pointer
// into another VM's heap. The VM keeps interned constants alive for its whole
// lifetime (Vm::const_strings), so a cached pointer never dangles.
#[derive(Debug)]
pub struct StrConst {
    pub s: String,
    cached: Cell<(usize, u64)>,
}

impl StrConst {
    pub fn new(s: String) -> Self {
        StrConst { s, cached: Cell::new((0, 0)) }
    }

    pub fn cached(&self) -> (usize, u64) { self.cached.get() }
    pub fn set_cached(&self, owner: usize, bits: u64) { self.cached.set((owner, bits)); }
}

impl Clone for StrConst {
    fn clone(&self) -> Self { StrConst::new(self.s.clone()) }
}

impl PartialEq for StrConst {
    fn eq(&self, other: &Self) -> bool { self.s == other.s }
}

#[derive(Debug, Clone)]
pub struct UpvalDesc {
    pub name: String,
    pub in_stack: bool,
    pub idx: u8,
}

#[derive(Debug)]
pub struct Proto {
    pub code: Vec<u32>,
    pub consts: Vec<Const>,
    pub protos: Vec<Proto>,
    pub upvals: Vec<UpvalDesc>,
    pub max_regs: u8,
    pub params: u8,
    pub is_vararg: bool,
    pub source: Option<String>,
    pub lines: Vec<u32>,
}

impl Proto {
    pub fn new() -> Self {
        Proto {
            code: Vec::new(),
            consts: Vec::new(),
            protos: Vec::new(),
            upvals: Vec::new(),
            max_regs: 2,
            params: 0,
            is_vararg: false,
            source: None,
            lines: Vec::new(),
        }
    }

    pub fn emit(&mut self, instr: u32, line: u32) -> usize {
        let idx = self.code.len();
        self.code.push(instr);
        self.lines.push(line);
        idx
    }

    pub fn emit_jump(&mut self, line: u32) -> usize {
        self.emit(enc_asbx(Op::Jmp, 0, 0), line)
    }
    pub fn add_string(&mut self, s: &str) -> usize {
        self.add_const(Const::Str(StrConst::new(s.to_owned())))
    }
    // callers must turn that into a compile error rather than emit a
    // truncated offset that would jump somewhere arbitrary.
    pub fn patch_jump(&mut self, idx: usize, target: usize) -> bool {
        let offset = target as i32 - idx as i32 - 1;
        if offset < -BIAS || offset > u16::MAX as i32 - BIAS {
            return false;
        }
        let a = ia(self.code[idx]) as u8;
        self.code[idx] = enc_asbx(Op::Jmp, a, offset);
        true
    }

    pub fn patch_jump_here(&mut self, idx: usize) -> bool {
        let here = self.code.len();
        self.patch_jump(idx, here)
    }

    pub fn add_const(&mut self, c: Const) -> usize {
        for (i, existing) in self.consts.iter().enumerate() {
            if *existing == c { return i; }
        }
        let i = self.consts.len();
        self.consts.push(c);
        i
    }


    pub fn current_pc(&self) -> usize { self.code.len() }
}
