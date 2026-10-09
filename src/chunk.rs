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
#[inline(always)] pub fn ici(i: u32)   -> i64   { (i >> 24) as u8 as i8 as i64 }

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

    // Immediate forms: C is a signed 8-bit integer literal, not an RK
    // operand. The metamethod fallback still sees the literal as a regular
    // int value, so behavior matches the generic op exactly.
    AddI,
    SubI,

    // Compare-and-jump with an 8-bit immediate: A is the jump sense, B the
    // register, C the literal. GtI/GeI compare imm against R[B] (the
    // operands of the source-level `x > k` / `k < x` are already swapped by
    // the compiler, matching how Gt/Ge reuse Lt/Le).
    LtI,
    LeI,
    GtI,
    GeI,
    EqI,

    // Field access with a constant-string key: C is a raw const index (not
    // RK), skipping the RK tag check on the lookup path.
    GetField,
    SetField,

    // Method-call setup: R[A+1] = R[B]; R[A] = R[B][K[C]] — fuses the
    // receiver move and the method lookup that a `obj:name(...)` call needs.
    SelfOp,

    // String interpolation: R[A] = tostring(R[B]) .. ... .. tostring(R[C]).
    // Its own op, not a call of the `tostring` global, so a script can't
    // shadow it.
    Interp,

    // `return f(...)`: reuses the current frame instead of pushing one.
    // Operands match Call; the compiler only emits it when no <close> local
    // is pending and the call is the sole return value. Keep it last: the
    // decoders bound opcodes by it.
    TailCall,
}

impl Op {
    #[inline(always)]
    pub fn from_u8(b: u8) -> Option<Self> {
        if b <= Op::TailCall as u8 {
            // SAFETY: Op is repr(u8) with contiguous variants 0..=TailCall.
            Some(unsafe { std::mem::transmute::<u8, Op>(b) })
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

// The interned Value is filled in by the VM that resolves the constant,
// tagged with that VM's id (Vm::id, never reused), so a pointer into one
// VM's heap is never handed to another. Not its address: a Vm that moves
// keeps its caches, and a new one at a dropped one's address doesn't
// inherit them. The collector marks a proto's cached strings with the
// proto, so a cached pointer never dangles.
#[derive(Debug)]
pub struct StrConst {
    pub s: String,
    cached: Cell<(u64, u64)>,
}

impl StrConst {
    pub fn new(s: String) -> Self {
        StrConst { s, cached: Cell::new((0, 0)) }
    }

    pub fn cached(&self) -> (u64, u64) { self.cached.get() }
    pub fn set_cached(&self, owner: u64, bits: u64) { self.cached.set((owner, bits)); }
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

// Debug info for naming locals in error messages and tracebacks: the
// register a local occupies and the half-open pc range [start_pc, end_pc)
// over which that register holds it. `boxed` marks locals captured by a
// nested closure — their register holds a 1-element cell, so the value
// itself lives behind a GetTable with key 1.
#[derive(Debug, Clone)]
pub struct LocVar {
    pub name: String,
    pub reg: u8,
    pub start_pc: u32,
    pub end_pc: u32,
    pub boxed: bool,
}

// A Proto as a collected object (GcKind::Proto). Frames, closures and
// function values point at the GcProto; references into it borrow only
// `proto` (heap::proto), never the header the collector writes while the
// interpreter runs the code.
#[repr(C)] // gc must stay first: the GC reads the header through the object pointer.
pub struct GcProto {
    pub(crate) gc: crate::gc::GcHeader,
    pub(crate) proto: Proto,
}

// How a declaration names a function: (kind, name), as tracebacks print it.
pub(crate) type FnDecl = (&'static str, String);

// Crate-private throughout: the interpreter trusts a Proto's bytecode, so
// only the compiler builds one, and hosts hold compiled code as
// compiler::Compiled.
pub struct Proto {
    pub(crate) code: Vec<u32>,
    pub(crate) consts: Vec<Const>,
    // Nested functions, each its own collected object; the collector marks
    // them through this list. Owned by compiler::Compiled until a Vm
    // registers the tree.
    pub(crate) protos: Vec<*mut GcProto>,
    pub(crate) upvals: Vec<UpvalDesc>,
    pub(crate) max_regs: u8,
    pub(crate) params: u8,
    pub(crate) is_vararg: bool,
    pub(crate) source: Option<String>,
    // Line the function definition starts on; used for the
    // `function <source:line>` fallback name in tracebacks.
    pub(crate) line_defined: u32,
    // True for the top-level chunk proto (compile's entry point), so
    // tracebacks can print "in main chunk" instead of a function name.
    pub(crate) is_main: bool,
    pub(crate) locvars: Vec<LocVar>,
    pub(crate) lines: Vec<u32>,
    // Per-const GetGlobal cache, parallel to `consts`: (owner Vm::id,
    // globals generation, cached value bits). Filled lazily by the VM; a
    // globals write bumps the generation and invalidates every entry. Sized
    // to consts.len() at compile time.
    pub(crate) global_cache: Vec<Cell<(u64, u64, u64)>>,
    // The name the source declared the function under, for tracebacks of
    // frames whose call site names nothing (a host call, pcall's target, a
    // metamethod): `fn f` and `let f = fn` give ("function", "f"), `fn a.b`
    // ("function", "a.b"), `fn A:m` ("method", "m").
    pub(crate) decl: Option<FnDecl>,
}

impl Proto {
    pub(crate) fn new() -> Self {
        Proto {
            code: Vec::new(),
            consts: Vec::new(),
            protos: Vec::new(),
            upvals: Vec::new(),
            max_regs: 2,
            params: 0,
            is_vararg: false,
            source: None,
            line_defined: 0,
            is_main: false,
            decl: None,
            locvars: Vec::new(),
            lines: Vec::new(),
            global_cache: Vec::new(),
        }
    }

    pub(crate) fn emit(&mut self, instr: u32, line: u32) -> usize {
        let idx = self.code.len();
        self.code.push(instr);
        self.lines.push(line);
        idx
    }

    pub(crate) fn emit_jump(&mut self, line: u32) -> usize {
        self.emit(enc_asbx(Op::Jmp, 0, 0), line)
    }
    pub(crate) fn add_string(&mut self, s: &str) -> usize {
        self.add_const(Const::Str(StrConst::new(s.to_owned())))
    }
    // callers must turn that into a compile error rather than emit a
    // truncated offset that would jump somewhere arbitrary.
    pub(crate) fn patch_jump(&mut self, idx: usize, target: usize) -> bool {
        let offset = target as i32 - idx as i32 - 1;
        if offset < -BIAS || offset > u16::MAX as i32 - BIAS {
            return false;
        }
        let a = ia(self.code[idx]) as u8;
        self.code[idx] = enc_asbx(Op::Jmp, a, offset);
        true
    }

    pub(crate) fn add_const(&mut self, c: Const) -> usize {
        for (i, existing) in self.consts.iter().enumerate() {
            if *existing == c { return i; }
        }
        let i = self.consts.len();
        self.consts.push(c);
        i
    }


    pub(crate) fn current_pc(&self) -> usize { self.code.len() }
}
