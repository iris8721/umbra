use crate::ast::*;
use crate::chunk::{Const, Op, Proto, RK_BIT, enc_abc, enc_abx, enc_asbx, iop, isbx};
const BIAS: i32 = crate::chunk::BIAS;
// Register operands share an 8-bit field with RK constants (bit 7 set), so
// both registers and RK-encodable constants are limited to 0..128.
const MAX_REGS: u8 = 128;
const MAX_RK_CONSTS: usize = 128;
// Call/Return operand meaning "as many results as the callee produced".
const MULTRET: u8 = 255;
// Recursion budget shared by the capture scan and the compiler proper; the
// parser enforces its own limit, but this guards ASTs from any source
// against overflowing the native stack.
const MAX_DEPTH: u32 = 100;

#[derive(Debug, Clone)]
pub struct CompileError {
    pub msg: String,
    pub line: u32,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

type CResult<T> = Result<T, CompileError>;

fn err(msg: impl Into<String>, line: u32) -> CompileError {
    CompileError { msg: msg.into(), line }
}

// Collects every identifier name referenced anywhere inside a nested
// function (at any depth) within this body — an over-approximation of
// "names this function's closures might capture as upvalues": it isn't
// scope-precise (a same-named local declared fresh inside a nested closure
// still gets counted), but over-boxing a name that's never actually
// captured only costs an extra indirection, never correctness. Boxing
// itself is a separate decision, made where each Local is declared.
//
// The scan recurses at the same points compile_expr/compile_stmt do, so it
// shares their depth budget: past MAX_DEPTH it stops early and sets
// `overflow`, which collect_captured_names turns into a compile error —
// silently dropping names here would under-box and miscompile captures.
fn collect_captured_names(body: &FuncBody) -> CResult<std::collections::HashSet<String>> {
    let mut out = std::collections::HashSet::new();
    let mut overflow = false;
    scan_block(&body.body, false, 0, &mut out, &mut overflow);
    if overflow {
        return Err(err("chunk too deeply nested", body.line));
    }
    Ok(out)
}

fn scan_block(block: &Block, in_closure: bool, depth: u32, out: &mut std::collections::HashSet<String>, overflow: &mut bool) {
    if depth >= MAX_DEPTH { *overflow = true; return; }
    for stmt in &block.stmts { scan_stmt(stmt, in_closure, depth + 1, out, overflow); }
    if let Some(ret) = &block.ret {
        for e in ret { scan_expr(e, in_closure, depth + 1, out, overflow); }
    }
}

fn scan_funcbody(body: &FuncBody, depth: u32, out: &mut std::collections::HashSet<String>, overflow: &mut bool) {
    scan_block(&body.body, true, depth, out, overflow);
}

fn scan_stmt(stmt: &Stmt, in_closure: bool, depth: u32, out: &mut std::collections::HashSet<String>, overflow: &mut bool) {
    if depth >= MAX_DEPTH { *overflow = true; return; }
    match stmt {
        Stmt::Assign { targets, values, .. } => {
            for e in targets { scan_expr(e, in_closure, depth + 1, out, overflow); }
            for e in values { scan_expr(e, in_closure, depth + 1, out, overflow); }
        }
        Stmt::Local { values, .. } => for e in values { scan_expr(e, in_closure, depth + 1, out, overflow); },
        Stmt::Destructure { value, .. } => scan_expr(value, in_closure, depth + 1, out, overflow),
        Stmt::Do { body, .. } => scan_block(body, in_closure, depth + 1, out, overflow),
        Stmt::While { cond, body, .. } => { scan_expr(cond, in_closure, depth + 1, out, overflow); scan_block(body, in_closure, depth + 1, out, overflow); }
        Stmt::RepeatUntil { body, cond, .. } => { scan_block(body, in_closure, depth + 1, out, overflow); scan_expr(cond, in_closure, depth + 1, out, overflow); }
        Stmt::If { cond, then, elseifs, else_, .. } => {
            scan_expr(cond, in_closure, depth + 1, out, overflow);
            scan_block(then, in_closure, depth + 1, out, overflow);
            for (c, b) in elseifs { scan_expr(c, in_closure, depth + 1, out, overflow); scan_block(b, in_closure, depth + 1, out, overflow); }
            if let Some(b) = else_ { scan_block(b, in_closure, depth + 1, out, overflow); }
        }
        Stmt::ForNum { start, limit, step, body, .. } => {
            scan_expr(start, in_closure, depth + 1, out, overflow);
            scan_expr(limit, in_closure, depth + 1, out, overflow);
            if let Some(s) = step { scan_expr(s, in_closure, depth + 1, out, overflow); }
            scan_block(body, in_closure, depth + 1, out, overflow);
        }
        Stmt::ForIn { iters, body, .. } => {
            for e in iters { scan_expr(e, in_closure, depth + 1, out, overflow); }
            scan_block(body, in_closure, depth + 1, out, overflow);
        }
        Stmt::FuncDef { body, .. } => scan_funcbody(body, depth + 1, out, overflow),
        Stmt::LocalFunc { body, .. } => scan_funcbody(body, depth + 1, out, overflow),
        Stmt::Call(c) => scan_call(c, in_closure, depth + 1, out, overflow),
        Stmt::MethodCall(m) => scan_methodcall(m, in_closure, depth + 1, out, overflow),
        Stmt::ExprStmt(e) => scan_expr(e, in_closure, depth + 1, out, overflow),
        Stmt::Break(_) | Stmt::Continue(_) | Stmt::Goto(_, _) | Stmt::Label(_, _) => {}
    }
}

fn scan_call(c: &CallExpr, in_closure: bool, depth: u32, out: &mut std::collections::HashSet<String>, overflow: &mut bool) {
    scan_expr(&c.callee, in_closure, depth, out, overflow);
    scan_args(&c.args, in_closure, depth, out, overflow);
}

fn scan_methodcall(m: &MethodCallExpr, in_closure: bool, depth: u32, out: &mut std::collections::HashSet<String>, overflow: &mut bool) {
    scan_expr(&m.receiver, in_closure, depth, out, overflow);
    scan_args(&m.args, in_closure, depth, out, overflow);
}

fn scan_args(args: &Args, in_closure: bool, depth: u32, out: &mut std::collections::HashSet<String>, overflow: &mut bool) {
    match args {
        Args::Exprs(exprs) => for e in exprs { scan_expr(e, in_closure, depth, out, overflow); },
        Args::Table(tc) => scan_expr(&Expr::Table(tc.clone()), in_closure, depth, out, overflow),
        Args::String(_) => {}
    }
}

fn scan_expr(expr: &Expr, in_closure: bool, depth: u32, out: &mut std::collections::HashSet<String>, overflow: &mut bool) {
    if depth >= MAX_DEPTH { *overflow = true; return; }
    match expr {
        Expr::Nil(_) | Expr::True(_) | Expr::False(_) | Expr::Int(_, _) | Expr::Float(_, _)
        | Expr::String(_, _) | Expr::Vararg(_) => {}
        Expr::Ident(id) => { if in_closure { out.insert(id.name.clone()); } }
        Expr::Index { table, key, .. } => { scan_expr(table, in_closure, depth + 1, out, overflow); scan_expr(key, in_closure, depth + 1, out, overflow); }
        Expr::Field { table, .. } => scan_expr(table, in_closure, depth + 1, out, overflow),
        Expr::Unop { operand, .. } => scan_expr(operand, in_closure, depth + 1, out, overflow),
        Expr::Binop { lhs, rhs, .. } => { scan_expr(lhs, in_closure, depth + 1, out, overflow); scan_expr(rhs, in_closure, depth + 1, out, overflow); }
        Expr::Concat { parts, .. } => for e in parts { scan_expr(e, in_closure, depth + 1, out, overflow); },
        Expr::Call(c) => scan_call(c, in_closure, depth + 1, out, overflow),
        Expr::MethodCall(m) => scan_methodcall(m, in_closure, depth + 1, out, overflow),
        Expr::Function(body) => scan_funcbody(body, depth + 1, out, overflow),
        Expr::Table(tc) => for f in &tc.fields {
            match f {
                TableField::Indexed { key, val } => { scan_expr(key, in_closure, depth + 1, out, overflow); scan_expr(val, in_closure, depth + 1, out, overflow); }
                TableField::Named { val, .. } => scan_expr(val, in_closure, depth + 1, out, overflow),
                TableField::Positional(e) => scan_expr(e, in_closure, depth + 1, out, overflow),
            }
        },
        Expr::Ternary { cond, then, else_, .. } => {
            scan_expr(cond, in_closure, depth + 1, out, overflow);
            scan_expr(then, in_closure, depth + 1, out, overflow);
            scan_expr(else_, in_closure, depth + 1, out, overflow);
        }
        Expr::IncrDecr { target, .. } => scan_expr(target, in_closure, depth + 1, out, overflow),
    }
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum ExprKind {
    Nil,
    True,
    False,
    IntK(i64),
    FloatK(f64),
    K(usize),
    Reg(u8),
    Global(usize),
    Indexed(u8, u8),
    Upval(u8),
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct Expr2 {
    kind: ExprKind,
    line: u32,
}

impl Expr2 {
    fn reg(r: u8, line: u32) -> Self { Self { kind: ExprKind::Reg(r), line } }
    #[allow(dead_code)]
    fn k(i: usize, line: u32) -> Self { Self { kind: ExprKind::K(i), line } }
}

#[derive(Debug, Clone)]
struct Local {
    name: String,
    reg: u8,
    mutable: bool,
    close: bool,
    // If true, `reg` holds a 1-element table (a heap cell) wrapping the real
    // value instead of the value itself, so every closure that captures this
    // local as an upvalue shares the same cell — a plain copy-by-value
    // upvalue (what non-boxed locals use) can't support a mutation made
    // inside one closure being visible to another closure or the outer
    // scope, since each copy would be independent. See box_in_place.
    boxed: bool,
}

#[derive(Debug, Clone)]
struct UpvalInfo {
    name: String,
    in_stack: bool,
    outer_idx: u8,
    boxed: bool,
}

struct LoopScope {
    break_jumps: Vec<usize>,
    continue_jumps: Vec<usize>,
    locals_top: usize,
}

// A label records where it sits: the block that owns it, whether that block
// is the last statement of its own parent (Lua's "label at the end of a
// block is visible in the enclosing block" rule, applied transitively), and
// how many locals were live at that point — a goto may not jump into the
// scope of a local declared after the label.
#[derive(Debug, Clone)]
struct LabelInfo {
    pc: usize,
    block: usize,
    locals: usize,
}

// A goto records everything needed to resolve it once the whole function is
// compiled: its jump slot, the block it appears in, the locals live there
// (count for the scope check, plus each <close> local's index/register so a
// jump that leaves their scope can run close() first), and the free
// register at that point so the close stub's scratch registers can't
// clobber live values.
struct PendingGoto {
    name: String,
    jump: usize,
    line: u32,
    block: usize,
    locals: usize,
    closers: Vec<(usize, u8, bool)>,
    free_reg: u8,
}

struct FnComp {
    proto: Proto,
    locals: Vec<Local>,
    upvals: Vec<UpvalInfo>,
    // Raw pointer to the enclosing function's still-being-compiled FnComp
    // (compile_fn recurses synchronously, so it's genuinely alive for the
    // whole time this one is), not an owned snapshot — so resolve_upval can
    // walk the *entire* live chain (see its doc) rather than being stuck
    // once the immediate parent's own locals/upvals don't have the name.
    outer_scope: Option<*mut FnComp>,
    free_reg: u8,
    loops: Vec<LoopScope>,
    line: u32,
    labels: std::collections::HashMap<String, LabelInfo>,
    pending_gotos: Vec<PendingGoto>,
    // Block ancestry for label visibility: block_parent[i] is the block that
    // lexically contains block i, block_tail[i] is whether block i was the
    // last statement of that parent. Block 0 is the function body. `blocks`
    // is the stack of currently open block ids.
    block_parent: Vec<Option<usize>>,
    block_tail: Vec<bool>,
    blocks: Vec<usize>,
    // statement of its block. Read by new_block for block_tail.
    tail_stmt: bool,
    // Recursion depth across compile_stmt/compile_expr and nested function
    // compilation; guards the native stack against pathological nesting.
    depth: u32,
    is_vararg: bool,
    // Names any nested closure in this function references — see
    // collect_captured_names. Consulted when a local/param is declared to
    // decide whether it needs boxing.
    captured: std::collections::HashSet<String>,
}

impl FnComp {
    fn new(source: Option<String>, outer: Option<*mut FnComp>, captured: std::collections::HashSet<String>, depth: u32, is_vararg: bool) -> Self {
        let mut proto = Proto::new();
        proto.source = source;
        Self {
            proto, locals: Vec::new(), upvals: Vec::new(), outer_scope: outer,
            free_reg: 0, loops: Vec::new(), line: 1,
            labels: std::collections::HashMap::new(), pending_gotos: Vec::new(),
            block_parent: vec![None], block_tail: vec![false], blocks: vec![0],
            tail_stmt: true, depth, is_vararg,
            captured,
        }
    }

    // Opens a lexical block and returns its id. `tail_stmt` (set by the
    // caller before compiling the statement that contains this block)
    // decides whether labels at this block's end are also visible in the
    // enclosing block, matching Lua's end-of-block label rule.
    fn new_block(&mut self) -> usize {
        let id = self.block_parent.len();
        self.block_parent.push(self.blocks.last().copied());
        self.block_tail.push(self.tail_stmt);
        self.blocks.push(id);
        id
    }

    fn end_block(&mut self) {
        self.blocks.pop();
    }

    fn alloc_reg(&mut self) -> CResult<u8> {
        let r = self.free_reg;
        self.reserve(r as usize + 1)?;
        self.free_reg = r + 1;
        Ok(r)
    }

    // Registers written by hint (to_reg with Some(dst), call results, arg
    // slots) never go through alloc_reg, so max_regs is bumped here instead;
    // the VM's GC root scan trusts max_regs to cover every live register.
    fn reserve(&mut self, top: usize) -> CResult<()> {
        if top > MAX_REGS as usize {
            return Err(err("function needs too many registers", self.line));
        }
        if self.proto.max_regs < top as u8 { self.proto.max_regs = top as u8; }
        Ok(())
    }

    fn free_reg_to(&mut self, base: u8) {
        self.free_reg = base;
    }

    fn const_str(&mut self, s: &str) -> usize { self.proto.add_string(s) }

    fn const_val(&mut self, c: Const) -> usize { self.proto.add_const(c) }

    // Constants past the RK range are loaded into a fresh register instead;
    // the register stays reserved until the caller resets free_reg.
    fn rk_const(&mut self, c: Const) -> CResult<usize> {
        let idx = self.const_val(c);
        if idx < MAX_RK_CONSTS { return Ok(idx | RK_BIT); }
        let r = self.alloc_reg()?;
        self.emit(enc_abx(Op::LoadK, r, idx as u16));
        Ok(r as usize)
    }

    fn rk_str(&mut self, s: &str) -> CResult<u8> {
        Ok(self.rk_const(Const::Str(crate::chunk::StrConst::new(s.to_owned())))? as u8)
    }

    // Returns (register, boxed).
    fn resolve_local(&self, name: &str) -> Option<(u8, bool)> {
        self.locals.iter().rev().find(|l| l.name == name).map(|l| (l.reg, l.boxed))
    }

    fn push_local(&mut self, name: String, mutable: bool) -> CResult<u8> {
        let boxed = self.captured.contains(&name);
        let r = self.alloc_reg()?;
        self.locals.push(Local { name, reg: r, mutable, close: false, boxed });
        Ok(r)
    }

    // Wraps the value currently sitting in `reg` into a fresh 1-element
    // table and leaves that table (the "box") in reg instead — see the
    // `boxed` field doc on Local.
    fn box_in_place(&mut self, reg: u8) -> CResult<()> {
        let box_reg = self.alloc_reg()?;
        self.emit(enc_abc(Op::NewTable, box_reg, 0, 0));
        let key = self.rk_const(Const::Int(1))? as u8;
        self.emit(enc_abc(Op::SetTable, box_reg, key, reg));
        self.emit_move(reg, box_reg);
        self.free_reg_to(box_reg);
        Ok(())
    }

    fn unbox_into(&mut self, dst: u8, box_reg: u8) -> CResult<()> {
        let key = self.rk_const(Const::Int(1))? as u8;
        self.emit(enc_abc(Op::GetTable, dst, box_reg, key));
        Ok(())
    }

    fn set_boxed(&mut self, box_reg: u8, src: u8) -> CResult<()> {
        let key = self.rk_const(Const::Int(1))? as u8;
        self.emit(enc_abc(Op::SetTable, box_reg, key, src));
        Ok(())
    }

    fn locals_top(&self) -> usize { self.locals.len() }

    fn pop_locals_to(&mut self, top: usize) {
        let new_free = self.locals.get(top).map(|l| l.reg).unwrap_or(self.free_reg);
        self.locals.truncate(top);
        self.free_reg = new_free;
    }

    // Emits `local:close()` calls (LIFO) for the <close> locals in
    // locals[from_top..]. Called at a scope's fall-through end and before
    // break/continue/return; each close is preceded by a TbcPop so the
    // register's unwind-time mark (see Op::Tbc) is dropped before the call —
    // an error propagating out of close() must not close it a second time.
    fn emit_closes(&mut self, from_top: usize) -> CResult<()> {
        let closers: Vec<(u8, bool)> = self.locals[from_top..].iter()
            .filter(|l| l.close)
            .map(|l| (l.reg, l.boxed))
            .collect();
        let saved_free = self.free_reg;
        for (reg, boxed) in closers.into_iter().rev() {
            self.emit_close_one(reg, boxed)?;
            self.free_reg = saved_free;
        }
        Ok(())
    }

    // Emits one `local:close()` call; the value lives in `reg` (or in the
    // box `reg` points at when `boxed`). Scratch registers come from
    // alloc_reg, so the caller must ensure free_reg sits above every live
    // value and restore it afterwards.
    fn emit_close_one(&mut self, reg: u8, boxed: bool) -> CResult<()> {
        self.emit(enc_abc(Op::TbcPop, 0, 0, 0));
        // A <close> local that's also captured by a nested closure holds
        // a box (see the `boxed` field), not the resource itself — unbox
        // first so `close` is looked up on the real value.
        let value_reg = if boxed {
            let dst = self.alloc_reg()?;
            self.unbox_into(dst, reg)?;
            dst
        } else {
            reg
        };
        let fki = self.rk_str("close")?;
        let fn_reg = self.alloc_reg()?;
        self.reserve(fn_reg as usize + 2)?;
        self.emit(enc_abc(Op::GetTable, fn_reg, value_reg, fki));
        self.emit_move(fn_reg + 1, value_reg);
        self.emit(enc_abc(Op::Call, fn_reg, 2, 1));
        Ok(())
    }

    fn has_closes(&self, from_top: usize) -> bool {
        self.locals[from_top..].iter().any(|l| l.close)
    }

    // Returns (upvalue index, boxed). Recurses through the *live* chain of
    // enclosing FnComps (via outer_scope), not just one level: if the
    // immediate parent doesn't have `name` either, this asks the parent to
    // resolve it too (which may itself recurse further up and, as a side
    // effect, gain its own new upvalue entry for `name`) — so a doubly (or
    // more) nested closure can still reach a name that no function in
    // between ever references directly.
    fn resolve_upval(&mut self, name: &str) -> Option<(u8, bool)> {
        if let Some(i) = self.upvals.iter().position(|u| u.name == name) {
            return Some((i as u8, self.upvals[i].boxed));
        }
        let outer_ptr = self.outer_scope?;
        let outer = unsafe { &mut *outer_ptr };
        if let Some((reg, boxed)) = outer.resolve_local(name) {
            let idx = self.upvals.len() as u8;
            self.upvals.push(UpvalInfo { name: name.to_owned(), in_stack: true, outer_idx: reg, boxed });
            return Some((idx, boxed));
        }
        if let Some((outer_uv_idx, boxed)) = outer.resolve_upval(name) {
            let idx = self.upvals.len() as u8;
            self.upvals.push(UpvalInfo { name: name.to_owned(), in_stack: false, outer_idx: outer_uv_idx, boxed });
            return Some((idx, boxed));
        }
        None
    }

    fn emit(&mut self, i: u32) -> usize { self.proto.emit(i, self.line) }
    fn pc(&self) -> usize { self.proto.current_pc() }

    fn emit_load_nil(&mut self, r: u8) { self.emit(enc_abc(Op::LoadNil, r, 0, 0)); }
    fn emit_move(&mut self, dst: u8, src: u8) { self.emit(enc_abc(Op::Move, dst, src, 0)); }

    // sBx jump offsets are signed 16-bit; a function body larger than that
    // must fail to compile instead of silently patching a truncated offset.
    fn patch(&mut self, idx: usize, target: usize) -> CResult<()> {
        if self.proto.patch_jump(idx, target) {
            Ok(())
        } else {
            Err(err("function too large (jump out of range)", self.line))
        }
    }

    fn patch_here(&mut self, idx: usize) -> CResult<()> {
        let here = self.pc();
        self.patch(idx, here)
    }

    // Emits a Jmp/ForLoop back-edge to `target`; same range check as patch.
    fn emit_back(&mut self, op: Op, a: u8, target: usize) -> CResult<()> {
        let off = target as i64 - self.pc() as i64 - 1;
        if off < -BIAS as i64 || off > u16::MAX as i64 - BIAS as i64 {
            return Err(err("function too large (jump out of range)", self.line));
        }
        self.emit(enc_asbx(op, a, off as i32));
        Ok(())
    }

    // Emits the branch for a condition, returning the jump to patch to the
    // "condition false" target. Comparisons emit Cmp;Jmp directly — the VM
    // fuses the pair — instead of materializing a boolean and testing it.
    fn emit_cond_test(&mut self, cond: &Expr) -> CResult<usize> {
        if let Expr::Binop { op, lhs, rhs, line } = cond {
            let cmp = match op {
                Binop::Eq => Some((Op::Eq, false, false)),
                Binop::Ne => Some((Op::Eq, true,  false)),
                Binop::Lt => Some((Op::Lt, false, false)),
                Binop::Le => Some((Op::Le, false, false)),
                Binop::Gt => Some((Op::Lt, false, true)),
                Binop::Ge => Some((Op::Le, false, true)),
                _ => None,
            };
            if let Some((vm_op, invert, swap)) = cmp {
                self.line = *line;
                let temp_base = self.free_reg;
                let (l, r) = if swap { (rhs, lhs) } else { (lhs, rhs) };
                let le = self.compile_expr(l)?;
                let lrk = self.to_rk(le)?;
                let re = self.compile_expr(r)?;
                let rrk = self.to_rk(re)?;
                self.free_reg = temp_base;
                // The VM jumps when the raw comparison equals A; jumping on
                // false means A = invert.
                self.emit(enc_abc(vm_op, invert as u8, lrk as u8, rrk as u8));
                return Ok(self.proto.emit_jump(*line));
            }
        }
        let base = self.free_reg;
        let e = self.compile_expr(cond)?;
        let r = self.to_reg(e, Some(base))?;
        if self.free_reg <= r { self.free_reg = r + 1; }
        self.emit(enc_abc(Op::Test, r as u8, 0, 0));
        let j = self.proto.emit_jump(self.line);
        self.free_reg_to(base);
        Ok(j)
    }

    fn check_vararg(&self, line: u32) -> CResult<()> {
        if self.is_vararg {
            Ok(())
        } else {
            Err(err("cannot use '...' outside a vararg function", line))
        }
    }

    fn cur_block(&self) -> usize { *self.blocks.last().unwrap_or(&0) }

    // Lua label visibility: a label is visible in its own block and every
    // nested block, plus — when its block is the last statement of the
    // parent — in the enclosing block too (transitively). Anything else
    // (sibling blocks, outer blocks) can't be jumped to.
    fn label_visible(&self, label: &LabelInfo, from_block: usize) -> bool {
        let mut b = label.block;
        loop {
            let mut f = from_block;
            loop {
                if f == b { return true; }
                match self.block_parent[f] {
                    Some(p) => f = p,
                    None => break,
                }
            }
            if self.block_tail[b] {
                match self.block_parent[b] {
                    Some(p) => b = p,
                    None => break,
                }
            } else {
                break;
            }
        }
        false
    }

    fn to_reg(&mut self, e: Expr2, hint: Option<u8>) -> CResult<u8> {
        let dst = hint.unwrap_or_else(|| self.free_reg);
        self.reserve(dst as usize + 1)?;
        match e.kind {
            ExprKind::Reg(r) => {
                if let Some(h) = hint {
                    if h != r { self.emit_move(h, r); return Ok(h); }
                }
                Ok(r)
            }
            ExprKind::Nil => {
                if hint.is_none() { self.alloc_reg()?; }
                self.emit_load_nil(dst);
                Ok(dst)
            }
            ExprKind::True => {
                if hint.is_none() { self.alloc_reg()?; }
                self.emit(enc_abc(Op::LoadBool, dst, 1, 0));
                Ok(dst)
            }
            ExprKind::False => {
                if hint.is_none() { self.alloc_reg()?; }
                self.emit(enc_abc(Op::LoadBool, dst, 0, 0));
                Ok(dst)
            }
            ExprKind::IntK(n) if n >= -BIAS as i64 && n <= BIAS as i64 => {
                if hint.is_none() { self.alloc_reg()?; }
                self.emit(enc_asbx(Op::LoadInt, dst, n as i32));
                Ok(dst)
            }
            ExprKind::IntK(n) => {
                if hint.is_none() { self.alloc_reg()?; }
                let ki = self.const_val(Const::Int(n));
                self.emit(enc_abx(Op::LoadK, dst, ki as u16));
                Ok(dst)
            }
            ExprKind::FloatK(f) => {
                if hint.is_none() { self.alloc_reg()?; }
                let ki = self.const_val(Const::Float(f));
                self.emit(enc_abx(Op::LoadK, dst, ki as u16));
                Ok(dst)
            }
            ExprKind::K(ki) => {
                if hint.is_none() { self.alloc_reg()?; }
                self.emit(enc_abx(Op::LoadK, dst, ki as u16));
                Ok(dst)
            }
            ExprKind::Global(ki) => {
                if hint.is_none() { self.alloc_reg()?; }
                self.emit(enc_abx(Op::GetGlobal, dst, ki as u16));
                Ok(dst)
            }
            ExprKind::Indexed(t, k) => {
                if hint.is_none() { self.alloc_reg()?; }
                self.emit(enc_abc(Op::GetTable, dst, t, k));
                Ok(dst)
            }
            ExprKind::Upval(uv_idx) => {
                if hint.is_none() { self.alloc_reg()?; }
                self.emit(enc_abc(Op::GetUpval, dst, uv_idx, 0));
                Ok(dst)
            }
        }
    }

    fn to_rk(&mut self, e: Expr2) -> CResult<usize> {
        match e.kind {
            ExprKind::Nil      => self.rk_const(Const::Nil),
            ExprKind::True     => self.rk_const(Const::Bool(true)),
            ExprKind::False    => self.rk_const(Const::Bool(false)),
            ExprKind::IntK(n)  => self.rk_const(Const::Int(n)),
            ExprKind::FloatK(f)=> self.rk_const(Const::Float(f)),
            ExprKind::K(i)     => if i < MAX_RK_CONSTS { Ok(i | RK_BIT) } else { Ok(self.to_reg(e, None)? as usize) },
            ExprKind::Reg(r)   => Ok(r as usize),
            ExprKind::Upval(_) => {
                let r = self.to_reg(e, None)?;
                Ok(r as usize)
            }
            _ => {
                let r = self.to_reg(e, None)?;
                Ok(r as usize)
            }
        }
    }

    fn compile_block(&mut self, block: &Block) -> CResult<()> {
        self.new_block();
        let locals_top = self.locals_top();
        let reg_top = self.free_reg;
        let last = block.stmts.len().saturating_sub(1);
        for (i, stmt) in block.stmts.iter().enumerate() {
            self.tail_stmt = i == last;
            self.compile_stmt(stmt)?;
        }
        if let Some(ret) = &block.ret {
            self.compile_return(ret, block.line)?;
        } else {
            self.emit_closes(locals_top)?;
        }
        self.pop_locals_to(locals_top);
        self.free_reg_to(reg_top);
        self.end_block();
        Ok(())
    }

    // A trailing call or `...` returns all its values (Return b=0). With
    // <close> locals pending the close calls need scratch registers above the
    // return values, which a variable-length tail can't guarantee, so a
    // trailing call is then truncated to one result and `...` is expanded
    // only after the closes have run.
    fn compile_return(&mut self, vals: &[Expr], line: u32) -> CResult<()> {
        self.line = line;
        let closes = self.has_closes(0);
        if vals.is_empty() {
            if closes { self.emit_closes(0)?; }
            self.emit(enc_abc(Op::Return, 0, 1, 0));
            return Ok(());
        }
        let base = self.free_reg;
        let n = vals.len();
        let tail_vararg = matches!(vals[n - 1], Expr::Vararg(_));
        if tail_vararg { self.check_vararg(line)?; }
        let tail_call = !closes && matches!(vals[n - 1], Expr::Call(_) | Expr::MethodCall(_));
        let fixed = if tail_vararg || tail_call { n - 1 } else { n };
        for (i, v) in vals[..fixed].iter().enumerate() {
            let dst = base + i as u8;
            let e = self.compile_expr(v)?;
            self.to_reg(e, Some(dst))?;
            self.free_reg = dst + 1;
        }
        if tail_vararg {
            if closes { self.emit_closes(0)?; }
            self.reserve(base as usize + fixed + 1)?;
            self.emit(enc_abc(Op::Vararg, base + fixed as u8, 0, 0));
            self.emit(enc_abc(Op::Return, base, 0, 0));
        } else if tail_call {
            match &vals[n - 1] {
                Expr::Call(c) => self.compile_call(c, base + fixed as u8, MULTRET)?,
                Expr::MethodCall(m) => self.compile_method_call(m, base + fixed as u8, MULTRET)?,
                _ => unreachable!(),
            }
            self.emit(enc_abc(Op::Return, base, 0, 0));
        } else {
            if closes { self.emit_closes(0)?; }
            self.emit(enc_abc(Op::Return, base, n as u8 + 1, 0));
        }
        Ok(())
    }

    fn compile_stmt(&mut self, stmt: &Stmt) -> CResult<()> {
        if self.depth >= MAX_DEPTH {
            return Err(err("chunk too deeply nested", self.line));
        }
        self.depth += 1;
        let r = self.compile_stmt_inner(stmt);
        self.depth -= 1;
        r
    }

    fn compile_stmt_inner(&mut self, stmt: &Stmt) -> CResult<()> {
        match stmt {
            Stmt::Local { mutable, names, closes, values, line } => {
                self.line = *line;
                let base = self.free_reg;
                let nv = values.len();
                let nn = names.len();
                for (i, val) in values.iter().enumerate() {
                    let dst = base as usize + i;
                    let is_last = i + 1 == nv;
                    let want = if is_last && nn > i { nn - i } else { 1 };
                    if want > 1 {
                        // The whole `want`-wide result window must fit before
                        // any operand is truncated to u8.
                        self.reserve(dst + want)?;
                        match val {
                            Expr::Call(c) => { self.compile_call(c, dst as u8, want as u8)?; }
                            Expr::MethodCall(m) => { self.compile_method_call(m, dst as u8, want as u8)?; }
                            Expr::Vararg(_) => {
                                self.check_vararg(*line)?;
                                self.emit(enc_abc(Op::Vararg, dst as u8, want as u8 + 1, 0));
                            }
                            _ => {
                                let e = self.compile_expr(val)?;
                                self.to_reg(e, Some(dst as u8))?;
                                for j in 1..want { self.emit_load_nil((dst + j) as u8); }
                            }
                        }
                        if (self.free_reg as usize) < dst + want { self.free_reg = (dst + want) as u8; }
                    } else {
                        let e = self.compile_expr(val)?;
                        self.to_reg(e, Some(dst as u8))?;
                        if self.free_reg as usize <= dst { self.free_reg = (dst + 1) as u8; }
                    }
                }
                let filled = if nv > 0 {
                    let last_i = nv - 1;
                    let last_want = if nn > last_i { nn - last_i } else { 1 };
                    last_i + last_want
                } else { 0 };
                for i in filled..nn {
                    let dst = base as usize + i;
                    self.reserve(dst + 1)?;
                    if self.free_reg as usize <= dst { self.free_reg = (dst + 1) as u8; }
                    self.emit_load_nil(dst as u8);
                }
                for (i, name) in names.iter().enumerate() {
                    let boxed = self.captured.contains(name);
                    self.locals.push(Local { name: name.clone(), reg: base + i as u8, mutable: *mutable, close: closes[i], boxed });
                }
                if self.free_reg < base + nn as u8 { self.free_reg = base + nn as u8; }
                for (i, name) in names.iter().enumerate() {
                    if self.captured.contains(name) { self.box_in_place(base + i as u8)?; }
                    if closes[i] {
                        let boxed = self.captured.contains(name) as u8;
                        self.emit(enc_abc(Op::Tbc, base + i as u8, boxed, 0));
                    }
                }
            }

            Stmt::Destructure { mutable, fields, value, line } => {
                self.line = *line;
                let ve = self.compile_expr(value)?;
                let vr = self.to_reg(ve, None)?;
                for name in fields {
                    let dst = self.alloc_reg()?;
                    let fki = self.rk_str(name)?;
                    self.emit(enc_abc(Op::GetTable, dst, vr, fki));
                    let boxed = self.captured.contains(name);
                    self.locals.push(Local { name: name.clone(), reg: dst, mutable: *mutable, close: false, boxed });
                    if boxed { self.box_in_place(dst)?; }
                }
                if self.proto.max_regs < self.free_reg { self.proto.max_regs = self.free_reg; }
            }

            Stmt::Assign { targets, values, line } => {
                self.line = *line;
                let tmp_base = self.free_reg;
                let nv = values.len();
                let nt = targets.len();
                for i in 0..nv {
                    let dst = tmp_base as usize + i;
                    let is_last = i + 1 == nv;
                    let want = if is_last && nt > i { nt - i } else { 1 };
                    if want > 1 {
                        self.reserve(dst + want)?;
                        match &values[i] {
                            Expr::Call(c) => { self.compile_call(c, dst as u8, want as u8)?; }
                            Expr::MethodCall(m) => { self.compile_method_call(m, dst as u8, want as u8)?; }
                            Expr::Vararg(_) => {
                                self.check_vararg(*line)?;
                                self.emit(enc_abc(Op::Vararg, dst as u8, want as u8 + 1, 0));
                            }
                            val => {
                                let e = self.compile_expr(val)?;
                                self.to_reg(e, Some(dst as u8))?;
                                for j in 1..want { self.emit_load_nil((dst + j) as u8); }
                            }
                        }
                        if (self.free_reg as usize) < dst + want { self.free_reg = (dst + want) as u8; }
                    } else {
                        let e = self.compile_expr(&values[i])?;
                        self.to_reg(e, Some(dst as u8))?;
                        if self.free_reg as usize <= dst { self.free_reg = (dst + 1) as u8; }
                    }
                }
                let filled = if nv > 0 {
                    let last_want = if nt > nv - 1 { nt - (nv - 1) } else { 1 };
                    (nv - 1) + last_want
                } else { 0 };
                for i in filled..nt {
                    let dst = tmp_base as usize + i;
                    self.reserve(dst + 1)?;
                    if self.free_reg as usize <= dst { self.free_reg = (dst + 1) as u8; }
                    self.emit_load_nil(dst as u8);
                }
                self.reserve(self.free_reg as usize)?;
                for (i, tgt) in targets.iter().enumerate() {
                    self.assign_target(tgt, tmp_base + i as u8)?;
                }
                self.free_reg_to(tmp_base);
            }

            Stmt::Do { body, .. } => {
                self.compile_block(body)?;
            }

            Stmt::While { cond, body, line } => {
                self.line = *line;
                let loop_top = self.pc();
                let exit_jump = self.emit_cond_test(cond)?;


                self.loops.push(LoopScope { break_jumps: Vec::new(), continue_jumps: Vec::new(), locals_top: self.locals_top() });
                self.compile_block(body)?;
                let scope = self.loops.pop().unwrap();

                // continue jumps straight to the backedge — same target the loop's
                // own fallthrough uses, so it re-checks cond exactly like a normal iteration.
                let back_pos = self.pc();
                for cj in scope.continue_jumps { self.patch(cj, back_pos)?; }

                self.emit_back(Op::Jmp, 0, loop_top)?;
                let here = self.pc();
                self.patch(exit_jump, here)?;
                for bj in scope.break_jumps {
                    self.patch(bj, here)?;
                }
            }

            Stmt::RepeatUntil { body, cond, line } => {
                self.line = *line;
                let loop_top = self.pc();
                let reg_top = self.free_reg;
                self.loops.push(LoopScope { break_jumps: Vec::new(), continue_jumps: Vec::new(), locals_top: self.locals_top() });
                self.new_block();
                let locals_top = self.locals_top();
                let last = body.stmts.len().saturating_sub(1);
                for (i, stmt) in body.stmts.iter().enumerate() {
                    self.tail_stmt = i == last;
                    self.compile_stmt(stmt)?;
                }
                if let Some(r) = &body.ret { self.compile_return(r, body.line)?; }

                // cond is compiled before popping the body's locals — Lua's repeat-until
                // scoping rule lets `until` see locals the body just declared.
                let cond_pos = self.pc();
                let back = self.emit_cond_test(cond)?;
                self.patch(back, loop_top)?;

                self.emit_closes(locals_top)?;
                self.pop_locals_to(locals_top);
                self.free_reg_to(reg_top);
                self.end_block();
                let scope = self.loops.pop().unwrap();

                let here = self.pc();
                for cj in scope.continue_jumps { self.patch(cj, cond_pos)?; }
                for bj in scope.break_jumps { self.patch(bj, here)?; }
            }

            Stmt::If { cond, then, elseifs, else_, line } => {
                self.line = *line;
                let mut exit_jumps = Vec::new();

                let test = |fc: &mut Self, cond: &Expr| -> CResult<usize> {
                    fc.emit_cond_test(cond)
                };

                let fail = test(self, cond)?;
                self.compile_block(then)?;
                if !elseifs.is_empty() || else_.is_some() {
                    exit_jumps.push(self.proto.emit_jump(*line));
                }
                self.patch_here(fail)?;

                for (ei_cond, ei_body) in elseifs {
                    let f2 = test(self, ei_cond)?;
                    self.compile_block(ei_body)?;
                    exit_jumps.push(self.proto.emit_jump(*line));
                    self.patch_here(f2)?;
                }
                if let Some(eb) = else_ {
                    self.compile_block(eb)?;
                }
                let here = self.pc();
                for j in exit_jumps { self.patch(j, here)?; }
            }

            Stmt::ForNum { var, start, limit, step, body, line } => {
                self.line = *line;
                let base = self.free_reg;
                // Slots: base=iter, base+1=limit, base+2=step, base+3=loop var
                for _ in 0..4 { self.alloc_reg()?; }

                let se = self.compile_expr(start)?; self.to_reg(se, Some(base))?;
                let le = self.compile_expr(limit)?; self.to_reg(le, Some(base + 1))?;
                let step_e = if let Some(s) = step {
                    self.compile_expr(s)?
                } else {
                    Expr2 { kind: ExprKind::IntK(1), line: *line }
                };
                self.to_reg(step_e, Some(base + 2))?;

                let prep = self.pc();
                self.emit(enc_asbx(Op::ForPrep, base as u8, 0));

                let loop_top = self.pc();
                let lv_reg = base + 3;
                // Never boxed: ForPrep/ForLoop write the raw counter into lv_reg
                // directly every iteration, which would stomp a box placed here.
                // A closure capturing the loop variable shares one variable across
                // all iterations — the classic pre-5.4-Lua for-loop-capture wart,
                // not something this fix attempts to solve.
                self.locals.push(Local { name: var.clone(), reg: lv_reg, mutable: true, close: false, boxed: false });
                let locals_save = self.locals.len() - 1;

                self.loops.push(LoopScope { break_jumps: Vec::new(), continue_jumps: Vec::new(), locals_top: self.locals_top() });
                self.new_block();
                let inner_locals = self.locals_top();
                let last = body.stmts.len().saturating_sub(1);
                for (i, stmt) in body.stmts.iter().enumerate() {
                    self.tail_stmt = i == last;
                    self.compile_stmt(stmt)?;
                }
                if let Some(r) = &body.ret {
                    self.compile_return(r, body.line)?;
                } else {
                    self.emit_closes(inner_locals)?;
                }
                self.pop_locals_to(inner_locals);
                self.end_block();
                let scope = self.loops.pop().unwrap();

                // continue jumps to the ForLoop step/condition-check itself.
                let step_pos = self.pc();
                for cj in scope.continue_jumps { self.patch(cj, step_pos)?; }

                self.emit_back(Op::ForLoop, base as u8, loop_top)?;
                let here = self.pc();

                // ForPrep skips the body straight to the ForLoop instruction
                // when the range is empty; same signed-offset range check.
                let prep_off = here as i64 - prep as i64 - 2;
                if prep_off < -BIAS as i64 || prep_off > u16::MAX as i64 - BIAS as i64 {
                    return Err(err("function too large (jump out of range)", *line));
                }
                self.proto.code[prep] = enc_asbx(Op::ForPrep, base as u8, prep_off as i32);

                self.locals.truncate(locals_save);
                for bj in scope.break_jumps { self.patch(bj, here)?; }
                self.free_reg_to(base);
            }

            Stmt::ForIn { vars, iters, body, line } => {
                self.line = *line;
                let base = self.free_reg;
                // Layout: base=iter fn, base+1=state, base+2=control, base+3=unused buf,
                // base+4..=loop vars (TForCall writes results starting at A+4).
                for _ in 0..(4 + vars.len()) { self.alloc_reg()?; }

                if iters.len() == 1 {
                    match &iters[0] {
                        Expr::Call(c) => { self.compile_call(c, base, 3)?; }
                        Expr::MethodCall(m) => { self.compile_method_call(m, base, 3)?; }
                        it0 => {
                            let e = self.compile_expr(it0)?;
                            self.to_reg(e, Some(base))?;
                            self.emit_load_nil(base + 1);
                            self.emit_load_nil(base + 2);
                        }
                    }
                    self.free_reg = base + 4 + vars.len() as u8;
                } else {
                    for (i, it) in iters.iter().enumerate().take(3) {
                        let e = self.compile_expr(it)?;
                        self.to_reg(e, Some(base + i as u8))?;
                    }
                    if iters.len() < 2 { self.emit_load_nil(base + 1); }
                    if iters.len() < 3 { self.emit_load_nil(base + 2); }
                }

                let loop_jmp = self.proto.emit_jump(*line);
                let loop_top = self.pc();

                let lv_base = self.locals_top();
                // Never boxed, same reasoning as ForNum: TForCall overwrites these
                // registers directly every iteration.
                for (i, v) in vars.iter().enumerate() {
                    self.locals.push(Local { name: v.clone(), reg: base + 4 + i as u8, mutable: true, close: false, boxed: false });
                }

                self.loops.push(LoopScope { break_jumps: Vec::new(), continue_jumps: Vec::new(), locals_top: self.locals_top() });
                self.new_block();
                let inner_locals = self.locals_top();
                let last = body.stmts.len().saturating_sub(1);
                for (i, s) in body.stmts.iter().enumerate() {
                    self.tail_stmt = i == last;
                    self.compile_stmt(s)?;
                }
                if let Some(r) = &body.ret { self.compile_return(r, body.line)?; } else { self.emit_closes(inner_locals)?; }
                self.pop_locals_to(inner_locals);
                self.end_block();
                let scope = self.loops.pop().unwrap();

                // continue jumps to the TForCall/TForLoop pair that advances the iterator.
                let advance_pos = self.pc();
                for cj in scope.continue_jumps { self.patch(cj, advance_pos)?; }

                self.patch_here(loop_jmp)?;
                self.emit(enc_abc(Op::TForCall, base as u8, 0, vars.len() as u8));
                self.emit_back(Op::TForLoop, base as u8, loop_top)?;

                let here = self.pc();
                self.locals.truncate(lv_base);
                for bj in scope.break_jumps { self.patch(bj, here)?; }
                self.free_reg_to(base);
            }

            Stmt::FuncDef { name, body, line } => {
                self.line = *line;
                let outer = Some(self as *mut FnComp);
                let proto = compile_fn(body, self.proto.source.clone(), outer, self.depth)?;
                let pi = self.proto.protos.len();
                self.proto.protos.push(proto);
                let dst = self.alloc_reg()?;
                self.emit(enc_abx(Op::Closure, dst, pi as u16));

                let nparts = name.parts.len();
                if nparts == 1 && name.method.is_none() {
                    let ki = self.const_str(&name.parts[0]);
                    self.emit(enc_abx(Op::SetGlobal, dst, ki as u16));
                } else {
                    let ki = self.const_str(&name.parts[0]);
                    let base_r = self.alloc_reg()?;
                    if let Some((lr, boxed)) = self.resolve_local(&name.parts[0]) {
                        if boxed { self.unbox_into(base_r, lr)?; } else { self.emit_move(base_r, lr); }
                    } else if let Some((uv, boxed)) = self.resolve_upval(&name.parts[0]) {
                        self.emit(enc_abc(Op::GetUpval, base_r, uv, 0));
                        if boxed { self.unbox_into(base_r, base_r)?; }
                    } else {
                        self.emit(enc_abx(Op::GetGlobal, base_r, ki as u16));
                    }
                    let mut cur = base_r;
                    let field_end = if name.method.is_some() { nparts } else { nparts - 1 };
                    for p in &name.parts[1..field_end] {
                        let fki = self.rk_str(p)?;
                        let next = self.alloc_reg()?;
                        self.emit(enc_abc(Op::GetTable, next, cur, fki));
                        cur = next;
                    }
                    let last_key = name.method.as_deref()
                        .unwrap_or_else(|| name.parts.last().unwrap());
                    let lki = self.rk_str(last_key)?;
                    self.emit(enc_abc(Op::SetTable, cur, lki, dst));
                    self.free_reg_to(base_r);
                }
                self.free_reg_to(dst);
            }

            Stmt::LocalFunc { name, body, line } => {
                self.line = *line;
                // outer_scope is a live pointer back to this FnComp, so a
                // self-recursive call inside body needs "name" registered as
                // a local *before* compiling body, so resolve_upval can find
                // it. That's only safe because of boxing: an empty box is
                // created up front and registered now, body captures that
                // box by reference if it self-recurses, and only afterward
                // does the actual closure value get written into the box —
                // so the recursive call sees a real closure once it runs,
                // never the empty placeholder. Without boxing (the `else`
                // arm) there's no such placeholder to capture, so a
                // non-recursive LocalFunc just gets Closure written directly.
                let outer = Some(self as *mut FnComp);
                let r = self.alloc_reg()?;
                let boxed = self.captured.contains(name);
                if boxed { self.emit(enc_abc(Op::NewTable, r, 0, 0)); }
                self.locals.push(Local { name: name.clone(), reg: r, mutable: true, close: false, boxed });
                let proto = compile_fn(body, self.proto.source.clone(), outer, self.depth)?;
                let pi = self.proto.protos.len();
                self.proto.protos.push(proto);
                if boxed {
                    let tmp = self.alloc_reg()?;
                    self.emit(enc_abx(Op::Closure, tmp, pi as u16));
                    self.set_boxed(r, tmp)?;
                    self.free_reg_to(tmp);
                } else {
                    self.emit(enc_abx(Op::Closure, r, pi as u16));
                }
            }

            Stmt::Call(c) => {
                self.line = c.line;
                let base = self.free_reg;
                self.compile_call(c, base, 0)?;
                self.free_reg_to(base);
            }
            Stmt::ExprStmt(e) => {
                let base = self.free_reg;
                self.compile_expr(e)?;
                self.free_reg_to(base);
            }

            Stmt::MethodCall(m) => {
                self.line = m.line;
                let base = self.free_reg;
                self.compile_method_call(m, base, 0)?;
                self.free_reg_to(base);
            }

            Stmt::Break(line) | Stmt::Continue(line) => {
                self.line = *line;
                let is_break = matches!(stmt, Stmt::Break(_));
                let locals_top = match self.loops.last() {
                    Some(scope) => scope.locals_top,
                    None => return Err(err(if is_break { "'break' outside loop" } else { "'continue' outside loop" }, *line)),
                };
                self.emit_closes(locals_top)?;
                let j = self.proto.emit_jump(*line);
                let scope = self.loops.last_mut().unwrap();
                if is_break { scope.break_jumps.push(j); } else { scope.continue_jumps.push(j); }
            }
            Stmt::Goto(name, line) => {
                self.line = *line;
                let j = self.proto.emit_jump(*line);
                let closers = self.locals.iter().enumerate()
                    .filter(|(_, l)| l.close)
                    .map(|(i, l)| (i, l.reg, l.boxed))
                    .collect();
                self.pending_gotos.push(PendingGoto {
                    name: name.clone(), jump: j, line: *line,
                    block: self.cur_block(), locals: self.locals_top(),
                    closers, free_reg: self.free_reg,
                });
            }
            Stmt::Label(name, line) => {
                self.line = *line;
                if self.labels.contains_key(name) {
                    return Err(err(format!("label '{name}' already defined in this function"), *line));
                }
                self.labels.insert(name.clone(), LabelInfo {
                    pc: self.pc(), block: self.cur_block(), locals: self.locals_top(),
                });
            }
        }
        Ok(())
    }

    fn assign_target(&mut self, tgt: &Expr, src: u8) -> CResult<()> {
        match tgt {
            Expr::Ident(id) => {
                let local = self.locals.iter().rev().find(|l| l.name == id.name).cloned();
                if let Some(loc) = local {
                    if !loc.mutable {
                        return Err(err(
                            format!("cannot assign to immutable 'let' binding '{}'", id.name),
                            id.line,
                        ));
                    }
                    if loc.boxed {
                        self.set_boxed(loc.reg, src)?;
                    } else if loc.reg != src {
                        self.emit_move(loc.reg, src);
                    }
                } else if let Some((uv_idx, boxed)) = self.resolve_upval(&id.name) {
                    if boxed {
                        let tmp = self.alloc_reg()?;
                        self.emit(enc_abc(Op::GetUpval, tmp, uv_idx, 0));
                        self.set_boxed(tmp, src)?;
                        self.free_reg_to(tmp);
                    } else {
                        self.emit(enc_abc(Op::SetUpval, src, uv_idx, 0));
                    }
                } else {
                    let ki = self.const_str(&id.name);
                    self.emit(enc_abx(Op::SetGlobal, src, ki as u16));
                }
            }
            Expr::Field { table, field, line } => {
                self.line = *line;
                let reg_top = self.free_reg;
                let te = self.compile_expr(table)?;
                let tr = self.to_reg(te, None)?;
                let fki = self.rk_str(field)?;
                self.emit(enc_abc(Op::SetTable, tr, fki, src));
                self.free_reg_to(reg_top);
            }
            Expr::Index { table, key, line } => {
                self.line = *line;
                let reg_top = self.free_reg;
                let te = self.compile_expr(table)?;
                let tr = self.to_reg(te, None)?;
                let ke = self.compile_expr(key)?;
                let kr = self.to_rk(ke)?;
                self.emit(enc_abc(Op::SetTable, tr, kr as u8, src));
                self.free_reg_to(reg_top);
            }
            other => return Err(err("invalid assignment target", other.line())),
        }
        Ok(())
    }

    fn compile_expr(&mut self, expr: &Expr) -> CResult<Expr2> {
        if self.depth >= MAX_DEPTH {
            return Err(err("chunk too deeply nested", expr.line()));
        }
        self.depth += 1;
        let r = self.compile_expr_inner(expr);
        self.depth -= 1;
        r
    }

    fn compile_expr_inner(&mut self, expr: &Expr) -> CResult<Expr2> {
        let line = expr.line();
        self.line = line;
        match expr {
            Expr::Nil(_)   => Ok(Expr2 { kind: ExprKind::Nil, line }),
            Expr::True(_)  => Ok(Expr2 { kind: ExprKind::True, line }),
            Expr::False(_) => Ok(Expr2 { kind: ExprKind::False, line }),
            Expr::Int(n, _)   => Ok(Expr2 { kind: ExprKind::IntK(*n), line }),
            Expr::Float(f, _) => Ok(Expr2 { kind: ExprKind::FloatK(*f), line }),
            Expr::String(s, _) => {
                let ki = self.const_str(s);
                Ok(Expr2 { kind: ExprKind::K(ki), line })
            }
            Expr::Ident(id) => {
                if let Some((r, boxed)) = self.resolve_local(&id.name) {
                    if boxed {
                        let dst = self.alloc_reg()?;
                        self.unbox_into(dst, r)?;
                        Ok(Expr2::reg(dst, line))
                    } else {
                        Ok(Expr2 { kind: ExprKind::Reg(r), line })
                    }
                } else if let Some((uv, boxed)) = self.resolve_upval(&id.name) {
                    if boxed {
                        let tmp = self.alloc_reg()?;
                        self.emit(enc_abc(Op::GetUpval, tmp, uv, 0));
                        let dst = self.alloc_reg()?;
                        self.unbox_into(dst, tmp)?;
                        Ok(Expr2::reg(dst, line))
                    } else {
                        Ok(Expr2 { kind: ExprKind::Upval(uv), line })
                    }
                } else {
                    let ki = self.const_str(&id.name);
                    Ok(Expr2 { kind: ExprKind::Global(ki), line })
                }
            }
            Expr::Vararg(l) => {
                self.check_vararg(*l)?;
                let r = self.alloc_reg()?;
                self.emit(enc_abc(Op::Vararg, r, 2, 0));
                Ok(Expr2::reg(r, line))
            }
            Expr::Unop { op, operand, line } => {
                self.line = *line;
                let e = self.compile_expr(operand)?;
                let r = self.to_reg(e, None)?;
                // When the operand landed in a scratch register (anything
                // that isn't a named local's slot), the result can reuse it —
                // same convention Expr::Field uses — instead of allocating a
                // fresh register and then freeing the result slot by mistake.
                let dst = if matches!(e.kind, ExprKind::Reg(_)) { self.alloc_reg()? } else { r };
                let instr = match op {
                    Unop::Neg  => enc_abc(Op::Unm,  dst, r, 0),
                    Unop::Not  => enc_abc(Op::Not,  dst, r, 0),
                    Unop::Len  => enc_abc(Op::Len,  dst, r, 0),
                    Unop::BNot => enc_abc(Op::BNot, dst, r, 0),
                };
                self.emit(instr);
                Ok(Expr2::reg(dst, *line))
            }
            Expr::Binop { op, lhs, rhs, line } => self.compile_binop(*op, lhs, rhs, *line),
            Expr::Concat { parts, line } => {
                self.line = *line;
                let base = self.free_reg;
                for (i, p) in parts.iter().enumerate() {
                    let slot = base + i as u8;
                    self.free_reg = slot;
                    let e = self.compile_expr(p)?;
                    match e.kind {
                        ExprKind::Reg(r) if r == slot => {}
                        ExprKind::Reg(r) => { self.emit_move(slot, r); }
                        _ => { self.free_reg = slot; self.to_reg(e, Some(slot))?; }
                    }
                    self.free_reg = slot + 1;
                }
                let top = base + parts.len() as u8 - 1;
                let dst = base;
                self.emit(enc_abc(Op::Concat, dst, base, top));
                self.free_reg = base + 1;
                Ok(Expr2::reg(dst, *line))
            }
            Expr::Function(body) => {
                let outer = Some(self as *mut FnComp);
                let proto = compile_fn(body, self.proto.source.clone(), outer, self.depth)?;
                let pi = self.proto.protos.len();
                self.proto.protos.push(proto);
                let r = self.alloc_reg()?;
                self.emit(enc_abx(Op::Closure, r, pi as u16));
                Ok(Expr2::reg(r, line))
            }
            Expr::Table(tc) => self.compile_table(tc),
            Expr::Field { table, field, line } => {
                self.line = *line;
                let te = self.compile_expr(table)?;
                let tr = self.to_reg(te, None)?;
                let fki = self.rk_str(field)?;
                let dst = self.alloc_reg()?;
                self.emit(enc_abc(Op::GetTable, dst, tr, fki));
                if !matches!(te.kind, ExprKind::Reg(_)) {
                    self.emit_move(tr, dst);
                    self.free_reg = tr + 1;
                    Ok(Expr2::reg(tr, *line))
                } else {
                    Ok(Expr2::reg(dst, *line))
                }
            }
            Expr::Index { table, key, line } => {
                self.line = *line;
                let te = self.compile_expr(table)?;
                let tr = self.to_reg(te, None)?;
                let ke = self.compile_expr(key)?;
                let kr = self.to_rk(ke)?;
                let dst = self.alloc_reg()?;
                self.emit(enc_abc(Op::GetTable, dst, tr, kr as u8));
                Ok(Expr2::reg(dst, *line))
            }
            Expr::Call(c) => {
                self.line = c.line;
                let base = self.free_reg;
                let results = 1;
                self.compile_call(c, base, results)?;
                Ok(Expr2::reg(base, c.line))
            }
            Expr::MethodCall(m) => {
                self.line = m.line;
                let base = self.free_reg;
                self.compile_method_call(m, base, 1)?;
                Ok(Expr2::reg(base, m.line))
            }
            Expr::Ternary { cond, then, else_, line } => {
                self.line = *line;
                let base = self.free_reg;
                let else_jump = self.emit_cond_test(cond)?;

                let dst = base;
                let te = self.compile_expr(then)?;
                let tr = self.to_reg(te, Some(dst))?;
                if tr != dst { self.emit_move(dst, tr); }
                self.free_reg = dst + 1;
                let end_jump = self.proto.emit_jump(*line);

                self.patch_here(else_jump)?;
                self.free_reg = dst;
                let ee = self.compile_expr(else_)?;
                let er = self.to_reg(ee, Some(dst))?;
                if er != dst { self.emit_move(dst, er); }
                self.free_reg = dst + 1;

                self.patch_here(end_jump)?;
                Ok(Expr2::reg(dst, *line))
            }
            Expr::IncrDecr { target, delta, prefix, line } => {
                self.line = *line;
                let base = self.free_reg;
                let te = self.compile_expr(target)?;
                let old_reg = self.to_reg(te, Some(base))?;
                if self.free_reg <= old_reg { self.free_reg = old_reg + 1; }

                let one = self.to_rk(Expr2 { kind: ExprKind::IntK(1), line: *line })?;
                let new_reg = self.alloc_reg()?;
                let vm_op = if *delta >= 0 { Op::Add } else { Op::Sub };
                self.emit(enc_abc(vm_op, new_reg, old_reg, one as u8));

                // Re-evaluates target's own subexpressions (e.g. an Index's key)
                // a second time for the write-back — a double-evaluation that only
                // matters if that subexpression has side effects, same simplification
                // already accepted for compound assignment.
                self.assign_target(target, new_reg)?;

                let result_reg = if *prefix { new_reg } else { old_reg };
                self.free_reg = new_reg + 1;
                Ok(Expr2::reg(result_reg, *line))
            }
        }
    }

    fn compile_binop(&mut self, op: Binop, lhs: &Expr, rhs: &Expr, line: u32) -> CResult<Expr2> {
        self.line = line;

        if op == Binop::And || op == Binop::Or {
            return self.compile_logical(op, lhs, rhs, line);
        }

        let temp_base = self.free_reg;
        let le = self.compile_expr(lhs)?;
        let lrk = self.to_rk(le)?;
        let re = self.compile_expr(rhs)?;
        let rrk = self.to_rk(re)?;

        self.free_reg = temp_base;
        let dst = self.alloc_reg()?;

        let vm_op = match op {
            Binop::Add => Op::Add, Binop::Sub => Op::Sub,
            Binop::Mul => Op::Mul, Binop::Div => Op::Div,
            Binop::IDiv=> Op::IDiv,Binop::Mod => Op::Mod,
            Binop::Pow => Op::Pow,
            Binop::BAnd=> Op::BAnd,Binop::BOr => Op::BOr,
            Binop::BXor=> Op::BXor,Binop::Shl => Op::Shl,
            Binop::Shr => Op::Shr,
            Binop::Eq  => {
                self.emit(enc_abc(Op::Eq, 1, lrk as u8, rrk as u8));
                return self.emit_bool_from_cmp(dst, line);
            }
            Binop::Ne  => {
                self.emit(enc_abc(Op::Eq, 0, lrk as u8, rrk as u8));
                return self.emit_bool_from_cmp(dst, line);
            }
            Binop::Lt  => {
                self.emit(enc_abc(Op::Lt, 1, lrk as u8, rrk as u8));
                return self.emit_bool_from_cmp(dst, line);
            }
            Binop::Le  => {
                self.emit(enc_abc(Op::Le, 1, lrk as u8, rrk as u8));
                return self.emit_bool_from_cmp(dst, line);
            }
            Binop::Gt  => {
                self.emit(enc_abc(Op::Lt, 1, rrk as u8, lrk as u8));
                return self.emit_bool_from_cmp(dst, line);
            }
            Binop::Ge  => {
                self.emit(enc_abc(Op::Le, 1, rrk as u8, lrk as u8));
                return self.emit_bool_from_cmp(dst, line);
            }
            Binop::Concat | Binop::And | Binop::Or => unreachable!(),
        };
        self.emit(enc_abc(vm_op, dst, lrk as u8, rrk as u8));
        Ok(Expr2::reg(dst, line))
    }

    // The comparison opcode's A=1 makes it skip the following JMP when the
    // condition is FALSE; the JMP (taken on TRUE) lands past the false-case
    // LoadBool, so both paths converge with dst set correctly.
    fn emit_bool_from_cmp(&mut self, dst: u8, line: u32) -> CResult<Expr2> {
        let to_true = self.proto.emit_jump(line);
        self.emit(enc_abc(Op::LoadBool, dst, 0, 1));
        self.patch_here(to_true)?;
        self.emit(enc_abc(Op::LoadBool, dst, 1, 0));
        Ok(Expr2::reg(dst, line))
    }

    fn compile_logical(&mut self, op: Binop, lhs: &Expr, rhs: &Expr, line: u32) -> CResult<Expr2> {
        let base = self.free_reg;
        let le = self.compile_expr(lhs)?;
        let lr = self.to_reg(le, Some(base))?;
        if self.free_reg <= lr { self.free_reg = lr + 1; }

        let want_c: u8 = if op == Binop::Or { 1 } else { 0 };
        self.emit(enc_abc(Op::TestSet, base, lr, want_c));
        let skip = self.proto.emit_jump(line);
        self.free_reg_to(base);

        let re = self.compile_expr(rhs)?;
        let rr = self.to_reg(re, Some(base))?;
        if rr != base { self.emit_move(base, rr); }

        self.patch_here(skip)?;
        self.free_reg = base + 1;
        Ok(Expr2::reg(base, line))
    }

    fn compile_table(&mut self, tc: &TableConstructor) -> CResult<Expr2> {
        self.line = tc.line;
        let dst = self.alloc_reg()?;
        let arr_hint = tc.fields.iter().filter(|f| matches!(f, TableField::Positional(_))).count();
        let hash_hint = tc.fields.len() - arr_hint;
        self.emit(enc_abc(Op::NewTable, dst, arr_hint.min(255) as u8, hash_hint.min(255) as u8));

        let mut arr_idx: usize = 0;
        let field_base = self.free_reg;
        let nfields = tc.fields.len();
        // SetList's C operand is the 1-based batch number in a u8, so the
        // array part can't exceed 255 batches of 50.
        let arr_total = tc.fields.iter().filter(|f| matches!(f, TableField::Positional(_))).count();
        if arr_total > 255 * 50 {
            return Err(err("table constructor too large", tc.line));
        }
        // Like Lua, `...` or a call as the very last field contributes every
        // value it produces (SetList b=0 reads up to frame.top).
        let mut open_tail = false;

        for (fi, field) in tc.fields.iter().enumerate() {
            match field {
                TableField::Named { key, val, line } => {
                    self.line = *line;
                    let fki = self.rk_str(key)?;
                    let ve = self.compile_expr(val)?;
                    let vr = self.to_rk(ve)?;
                    self.emit(enc_abc(Op::SetTable, dst, fki, vr as u8));
                }
                TableField::Indexed { key, val } => {
                    let ke = self.compile_expr(key)?;
                    let kr = self.to_rk(ke)?;
                    let ve = self.compile_expr(val)?;
                    let vr = self.to_rk(ve)?;
                    self.emit(enc_abc(Op::SetTable, dst, kr as u8, vr as u8));
                }
                TableField::Positional(val) => {
                    arr_idx += 1;
                    let slot = field_base + ((arr_idx - 1) % 50) as u8;
                    let is_last = fi + 1 == nfields;
                    match val {
                        Expr::Vararg(_) if is_last => {
                            self.check_vararg(tc.line)?;
                            self.reserve(slot as usize + 1)?;
                            self.emit(enc_abc(Op::Vararg, slot, 0, 0));
                            open_tail = true;
                        }
                        Expr::Call(c) if is_last => { self.compile_call(c, slot, MULTRET)?; open_tail = true; }
                        Expr::MethodCall(m) if is_last => { self.compile_method_call(m, slot, MULTRET)?; open_tail = true; }
                        _ => {
                            let ve = self.compile_expr(val)?;
                            self.to_reg(ve, Some(slot))?;
                            self.free_reg = slot + 1;
                        }
                    }
                    if !open_tail && arr_idx % 50 == 0 {
                        let c = (arr_idx / 50) as u8;
                        self.emit(enc_abc(Op::SetList, dst, 50, c));
                        self.free_reg_to(field_base);
                    }
                }
            }
        }
        let rem = arr_idx % 50;
        if open_tail {
            let c = ((arr_idx - 1) / 50 + 1) as u8;
            self.emit(enc_abc(Op::SetList, dst, 0, c));
            self.free_reg_to(field_base);
        } else if rem > 0 {
            let c = (arr_idx / 50 + 1) as u8;
            self.emit(enc_abc(Op::SetList, dst, rem as u8, c));
            self.free_reg_to(field_base);
        }

        Ok(Expr2::reg(dst, tc.line))
    }

    // Callee at `base`, args above it, results land back at `base`.
    fn compile_call(&mut self, c: &CallExpr, base: u8, nresults: u8) -> CResult<()> {
        let ce = self.compile_expr(&c.callee)?;
        self.to_reg(ce, Some(base))?;
        self.free_reg = base + 1;

        let (nargs, is_variable) = self.push_args(&c.args, base + 1)?;
        let b = if is_variable { 0 } else { nargs + 1 };
        self.emit_call(base, b, nresults)
    }

    // Method at `base`, receiver at `base+1` (the implicit first arg), so the
    // results land at `base` exactly like compile_call.
    fn compile_method_call(&mut self, m: &MethodCallExpr, base: u8, nresults: u8) -> CResult<()> {
        let re = self.compile_expr(&m.receiver)?;
        self.to_reg(re, Some(base + 1))?;
        self.free_reg = base + 2;

        let fki = self.rk_str(&m.method)?;
        self.emit(enc_abc(Op::GetTable, base, base + 1, fki));
        let (nargs, is_variable) = self.push_args(&m.args, base + 2)?;
        let b = if is_variable { 0 } else { nargs + 2 };
        self.emit_call(base, b, nresults)
    }

    fn emit_call(&mut self, base: u8, b: u8, nresults: u8) -> CResult<()> {
        if nresults == MULTRET {
            self.emit(enc_abc(Op::Call, base, b, 0));
            self.free_reg = base + 1;
        } else {
            self.reserve(base as usize + nresults as usize)?;
            self.emit(enc_abc(Op::Call, base, b, nresults + 1));
            self.free_reg = base + nresults;
        }
        Ok(())
    }

    // `is_variable` means the last argument was `...` or a call: the VM then
    // reads frame.top (set by Vararg b=0 / Call c=0) for the real arg count.
    fn push_args(&mut self, args: &Args, arg_base: u8) -> CResult<(u8, bool)> {
        self.free_reg = arg_base;
        match args {
            Args::Exprs(exprs) => {
                let n = exprs.len();
                if n == 0 { return Ok((0, false)); }
                if arg_base as usize + n > MAX_REGS as usize { return Err(err("too many arguments", self.line)); }
                let variable = matches!(exprs[n - 1], Expr::Vararg(_) | Expr::Call(_) | Expr::MethodCall(_));
                let fixed = if variable { n - 1 } else { n };
                for (i, e) in exprs[..fixed].iter().enumerate() {
                    let slot = arg_base + i as u8;
                    let ev = self.compile_expr(e)?;
                    self.to_reg(ev, Some(slot))?;
                    self.free_reg = slot + 1;
                }
                if !variable { return Ok((n as u8, false)); }
                let slot = arg_base + fixed as u8;
                match &exprs[n - 1] {
                    Expr::Vararg(_) => { self.check_vararg(self.line)?; self.emit(enc_abc(Op::Vararg, slot, 0, 0)); }
                    Expr::Call(c) => self.compile_call(c, slot, MULTRET)?,
                    Expr::MethodCall(m) => self.compile_method_call(m, slot, MULTRET)?,
                    _ => unreachable!(),
                }
                Ok((fixed as u8, true))
            }
            Args::String(s) => {
                let ki = self.const_str(s);
                self.reserve(arg_base as usize + 1)?;
                self.emit(enc_abx(Op::LoadK, arg_base, ki as u16));
                self.free_reg = arg_base + 1;
                Ok((1, false))
            }
            Args::Table(tc) => {
                let te = self.compile_table(tc)?;
                self.to_reg(te, Some(arg_base))?;
                self.free_reg = arg_base + 1;
                Ok((1, false))
            }
        }
    }
}

fn compile_fn(body: &FuncBody, source: Option<String>, outer: Option<*mut FnComp>, depth: u32) -> CResult<Proto> {
    let captured = collect_captured_names(body)?;
    let mut fc = FnComp::new(source, outer, captured, depth, body.vararg);
    if body.params.len() > MAX_REGS as usize {
        return Err(err("too many parameters", body.line));
    }
    fc.proto.params = body.params.len() as u8;
    fc.proto.is_vararg = body.vararg;

    // Two passes: the calling convention places every argument into its
    // register before the function body runs at all, so free_reg must
    // reflect all of them before any box_in_place scratch allocation — boxing
    // param i while param i+1's register is still "unclaimed" from the
    // compiler's perspective would let the scratch table clobber param i+1's
    // live incoming value.
    let mut param_regs = Vec::with_capacity(body.params.len());
    for p in &body.params {
        param_regs.push(fc.push_local(p.clone(), true)?);
    }
    for (p, r) in body.params.iter().zip(param_regs) {
        if fc.captured.contains(p) { fc.box_in_place(r)?; }
    }

    fc.compile_block(&body.body)?;

    // The implicit return must precede any goto stubs: a stub is only
    // reachable through its patched jump, so it has to live past the point
    // where normal fall-through exits the function.
    if fc.proto.code.last().map(|&i| iop(i)) != Some(Op::Return as u8) {
        fc.emit(enc_abc(Op::Return, 0, 1, 0));
    }

    // Resolve gotos now that every label is known. A goto may only reach a
    // label visible from its block (Lua's scoping rule) and may not enter
    // the scope of a local declared after the label. When the jump leaves
    // the scope of <close> locals, it is retargeted at a stub appended here
    // that runs their close() calls before jumping on to the label.
    let gotos = std::mem::take(&mut fc.pending_gotos);
    for g in &gotos {
        match fc.labels.get(&g.name).cloned() {
            Some(l) => {
                if !fc.label_visible(&l, g.block) {
                    return Err(err(format!("no visible label '{}' for goto", g.name), g.line));
                }
                if l.locals > g.locals {
                    return Err(err(format!("goto '{}' jumps into the scope of a local", g.name), g.line));
                }
                let leavers: Vec<(u8, bool)> = g.closers.iter()
                    .filter(|(i, _, _)| *i >= l.locals)
                    .map(|(_, r, b)| (*r, *b))
                    .collect();
                if leavers.is_empty() {
                    fc.patch(g.jump, l.pc)?;
                } else {
                    let stub = fc.pc();
                    fc.patch(g.jump, stub)?;
                    let saved_free = fc.free_reg;
                    let saved_line = fc.line;
                    fc.free_reg = g.free_reg;
                    fc.line = g.line;
                    for (reg, boxed) in leavers.into_iter().rev() {
                        fc.emit_close_one(reg, boxed)?;
                        fc.free_reg = g.free_reg;
                    }
                    fc.free_reg = saved_free;
                    fc.line = saved_line;
                    fc.emit_back(Op::Jmp, 0, l.pc)?;
                }
            }
            None => return Err(err(format!("no visible label '{}' for goto", g.name), g.line)),
        }
    }

    // A conditional at the tail can jump one past the last instruction
    // (e.g. `if x { return }` at function end). The VM no longer bounds-checks
    // pc per instruction, so such a proto needs a real Return to land on.
    let needs_tail = fc.proto.code.iter().enumerate().any(|(i, &instr)| {
        matches!(Op::from_u8(iop(instr)),
            Some(Op::Jmp) | Some(Op::ForPrep) | Some(Op::ForLoop) | Some(Op::TForLoop))
            && i as i32 + 1 + isbx(instr) == fc.proto.code.len() as i32
    });
    if needs_tail {
        fc.emit(enc_abc(Op::Return, 0, 1, 0));
    }

    // Bx and upvalue operands are 16 and 8 bits wide; anything past that
    // would have been silently truncated at emit time.
    if fc.proto.consts.len() > u16::MAX as usize + 1 {
        return Err(err("too many constants", body.line));
    }
    if fc.proto.protos.len() > u16::MAX as usize + 1 {
        return Err(err("too many nested functions", body.line));
    }
    if fc.upvals.len() > u8::MAX as usize + 1 {
        return Err(err("too many upvalues", body.line));
    }

    fc.proto.upvals = fc.upvals.iter().map(|u| {
        crate::chunk::UpvalDesc { name: u.name.clone(), in_stack: u.in_stack, idx: u.outer_idx }
    }).collect();

    Ok(fc.proto)
}

pub fn compile(block: Block, source: Option<String>) -> CResult<Proto> {
    let line = block.line;
    let chunk_body = FuncBody {
        params: vec![],
        vararg: true,
        body: block,
        line,
    };
    compile_fn(&chunk_body, source, None, 0)
}
