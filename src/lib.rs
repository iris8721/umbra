pub mod api;
pub mod ast;
pub mod gc;
pub mod chunk;
pub mod compiler;
pub mod lexer;
pub mod parser;
pub mod value;
pub mod vm;

pub use parser::parse;
pub use compiler::compile;

pub fn run(src: &str) -> Result<(), String> {
    run_with_vm(src, &mut vm::Vm::new())
}

pub fn run_with_vm(src: &str, vm: &mut vm::Vm) -> Result<(), String> {
    let (block, parse_errs) = parse(src);
    if !parse_errs.is_empty() {
        return Err(parse_errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"));
    }
    let proto = compile(block, None).map_err(|e| e.to_string())?;
    vm.exec_owned(proto).map_err(|e| e.to_string())
}

pub fn run_capture(src: &str) -> Result<Vec<String>, String> {
    use std::sync::{Arc, Mutex};
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let cap2 = captured.clone();

    let (block, parse_errs) = parse(src);
    if !parse_errs.is_empty() {
        return Err(parse_errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"));
    }
    let proto = compile(block, None).map_err(|e| e.to_string())?;
    let mut vm = vm::Vm::new_with_print(move |line| {
        cap2.lock().unwrap().push(line);
    });
    let result = vm.exec(&proto).map_err(|e| e.to_string());
    vm::clear_print_hook();
    result?;
    Ok(Arc::try_unwrap(captured).unwrap().into_inner().unwrap())
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    #[test]
    fn lex_basic() {
        use lexer::{Lexer, TokenKind};
        let tokens = Lexer::tokenize("let x = 42 + 3.14").unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Let));
        assert!(matches!(tokens[1].kind, TokenKind::Ident(_)));
        assert!(matches!(tokens[2].kind, TokenKind::Assign));
        assert!(matches!(tokens[3].kind, TokenKind::Int(42)));
        assert!(matches!(tokens[4].kind, TokenKind::Plus));
        assert!(matches!(tokens[5].kind, TokenKind::Float(_)));
    }

    #[test]
    fn lex_string_escapes() {
        use lexer::Lexer;
        let toks = Lexer::tokenize(r#""hello\nworld""#).unwrap();
        assert!(matches!(&toks[0].kind, lexer::TokenKind::String(s) if s == "hello\nworld"));
    }

    #[test]
    fn lex_long_string() {
        use lexer::Lexer;
        let toks = Lexer::tokenize("[[hello\nworld]]").unwrap();
        assert!(matches!(&toks[0].kind, lexer::TokenKind::String(s) if s == "hello\nworld"));
    }

    #[test]
    fn lex_line_comment() {
        use lexer::Lexer;
        let toks = Lexer::tokenize("42 // this is a comment\n99").unwrap();
        assert!(matches!(toks[0].kind, lexer::TokenKind::Int(42)));
        assert!(matches!(toks[1].kind, lexer::TokenKind::Int(99)));
    }

    #[test]
    fn lex_block_comment() {
        use lexer::Lexer;
        let toks = Lexer::tokenize("1 /* hello */ 2").unwrap();
        assert!(matches!(toks[0].kind, lexer::TokenKind::Int(1)));
        assert!(matches!(toks[1].kind, lexer::TokenKind::Int(2)));
    }

    #[test]
    fn lex_bang_eq() {
        use lexer::Lexer;
        let toks = Lexer::tokenize("a != b").unwrap();
        assert!(matches!(toks[1].kind, lexer::TokenKind::BangEq));
    }

    #[test]
    fn parse_while_loop() {
        use ast::Stmt;
        let (block, errs) = parse("while true { let x = 1 }");
        assert!(errs.is_empty(), "{:?}", errs);
        assert!(matches!(block.stmts[0], Stmt::While { .. }));
    }

    #[test]
    fn parse_for_numeric() {
        use ast::Stmt;
        let (block, errs) = parse("for i = 1, 10, 2 { }");
        assert!(errs.is_empty(), "{:?}", errs);
        assert!(matches!(block.stmts[0], Stmt::ForNum { .. }));
    }

    #[test]
    fn parse_function_call() {
        use ast::Stmt;
        let (block, errs) = parse("print(\"hello\", 42)");
        assert!(errs.is_empty(), "{:?}", errs);
        assert!(matches!(block.stmts[0], Stmt::Call(_)));
    }

    #[test]
    fn parse_table_constructor() {
        let (_block, errs) = parse(r#"let t = { x = 1, [2] = 3, "val" }"#);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn parse_binary_precedence() {
        use ast::{Expr, Binop};
        let (block, errs) = parse("let _ = 1 + 2 * 3");
        assert!(errs.is_empty(), "{:?}", errs);
        if let ast::Stmt::Local { values, .. } = &block.stmts[0] {
            assert!(matches!(values[0], Expr::Binop { op: Binop::Add, .. }));
        }
    }

    #[test]
    fn parse_coroutine_yield() {
        let (_block, errs) = parse("while true { aimbot() yield() }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn parse_closure_expr() {
        let (_block, errs) = parse("let f = |x| x * 2");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn parse_implicit_return() {
        let (_block, errs) = parse("fn double(x) { x * 2 }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn parse_error_recovery() {
        let (_block, errs) = parse("let x = @@@");
        assert!(!errs.is_empty());
    }

    #[test]
    fn value_nil() {
        use value::Value;
        let v = Value::nil();
        assert!(v.is_nil());
        assert!(!v.is_bool());
        assert!(!v.is_int());
        assert!(!v.is_float());
        assert!(!v.is_truthy());
    }

    #[test]
    fn value_bool() {
        use value::Value;
        assert!(Value::bool(true).is_truthy());
        assert!(!Value::bool(false).is_truthy());
        assert_eq!(Value::bool(true).as_bool(), Some(true));
        assert_eq!(Value::bool(false).as_bool(), Some(false));
    }

    #[test]
    fn value_int_roundtrip() {
        use value::Value;
        for n in [0i64, 1, -1, 42, -42, i32::MAX as i64, i32::MIN as i64,
                  (1i64 << 47) - 1, -((1i64 << 47))] {
            let v = Value::int(n);
            assert!(v.is_int(), "n={n}");
            assert_eq!(v.as_int(), Some(n), "n={n}");
        }
    }

    #[test]
    fn value_float_roundtrip() {
        use value::Value;
        for f in [0.0f64, 1.0, -1.0, 3.14, f64::INFINITY, f64::NEG_INFINITY,
                  f64::MAX, f64::MIN_POSITIVE] {
            let v = Value::float(f);
            assert!(v.is_float(), "f={f}");
            assert_eq!(v.as_float().unwrap().to_bits(), f.to_bits(), "f={f}");
        }
    }

    #[test]
    fn value_nan_becomes_nil() {
        use value::Value;
        let v = Value::float(f64::NAN);
        assert!(v.is_nil());
    }

    #[test]
    fn value_int_float_equality() {
        use value::Value;
        assert_eq!(Value::int(3), Value::float(3.0));
        assert_eq!(Value::float(0.0), Value::int(0));
        assert_ne!(Value::int(3), Value::float(3.5));
    }

    #[test]
    fn value_size() {
        assert_eq!(std::mem::size_of::<value::Value>(), 8);
    }

    #[test]
    fn value_ptr_roundtrip() {
        use value::Value;
        let x: u64 = 0x0000_DEAD_BEEF_0010;
        let ptr = x as *mut u8;
        let v = Value::table(ptr);
        assert!(v.is_table());
        assert_eq!(v.as_table().unwrap() as u64, x);
    }

    #[test]
    fn value_type_names() {
        use value::Value;
        assert_eq!(Value::nil().type_name(),       "nil");
        assert_eq!(Value::bool(true).type_name(),  "boolean");
        assert_eq!(Value::int(0).type_name(),       "integer");
        assert_eq!(Value::float(1.0).type_name(),   "float");
    }

    fn capture(src: &str) -> Vec<String> {
        run_capture(src).expect(src)
    }

    fn assert_output(src: &str, expected: &[&str]) {
        assert_eq!(capture(src), expected, "script: {src}");
    }

    #[test]
    fn vm_hello_world() {
        assert_output(r#"print("hello")"#, &["hello"]);
    }

    #[test]
    fn vm_arithmetic() {
        assert_output("print(1 + 2)",   &["3"]);
        assert_output("print(10 - 3)",  &["7"]);
        assert_output("print(2 * 6)",   &["12"]);
        assert_output("print(7 / 2)",   &["3.5"]);
        assert_output("print(10 % 3)",  &["1"]);
        assert_output("print(2 ^ 10)",  &["1024"]);
    }

    #[test]
    fn vm_locals_and_assignment() {
        assert_output("let x = 5 let y = 3 print(x + y)", &["8"]);
        assert_output("var x = 1 x = x + 1 print(x)", &["2"]);
    }

    #[test]
    fn vm_let_immutable() {
        let result = run("let x = 1 x = 2");
        assert!(result.is_err(), "assigning to let should error");
    }

    #[test]
    fn vm_if_else() {
        assert_output(r#"if true { print("yes") } else { print("no") }"#, &["yes"]);
        assert_output(r#"if false { print("yes") } else { print("no") }"#, &["no"]);
        assert_output(r#"let x = 5 if x > 3 { print("big") }"#, &["big"]);
    }

    #[test]
    fn vm_while_loop() {
        assert_output(
            "var i = 0 var s = 0 while i < 5 { i = i + 1 s = s + i } print(s)",
            &["15"],
        );
    }

    #[test]
    fn vm_for_numeric() {
        assert_output(
            "var s = 0 for i = 1, 10 { s = s + i } print(s)",
            &["55"],
        );
    }

    #[test]
    fn vm_for_step() {
        assert_output(
            "var s = 0 for i = 0, 10, 2 { s = s + i } print(s)",
            &["30"],
        );
    }

    #[test]
    fn vm_function_call_and_return() {
        assert_output(
            "let fn add(a, b) { return a + b } print(add(3, 4))",
            &["7"],
        );
    }

    #[test]
    fn vm_implicit_return() {
        assert_output(
            "fn double(x) { x * 2 } print(double(21))",
            &["42"],
        );
    }

    #[test]
    fn vm_closure_shorthand() {
        assert_output(
            "let sq = |x| x * x print(sq(9))",
            &["81"],
        );
    }

    #[test]
    fn vm_recursion() {
        assert_output(
            "fn fact(n) { if n <= 1 { return 1 } return n * fact(n - 1) } print(fact(10))",
            &["3628800"],
        );
    }

    #[test]
    fn vm_concat() {
        assert_output(r#"print("hello" .. " " .. "world")"#, &["hello world"]);
    }

    #[test]
    fn vm_not_equal() {
        assert_output("print(1 != 2)", &["true"]);
        assert_output("print(1 != 1)", &["false"]);
    }

    #[test]
    fn vm_table_basic() {
        assert_output(
            "let t = {} t.x = 10 t.y = 20 print(t.x + t.y)",
            &["30"],
        );
    }

    #[test]
    fn vm_table_array() {
        assert_output(
            "let t = {10, 20, 30} print(t[1] + t[2] + t[3])",
            &["60"],
        );
    }

    #[test]
    fn vm_closures() {
        assert_output(
            "let fn make() { let fn f(x) { return x * 2 } return f } let g = make() print(g(21))",
            &["42"],
        );
    }

    #[test]
    fn vm_globals() {
        assert_output("x = 100 print(x)", &["100"]);
    }

    #[test]
    fn vm_logical_and_or() {
        assert_output(r#"print(true and "yes" or "no")"#, &["yes"]);
        assert_output(r#"print(false and "yes" or "no")"#, &["no"]);
    }

    #[test]
    fn vm_string_len() {
        assert_output(r#"print(#"hello")"#, &["5"]);
    }

    #[test]
    fn vm_error_propagation() {
        assert!(run("error(\"boom\")").is_err());
    }

    #[test]
    fn api_dostring_and_pcall() {
        use std::ffi::CString;

        let U = api::umbra_newstate();

        let src = CString::new("fn add(a, b) { return a + b }").unwrap();
        let rc = unsafe { api::umbra_dostring(U, src.as_ptr()) };
        assert_eq!(rc, 0, "dostring failed");

        let name = CString::new("add").unwrap();
        unsafe { api::umbra_getglobal(U, name.as_ptr()) };
        unsafe { api::umbra_pushnumber(U, 10.0) };
        unsafe { api::umbra_pushnumber(U, 32.0) };

        let rc = unsafe { api::umbra_pcall(U, 2, 1) };
        assert_eq!(rc, 0, "pcall failed");

        let result = unsafe { api::umbra_tonumber(U, 1) };
        assert_eq!(result, 42.0);

        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn gc_collect_frees_dead_strings() {
        use std::ffi::CString;

        let U = api::umbra_newstate();

        let src = CString::new(
            r#"let a = "gc_test_alpha" let b = "gc_test_beta" let c = a .. b"#
        ).unwrap();
        unsafe { api::umbra_dostring(U, src.as_ptr()) };

        let before = unsafe { api::umbra_gc_livecount(U) };
        assert!(before > 0, "should have live objects before collect");

        unsafe { api::umbra_gc_collect(U) };

        let after = unsafe { api::umbra_gc_livecount(U) };
        assert!(after < before, "GC should have freed dead objects (before={before} after={after})");

        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn gc_collect_keeps_live_globals() {
        use std::ffi::CString;

        let U = api::umbra_newstate();

        let src = CString::new(r#"persistent = "keep_me""#).unwrap();
        unsafe { api::umbra_dostring(U, src.as_ptr()) };

        unsafe { api::umbra_gc_collect(U) };

        let src2 = CString::new(r#"assert(persistent == "keep_me")"#).unwrap();
        let rc = unsafe { api::umbra_dostring(U, src2.as_ptr()) };
        assert_eq!(rc, 0, "global string must survive GC");

        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn coroutine_basic_yield_resume() {
        assert_output(
            r#"
let co = coroutine.create(fn() {
  yield(1)
  yield(2)
  return 3
})
var ok, v = coroutine.resume(co)
print(ok, v)
ok, v = coroutine.resume(co)
print(ok, v)
ok, v = coroutine.resume(co)
print(ok, v)
"#,
            &["true\t1", "true\t2", "true\t3"],
        );
    }

    #[test]
    fn coroutine_dead_after_return() {
        assert_output(
            r#"
let co = coroutine.create(fn() { return 42 })
var ok, v = coroutine.resume(co)
print(ok, v)
print(coroutine.status(co))
ok, v = coroutine.resume(co)
print(ok)
"#,
            &["true\t42", "dead", "false"],
        );
    }

    #[test]
    fn coroutine_status() {
        assert_output(
            r#"
let co = coroutine.create(fn() { yield() })
print(coroutine.status(co))
coroutine.resume(co)
print(coroutine.status(co))
coroutine.resume(co)
print(coroutine.status(co))
"#,
            &["suspended", "suspended", "dead"],
        );
    }

    #[test]
    fn coroutine_infinite_loop_yield() {
        assert_output(
            r#"
ticks = 0
let co = coroutine.create(fn() {
  while true {
    ticks = ticks + 1
    yield()
  }
})
coroutine.resume(co)
coroutine.resume(co)
coroutine.resume(co)
print(ticks)
"#,
            &["3"],
        );
    }

    #[test]
    fn coroutine_error_returns_false() {
        assert_output(
            r#"
let co = coroutine.create(fn() {
  error("boom")
})
let ok, msg = coroutine.resume(co)
print(ok)
"#,
            &["false"],
        );
    }

    #[test]
    fn coroutine_wrap() {
        assert_output(
            r#"
let resume = coroutine.wrap(fn() {
  yield(1)
  yield(2)
  return 3
})
print(resume())
print(resume())
print(resume())
"#,
            &["1", "2", "3"],
        );
    }

    #[test]
    fn coroutine_wrap_error_on_dead() {
        assert_output(
            r#"
let resume = coroutine.wrap(fn() { return 42 })
resume()
let ok, err = pcall(resume)
print(ok)
"#,
            &["false"],
        );
    }

    #[test]
    fn stdlib_ipairs() {
        assert_output(
            r#"
let t = {10, 20, 30}
var s = 0
for i, v in ipairs(t) { s = s + v }
print(s)
"#,
            &["60"],
        );
    }

    #[test]
    fn stdlib_ipairs_stops_at_nil() {
        assert_output(
            r#"
let t = {1, 2, none, 4}
var n = 0
for i, v in ipairs(t) { n = n + 1 }
print(n)
"#,
            &["2"],
        );
    }

    #[test]
    fn stdlib_pairs_visits_all_keys() {
        assert_output(
            r#"
let t = {a=1, b=2, c=3}
var s = 0
for k, v in pairs(t) { s = s + v }
print(s)
"#,
            &["6"],
        );
    }

    #[test]
    fn stdlib_next_manual() {
        assert_output(
            r#"
let t = {10, 20, 30}
var k, v = next(t, none)
print(k, v)
k, v = next(t, k)
print(k, v)
"#,
            &["1\t10", "2\t20"],
        );
    }

    #[test]
    fn mt_setget_metatable() {
        assert_output(
            r#"
let t = {}
let mt = {}
setmetatable(t, mt)
print(getmetatable(t) == mt)
setmetatable(t, none)
print(getmetatable(t))
"#,
            &["true", "nil"],
        );
    }

    #[test]
    fn mt_index_table() {
        assert_output(
            r#"
let proto = { greet = "hello" }
let obj = setmetatable({}, { __index = proto })
print(obj.greet)
print(obj.missing)
"#,
            &["hello", "nil"],
        );
    }

    #[test]
    fn mt_index_function() {
        assert_output(
            r#"
let obj = setmetatable({}, {
    __index = fn(t, k) { return k .. "!" }
})
print(obj.foo)
print(obj.bar)
"#,
            &["foo!", "bar!"],
        );
    }

    #[test]
    fn mt_newindex_function() {
        assert_output(
            r#"
log = {}
let obj = setmetatable({}, {
    __newindex = fn(t, k, v) {
        log[#log + 1] = k .. "=" .. tostring(v)
        rawset(t, k, v)
    }
})
obj.x = 10
obj.x = 20
obj.y = 30
print(#log)
print(log[1])
print(log[2])
print(obj.x, obj.y)
"#,
            &["2", "x=10", "y=30", "20\t30"],
        );
    }

    #[test]
    fn mt_call() {
        assert_output(
            r#"
let adder = setmetatable({}, {
    __call = fn(self, a, b) { return a + b }
})
print(adder(3, 4))
"#,
            &["7"],
        );
    }

    #[test]
    fn mt_add() {
        assert_output(
            r#"
Vec = {}
Vec.__index = Vec
Vec.__add = fn(a, b) { return setmetatable({ x = a.x + b.x }, Vec) }
Vec.__tostring = fn(v) { return "Vec(" .. v.x .. ")" }
fn Vec.new(x) { return setmetatable({ x = x }, Vec) }

let a = Vec.new(3)
let b = Vec.new(7)
let c = a + b
print(tostring(c))
"#,
            &["Vec(10)"],
        );
    }

    #[test]
    fn mt_tostring() {
        assert_output(
            r#"
let mt = { __tostring = fn(t) { return "MyObj<" .. t.id .. ">" } }
let obj = setmetatable({ id = 42 }, mt)
print(tostring(obj))
print(obj)
"#,
            &["MyObj<42>", "MyObj<42>"],
        );
    }

    #[test]
    fn mt_eq() {
        assert_output(
            r#"
let mt = {
    __eq = fn(a, b) { return a.val == b.val }
}
let a = setmetatable({ val = 5 }, mt)
let b = setmetatable({ val = 5 }, mt)
let c = setmetatable({ val = 9 }, mt)
print(a == b)
print(a == c)
"#,
            &["true", "false"],
        );
    }

    #[test]
    fn mt_lt_le() {
        assert_output(
            r#"
let mt = {
    __lt = fn(a, b) { return a.val < b.val },
    __le = fn(a, b) { return a.val <= b.val },
}
let a = setmetatable({ val = 3 }, mt)
let b = setmetatable({ val = 5 }, mt)
print(a < b)
print(b < a)
print(a <= a)
"#,
            &["true", "false", "true"],
        );
    }

    #[test]
    fn mt_len() {
        assert_output(
            r#"
let obj = setmetatable({}, { __len = fn() { return 42 } })
print(#obj)
"#,
            &["42"],
        );
    }

    #[test]
    fn mt_concat() {
        assert_output(
            r#"
let mt = { __concat = fn(a, b) { return a.s .. b.s } }
let a = setmetatable({ s = "hello" }, mt)
let b = setmetatable({ s = " world" }, mt)
print(a .. b)
"#,
            &["hello world"],
        );
    }

    #[test]
    fn mt_rawget_rawset() {
        assert_output(
            r#"
var calls = 0
let obj = setmetatable({}, {
    __index    = fn() { calls = calls + 1 return none },
    __newindex = fn() { calls = calls + 1 },
})
rawset(obj, "x", 99)
let v = rawget(obj, "x")
print(v, calls)
"#,
            &["99\t0"],
        );
    }

    #[test]
    fn mt_metatable_guard() {
        assert_output(
            r#"
let mt = { __metatable = "protected" }
let t = setmetatable({}, mt)
print(getmetatable(t))
"#,
            &["protected"],
        );
    }

    #[test]
    fn mt_index_chain_oop() {
        assert_output(
            r#"
Base = {}
Base.__index = Base
fn Base.new() { return setmetatable({}, Base) }
fn Base:greet() { return "hello from base" }

Child = setmetatable({}, { __index = Base })
Child.__index = Child
fn Child.new() { return setmetatable({}, Child) }

let obj = Child.new()
print(obj:greet())
"#,
            &["hello from base"],
        );
    }

    #[test]
    fn mt_gc_finalizer() {
        use vm::Vm;
        use std::sync::{Arc, Mutex};

        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();

        let mut vm = Vm::new_with_print(move |l| log2.lock().unwrap().push(l));

        run_with_vm(
            r#"gcfired = false
gcobj = setmetatable({}, { __gc = fn(self) { gcfired = true } })"#,
            &mut vm,
        ).unwrap();

        run_with_vm("gcobj = none", &mut vm).unwrap();

        vm.gc_collect();

        run_with_vm("print(gcfired)", &mut vm).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["true"]);
    }

    #[test]
    fn api_register_cfn() {
        use std::ffi::CString;
        use std::sync::{Arc, Mutex};

        static CALLED_WITH: std::sync::OnceLock<Arc<Mutex<Vec<f64>>>> =
            std::sync::OnceLock::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        CALLED_WITH.get_or_init(|| log.clone());

        unsafe extern "C" fn my_fn(U: *mut api::UmbraState) -> std::ffi::c_int {
            let x = unsafe { api::umbra_tonumber(U, 1) };
            CALLED_WITH.get().unwrap().lock().unwrap().push(x);
            unsafe { api::umbra_pushnumber(U, x * 2.0) };
            1
        }

        let U = api::umbra_newstate();
        let name = CString::new("double_it").unwrap();
        unsafe { api::umbra_register(U, name.as_ptr(), my_fn) };

        let src = CString::new("double_it(21)").unwrap();
        let rc = unsafe { api::umbra_dostring(U, src.as_ptr()) };
        assert_eq!(rc, 0, "dostring failed");

        assert_eq!(*CALLED_WITH.get().unwrap().lock().unwrap(), vec![21.0]);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn type_fn_returns_function() {
        assert_output(
            r#"
fn foo() {}
let bar = fn() {}
let baz = |x| x
print(type(foo))
print(type(bar))
print(type(baz))
"#,
            &["function", "function", "function"],
        );
    }

    #[test]
    fn vararg_basic() {
        assert_output(
            r#"
fn sum(...) {
    var a, b, c = ...
    return a + b + c
}
print(sum(1, 2, 3))
"#,
            &["6"],
        );
    }

    #[test]
    fn vararg_select_count() {
        assert_output(
            r##"
fn count(...) {
    return select("#", ...)
}
print(count(10, 20, 30))
"##,
            &["3"],
        );
    }

    #[test]
    fn upvalue_read_outer_local() {
        assert_output(
            r#"
let msg = "hello"
let f = fn() { print(msg) }
f()
"#,
            &["hello"],
        );
    }

    #[test]
    fn upvalue_closure_over_int() {
        assert_output(
            r#"
var x = 10
let add = fn(n) { x + n }
print(add(5))
"#,
            &["15"],
        );
    }

    #[test]
    fn upvalue_mutate_local_copy() {
        assert_output(
            r#"
var x = 1
let inc = fn() { x = x + 1 }
inc()
inc()
print(x)
"#,
            &["1"],
        );
    }

    #[test]
    fn upvalue_table_is_reference() {
        assert_output(
            r#"
let t = { n = 0 }
let bump = fn() { t.n = t.n + 1 }
bump()
bump()
bump()
print(t.n)
"#,
            &["3"],
        );
    }

    #[test]
    fn upvalue_named_fn_captures() {
        assert_output(
            r#"
let prefix = ">>>"
fn greet(name) { print(prefix .. name) }
greet("world")
"#,
            &[">>>world"],
        );
    }

    #[test]
    fn upvalue_closure_in_coroutine() {
        assert_output(
            r#"
let base = 100
let co = coroutine.wrap(fn() {
    yield(base + 1)
    yield(base + 2)
})
print(co())
print(co())
"#,
            &["101", "102"],
        );
    }

    #[test]
    fn stdlib_string_len() {
        assert_output(r#"print(string.len("hello"))"#, &["5"]);
    }

    #[test]
    fn stdlib_string_sub() {
        assert_output(r#"print(string.sub("hello", 2, 4))"#, &["ell"]);
        assert_output(r#"print(string.sub("hello", -3))"#, &["llo"]);
        assert_output(r#"print(string.sub("hello", 1, -1))"#, &["hello"]);
    }

    #[test]
    fn stdlib_string_rep() {
        assert_output(r#"print(string.rep("ab", 3))"#, &["ababab"]);
        assert_output(r#"print(string.rep("x", 3, "-"))"#, &["x-x-x"]);
    }

    #[test]
    fn stdlib_string_upper_lower() {
        assert_output(r#"print(string.upper("hello"))"#, &["HELLO"]);
        assert_output(r#"print(string.lower("WORLD"))"#, &["world"]);
    }

    #[test]
    fn stdlib_string_reverse() {
        assert_output(r#"print(string.reverse("hello"))"#, &["olleh"]);
    }

    #[test]
    fn stdlib_string_byte_char() {
        assert_output(r#"print(string.byte("A"))"#, &["65"]);
        assert_output(r#"print(string.char(65, 66, 67))"#, &["ABC"]);
    }

    #[test]
    fn stdlib_string_find() {
        assert_output(r#"let s, e = string.find("hello world", "world")
print(s, e)"#, &["7\t11"]);
        assert_output(r#"print(string.find("hello", "xyz"))"#, &["nil"]);
    }

    #[test]
    fn stdlib_string_format() {
        assert_output(r#"print(string.format("%d + %d = %d", 1, 2, 3))"#, &["1 + 2 = 3"]);
        assert_output(r#"print(string.format("%.2f", 3.14159))"#, &["3.14"]);
        assert_output(r#"print(string.format("%05d", 42))"#, &["00042"]);
        assert_output(r#"print(string.format("%s", "hi"))"#, &["hi"]);
        assert_output(r#"print(string.format("%%"))"#, &["%"]);
    }

    #[test]
    fn string_method_call() {
        assert_output(r#"print(("hello"):upper())"#, &["HELLO"]);
        assert_output(r#"print(("world"):sub(1, 3))"#, &["wor"]);
        assert_output(r#"let s = "hello"
print(s:len())"#, &["5"]);
    }

    #[test]
    fn stdlib_math_floor_ceil() {
        assert_output(r#"print(math.floor(3.7))"#, &["3"]);
        assert_output(r#"print(math.ceil(3.2))"#, &["4"]);
        assert_output(r#"print(math.floor(4))"#, &["4"]);
    }

    #[test]
    fn stdlib_math_abs() {
        assert_output(r#"print(math.abs(-5))"#, &["5"]);
        assert_output(r#"print(math.abs(-3.5))"#, &["3.5"]);
    }

    #[test]
    fn stdlib_math_max_min() {
        assert_output(r#"print(math.max(1, 5, 3, 2))"#, &["5"]);
        assert_output(r#"print(math.min(4, 1, 7))"#, &["1"]);
    }

    #[test]
    fn stdlib_math_sqrt() {
        assert_output(r#"print(math.sqrt(4.0))"#, &["2"]);
    }

    #[test]
    fn stdlib_math_type() {
        assert_output(r#"print(math.type(1))"#, &["integer"]);
        assert_output(r#"print(math.type(1.0))"#, &["float"]);
        assert_output(r#"print(math.type("x"))"#, &["false"]);
    }

    #[test]
    fn stdlib_table_insert() {
        assert_output(r#"let t = {1, 2, 3}
table.insert(t, 4)
print(#t, t[4])"#, &["4\t4"]);
        assert_output(r#"let t = {1, 2, 3}
table.insert(t, 2, 10)
print(t[1], t[2], t[3], t[4])"#, &["1\t10\t2\t3"]);
    }

    #[test]
    fn stdlib_table_remove() {
        assert_output(r#"let t = {10, 20, 30}
let v = table.remove(t)
print(v, #t)"#, &["30\t2"]);
        assert_output(r#"let t = {10, 20, 30}
let v = table.remove(t, 2)
print(v, t[1], t[2])"#, &["20\t10\t30"]);
    }

    #[test]
    fn stdlib_table_concat() {
        assert_output(r#"let t = {"a", "b", "c"}
print(table.concat(t, "-"))"#, &["a-b-c"]);
    }

    #[test]
    fn stdlib_table_sort() {
        assert_output(r#"let t = {3, 1, 4, 1, 5, 9, 2, 6}
table.sort(t)
print(t[1], t[2], t[3], t[8])"#, &["1\t1\t2\t9"]);
        assert_output(r#"let t = {3, 1, 2}
table.sort(t, fn(a, b) { a > b })
print(t[1], t[2], t[3])"#, &["3\t2\t1"]);
    }

    #[test]
    fn coroutine_resume_yield_values() {
        assert_output(r#"let co = coroutine.create(fn() {
    let x = yield(1)
    let y = yield(2)
    print(x, y)
})
coroutine.resume(co)
coroutine.resume(co, 10)
coroutine.resume(co, 20)
"#, &["10\t20"]);
    }
}
