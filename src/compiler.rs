use crate::ast::*;
use crate::chunk::{Const, Op, Proto, RK_BIT, enc_abc, enc_abx, enc_asbx, iop};
const BIAS: i32 = crate::chunk::BIAS;

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
}

#[derive(Debug, Clone)]
struct UpvalInfo {
    name: String,
    in_stack: bool,
    outer_idx: u8,
}

#[derive(Clone)]
struct OuterScope {
    locals: Vec<Local>,
    upvals: Vec<UpvalInfo>,
}

struct LoopScope {
    break_jumps: Vec<usize>,
}

struct FnComp {
    proto: Proto,
    locals: Vec<Local>,
    upvals: Vec<UpvalInfo>,
    outer_scope: Option<OuterScope>,
    free_reg: u8,
    loops: Vec<LoopScope>,
    line: u32,
}

impl FnComp {
    fn new(source: Option<String>, outer: Option<OuterScope>) -> Self {
        let mut proto = Proto::new();
        proto.source = source;
        Self { proto, locals: Vec::new(), upvals: Vec::new(), outer_scope: outer, free_reg: 0, loops: Vec::new(), line: 1 }
    }

    fn alloc_reg(&mut self) -> CResult<u8> {
        let r = self.free_reg;
        self.free_reg += 1;
        if self.free_reg > self.proto.max_regs {
            self.proto.max_regs = self.free_reg;
        }
        if self.free_reg == 0 {
            return Err(err("too many registers", self.line));
        }
        Ok(r)
    }

    fn free_reg_to(&mut self, base: u8) {
        self.free_reg = base;
    }

    #[allow(dead_code)]
    fn top(&self) -> u8 { self.free_reg }

    fn const_str(&mut self, s: &str) -> usize { self.proto.add_string(s) }

    fn const_val(&mut self, c: Const) -> usize { self.proto.add_const(c) }

    fn rk_const(&mut self, c: Const) -> CResult<usize> {
        let idx = self.const_val(c);
        if idx < 256 { Ok(idx | RK_BIT) }
        else { Err(err("too many constants", self.line)) }
    }

    fn resolve_local(&self, name: &str) -> Option<u8> {
        self.locals.iter().rev().find(|l| l.name == name).map(|l| l.reg)
    }

    fn push_local(&mut self, name: String, mutable: bool) -> CResult<u8> {
        let r = self.alloc_reg()?;
        self.locals.push(Local { name, reg: r, mutable });
        Ok(r)
    }

    fn locals_top(&self) -> usize { self.locals.len() }

    fn pop_locals_to(&mut self, top: usize) {
        let new_free = self.locals.get(top).map(|l| l.reg).unwrap_or(self.free_reg);
        self.locals.truncate(top);
        self.free_reg = new_free;
    }

    fn resolve_upval(&mut self, name: &str) -> Option<u8> {
        if let Some(i) = self.upvals.iter().position(|u| u.name == name) {
            return Some(i as u8);
        }
        let outer = self.outer_scope.as_ref()?;
        if let Some(loc) = outer.locals.iter().rev().find(|l| l.name == name).cloned() {
            let idx = self.upvals.len() as u8;
            self.upvals.push(UpvalInfo { name: name.to_owned(), in_stack: true, outer_idx: loc.reg });
            return Some(idx);
        }
        if let Some(pos) = outer.upvals.iter().position(|u| u.name == name) {
            let idx = self.upvals.len() as u8;
            self.upvals.push(UpvalInfo { name: name.to_owned(), in_stack: false, outer_idx: pos as u8 });
            return Some(idx);
        }
        None
    }

    fn emit(&mut self, i: u32) -> usize { self.proto.emit(i, self.line) }
    fn pc(&self) -> usize { self.proto.current_pc() }

    fn emit_load_nil(&mut self, r: u8) { self.emit(enc_abc(Op::LoadNil, r, 0, 0)); }
    fn emit_move(&mut self, dst: u8, src: u8) { self.emit(enc_abc(Op::Move, dst, src, 0)); }

    fn to_reg(&mut self, e: Expr2, hint: Option<u8>) -> CResult<u8> {
        let dst = hint.unwrap_or_else(|| self.free_reg);
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
            ExprKind::K(i)     => Ok(i | RK_BIT),
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
        let locals_top = self.locals_top();
        let reg_top = self.free_reg;
        for stmt in &block.stmts {
            self.compile_stmt(stmt)?;
        }
        if let Some(ret) = &block.ret {
            self.compile_return(ret, block.line)?;
        }
        self.pop_locals_to(locals_top);
        self.free_reg_to(reg_top);
        Ok(())
    }

    fn compile_return(&mut self, vals: &[Expr], line: u32) -> CResult<()> {
        self.line = line;
        if vals.is_empty() {
            self.emit(enc_abc(Op::Return, 0, 1, 0));
            return Ok(());
        }
        let base = self.free_reg;
        let n = vals.len();
        let last_is_vararg = matches!(vals[n - 1], Expr::Vararg(_));
        let fixed = if last_is_vararg { n - 1 } else { n };
        for (i, v) in vals[..fixed].iter().enumerate() {
            let dst = base + i as u8;
            let e = self.compile_expr(v)?;
            self.to_reg(e, Some(dst))?;
            if self.free_reg <= dst { self.free_reg = dst + 1; }
        }
        if last_is_vararg {
            self.emit(enc_abc(Op::Vararg, base + fixed as u8, 0, 0));
            self.emit(enc_abc(Op::Return, base, 0, 0));
        } else {
            let nv = self.free_reg - base;
            self.emit(enc_abc(Op::Return, base, nv + 1, 0));
        }
        Ok(())
    }

    fn compile_stmt(&mut self, stmt: &Stmt) -> CResult<()> {
        match stmt {
            Stmt::Local { mutable, names, values, line } => {
                self.line = *line;
                let base = self.free_reg;
                let nv = values.len();
                let nn = names.len();
                for (i, val) in values.iter().enumerate() {
                    let dst = base + i as u8;
                    let is_last = i + 1 == nv;
                    let want = if is_last && nn > i { (nn - i) as u8 } else { 1 };
                    if want > 1 {
                        match val {
                            Expr::Call(c) => { self.compile_call(c, dst, want)?; }
                            Expr::MethodCall(m) => { self.compile_method_call(m, dst, want)?; }
                            Expr::Vararg(_) => {
                                self.emit(enc_abc(Op::Vararg, dst, want + 1, 0));
                            }
                            _ => {
                                let e = self.compile_expr(val)?;
                                self.to_reg(e, Some(dst))?;
                                for j in 1..want { self.emit_load_nil(dst + j); }
                            }
                        }
                        if self.free_reg < dst + want { self.free_reg = dst + want; }
                    } else {
                        let e = self.compile_expr(val)?;
                        self.to_reg(e, Some(dst))?;
                        if self.free_reg <= dst { self.free_reg = dst + 1; }
                    }
                }
                let filled = if nv > 0 {
                    let last_i = nv - 1;
                    let last_want = if nn > last_i { (nn - last_i) as u8 } else { 1 };
                    last_i as u8 + last_want
                } else { 0 };
                for i in (filled as usize)..nn {
                    let dst = base + i as u8;
                    if self.free_reg <= dst { self.free_reg = dst + 1; }
                    self.emit_load_nil(dst);
                }
                for (i, name) in names.iter().enumerate() {
                    self.locals.push(Local { name: name.clone(), reg: base + i as u8, mutable: *mutable });
                }
                if self.free_reg < base + nn as u8 { self.free_reg = base + nn as u8; }
                if self.proto.max_regs < self.free_reg { self.proto.max_regs = self.free_reg; }
            }

            Stmt::Assign { targets, values, line } => {
                self.line = *line;
                let tmp_base = self.free_reg;
                let nv = values.len();
                let nt = targets.len();
                for i in 0..nv {
                    let dst = tmp_base + i as u8;
                    let is_last = i + 1 == nv;
                    let want = if is_last && nt > i { (nt - i) as u8 } else { 1 };
                    if want > 1 {
                        match &values[i] {
                            Expr::Call(c) => { self.compile_call(c, dst, want)?; }
                            Expr::MethodCall(m) => { self.compile_method_call(m, dst, want)?; }
                            Expr::Vararg(_) => {
                                self.emit(enc_abc(Op::Vararg, dst, want + 1, 0));
                            }
                            val => {
                                let e = self.compile_expr(val)?;
                                self.to_reg(e, Some(dst))?;
                                for j in 1..want { self.emit_load_nil(dst + j); }
                            }
                        }
                        if self.free_reg < dst + want { self.free_reg = dst + want; }
                    } else {
                        let e = self.compile_expr(&values[i])?;
                        self.to_reg(e, Some(dst))?;
                        if self.free_reg <= dst { self.free_reg = dst + 1; }
                    }
                }
                let filled = if nv > 0 {
                    let last_want = if nt > nv - 1 { (nt - (nv - 1)) as u8 } else { 1 };
                    (nv - 1) as u8 + last_want
                } else { 0 };
                for i in (filled as usize)..nt {
                    let dst = tmp_base + i as u8;
                    if self.free_reg <= dst { self.free_reg = dst + 1; }
                    self.emit_load_nil(dst);
                }
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
                let cond_reg = self.free_reg;
                let e = self.compile_expr(cond)?;
                let r = self.to_reg(e, Some(cond_reg))?;
                if self.free_reg <= r { self.free_reg = r + 1; }
                self.emit(enc_abc(Op::Test, r as u8, 0, 0));
                let exit_jump = self.proto.emit_jump(*line);
                self.free_reg_to(cond_reg);

                self.loops.push(LoopScope { break_jumps: Vec::new() });
                self.compile_block(body)?;
                let scope = self.loops.pop().unwrap();

                let back = loop_top as i32 - self.pc() as i32 - 1;
                self.emit(enc_asbx(Op::Jmp, 0, back));
                let here = self.pc();
                self.proto.patch_jump(exit_jump, here);
                for bj in scope.break_jumps {
                    self.proto.patch_jump(bj, here);
                }
            }

            Stmt::If { cond, then, elseifs, else_, line } => {
                self.line = *line;
                let mut exit_jumps = Vec::new();

                let test = |fc: &mut Self, cond: &Expr| -> CResult<usize> {
                    let base = fc.free_reg;
                    let e = fc.compile_expr(cond)?;
                    let r = fc.to_reg(e, Some(base))?;
                    if fc.free_reg <= r { fc.free_reg = r + 1; }
                    fc.emit(enc_abc(Op::Test, r as u8, 0, 0));
                    let j = fc.proto.emit_jump(fc.line);
                    fc.free_reg_to(base);
                    Ok(j)
                };

                let fail = test(self, cond)?;
                self.compile_block(then)?;
                if !elseifs.is_empty() || else_.is_some() {
                    exit_jumps.push(self.proto.emit_jump(*line));
                }
                self.proto.patch_jump_here(fail);

                for (ei_cond, ei_body) in elseifs {
                    let f2 = test(self, ei_cond)?;
                    self.compile_block(ei_body)?;
                    exit_jumps.push(self.proto.emit_jump(*line));
                    self.proto.patch_jump_here(f2);
                }
                if let Some(eb) = else_ {
                    self.compile_block(eb)?;
                }
                let here = self.pc();
                for j in exit_jumps { self.proto.patch_jump(j, here); }
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
                self.locals.push(Local { name: var.clone(), reg: lv_reg, mutable: true });
                let locals_save = self.locals.len() - 1;

                self.loops.push(LoopScope { break_jumps: Vec::new() });
                let inner_locals = self.locals_top();
                for stmt in &body.stmts {
                    self.compile_stmt(stmt)?;
                }
                if let Some(r) = &body.ret {
                    self.compile_return(r, body.line)?;
                }
                self.pop_locals_to(inner_locals);
                let scope = self.loops.pop().unwrap();

                let back = loop_top as i32 - self.pc() as i32 - 1;
                self.emit(enc_asbx(Op::ForLoop, base as u8, back));
                let here = self.pc();

                let prep_off = here as i32 - prep as i32 - 1;
                self.proto.code[prep] = enc_asbx(Op::ForPrep, base as u8, prep_off - 1);

                self.locals.truncate(locals_save);
                for bj in scope.break_jumps { self.proto.patch_jump(bj, here); }
                self.free_reg_to(base);
            }

            Stmt::ForIn { vars, iters, body, line } => {
                self.line = *line;
                let base = self.free_reg;
                // Layout: base=iter fn, base+1=state, base+2=control, base+3=unused buf,
                // base+4..=loop vars (TForCall writes results starting at A+4).
                for _ in 0..(4 + vars.len()) { self.alloc_reg()?; }

                if iters.len() == 1 {
                    let it0 = iters[0].clone();
                    match it0 {
                        Expr::Call(ref c) => { self.compile_call(c, base, 3)?; }
                        Expr::MethodCall(ref m) => { self.compile_method_call(m, base, 3)?; }
                        _ => {
                            let e = self.compile_expr(&it0)?;
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
                for (i, v) in vars.iter().enumerate() {
                    self.locals.push(Local { name: v.clone(), reg: base + 4 + i as u8, mutable: true });
                }

                self.loops.push(LoopScope { break_jumps: Vec::new() });
                let inner_locals = self.locals_top();
                for s in &body.stmts { self.compile_stmt(s)?; }
                if let Some(r) = &body.ret { self.compile_return(r, body.line)?; }
                self.pop_locals_to(inner_locals);
                let scope = self.loops.pop().unwrap();

                self.proto.patch_jump_here(loop_jmp);
                self.emit(enc_abc(Op::TForCall, base as u8, 0, vars.len() as u8));
                let back = loop_top as i32 - self.pc() as i32 - 1;
                self.emit(enc_asbx(Op::TForLoop, base as u8, back));

                let here = self.pc();
                self.locals.truncate(lv_base);
                for bj in scope.break_jumps { self.proto.patch_jump(bj, here); }
                self.free_reg_to(base);
            }

            Stmt::FuncDef { name, body, line } => {
                self.line = *line;
                let outer = Some(OuterScope { locals: self.locals.clone(), upvals: self.upvals.clone() });
                let proto = compile_fn(body, self.proto.source.clone(), outer)?;
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
                    if let Some(lr) = self.resolve_local(&name.parts[0]) {
                        self.emit_move(base_r, lr);
                    } else {
                        self.emit(enc_abx(Op::GetGlobal, base_r, ki as u16));
                    }
                    let mut cur = base_r;
                    let field_end = if name.method.is_some() { nparts } else { nparts - 1 };
                    for p in &name.parts[1..field_end] {
                        let fki = self.const_str(p) | RK_BIT;
                        let next = self.alloc_reg()?;
                        self.emit(enc_abc(Op::GetTable, next, cur, fki as u8));
                        cur = next;
                    }
                    let last_key = name.method.as_deref()
                        .unwrap_or_else(|| name.parts.last().unwrap());
                    let lki = self.const_str(last_key) | RK_BIT;
                    self.emit(enc_abc(Op::SetTable, cur, lki as u8, dst));
                    self.free_reg_to(base_r);
                }
                self.free_reg_to(dst);
            }

            Stmt::LocalFunc { name, body, line } => {
                self.line = *line;
                // Snapshot before pushing the function's own name so it can't
                // accidentally capture itself as an upvalue (would be nil at creation time).
                let outer = Some(OuterScope { locals: self.locals.clone(), upvals: self.upvals.clone() });
                let r = self.push_local(name.clone(), true)?;
                let proto = compile_fn(body, self.proto.source.clone(), outer)?;
                let pi = self.proto.protos.len();
                self.proto.protos.push(proto);
                self.emit(enc_abx(Op::Closure, r, pi as u16));
            }

            Stmt::Call(c) => {
                self.line = c.line;
                let base = self.free_reg;
                self.compile_call(c, base, 0)?;
                self.free_reg_to(base);
            }

            Stmt::MethodCall(m) => {
                self.line = m.line;
                let base = self.free_reg;
                self.compile_method_call(m, base, 0)?;
                self.free_reg_to(base);
            }

            Stmt::Break(line) => {
                self.line = *line;
                let j = self.proto.emit_jump(*line);
                if let Some(scope) = self.loops.last_mut() {
                    scope.break_jumps.push(j);
                } else {
                    return Err(err("'break' outside loop", *line));
                }
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
                    if loc.reg != src { self.emit_move(loc.reg, src); }
                } else if let Some(uv_idx) = self.resolve_upval(&id.name) {
                    self.emit(enc_abc(Op::SetUpval, src, uv_idx, 0));
                } else {
                    let ki = self.const_str(&id.name);
                    self.emit(enc_abx(Op::SetGlobal, src, ki as u16));
                }
            }
            Expr::Field { table, field, line } => {
                self.line = *line;
                let te = self.compile_expr(table)?;
                let tr = self.to_reg(te, None)?;
                let fki = (self.const_str(field) | RK_BIT) as u8;
                self.emit(enc_abc(Op::SetTable, tr, fki, src));
                if !matches!(te.kind, ExprKind::Reg(_)) {
                    self.free_reg -= 1;
                }
            }
            Expr::Index { table, key, line } => {
                self.line = *line;
                let te = self.compile_expr(table)?;
                let tr = self.to_reg(te, None)?;
                let ke = self.compile_expr(key)?;
                let kr = self.to_rk(ke)?;
                self.emit(enc_abc(Op::SetTable, tr, kr as u8, src));
                self.free_reg_to(tr + 1);
            }
            other => return Err(err("invalid assignment target", other.line())),
        }
        Ok(())
    }

    fn compile_expr(&mut self, expr: &Expr) -> CResult<Expr2> {
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
                if let Some(r) = self.resolve_local(&id.name) {
                    Ok(Expr2 { kind: ExprKind::Reg(r), line })
                } else if let Some(uv) = self.resolve_upval(&id.name) {
                    Ok(Expr2 { kind: ExprKind::Upval(uv), line })
                } else {
                    let ki = self.const_str(&id.name);
                    Ok(Expr2 { kind: ExprKind::Global(ki), line })
                }
            }
            Expr::Vararg(_) => {
                let r = self.alloc_reg()?;
                self.emit(enc_abc(Op::Vararg, r, 2, 0));
                Ok(Expr2::reg(r, line))
            }
            Expr::Unop { op, operand, line } => {
                self.line = *line;
                let e = self.compile_expr(operand)?;
                let r = self.to_reg(e, None)?;
                let dst = self.alloc_reg()?;
                let instr = match op {
                    Unop::Neg  => enc_abc(Op::Unm,  dst, r, 0),
                    Unop::Not  => enc_abc(Op::Not,  dst, r, 0),
                    Unop::Len  => enc_abc(Op::Len,  dst, r, 0),
                    Unop::BNot => enc_abc(Op::BNot, dst, r, 0),
                };
                if !matches!(e.kind, ExprKind::Reg(_)) {
                    self.free_reg = dst;
                }
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
                let outer = Some(OuterScope { locals: self.locals.clone(), upvals: self.upvals.clone() });
                let proto = compile_fn(body, self.proto.source.clone(), outer)?;
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
                let fki = (self.const_str(field) | RK_BIT) as u8;
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
                Ok(Expr2::reg(base + 1, m.line))
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
        self.proto.patch_jump_here(to_true);
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

        self.proto.patch_jump_here(skip);
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

        for field in &tc.fields {
            match field {
                TableField::Named { key, val, line } => {
                    self.line = *line;
                    let fki = (self.const_str(key) | RK_BIT) as u8;
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
                    let slot = field_base + (arr_idx - 1) as u8;
                    let ve = self.compile_expr(val)?;
                    self.to_reg(ve, Some(slot))?;
                    if self.free_reg <= slot { self.free_reg = slot + 1; }
                    if arr_idx % 50 == 0 {
                        let b = 50u8;
                        let c = (arr_idx / 50) as u8;
                        self.emit(enc_abc(Op::SetList, dst, b, c));
                        self.free_reg_to(field_base);
                    }
                }
            }
        }
        let rem = arr_idx % 50;
        if rem > 0 {
            let c = (arr_idx / 50 + 1) as u8;
            self.emit(enc_abc(Op::SetList, dst, rem as u8, c));
            self.free_reg_to(field_base);
        }

        Ok(Expr2::reg(dst, tc.line))
    }

    fn compile_call(&mut self, c: &CallExpr, base: u8, nresults: u8) -> CResult<()> {
        let ce = self.compile_expr(&c.callee)?;
        self.to_reg(ce, Some(base))?;
        self.free_reg = base + 1;

        let (nargs, is_variable) = self.push_args(&c.args, base + 1)?;
        let b = if is_variable { 0 } else { nargs + 1 };
        let cr = nresults + 1;
        self.emit(enc_abc(Op::Call, base, b, cr));
        self.free_reg = base + nresults as u8;
        Ok(())
    }

    fn compile_method_call(&mut self, m: &MethodCallExpr, base: u8, nresults: u8) -> CResult<()> {
        let re = self.compile_expr(&m.receiver)?;
        self.to_reg(re, Some(base))?;
        self.free_reg = base + 1;

        let fn_reg = self.alloc_reg()?;
        let fki = (self.const_str(&m.method) | RK_BIT) as u8;
        self.emit(enc_abc(Op::GetTable, fn_reg, base, fki));
        let self_reg = self.alloc_reg()?;
        self.emit_move(self_reg, base);
        let (nargs, is_variable) = self.push_args(&m.args, base + 3)?;
        let b = if is_variable { 0 } else { nargs + 2 };
        let cr = nresults + 1;
        self.emit(enc_abc(Op::Call, fn_reg, b, cr));
        self.free_reg = base + nresults as u8;
        Ok(())
    }

    // `is_variable` means the last argument was `...`; the VM reads frame.top
    // (set by the preceding Vararg b=0) to get the real arg count at b=0.
    fn push_args(&mut self, args: &Args, arg_base: u8) -> CResult<(u8, bool)> {
        self.free_reg = arg_base;
        match args {
            Args::Exprs(exprs) => {
                let n = exprs.len();
                if n == 0 { return Ok((0, false)); }
                let last_is_vararg = matches!(exprs[n - 1], Expr::Vararg(_));
                let fixed = if last_is_vararg { n - 1 } else { n };
                for (i, e) in exprs[..fixed].iter().enumerate() {
                    let slot = arg_base + i as u8;
                    let ev = self.compile_expr(e)?;
                    self.to_reg(ev, Some(slot))?;
                    if self.free_reg <= slot { self.free_reg = slot + 1; }
                }
                if last_is_vararg {
                    self.emit(enc_abc(Op::Vararg, arg_base + fixed as u8, 0, 0));
                    Ok((fixed as u8, true))
                } else {
                    Ok((n as u8, false))
                }
            }
            Args::String(s) => {
                let ki = self.const_str(s);
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

fn compile_fn(body: &FuncBody, source: Option<String>, outer: Option<OuterScope>) -> CResult<Proto> {
    let mut fc = FnComp::new(source, outer);
    fc.proto.params = body.params.len() as u8;
    fc.proto.is_vararg = body.vararg;

    for p in &body.params {
        fc.push_local(p.clone(), true)?;
    }

    fc.compile_block(&body.body)?;

    fc.proto.upvals = fc.upvals.iter().map(|u| {
        crate::chunk::UpvalDesc { name: u.name.clone(), in_stack: u.in_stack, idx: u.outer_idx }
    }).collect();

    if fc.proto.code.last().map(|&i| iop(i)) != Some(Op::Return as u8) {
        fc.emit(enc_abc(Op::Return, 0, 1, 0));
    }
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
    compile_fn(&chunk_body, source, None)
}
