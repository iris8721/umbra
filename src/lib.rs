pub mod api;
pub mod ast;
pub mod gc;
pub mod chunk;
pub mod compiler;
pub mod lexer;
pub mod pack;
pub mod parser;
pub mod pattern;
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
    vm.exec_owned(proto).map(|_| ()).map_err(|e| e.to_string())
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
    drop(vm);
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
        let toks = Lexer::tokenize("42 @ this is a comment\n99").unwrap();
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
    fn vm_gc_preserves_suspended_coroutine_locals() {
        // Without scanning a suspended coroutine's own regs as a GC root, the
        // allocation pressure below frees `t` and resuming reads a dangling pointer.
        assert_output(
            "let co = coroutine.create(fn() {
                let t = {123}
                yield()
                print(t[1])
            })
            coroutine.resume(co)
            var i = 0
            while i < 2000 {
                let junk = {i}
                i = i + 1
            }
            coroutine.resume(co)",
            &["123"],
        );
    }

    #[test]
    fn pcall_catches_internal_panic_cleanly() {
        assert_output(
            "let ok, msg = pcall(__debug_panic)
            print(ok)
            print(msg:sub(1, 14))",
            &["false", "internal error"],
        );
    }

    #[test]
    fn top_level_panic_without_pcall_is_caught_not_crashed() {
        // A bare __debug_panic() unwinds out of Op::Call; Vm::run must catch it.
        let mut vm = vm::Vm::new();
        assert!(run_with_vm("__debug_panic()", &mut vm).is_err());
        assert!(vm.poisoned);
    }

    #[test]
    fn coroutine_resume_panic_is_caught_not_crashed() {
        // A panic inside a coroutine body must poison the VM, not escape.
        let mut vm = vm::Vm::new();
        let result = run_with_vm(
            "let co = coroutine.create(fn() { __debug_panic() })
            coroutine.resume(co)",
            &mut vm,
        );
        assert!(result.is_err());
        assert!(vm.poisoned);
    }

    #[test]
    fn api_pcall_catches_panic_in_native_function_directly() {
        // Exercises api.rs's own call_value()/umbra_pcall catch_unwind, independent
        // of vm.rs's — this path never goes through call_value_isolated at all.
        use std::ffi::CString;
        let U = api::umbra_newstate();
        let name = CString::new("__debug_panic").unwrap();
        unsafe { api::umbra_getglobal(U, name.as_ptr()) };
        let rc = unsafe { api::umbra_pcall(U, 0, 0) };
        assert_eq!(rc, 1);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn vm_poisons_after_panic_and_refuses_further_top_level_runs() {
        let mut vm = vm::Vm::new();
        assert!(run_with_vm("pcall(__debug_panic)", &mut vm).is_ok());
        assert!(vm.poisoned);
        assert!(run_with_vm("print(1)", &mut vm).is_err());
    }

    #[test]
    fn stdlib_string_rep_rejects_oversized_result() {
        assert!(run("let s = \"x\"\nlet r = s:rep(999999999999)").is_err());
    }

    #[test]
    fn stdlib_string_format_clamps_huge_width() {
        assert_output(
            "print(string.len(string.format(\"%999999999999d\", 1)))",
            &["67108864"],
        );
    }

    #[test]
    fn stdlib_table_concat_unpack_move_reject_huge_ranges() {
        assert!(run("let t = {1, 2, 3}\ntable.concat(t, \",\", 1, 999999999999)").is_err());
        assert!(run("let t = {1, 2, 3}\ntable.unpack(t, 1, 999999999999)").is_err());
        assert!(run("let t = {1, 2, 3}\nlet u = {}\ntable.move(t, 1, 999999999999, 1, u)").is_err());
    }

    #[test]
    fn stdlib_table_insert_rejects_out_of_bounds_position() {
        assert!(run("let t = {1, 2, 3}\ntable.insert(t, math.mininteger, 99)").is_err());
    }

    #[test]
    fn vm_mod_min_int_overflow() {
        // 9223372036854775808 (2^63) itself doesn't fit in i64 and can't be lexed,
        // so i64::MIN is constructed as (i64::MAX negated) - 1.
        assert_output("print((-9223372036854775807 - 1) % -1)", &["0"]);
        assert_output("print((-9223372036854775807 - 1) // -1)", &["-9223372036854775808"]);
    }

    #[test]
    fn vm_continue_in_while_loop() {
        assert_output(
            "var i = 0
            var sum = 0
            while i < 5 {
                i = i + 1
                if i % 2 == 0 { continue }
                sum = sum + i
            }
            print(sum)",
            &["9"], // 1 + 3 + 5
        );
    }

    #[test]
    fn vm_continue_in_for_num_loop() {
        assert_output(
            "var sum = 0
            for i = 1, 5 {
                if i % 2 == 0 { continue }
                sum = sum + i
            }
            print(sum)",
            &["9"],
        );
    }

    #[test]
    fn vm_continue_in_for_in_loop() {
        assert_output(
            "var sum = 0
            for i, v in ipairs({1, 2, 3, 4, 5}) {
                if v % 2 == 0 { continue }
                sum = sum + v
            }
            print(sum)",
            &["9"],
        );
    }

    #[test]
    fn vm_continue_outside_loop_is_compile_error() {
        assert!(run("continue").is_err());
    }

    #[test]
    fn vm_repeat_until_runs_body_at_least_once() {
        assert_output(
            "var i = 0
            repeat {
                i = i + 1
            } until i >= 3
            print(i)",
            &["3"],
        );
        assert_output(
            "var i = 10
            repeat {
                i = i + 1
            } until true
            print(i)",
            &["11"],
        );
    }

    #[test]
    fn vm_repeat_until_condition_sees_body_locals() {
        // Lua's repeat-until scoping rule: `until` can reference locals the body just declared.
        assert_output(
            "var i = 0
            repeat {
                let done = i >= 2
                i = i + 1
            } until done
            print(i)",
            &["3"],
        );
    }

    #[test]
    fn vm_repeat_until_break_and_continue() {
        assert_output(
            "var i = 0
            var sum = 0
            repeat {
                i = i + 1
                if i % 2 == 0 { continue }
                sum = sum + i
            } until i >= 5
            print(sum)",
            &["9"],
        );
        assert_output(
            "var i = 0
            repeat {
                i = i + 1
                if i == 3 { break }
            } until false
            print(i)",
            &["3"],
        );
    }

    #[test]
    fn vm_goto_forward_skips_code() {
        assert_output(
            "print(\"a\")
            goto skip
            print(\"b\")
            ::skip::
            print(\"c\")",
            &["a", "c"],
        );
    }

    #[test]
    fn vm_goto_backward_loops() {
        assert_output(
            "var i = 0
            ::top::
            i = i + 1
            print(i)
            if i < 3 { goto top }",
            &["1", "2", "3"],
        );
    }

    #[test]
    fn vm_goto_undefined_label_is_compile_error() {
        assert!(run("goto nowhere").is_err());
    }

    #[test]
    fn vm_goto_duplicate_label_is_compile_error() {
        assert!(run("::here:: ::here::").is_err());
    }

    #[test]
    fn vm_runtime_errors_carry_line_numbers() {
        let e = run("print(1)\nprint(2)\nerror(\"boom\")").unwrap_err();
        assert_eq!(e, "line 3: boom");
    }

    #[test]
    fn vm_type_errors_carry_line_numbers() {
        let e = run("let x = nil\nprint(1)\nx + 1").unwrap_err();
        assert!(e.starts_with("line 3:"), "expected line 3 prefix, got: {e}");
    }

    #[test]
    fn vm_error_level_2_attributes_to_caller_line() {
        let e = run("fn f() {\n    error(\"boom\", 2)\n}\nf()").unwrap_err();
        assert_eq!(e, "line 4: boom");
    }

    #[test]
    fn vm_error_level_1_is_same_as_default() {
        let e = run("fn f() {\n    error(\"boom\", 1)\n}\nf()").unwrap_err();
        assert_eq!(e, "line 2: boom");
    }

    #[test]
    fn vm_pcall_error_message_carries_line_number() {
        assert_output(
            "let ok, msg = pcall(fn() {
                print(\"a\")
                error(\"deep boom\")
            })
            print(msg)",
            &["a", "line 3: deep boom"],
        );
    }

    #[test]
    fn vm_xpcall_success_passes_through_results() {
        assert_output(
            "let ok, a, b = xpcall(fn() { return 1, 2 }, fn(m) { return m })
            print(ok)
            print(a)
            print(b)",
            &["true", "1", "2"],
        );
    }

    #[test]
    fn vm_xpcall_runs_handler_on_error() {
        assert_output(
            "let ok, msg = xpcall(fn() { error(\"boom\") }, fn(m) { return \"handled: \" .. m })
            print(ok)
            print(msg)",
            &["false", "handled: line 1: boom"],
        );
    }

    #[test]
    fn vm_xpcall_survives_handler_that_itself_errors() {
        assert_output(
            "let ok, msg = xpcall(fn() { error(\"boom\") }, fn(m) { error(\"handler boom\") })
            print(ok)
            print(type(msg))",
            &["false", "string"],
        );
    }

    #[test]
    fn pattern_find_character_classes_and_anchors() {
        assert_output(
            "let a, b = string.find(\"hello 123 world\", \"%d+\")
            print(a)
            print(b)",
            &["7", "9"],
        );
        assert_output(
            "let a, b = string.find(\"abc\", \"^a\")\nprint(a)\nprint(b)",
            &["1", "1"],
        );
        assert_output(r#"print(string.find("abc", "^b"))"#, &["nil"]);
        assert_output(
            "let a, b = string.find(\"abc\", \"c$\")\nprint(a)\nprint(b)",
            &["3", "3"],
        );
    }

    #[test]
    fn pattern_sets_and_quantifiers() {
        assert_output(r#"print(string.match("hello123world", "[%a]+"))"#, &["hello"]);
        assert_output(r#"print(string.match("hello123world", "[^%d]+"))"#, &["hello"]);
        assert_output(r#"print(string.match("aaa", "a-b") == nil)"#, &["true"]);
        assert_output(r#"print(string.match("<b>bold</b>", "<(.-)>"))"#, &["b"]);
        assert_output(r#"print(string.match("color", "colou?r"))"#, &["color"]);
        assert_output(r#"print(string.match("colour", "colou?r"))"#, &["colour"]);
    }

    #[test]
    fn pattern_captures() {
        assert_output(
            r#"let y, m, d = string.match("2026-07-19", "(%d+)-(%d+)-(%d+)")
            print(y)
            print(m)
            print(d)"#,
            &["2026", "07", "19"],
        );
        assert_output(
            "let a, b = string.match(\"hello\", \"()ll()\")\nprint(a)\nprint(b)",
            &["3", "5"],
        );
    }

    #[test]
    fn pattern_balanced_match() {
        assert_output(r#"print(string.match("(foo(bar)baz)", "%b()"))"#, &["(foo(bar)baz)"]);
    }

    #[test]
    fn pattern_gmatch_iterates_all_matches() {
        assert_output(
            "var words = \"\"
            for w in string.gmatch(\"the quick brown fox\", \"%a+\") {
                words = words .. w .. \",\"
            }
            print(words)",
            &["the,quick,brown,fox,"],
        );
    }

    #[test]
    fn pattern_gsub_string_replacement_with_backreference() {
        assert_output(
            r#"let s, n = string.gsub("hello world", "(%a+)", "<%1>")
            print(s)
            print(n)"#,
            &["<hello> <world>", "2"],
        );
    }

    #[test]
    fn pattern_gsub_function_replacement() {
        assert_output(
            "let s = string.gsub(\"abc\", \"%a\", fn(c) { return string.upper(c) })
            print(s)",
            &["ABC"],
        );
    }

    #[test]
    fn pattern_gsub_table_replacement() {
        assert_output(
            "let t = {foo = \"bar\"}
            let s = string.gsub(\"hello foo world\", \"%a+\", t)
            print(s)",
            &["hello bar world"],
        );
    }

    #[test]
    fn pattern_gsub_respects_max_count() {
        assert_output(
            r#"let s, n = string.gsub("aaaa", "a", "b", 2)
            print(s)
            print(n)"#,
            &["bbaa", "2"],
        );
    }

    #[test]
    fn stdlib_table_pack_and_unpack_round_trip() {
        assert_output(
            "let t = table.pack(10, 20, 30)
            print(t.n)
            print(t[1])
            print(t[3])
            let a, b, c = table.unpack(t, 1, t.n)
            print(a + b + c)",
            &["3", "10", "30", "60"],
        );
    }

    #[test]
    fn gc_weak_values_are_cleared_once_unreachable() {
        assert_output(
            "let cache = {}
            setmetatable(cache, {__mode = \"v\"})
            cache.item = {1, 2, 3}
            print(cache.item == nil)
            var i = 0
            while i < 2000 {
                let junk = {i}
                i = i + 1
            }
            print(cache.item == nil)",
            &["false", "true"],
        );
    }

    #[test]
    fn gc_weak_keys_are_cleared_once_unreachable() {
        assert_output(
            "let registry = {}
            setmetatable(registry, {__mode = \"k\"})
            var k = {}
            registry[k] = \"data\"
            var count = 0
            for key, val in pairs(registry) { count = count + 1 }
            print(count)
            k = nil
            var i = 0
            while i < 2000 {
                let junk = {i}
                i = i + 1
            }
            count = 0
            for key, val in pairs(registry) { count = count + 1 }
            print(count)",
            &["1", "0"],
        );
    }

    #[test]
    fn gc_strong_table_is_unaffected_by_weak_sweep() {
        assert_output(
            "let cache = {}
            cache.item = {1, 2, 3}
            var i = 0
            while i < 2000 {
                let junk = {i}
                i = i + 1
            }
            print(cache.item == nil)
            print(cache.item[1])",
            &["false", "1"],
        );
    }

    #[test]
    fn io_write_has_no_trailing_newline_or_separator() {
        // io.write concatenates args directly, unlike print's tab-joining +
        // trailing newline; each call surfaces as its own hook entry here.
        assert_output(
            "io.write(\"a\")
            io.write(\"b\", \"c\")",
            &["a", "bc"],
        );
    }

    #[test]
    fn os_time_and_clock_return_sane_values() {
        assert_output(
            "print(math.type(os.time()))
            print(os.clock() >= 0)",
            &["integer", "true"],
        );
    }

    #[test]
    fn os_getenv_returns_nil_for_unset_var() {
        assert_output(
            "print(os.getenv(\"UMBRA_DEFINITELY_UNSET_VAR_XYZ\"))",
            &["nil"],
        );
    }

    #[test]
    fn os_date_from_script_with_explicit_epoch() {
        assert_output(
            "print(os.date(\"%Y-%m-%d\", 0))",
            &["1970-01-01"],
        );
    }

    #[test]
    fn gc_auto_triggers_inside_a_call_free_tight_loop() {
        // The GC/step-limit checks live in run_inner's fast (inner) dispatch
        // loop, which handles simple instructions (arithmetic, table ops, Jmp)
        // without ever returning to the outer loop unless a threshold is hit —
        // a pure allocation loop with no function calls must still trigger
        // automatic collection, not just calls/coroutine-resume boundaries.
        use std::ffi::CString;
        let U = api::umbra_newstate();
        let src = CString::new(
            "var i = 0
            while i < 5000 {
                let junk = {i, i, i}
                i = i + 1
            }"
        ).unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, src.as_ptr()) }, 0);
        let live = unsafe { api::umbra_gc_livecount(U) };
        assert!(live < 5000, "expected auto-GC to keep live count well below total allocations, got {live}");
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn api_step_limit_bounds_a_runaway_script() {
        // A real infinite loop: if the budget check were broken, this test
        // would hang the whole test binary rather than fail cleanly.
        use std::ffi::CString;
        let U = api::umbra_newstate();
        unsafe { api::umbra_set_step_limit(U, 1000) };
        let src = CString::new("while true { }").unwrap();
        let rc = unsafe { api::umbra_dostring(U, src.as_ptr()) };
        assert_eq!(rc, 1);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn api_step_limit_zero_means_unlimited() {
        use std::ffi::CString;
        let U = api::umbra_newstate();
        unsafe { api::umbra_set_step_limit(U, 0) };
        let src = CString::new("var i = 0\nwhile i < 5000 { i = i + 1 }").unwrap();
        let rc = unsafe { api::umbra_dostring(U, src.as_ptr()) };
        assert_eq!(rc, 0);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn require_loads_compiles_and_caches_a_module() {
        let mod_name = "umbra_test_require_module_xyz";
        let path = format!("{mod_name}.umbra");
        std::fs::write(&path, "let calls = 0\nfn get() { calls = calls + 1; return calls }\nreturn { get = get }").unwrap();

        let result = run_capture(&format!(
            "let m = require(\"{mod_name}\")
            print(m.get())
            print(m.get())
            let m2 = require(\"{mod_name}\")
            print(m2.get())"
        ));

        std::fs::remove_file(&path).ok();

        // Cached: m2 is the SAME module table as m, so its internal `calls`
        // state carries over rather than require() re-running the file.
        assert_eq!(result.unwrap(), vec!["1", "2", "3"]);
    }

    #[test]
    fn require_missing_module_is_a_clean_error() {
        assert!(run("require(\"umbra_test_definitely_missing_module_xyz\")").is_err());
    }

    #[test]
    fn io_open_writes_then_reads_a_file() {
        let path = "umbra_test_io_open_rw_xyz.txt";
        let result = run_capture(&format!(
            "let f = io.open(\"{path}\", \"w\")
            f:write(\"line one\\n\")
            f:write(\"line two\\n\")
            f:close()
            let g = io.open(\"{path}\", \"r\")
            print(g:read())
            print(g:read())
            print(g:read())
            g:close()"
        ));
        std::fs::remove_file(path).ok();
        assert_eq!(result.unwrap(), vec!["line one", "line two", "nil"]);
    }

    #[test]
    fn io_open_lines_iterates_each_line() {
        let path = "umbra_test_io_open_lines_xyz.txt";
        std::fs::write(path, "a\nb\nc\n").unwrap();
        let result = run_capture(&format!(
            "let f = io.open(\"{path}\", \"r\")
            for line in f:lines() {{
                print(line)
            }}
            f:close()"
        ));
        std::fs::remove_file(path).ok();
        assert_eq!(result.unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    fn io_open_missing_file_returns_nil_and_error() {
        assert_output(
            "let f, err = io.open(\"umbra_test_io_open_missing_xyz.txt\", \"r\")
            print(f)
            print(err != nil)",
            &["nil", "true"],
        );
    }

    #[test]
    fn io_open_read_after_close_is_an_error() {
        let path = "umbra_test_io_open_closed_xyz.txt";
        std::fs::write(path, "x").unwrap();
        let result = run(&format!(
            "let f = io.open(\"{path}\", \"r\")
            f:close()
            f:read()"
        ));
        std::fs::remove_file(path).ok();
        assert!(result.is_err());
    }

    #[test]
    fn api_max_objects_bounds_unbounded_allocation() {
        // A real infinite allocation loop: if the ceiling check were broken,
        // this would hang/exhaust memory rather than fail cleanly.
        use std::ffi::CString;
        let U = api::umbra_newstate();
        unsafe { api::umbra_set_max_objects(U, 500) };
        // Each new table is kept reachable via `arr`, so these are genuinely
        // live, not garbage the collector could just reclaim each cycle.
        let src = CString::new(
            "let arr = {}
            var i = 0
            while true {
                arr[i] = {1, 2, 3}
                i = i + 1
            }"
        ).unwrap();
        let rc = unsafe { api::umbra_dostring(U, src.as_ptr()) };
        assert_eq!(rc, 1);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn api_max_objects_zero_means_unlimited() {
        use std::ffi::CString;
        let U = api::umbra_newstate();
        unsafe { api::umbra_set_max_objects(U, 0) };
        let src = CString::new("var i = 0\nwhile i < 3000 { let junk = {i}\ni = i + 1 }").unwrap();
        let rc = unsafe { api::umbra_dostring(U, src.as_ptr()) };
        assert_eq!(rc, 0);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn compound_assign_arithmetic_ops() {
        assert_output(
            "var x = 10
            x += 5   print(x)
            x -= 3   print(x)
            x *= 2   print(x)
            x /= 4   print(x)
            x = 17
            x //= 5  print(x)
            x %= 4   print(x)
            x = 2
            x ^= 8   print(x)",
            &["15", "12", "24", "6", "3", "3", "256"],
        );
    }

    #[test]
    fn compound_assign_bitwise_and_shift_ops() {
        assert_output(
            "var x = 0xF0
            x &= 0x3C  print(x)
            x |= 0x01  print(x)
            x ~= 0xFF  print(x)
            x = 1
            x <<= 4    print(x)
            x >>= 2    print(x)",
            &["48", "49", "206", "16", "4"],
        );
    }

    #[test]
    fn compound_assign_concat() {
        assert_output(
            "var s = \"a\"
            s ..= \"b\"
            s ..= \"c\"
            print(s)",
            &["abc"],
        );
    }

    #[test]
    fn compound_assign_on_field_and_index_targets() {
        assert_output(
            "let t = {count = 10}
            t.count += 5
            print(t.count)
            let arr = {1, 2, 3}
            arr[2] *= 10
            print(arr[2])",
            &["15", "20"],
        );
    }

    #[test]
    fn at_sign_is_the_comment_token() {
        assert_output("print(1) @ this is a comment\nprint(2)", &["1", "2"]);
    }

    #[test]
    fn minus_minus_is_decrement_not_a_comment() {
        // The exact case flagged during implementation: with @ as the comment
        // token, -- unambiguously means decrement, not "start of comment".
        assert_output("var i = 5\ni--\nprint(i)", &["4"]);
        assert_output("var a = 10\nvar b = a - -1\nprint(b)", &["11"]);
    }

    #[test]
    fn postfix_increment_returns_old_value() {
        assert_output(
            "var i = 5
            let old = i++
            print(old)
            print(i)",
            &["5", "6"],
        );
    }

    #[test]
    fn prefix_increment_returns_new_value() {
        assert_output(
            "var i = 5
            let new = ++i
            print(new)
            print(i)",
            &["6", "6"],
        );
    }

    #[test]
    fn decrement_on_field_and_index_targets() {
        assert_output(
            "let t = {count = 10}
            t.count--
            print(t.count)
            let arr = {1, 2, 3}
            arr[1]++
            print(arr[1])",
            &["9", "2"],
        );
    }

    #[test]
    fn increment_as_a_mid_block_statement() {
        // The actual parser bug found while implementing this: a bare expression
        // statement is normally only valid at the tail of a block (implicit
        // return); ++/-- needed the same Stmt-wrapping treatment as Call/MethodCall.
        assert_output(
            "var i = 0
            i++
            i++
            print(i)
            print(\"reached end\")",
            &["2", "reached end"],
        );
    }

    #[test]
    fn ternary_basic_and_nested() {
        assert_output("print(true ? \"a\" : \"b\")", &["a"]);
        assert_output("print(false ? \"a\" : \"b\")", &["b"]);
        assert_output("print(1 < 2 ? 10 : 20)", &["10"]);
        // Right-associative: a ? b : (c ? d : e)
        assert_output("print(false ? 1 : true ? 2 : 3)", &["2"]);
    }

    #[test]
    fn ternary_fixes_the_and_or_falsy_footgun() {
        // The classic Lua wart: `cond and a or b` breaks when `a` is falsy,
        // silently falling through to `b`. A real ternary doesn't have this bug.
        assert_output("print(true and false or \"fallback\")", &["fallback"]);
        assert_output("print(true ? false : \"fallback\")", &["false"]);
    }

    #[test]
    fn ternary_only_evaluates_the_taken_branch() {
        assert_output(
            "fn boom() { error(\"should not run\") }
            print(true ? \"ok\" : boom())",
            &["ok"],
        );
    }

    #[test]
    fn utf8_len_counts_codepoints_not_bytes() {
        // "héllo" has 5 codepoints but 6 bytes (é is 2 bytes in UTF-8).
        assert_output("print(utf8.len(\"h\\xc3\\xa9llo\"))", &["5"]);
        assert_output("print(string.len(\"h\\xc3\\xa9llo\"))", &["6"]);
    }

    #[test]
    fn utf8_char_and_codepoint_round_trip() {
        assert_output(
            "let s = utf8.char(104, 233, 108, 108, 111)
            print(s)
            print(utf8.codepoint(s, 1))
            print(utf8.codepoint(s, 2))",
            &["h\u{e9}llo", "104", "233"],
        );
    }

    #[test]
    fn utf8_codes_iterates_codepoints_with_byte_positions() {
        assert_output(
            "var out = \"\"
            for pos, cp in utf8.codes(\"a\\xc3\\xa9b\") {
                out = out .. pos .. \":\" .. cp .. \",\"
            }
            print(out)",
            &["1:97,2:233,4:98,"],
        );
    }

    #[test]
    fn debug_traceback_via_xpcall_handler_shows_call_chain() {
        assert_output(
            "fn inner() { error(\"boom\") }
            fn outer() { inner() }
            let ok, tb = xpcall(outer, debug.traceback)
            print(ok)
            print(string.find(tb, \"stack traceback\") != nil)
            let a, b = string.gsub(tb, \"\\n\", \"|\")
            print(b >= 2)",
            &["false", "true", "true"],
        );
    }

    #[test]
    fn debug_traceback_called_directly_still_works() {
        assert_output(
            "fn f() { return debug.traceback(\"hi\") }
            let tb = f()
            print(string.find(tb, \"^hi\") != nil)
            print(string.find(tb, \"stack traceback\") != nil)",
            &["true", "true"],
        );
    }

    #[test]
    fn os_date_matches_known_epoch_values() {
        assert_eq!(vm::format_civil_time(0, "%Y-%m-%d %H:%M:%S"), "1970-01-01 00:00:00");
        assert_eq!(vm::format_civil_time(1700000000, "%Y-%m-%d %H:%M:%S"), "2023-11-14 22:13:20");
        assert_eq!(vm::format_civil_time(1000000000, "%Y-%m-%d %H:%M:%S"), "2001-09-09 01:46:40");
    }

    #[test]
    fn vm_bigint_literals_survive_round_trip() {
        // 2^47 is the smallest magnitude that overflows the 48-bit inline payload;
        // i64::MAX/MIN can't be written as literals directly (see the mod-overflow
        // test above), so MIN is built the same way: negate MAX, then subtract 1.
        assert_output("print(9223372036854775807)", &["9223372036854775807"]);
        assert_output("print(-9223372036854775807 - 1)", &["-9223372036854775808"]);
    }

    #[test]
    fn vm_bigint_arithmetic_stays_correct() {
        assert_output("print(9223372036854775807 - 1)", &["9223372036854775806"]);
        // wrapping_mul overflow past i64::MAX wraps to i64::MIN, same convention Add/Sub already use.
        assert_output("print(4611686018427387904 * 2)", &["-9223372036854775808"]);
        assert_output("print(-(9223372036854775807))", &["-9223372036854775807"]);
    }

    #[test]
    fn vm_bigint_equality_and_table_keys_are_by_value() {
        // Two separately-computed bigints holding the same value must compare
        // equal and hash to the same table key, not just alias the same pointer.
        assert_output(
            "let a = 9223372036854775807 - 0
            let b = 9223372036854775806 + 1
            print(a == b)
            let t = {}
            t[a] = \"first\"
            t[b] = \"second\"
            print(t[a])",
            &["true", "second"],
        );
    }

    #[test]
    fn vm_math_maxinteger_mininteger_are_correct() {
        assert_output("print(math.maxinteger)", &["9223372036854775807"]);
        assert_output("print(math.mininteger)", &["-9223372036854775808"]);
        assert_output("print(math.type(math.maxinteger))", &["integer"]);
    }

    #[test]
    fn api_pushinteger_roundtrips_full_i64_range() {
        let U = api::umbra_newstate();
        unsafe { api::umbra_pushinteger(U, i64::MAX) };
        assert_eq!(unsafe { api::umbra_tointeger(U, -1) }, i64::MAX);
        unsafe { api::umbra_pushinteger(U, i64::MIN) };
        assert_eq!(unsafe { api::umbra_tointeger(U, -1) }, i64::MIN);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn vm_idiv_floor_division() {
        assert_output("print(7 // 2)", &["3"]);
        assert_output("print(-7 // 2)", &["-4"]);
        assert_output("print(7.5 // 2)", &["3"]);
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
    fn vm_close_attribute_calls_close_at_end_of_block() {
        assert_output(
            "if true {
                let r <close> = {close = fn(self) { print(\"closed\") }}
                print(\"inside\")
            }
            print(\"after\")",
            &["inside", "closed", "after"],
        );
    }

    #[test]
    fn vm_close_attribute_runs_in_reverse_declaration_order() {
        assert_output(
            "if true {
                let a <close> = {close = fn(self) { print(\"close a\") }}
                let b <close> = {close = fn(self) { print(\"close b\") }}
            }",
            &["close b", "close a"],
        );
    }

    #[test]
    fn vm_close_attribute_works_with_var() {
        assert_output(
            "if true {
                var r <close> = {close = fn(self) { print(\"closed\") }}
                print(\"inside\")
            }",
            &["inside", "closed"],
        );
    }

    #[test]
    fn vm_plain_let_is_not_auto_closed() {
        assert_output(
            "if true {
                let r = {close = fn(self) { print(\"should not run\") }}
            }
            print(\"after\")",
            &["after"],
        );
    }

    #[test]
    fn vm_close_attribute_works_inside_while_loop_body() {
        assert_output(
            "var i = 0
            while i < 2 {
                let r <close> = {close = fn(self) { print(\"closed\") }}
                i = i + 1
            }
            print(\"done\")",
            &["closed", "closed", "done"],
        );
    }

    #[test]
    fn vm_close_attribute_works_when_also_captured_by_a_closure() {
        // Regression: a <close> local that's also captured as an upvalue
        // holds a box (see Local::boxed), but emit_closes used to look up
        // "close" directly on the box itself instead of unboxing first.
        assert_output(
            "let r <close> = { tag = \"A\", close = fn(self) { print(\"closed\") } }
            let peek = fn() { return r.tag }
            print(peek())",
            &["A", "closed"],
        );
    }

    #[test]
    fn vm_close_attribute_closes_real_file_handle() {
        let path = "umbra_test_close_attr_io_xyz.txt";
        let result = run_capture(&format!(
            "if true {{
                let f <close> = io.open(\"{path}\", \"w\")
                f:write(\"data\")
            }}
            let g = io.open(\"{path}\", \"r\")
            print(g:read())
            g:close()"
        ));
        std::fs::remove_file(path).ok();
        assert_eq!(result.unwrap(), vec!["data"]);
    }

    #[test]
    fn stdlib_string_pack_unpack_roundtrips_integers() {
        assert_output(
            "let packed = string.pack(\"<i4\", 12345)
            let n, pos = string.unpack(\"<i4\", packed)
            print(n)
            print(pos)",
            &["12345", "5"],
        );
    }

    #[test]
    fn stdlib_string_pack_unpack_roundtrips_negative_integers() {
        assert_output(
            "let packed = string.pack(\"<i4\", -42)
            let n = string.unpack(\"<i4\", packed)
            print(n)",
            &["-42"],
        );
    }

    #[test]
    fn stdlib_string_pack_respects_endianness() {
        assert_output(
            "let le = string.pack(\"<I2\", 1)
            let be = string.pack(\">I2\", 1)
            print(le)
            print(be)",
            &["0100", "0001"],
        );
    }

    #[test]
    fn stdlib_string_pack_unpack_roundtrips_double() {
        assert_output(
            "let packed = string.pack(\"d\", 3.5)
            let f = string.unpack(\"d\", packed)
            print(f)",
            &["3.5"],
        );
    }

    #[test]
    fn stdlib_string_pack_unpack_roundtrips_length_prefixed_string() {
        assert_output(
            r#"let packed = string.pack("s1", "hi")
            let s, pos = string.unpack("s1", packed)
            print(s)
            print(pos)"#,
            &["hi", "4"],
        );
    }

    #[test]
    fn stdlib_string_pack_fixed_string_pads_with_zeros() {
        assert_output(
            r#"let packed = string.pack("c5", "ab")
            print(#packed)"#,
            &["10"],
        );
    }

    #[test]
    fn stdlib_string_pack_unpack_multiple_fields() {
        assert_output(
            r#"let packed = string.pack("<i4B", 300, 7)
            let n, b, pos = string.unpack("<i4B", packed)
            print(n)
            print(b)
            print(pos)"#,
            &["300", "7", "6"],
        );
    }

    #[test]
    fn vm_switch_runs_matching_case() {
        assert_output(
            "let x = 2
            switch x {
                case 1 { print(\"one\") }
                case 2 { print(\"two\") }
                else { print(\"other\") }
            }",
            &["two"],
        );
    }

    #[test]
    fn vm_switch_falls_to_else_when_no_case_matches() {
        assert_output(
            "let x = 99
            switch x {
                case 1 { print(\"one\") }
                case 2 { print(\"two\") }
                else { print(\"other\") }
            }",
            &["other"],
        );
    }

    #[test]
    fn vm_switch_with_no_match_and_no_else_does_nothing() {
        assert_output(
            "let x = 99
            switch x {
                case 1 { print(\"one\") }
            }
            print(\"after\")",
            &["after"],
        );
    }

    #[test]
    fn vm_switch_case_with_multiple_values_matches_any() {
        assert_output(
            "let x = 3
            switch x {
                case 1, 2, 3 { print(\"low\") }
                else { print(\"high\") }
            }",
            &["low"],
        );
    }

    #[test]
    fn vm_switch_evaluates_subject_expr_only_once() {
        assert_output(
            "fn next() { print(\"called\"); return 2 }
            switch next() {
                case 1 { print(\"one\") }
                case 2 { print(\"two\") }
            }",
            &["called", "two"],
        );
    }

    #[test]
    fn vm_string_interpolation_embeds_expr_value() {
        assert_output(
            r#"let name = "world"
            print("hello ${name}!")"#,
            &["hello world!"],
        );
    }

    #[test]
    fn vm_string_interpolation_coerces_numbers() {
        assert_output(
            r#"let a = 2
            let b = 3
            print("${a} + ${b} = ${a + b}")"#,
            &["2 + 3 = 5"],
        );
    }

    #[test]
    fn vm_string_interpolation_handles_nested_braces_and_quotes() {
        assert_output(
            r#"fn f(x) { return x }
            print("value: ${f({a = 1}).a}")
            print("quoted: ${f("}")}")"#,
            &["value: 1", "quoted: }"],
        );
    }

    #[test]
    fn vm_string_interpolation_empty_and_plain_still_work() {
        assert_output(
            r#"print("${""}")
            print("plain string")"#,
            &["", "plain string"],
        );
    }

    #[test]
    fn vm_table_destructure_binds_named_fields() {
        assert_output(
            "let point = {x = 1, y = 2}
            let {x, y} = point
            print(x)
            print(y)",
            &["1", "2"],
        );
    }

    #[test]
    fn vm_table_destructure_var_is_mutable() {
        assert_output(
            "let point = {x = 1}
            var {x} = point
            x = x + 1
            print(x)",
            &["2"],
        );
    }

    #[test]
    fn vm_table_destructure_missing_field_is_nil() {
        assert_output(
            "let point = {x = 1}
            let {x, y} = point
            print(x)
            print(y)",
            &["1", "nil"],
        );
    }

    #[test]
    fn vm_colon_method_def_implicitly_binds_self() {
        assert_output(
            "let T = {}
            T.__index = T
            fn T.new(x) { return setmetatable({x = x}, T) }
            fn T:get() { return self.x }
            fn T:add(n) { return self.x + n }
            let t = T.new(42)
            print(t:get())
            print(t:add(8))",
            &["42", "50"],
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
    fn api_tostring_is_nul_terminated_for_c_hosts() {
        // Read via CStr::from_ptr, exactly how a real C host would, rather than
        // through any length umbra already knows internally.
        use std::ffi::{CStr, CString};

        let U = api::umbra_newstate();
        let src = CString::new(r#"fn greet() { return "hello, umbra!" }"#).unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, src.as_ptr()) }, 0);

        let name = CString::new("greet").unwrap();
        unsafe { api::umbra_getglobal(U, name.as_ptr()) };
        assert_eq!(unsafe { api::umbra_pcall(U, 0, 1) }, 0);

        let ptr = unsafe { api::umbra_tostring(U, -1) };
        let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap();
        assert_eq!(s, "hello, umbra!");

        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn api_pop_and_settop_clamp_extreme_indices() {
        let U = api::umbra_newstate();
        unsafe { api::umbra_pushnumber(U, 1.0) };
        unsafe { api::umbra_pop(U, i32::MIN) }; // must not panic
        assert!(unsafe { api::umbra_gettop(U) } > 0);

        unsafe { api::umbra_settop(U, i32::MAX) }; // must not try to allocate ~17GB
        assert!(unsafe { api::umbra_gettop(U) } as i64 <= 64 * 1024 * 1024 + 1);

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
    fn gc_tracks_and_frees_stdlib_computed_strings() {
        // Computed strings (alloc_string_val) must be GC-tracked and freed.
        use std::ffi::CString;

        let U = api::umbra_newstate();
        let src = CString::new(r#"x = ("hello"):upper()"#).unwrap();

        // Warm-up: interns any string constants ("hello", method-name lookup key)
        // so the cache-hit path doesn't add to livecount on the next identical call —
        // isolating what's left is purely the upper() result's own allocation.
        assert_eq!(unsafe { api::umbra_dostring(U, src.as_ptr()) }, 0);
        let before = unsafe { api::umbra_gc_livecount(U) };
        assert_eq!(unsafe { api::umbra_dostring(U, src.as_ptr()) }, 0);
        let after_alloc = unsafe { api::umbra_gc_livecount(U) };
        assert!(after_alloc > before, "string.upper's result should be GC-tracked (before={before} after={after_alloc})");

        unsafe { api::umbra_gc_collect(U) };
        let after_collect = unsafe { api::umbra_gc_livecount(U) };
        assert!(after_collect < after_alloc, "unreachable computed string should be freed");

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
        unsafe { api::umbra_register(U, name.as_ptr(), Some(my_fn)) };

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
    fn upvalue_mutation_is_visible_to_the_enclosing_scope() {
        assert_output(
            r#"
var x = 1
let inc = fn() { x = x + 1 }
inc()
inc()
print(x)
"#,
            &["3"],
        );
    }

    #[test]
    fn upvalue_mutation_is_shared_across_multiple_closures() {
        assert_output(
            r#"
var x = 1
let inc = fn() { x = x + 1 }
let read = fn() { return x }
inc()
inc()
print(read())
"#,
            &["3"],
        );
    }

    #[test]
    fn upvalue_each_outer_call_gets_independent_state() {
        assert_output(
            r#"
fn make_counter() {
    var n = 0
    return fn() { n = n + 1; return n }
}
let c1 = make_counter()
let c2 = make_counter()
print(c1())
print(c1())
print(c2())
"#,
            &["1", "2", "1"],
        );
    }

    #[test]
    fn upvalue_two_levels_deep_still_shares_state() {
        assert_output(
            r#"
fn outer() {
    var n = 0
    fn middle() {
        return fn() { n = n + 1; return n }
    }
    return middle()
}
let inc = outer()
print(inc())
print(inc())
"#,
            &["1", "2"],
        );
    }

    #[test]
    fn upvalue_three_levels_deep_still_shares_state() {
        assert_output(
            r#"
fn level1() {
    var n = 0
    fn level2() {
        fn level3() {
            return fn() { n = n + 1; return n }
        }
        return level3()
    }
    return level2()
}
let inc = level1()
print(inc())
print(inc())
print(inc())
"#,
            &["1", "2", "3"],
        );
    }

    #[test]
    fn upvalue_local_func_recurses_via_upvalue_capture() {
        // Side effect of the live outer-scope chain: a `let fn name(){}`
        // (local function statement) can now legitimately capture itself as
        // an upvalue for recursion, since the box it's registered under
        // exists (empty) before the body compiles, and only gets filled in
        // with the real closure afterward.
        assert_output(
            r#"
fn make() {
    let fn fact(n) {
        if n <= 1 { return 1 }
        return n * fact(n - 1)
    }
    return fact(5)
}
print(make())
"#,
            &["120"],
        );
    }

    #[test]
    fn upvalue_boxing_two_captured_params_dont_clobber_each_other() {
        // Regression: box_in_place used to run param-by-param in the same
        // loop that reserves param registers, so boxing param i's scratch
        // register aliased param i+1's not-yet-reserved (but already
        // populated by the calling convention) register, corrupting it
        // before it was ever read.
        assert_output(
            r#"
fn make(name, steps) {
    return fn() {
        for i = 1, steps {
            print(name)
            print(i)
        }
    }
}
let f = make("a", 3)
f()
"#,
            &["a", "1", "a", "2", "a", "3"],
        );
    }

    #[test]
    fn upvalue_shared_state_survives_coroutine_yield_boundaries() {
        assert_output(
            r#"
fn make_gen()
{
    var total = 0
    let gen = coroutine.wrap(fn() {
        var i = 0
        while i < 3 {
            i = i + 1
            total = total + i
            yield(total)
        }
    })
    return { next = gen, get_total = fn() { return total } }
}
let g = make_gen()
print(g.next())
print(g.next())
print(g.get_total())
"#,
            &["1", "3", "3"],
        );
    }

    #[test]
    fn upvalue_boxed_closures_survive_gc_collection() {
        assert_output(
            r#"
fn make_counter()
{
    var n = 0
    return fn() { n = n + 1; return n }
}
let counters = {}
for i = 1, 20 {
    counters[i] = make_counter()
    let junk = {1, 2, 3, 4, 5}
    let junk2 = {junk, junk, junk}
}
for i = 1, 20 {
    for j = 1, i { counters[i]() }
}
print(counters[1]())
print(counters[20]())
"#,
            &["2", "21"],
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

    #[test]
    fn gc_finalizer_closure_and_fields_survive_until_it_runs() {
        use vm::Vm;
        use std::sync::{Arc, Mutex};

        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l| log2.lock().unwrap().push(l));

        // __gc is a real closure (captures `tag`) and reads the dying table's
        // own fields: both must still be alive when the finalizer runs.
        run_with_vm(
            r#"seen = none
let tag = "tagged"
gcobj = setmetatable({ payload = {1, 2, 3} }, { __gc = fn(self) { seen = tag .. ":" .. #self.payload } })"#,
            &mut vm,
        ).unwrap();
        run_with_vm("gcobj = none", &mut vm).unwrap();
        vm.gc_collect();
        run_with_vm("print(seen)", &mut vm).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["tagged:3"]);
    }

    #[test]
    fn gc_finalizer_runs_once_per_object_with_shared_metatable() {
        use vm::Vm;
        use std::sync::{Arc, Mutex};

        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l| log2.lock().unwrap().push(l));

        run_with_vm(
            r#"count = 0
let mt = { __gc = fn(self) { count = count + 1 } }
objs = {}
for i = 1, 5 { objs[i] = setmetatable({}, mt) }"#,
            &mut vm,
        ).unwrap();
        run_with_vm("objs = none", &mut vm).unwrap();
        vm.gc_collect();
        vm.gc_collect();
        run_with_vm("print(count)", &mut vm).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["5"]);
    }

    #[test]
    fn gc_nested_collection_inside_a_finalizer_keeps_pending_finalizers_alive() {
        use vm::Vm;
        use std::sync::{Arc, Mutex};

        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l| log2.lock().unwrap().push(l));
        vm.gc.threshold = 8;

        // Each finalizer allocates well past the threshold, so a collection
        // runs while the other finalizable tables are still queued.
        run_with_vm(
            r#"count = 0
let mt = { __gc = fn(self) {
    var junk = {}
    for i = 1, 64 { junk[i] = { i } }
    count = count + #self
} }
objs = {}
for i = 1, 8 { objs[i] = setmetatable({ i }, mt) }"#,
            &mut vm,
        ).unwrap();
        run_with_vm("objs = none", &mut vm).unwrap();
        vm.gc_collect();
        run_with_vm("print(count)", &mut vm).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["8"]);
    }

    #[test]
    fn gc_host_stack_values_are_roots() {
        use std::ffi::{CStr, CString};

        let U = api::umbra_newstate();
        let s = CString::new("umbra_host_stack_root_string_xyz").unwrap();
        unsafe { api::umbra_pushstring(U, s.as_ptr()) };
        unsafe { api::umbra_gc_collect(U) };

        let ptr = unsafe { api::umbra_tostring(U, -1) };
        assert_eq!(unsafe { CStr::from_ptr(ptr) }.to_str().unwrap(), "umbra_host_stack_root_string_xyz");

        // Still interned: pushing the same content again must not allocate.
        let before = unsafe { api::umbra_gc_livecount(U) };
        unsafe { api::umbra_pushstring(U, s.as_ptr()) };
        assert_eq!(unsafe { api::umbra_gc_livecount(U) }, before);

        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn gc_string_methods_survive_clearing_the_string_global() {
        let mut vm = vm::Vm::new();
        run_with_vm("string = none", &mut vm).unwrap();
        vm.gc_collect();
        run_with_vm(r#"assert(("abc"):len() == 3)"#, &mut vm).unwrap();
    }

    #[test]
    fn gc_cached_modules_are_roots() {
        let mod_name = "umbra_test_gc_module_root_xyz";
        let path = format!("{mod_name}.umbra");
        std::fs::write(&path, "return { answer = 42 }").unwrap();

        let mut vm = vm::Vm::new();
        let first = run_with_vm(&format!("require(\"{mod_name}\")"), &mut vm);
        vm.gc_collect();
        let second = run_with_vm(&format!("assert(require(\"{mod_name}\").answer == 42)"), &mut vm);
        std::fs::remove_file(&path).ok();

        first.unwrap();
        second.unwrap();
    }

    #[test]
    fn gc_dead_coroutines_release_their_registers() {
        use std::ffi::CString;

        let U = api::umbra_newstate();
        let src = CString::new(
            "var co = coroutine.create(fn() {
                let junk = {}
                for i = 1, 200 { junk[i] = {} }
            })
            coroutine.resume(co)
            co = none"
        ).unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, src.as_ptr()) }, 0);
        unsafe { api::umbra_gc_collect(U) };
        let live = unsafe { api::umbra_gc_livecount(U) };
        assert!(live < 200, "finished coroutine still roots its garbage: {live} live objects");
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn gc_temporaries_in_call_arguments_are_roots() {
        // Argument registers beyond the last alloc_reg'd slot were not
        // counted in max_regs, so a collection between LoadK and Call could
        // free a string that was about to be passed.
        let mut vm = vm::Vm::new();
        vm.gc.threshold = 1;
        run_with_vm(
            r#"fn f(a, b, c, d, e) { return a .. b .. c .. d .. e }
for i = 1, 200 {
    assert(f("aa" .. i, "bb" .. i, "cc" .. i, "dd" .. i, "ee" .. i) == "aa" .. i .. "bb" .. i .. "cc" .. i .. "dd" .. i .. "ee" .. i)
}"#,
            &mut vm,
        ).unwrap();
    }

    #[test]
    fn for_in_over_script_closure_iterator() {
        assert_output(
            r#"fn range(n) {
    var i = 0
    return fn() { i = i + 1; if i <= n { return i } }
}
for v in range(3) { print(v) }
fn step(state, ctrl) { if ctrl < state { return ctrl + 1, ctrl * 10 } }
for i, v in step, 3, 0 { print(i, v) }"#,
            &["1", "2", "3", "1\t0", "2\t10", "3\t20"],
        );
    }

    #[test]
    fn gc_varargs_are_roots() {
        use vm::Vm;
        use std::sync::{Arc, Mutex};
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l| log2.lock().unwrap().push(l));
        run_with_vm(
            r#"finalized = 0
let mt = { __gc = fn(self) { finalized = finalized + 1 } }
fn f(...) {
    var junk = {}
    for i = 1, 3000 { junk[i] = {i} }
    junk = none
    let a, b = ...
    print(a.x)
}
f(setmetatable({x = 7}, mt), setmetatable({x = 8}, mt))
print(finalized)"#,
            &mut vm,
        ).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["7", "0"]);
    }

    #[test]
    fn gc_frame_top_results_are_roots() {
        use vm::Vm;
        use std::sync::{Arc, Mutex};
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l| log2.lock().unwrap().push(l));
        vm.gc.threshold = 1;
        run_with_vm(
            r#"finalized = 0
let mt = { __gc = fn(self) { finalized = finalized + 1 } }
fn many() {
    return setmetatable({}, mt), setmetatable({}, mt), setmetatable({}, mt),
           setmetatable({}, mt), setmetatable({}, mt), setmetatable({}, mt),
           setmetatable({}, mt), setmetatable({}, mt), setmetatable({}, mt),
           setmetatable({}, mt), setmetatable({}, mt), setmetatable({}, mt)
}
print(many())
print(finalized)"#,
            &mut vm,
        ).unwrap();
        let lines = log.lock().unwrap().clone();
        assert_eq!(lines.last().unwrap(), "0", "live call results were finalized early: {lines:?}");
    }

    #[test]
    fn gc_weak_key_entry_whose_value_keeps_its_key_is_collected() {
        use vm::Vm;
        use std::sync::{Arc, Mutex};
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l| log2.lock().unwrap().push(l));
        run_with_vm(
            r#"wt = setmetatable({}, { __mode = "k" })
var k = {}
wt[k] = { owner = k }
k = none"#,
            &mut vm,
        ).unwrap();
        vm.gc_collect();
        run_with_vm(
            "var n = 0
for k, v in pairs(wt) { n = n + 1 }
print(n)",
            &mut vm,
        ).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["0"]);
    }

    #[test]
    fn gc_finalizers_queued_during_a_drain_run_on_later_collects() {
        use vm::Vm;
        use std::sync::{Arc, Mutex};
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l| log2.lock().unwrap().push(l));
        vm.gc.threshold = 4;
        // Each finalizer drops four more finalizable tables into a weak-key
        // table and allocates past the threshold, forcing nested collections
        // mid-drain. Those collections must not recurse into the finalizer
        // drain; the queued finalizers run on subsequent collects instead.
        // (mt/wt are globals: a `let mt = {__gc = ...}` local is not in scope
        // inside its own initializer, so the closure would see global `mt`.)
        run_with_vm(
            r#"depth = 0
made = 0
wt = setmetatable({}, { __mode = "k" })
mt = { __gc = fn(self) {
    depth = depth + 1
    if made < 12 {
        for i = 1, 4 {
            wt[setmetatable({}, mt)] = 1
            made = made + 1
        }
        var junk = {}
        for i = 1, 3000 { junk[i] = {i} }
    }
} }
var t = setmetatable({}, mt)
t = none"#,
            &mut vm,
        ).unwrap();
        for _ in 0..8 { vm.gc_collect(); }
        run_with_vm("print(depth, made)", &mut vm).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["13\t12"]);
    }



    #[test]
    fn gc_varargs_survive_collection_inside_pcall() {
        use vm::Vm;
        use std::sync::{Arc, Mutex};
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l| log2.lock().unwrap().push(l));
        run_with_vm(
            r#"finalized = 0
let mt = { __gc = fn(self) { finalized = finalized + 1 } }
fn f(...) {
    pcall(fn() {
        var junk = {}
        for i = 1, 3000 { junk[i] = {i} }
    })
    let a = ...
    print(a.x)
}
f(setmetatable({x = 7}, mt))
print(finalized)"#,
            &mut vm,
        ).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["7", "0"]);
    }


    #[test]
    fn gc_finalizer_can_resurrect_its_object() {
        use vm::Vm;
        use std::sync::{Arc, Mutex};
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l| log2.lock().unwrap().push(l));
        run_with_vm(
            r#"saved = none
var t = setmetatable({ x = 42 }, { __gc = fn(self) { saved = self } })
t = none"#,
            &mut vm,
        ).unwrap();
        vm.gc_collect();
        vm.gc_collect();
        run_with_vm("print(saved.x)", &mut vm).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["42"]);
    }

    #[test]
    fn gc_suspended_coroutine_varargs_are_roots() {
        use vm::Vm;
        use std::sync::{Arc, Mutex};
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l| log2.lock().unwrap().push(l));
        run_with_vm(
            r#"finalized = 0
let mt = { __gc = fn(self) { finalized = finalized + 1 } }
co = coroutine.create(fn(...) {
    yield()
    let a = ...
    print(a.x)
})
coroutine.resume(co, setmetatable({x = 9}, mt))"#,
            &mut vm,
        ).unwrap();
        vm.gc_collect();
        run_with_vm("coroutine.resume(co)\nprint(finalized)", &mut vm).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["9", "0"]);
    }

    #[test]
    fn gc_collects_cycles_through_metatables_and_upvalues() {
        use vm::Vm;
        let mut vm = Vm::new();
        run_with_vm(
            r#"var t = {}
var mt = { __index = t }
setmetatable(t, mt)
t.self = t
mt.back = t
var f = (fn() {
    let cell = {}
    let g = fn() { return cell }
    cell.owner = g
    return g
})()
t = none
mt = none
f = none"#,
            &mut vm,
        ).unwrap();
        vm.gc_collect();
        let after = vm.gc.live_count();
        vm.gc_collect();
        assert_eq!(vm.gc.live_count(), after);
        run_with_vm("x = 1", &mut vm).unwrap();
        vm.gc_collect();
        let baseline = vm.gc.live_count();
        assert!(after <= baseline + 4, "cycle leaked: after={after} baseline={baseline}");
    }

    #[test]
    fn gc_stress_leaves_no_live_garbage() {
        use vm::Vm;
        let mut vm = Vm::new();
        run_with_vm("x = 1", &mut vm).unwrap();
        vm.gc_collect();
        let baseline = vm.gc.live_count();
        run_with_vm(
            r#"for i = 1, 100000 {
    let t = { i, "s" .. i, { nested = i } }
}"#,
            &mut vm,
        ).unwrap();
        vm.gc_collect();
        vm.gc_collect();
        let after = vm.gc.live_count();
        assert!(after <= baseline + 8, "leak: baseline {baseline} after {after}");
    }

    #[test]
    fn table_float_keys_at_i64_boundary() {
        // 2^63-1 and 2^63 are the same f64, so both literals hit one key —
        // matches Lua 5.4's float-key normalization.
        assert_output(
            r#"let t = {}
t[9223372036854775807.0] = "a"
t[9223372036854775808.0] = "b"
t[-9223372036854775808.0] = "c"
t[-9223372036854775809.0] = "d"
t[0.0] = "z"
print(t[9223372036854775807.0], t[9223372036854775808.0], t[-9223372036854775808.0], t[-9223372036854775809.0], t[0])
print(t[2.5], t[-0.0])"#,
            &["b\tb\td\td\tz", "nil\tz"],
        );
    }

    #[test]
    fn gc_unpack_spill_results_are_roots() {
        use vm::Vm;
        use std::sync::{Arc, Mutex};
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l| log2.lock().unwrap().push(l));
        vm.gc.threshold = 1;
        run_with_vm(
            r#"finalized = 0
let mt = { __gc = fn(self) { finalized = finalized + 1 } }
fn mk() {
    let t = {}
    for i = 1, 60 { t[i] = setmetatable({}, mt) }
    return t
}
print(unpack(mk()))
print(finalized)"#,
            &mut vm,
        ).unwrap();
        let lines = log.lock().unwrap().clone();
        assert_eq!(lines.last().unwrap(), "0", "live call results were finalized early: {lines:?}");
    }

    #[test]
    fn index_assignment_does_not_clobber_live_locals() {
        assert_output(
            r#"let t = {}
let k = "a"
t[k] = 1
print(k, t.a)"#,
            &["a\t1"],
        );
    }

    #[test]
    fn method_call_results_land_in_order() {
        assert_output(
            r#"let o = { m = fn(self) { return 10, 20 } }
let a, b = o:m()
print(a, b)
var calls = 0
fn f() { calls = calls + 1; return 1 }
print(o:m() + f())
print(calls)"#,
            &["10\t20", "11", "1"],
        );
    }

    #[test]
    fn table_constructor_past_fifty_positional_fields() {
        let items: Vec<String> = (1..=120).map(|i| i.to_string()).collect();
        assert_output(
            &format!("let t = {{ {} }}\nprint(#t, t[50], t[51], t[100], t[101], t[120])", items.join(", ")),
            &["120\t50\t51\t100\t101\t120"],
        );
    }

    #[test]
    fn many_constants_still_compile_correctly() {
        // More string constants than the 7-bit RK operand can address: the
        // extras must be loaded through a register rather than aliased.
        let mut src = String::from("let t = {}\n");
        for i in 1..=300 { src.push_str(&format!("t.k{i} = {i}\n")); }
        src.push_str("print(t.k1, t.k129, t.k200, t.k300)");
        assert_output(&src, &["1\t129\t200\t300"]);
    }

    #[test]
    fn too_many_registers_is_a_compile_error() {
        let mut src = String::new();
        for i in 1..=140 { src.push_str(&format!("let v{i} = {i}\n")); }
        assert!(run(&src).unwrap_err().contains("registers"));
    }

    #[test]
    fn trailing_call_returns_and_passes_all_values() {
        assert_output(
            r##"fn two() { return 1, 2 }
fn pass() { return two() }
fn count(...) { return select("#", ...) }
print(pass())
print(count(two()))
print(count(two(), two()))
let t = { m = fn(self) { return 7, 8 } }
fn viam() { return t:m() }
print(viam())"##,
            &["1\t2", "2", "3", "7\t8"],
        );
    }

    #[test]
    fn close_runs_on_break_continue_and_return() {
        assert_output(
            r#"fn res(name) { return { close = fn(self) { print("close " .. name) } } }
fn early() {
    let a <close> = res("ret")
    return 1
}
print(early())
for i = 1, 2 {
    let b <close> = res("loop" .. i)
    if i == 1 { continue }
    break
}"#,
            &["close ret", "1", "close loop1", "close loop2"],
        );
    }

    #[test]
    fn goto_into_local_scope_is_rejected() {
        assert!(run("goto skip\nlet x = 1\n::skip::\nprint(x)").unwrap_err().contains("scope"));
        assert_output("var n = 0\n::top::\nn = n + 1\nif n < 3 { goto top }\nprint(n)", &["3"]);
    }

    #[test]
    fn integer_division_by_zero_is_an_error() {
        assert!(run("let x = 1 // 0").unwrap_err().contains("n//0"));
        assert!(run("let x = 1 % 0").unwrap_err().contains("n%0"));
        assert_output("print(1.0 // 0)", &["inf"]);
    }

    #[test]
    fn shifts_follow_lua_semantics() {
        assert_output(
            "print(1 << 64, 1 << -1, 8 >> -2, -1 >> 63, 1 << 63)",
            &["0\t0\t32\t1\t-9223372036854775808"],
        );
    }

    #[test]
    fn bitwise_ops_reject_non_integral_floats() {
        assert_output("print(2.0 & 3)", &["2"]);
        assert!(run("let x = 1.5 & 2").unwrap_err().contains("integer representation"));
    }

    #[test]
    fn numeric_for_with_zero_step_is_an_error() {
        assert!(run("for i = 1, 10, 0 { }").unwrap_err().contains("step is zero"));
        assert!(run("for i = 1, \"x\" { }").unwrap_err().contains("limit"));
    }

    #[test]
    fn vararg_expansion_grows_the_register_file() {
        assert_output(
            r##"fn f(...) { let t = {...} return #t, select("#", ...) }
let big = {}
for i = 1, 250 { big[i] = i }
print(f(table.unpack(big)))"##,
            &["250\t250"],
        );
    }

    #[test]
    fn mixed_int_float_comparison_is_exact() {
        assert_output(
            "print(math.maxinteger < 9223372036854775808.0, math.maxinteger == 9223372036854775808.0, -0.0 == 0.0, 1 == 1.0)",
            &["true\tfalse\ttrue\ttrue"],
        );
        assert_output(
            "let t = {}\nt[math.maxinteger] = \"int\"\nt[9223372036854775808.0] = \"float\"\nprint(t[math.maxinteger])",
            &["int"],
        );
    }

    #[test]
    fn select_negative_and_zero_indices() {
        assert_output("print(select(-1, 1, 2, 3))", &["3"]);
        assert_output("print(select(-2, 1, 2, 3))", &["2\t3"]);
        assert!(run("select(0, 1)").is_err());
    }

    #[test]
    fn rawequal_compares_numbers_by_value() {
        assert_output("print(rawequal(1, 1.0), rawequal(-0.0, 0.0), rawequal({}, {}))", &["true\ttrue\tfalse"]);
    }

    #[test]
    fn table_sort_keeps_contents_when_comparator_errors() {
        assert_output(
            r#"let t = {3, 1, 2}
let ok = pcall(table.sort, t, fn(a, b) { error("boom") })
print(ok, #t, t[1] + t[2] + t[3])"#,
            &["false\t3\t6"],
        );
    }

    #[test]
    fn table_remove_out_of_range_leaves_table_alone() {
        assert_output("let t = {1, 2, 3}\nprint(table.remove(t, 7), #t)", &["nil\t3"]);
    }

    #[test]
    fn string_find_plain_inside_multibyte_text() {
        assert_output(r#"print(string.find("héllo", "x", 3, true))"#, &["nil"]);
        assert_output(r#"print(string.find("héllo", "llo", 3, true))"#, &["4\t6"]);
    }

    #[test]
    fn anchored_gsub_and_gmatch_match_once() {
        assert_output(r#"print(string.gsub("aaa", "^a", "b"))"#, &["baa\t1"]);
        assert_output(r#"var n = 0
for m in string.gmatch("aaa", "^a") { n = n + 1 }
print(n)"#, &["1"]);
    }

    #[test]
    fn utf8_codes_rejects_positions_inside_a_character() {
        assert_output(r#"var n = 0
for p, c in utf8.codes("héllo") { n = n + 1 }
print(n)"#, &["5"]);
        assert!(run(r#"let it = utf8.codes("héllo")
it("héllo", 3)"#).is_err());
    }

    #[test]
    fn tonumber_handles_hex_and_bases() {
        assert_output(r#"print(tonumber("0x10"), tonumber("ff", 16), tonumber("z", 36), tonumber("12", 2))"#, &["16\t255\t35\tnil"]);
    }

    #[test]
    fn math_floor_of_huge_float_stays_float() {
        assert_output("print(math.floor(1e300) == 1e300, math.floor(-2.5), math.ceil(2.5))", &["true\t-3\t3"]);
    }

    #[test]
    fn math_random_near_integer_limits() {
        assert_output(
            "let r = math.random(math.maxinteger - 1, math.maxinteger)\nprint(r >= math.maxinteger - 1 and r <= math.maxinteger)",
            &["true"],
        );
    }

    #[test]
    fn string_format_extra_specifiers_and_tostring() {
        assert_output(
            r#"let o = setmetatable({}, { __tostring = fn() { return "obj" } })
print(string.format("%s|%c|%o|é%d", o, 65, 8, 1))"#,
            &["obj|A|10|é1"],
        );
    }

    #[test]
    fn yield_inside_pcall_is_a_clean_error() {
        assert_output(
            r#"let co = coroutine.create(fn() {
    let ok, err = pcall(fn() { yield(1) })
    print(ok, err)
    print(coroutine.isyieldable())
})
coroutine.resume(co)
print(coroutine.isyieldable())"#,
            &["false\tattempt to yield across a C-call boundary", "true", "false"],
        );
    }

    #[test]
    fn require_rejects_path_traversal() {
        let err = run("require(\"..secret\")").unwrap_err();
        assert!(err.contains("invalid module name"), "{err}");
        assert!(run("require(\"a/b\")").unwrap_err().contains("invalid module name"));
    }

    #[test]
    fn api_pcall_keeps_closure_upvalues() {
        use std::ffi::CString;

        let U = api::umbra_newstate();
        let src = CString::new("let base = 40\nfn add(n) { return base + n }").unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, src.as_ptr()) }, 0);

        let name = CString::new("add").unwrap();
        unsafe { api::umbra_getglobal(U, name.as_ptr()) };
        assert_eq!(unsafe { api::umbra_isfunction(U, -1) }, 1);
        unsafe { api::umbra_pushinteger(U, 2) };
        assert_eq!(unsafe { api::umbra_pcall(U, 1, 1) }, 0);
        assert_eq!(unsafe { api::umbra_tointeger(U, -1) }, 42);

        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn api_pcall_rejects_negative_nargs() {
        use std::ffi::CStr;

        let U = api::umbra_newstate();
        assert_ne!(unsafe { api::umbra_pcall(U, -1, 0) }, 0);
        let msg = unsafe { CStr::from_ptr(api::umbra_tostring(U, -1)) }.to_str().unwrap();
        assert!(msg.contains("negative"), "{msg}");
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn api_pop_removes_exactly_n_values() {
        let U = api::umbra_newstate();
        for i in 0..5 { unsafe { api::umbra_pushinteger(U, i) }; }
        unsafe { api::umbra_pop(U, 2) };
        assert_eq!(unsafe { api::umbra_gettop(U) }, 3);
        assert_eq!(unsafe { api::umbra_tointeger(U, -1) }, 2);
        unsafe { api::umbra_pop(U, 0) };
        assert_eq!(unsafe { api::umbra_gettop(U) }, 3);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn api_null_strings_are_harmless() {
        let U = api::umbra_newstate();
        unsafe { api::umbra_pushstring(U, std::ptr::null()) };
        assert_eq!(unsafe { api::umbra_isnil(U, -1) }, 1);
        unsafe { api::umbra_getglobal(U, std::ptr::null()) };
        assert_eq!(unsafe { api::umbra_isnil(U, -1) }, 1);
        assert_ne!(unsafe { api::umbra_dostring(U, std::ptr::null()) }, 0);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn lex_unterminated_block_comment_is_an_error() {
        use lexer::Lexer;
        assert!(Lexer::tokenize("/* never closed").is_err());
        assert!(Lexer::tokenize("1 /* ok */ 2 /* never closed").is_err());
        assert!(Lexer::tokenize("/* ok */ 42").is_ok());
    }

    #[test]
    fn lex_string_escape_edge_cases() {
        use lexer::{Lexer, TokenKind};
        // CRLF and lone-CR line continuations both fold to a single \n.
        let toks = Lexer::tokenize("\"a\\\r\nb\"").unwrap();
        assert!(matches!(&toks[0].kind, TokenKind::String(s) if s == "a\nb"));
        let toks = Lexer::tokenize("\"a\\\rb\"").unwrap();
        assert!(matches!(&toks[0].kind, TokenKind::String(s) if s == "a\nb"));
        // \z skips whitespace including newlines; \u{...} encodes UTF-8;
        // \$ escapes the interpolation sigil.
        let toks = Lexer::tokenize("\"a\\z\n   b\"").unwrap();
        assert!(matches!(&toks[0].kind, TokenKind::String(s) if s == "ab"));
        let toks = Lexer::tokenize("\"\\u{48}\\u{49}\"").unwrap();
        assert!(matches!(&toks[0].kind, TokenKind::String(s) if s == "HI"));
        let toks = Lexer::tokenize("\"\\${x}\"").unwrap();
        assert!(matches!(&toks[0].kind, TokenKind::String(s) if s == "${x}"));
        assert!(Lexer::tokenize("\"\\u{}\"").is_err());
        assert!(Lexer::tokenize("\"\\u{110000}\"").is_err());
        assert!(Lexer::tokenize("\"\\q\"").is_err());
    }

    #[test]
    fn lex_number_edge_cases() {
        use lexer::{Lexer, TokenKind};
        let toks = Lexer::tokenize("0xA.8p1").unwrap();
        assert!(matches!(toks[0].kind, TokenKind::Float(f) if f == 21.0));
        let toks = Lexer::tokenize("0x1p4").unwrap();
        assert!(matches!(toks[0].kind, TokenKind::Float(f) if f == 16.0));
        // Hex literals wrap mod 2^64 like Lua.
        let toks = Lexer::tokenize("0xFFFFFFFFFFFFFFFF").unwrap();
        assert!(matches!(toks[0].kind, TokenKind::Int(-1)));
        // Decimal literals past i64 become floats.
        let toks = Lexer::tokenize("9223372036854775808").unwrap();
        assert!(matches!(toks[0].kind, TokenKind::Float(_)));
        // Trailing dot is a float, but `..` still lexes as concat.
        let toks = Lexer::tokenize("5.").unwrap();
        assert!(matches!(toks[0].kind, TokenKind::Float(f) if f == 5.0));
        let toks = Lexer::tokenize("5..2").unwrap();
        assert!(matches!(toks[0].kind, TokenKind::Int(5)));
        assert!(matches!(toks[1].kind, TokenKind::DotDot));
        assert!(matches!(toks[2].kind, TokenKind::Int(2)));
    }

    #[test]
    fn parse_ternary_allows_ident_and_call_branches() {
        assert_output(
            "let x = 5\nlet big = \"B\"\nlet small = \"S\"\nprint(x > 3 ? big : small)",
            &["B"],
        );
        assert_output(
            "let t = {m = fn(self) { return \"M\" }}\nprint(true ? t:m() : \"no\")",
            &["M"],
        );
        assert_output("print(false ? 1 : true ? 2 : 3)", &["2"]);
        // `:` after a field is the separator even when a call follows;
        // `:` after a method call is too.
        assert_output(
            "let t = {b = 1, m = fn(self) { return \"M\" }}\nfn c(n) { return n * 10 }\nprint(5 > 0 ? t.b : c(1))\nprint(5 > 0 ? t:m() : c(1))",
            &["1", "M"],
        );
    }

    #[test]
    fn parse_deep_nesting_errors_instead_of_overflowing() {
        let parens = format!("print({}1{})", "(".repeat(5000), ")".repeat(5000));
        assert!(run(&parens).is_err());
        let chain = format!("let x = {}1", "1+".repeat(5000));
        assert!(run(&chain).is_err());
        let blocks = format!("if true {{ {} print(1) {}", "{ ".repeat(5000), "}".repeat(5000));
        assert!(run(&blocks).is_err());
        // Reasonable nesting still works.
        assert_output(
            &format!("print({}1{})", "(".repeat(50), ")".repeat(50)),
            &["1"],
        );
    }

    #[test]
    fn parse_interp_rejects_trailing_tokens() {
        assert!(run("print(\"${a b}\")").is_err());
        assert!(run("print(\"${{1,2}[1]}\")").is_err());
        // Interpolated strings work as call arguments too.
        assert_output(
            "fn f(s) { return s }\nlet n = \"x\"\nprint(f\"${n}\")",
            &["x"],
        );
    }

    #[test]
    fn api_reentrant_dostring_from_a_registered_function() {
        use std::ffi::CString;

        unsafe extern "C" fn reenter(U: *mut api::UmbraState) -> std::ffi::c_int {
            let src = CString::new("inner = inner + 1").unwrap();
            unsafe { api::umbra_dostring(U, src.as_ptr()) };
            unsafe { api::umbra_pushinteger(U, 7) };
            1
        }

        let U = api::umbra_newstate();
        let name = CString::new("reenter").unwrap();
        unsafe { api::umbra_register(U, name.as_ptr(), Some(reenter)) };
        let src = CString::new(
            "inner = 0
            fn outer(x) { let y = x * 2; let r = reenter(); return y + r }
            assert(outer(5) == 17)
            assert(inner == 1)"
        ).unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, src.as_ptr()) }, 0);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn api_register_result_count_is_clamped_to_the_frame() {
        use std::ffi::CString;

        unsafe extern "C" fn greedy(U: *mut api::UmbraState) -> std::ffi::c_int {
            unsafe { api::umbra_pushinteger(U, 1) };
            10
        }

        let U = api::umbra_newstate();
        unsafe { api::umbra_pushinteger(U, 99) };
        let name = CString::new("greedy").unwrap();
        unsafe { api::umbra_register(U, name.as_ptr(), Some(greedy)) };
        let src = CString::new("let a, b, c, d = greedy(5, 6)\nassert(a == 5 and b == 6 and c == 1 and d == none)").unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, src.as_ptr()) }, 0);
        assert_eq!(unsafe { api::umbra_gettop(U) }, 1);
        assert_eq!(unsafe { api::umbra_tointeger(U, 1) }, 99);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn failed_chunk_leaves_no_stale_frames() {
        let mut vm = vm::Vm::new();
        assert!(run_with_vm("fn f() { error(\"x\") }\nf()", &mut vm).is_err());
        let mut vm2 = vm;
        run_with_vm("assert(1 + 1 == 2)", &mut vm2).unwrap();
        run_with_vm("assert(1 + 2 == 3)", &mut vm2).unwrap();
    }

    #[test]
    fn lex_leading_dot_float_and_unterminated_long_string() {
        use lexer::Lexer;
        let toks = Lexer::tokenize(".5").unwrap();
        assert!(matches!(toks[0].kind, lexer::TokenKind::Float(f) if f == 0.5));
        assert!(Lexer::tokenize("[[abc").is_err());
    }

    #[test]
    fn parse_xor_binds_tighter_than_or() {
        assert_output("print(1 | 2 ~ 2)", &["1"]);
        assert_output("print(2 ~ 3 & 1)", &["3"]);
    }

    #[test]
    fn parse_statements_after_return_are_an_error() {
        assert!(run("fn f() { return 1\nprint(2) }").is_err());
        assert!(run("return 1\nprint(2)").is_err());
        assert!(run("}").is_err());
        assert!(run("if true { } else }").is_err());
    }

    #[test]
    fn pack_rejects_out_of_range_and_huge_padding() {
        assert!(run("string.pack(\"B\", -1)").unwrap_err().contains("overflow"));
        assert!(run("string.pack(\"I2\", 70000)").unwrap_err().contains("overflow"));
        assert!(run("string.pack(\"b\", 200)").unwrap_err().contains("overflow"));
        assert!(run("string.pack(\"c999999999999\", \"x\")").unwrap_err().contains("too large"));
        assert_output(r#"print(string.unpack("B", string.pack("B", 255)))"#, &["255\t2"]);
    }

    #[test]
    fn api_setglobal_in_c_fn_does_not_steal_caller_stack() {
        use std::ffi::CString;

        // setglobal pops the value to store; with an empty C frame that pop
        // must not reach below api_base into the host's own stack.
        unsafe extern "C" fn setg(U: *mut api::UmbraState) -> std::ffi::c_int {
            let name = CString::new("g_from_c").unwrap();
            unsafe { api::umbra_setglobal(U, name.as_ptr()) };
            0
        }

        let U = api::umbra_newstate();
        unsafe { api::umbra_pushinteger(U, 777) };
        let name = CString::new("setg").unwrap();
        unsafe { api::umbra_register(U, name.as_ptr(), Some(setg)) };
        let src = CString::new("setg()").unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, src.as_ptr()) }, 0);
        assert_eq!(unsafe { api::umbra_gettop(U) }, 1);
        assert_eq!(unsafe { api::umbra_tointeger(U, 1) }, 777);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn api_dostring_reports_syntax_errors_as_syntax() {
        use std::ffi::CString;

        let U = api::umbra_newstate();
        let bad = CString::new("this is not valid umbra !!!").unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, bad.as_ptr()) },
                   api::UmbraStatus::SyntaxError as std::ffi::c_int);
        let good = CString::new("x = 1").unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, good.as_ptr()) }, 0);
        let boom = CString::new("error(\"boom\")").unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, boom.as_ptr()) },
                   api::UmbraStatus::RuntimeError as std::ffi::c_int);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn api_pcall_invokes_callable_tables() {
        use std::ffi::CString;

        let U = api::umbra_newstate();
        let src = CString::new(
            "t = setmetatable({}, {__call = fn(self, x) { return x * 2 }})"
        ).unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, src.as_ptr()) }, 0);
        let name = CString::new("t").unwrap();
        unsafe { api::umbra_getglobal(U, name.as_ptr()) };
        unsafe { api::umbra_pushinteger(U, 21) };
        assert_eq!(unsafe { api::umbra_pcall(U, 1, 1) }, 0);
        assert_eq!(unsafe { api::umbra_tointeger(U, -1) }, 42);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn api_cross_state_reentry_keeps_allocations_on_their_own_vm() {
        use std::ffi::{CStr, CString};

        // A registered fn that runs a script on a *second* state: without
        // CURRENT_VM save/restore, the outer script's later allocations are
        // registered on the inner VM's GC and dangle after it collects.
        static mut INNER: *mut api::UmbraState = std::ptr::null_mut();
        unsafe extern "C" fn cross(U: *mut api::UmbraState) -> std::ffi::c_int {
            let src = CString::new("q = 1").unwrap();
            unsafe { api::umbra_dostring(INNER, src.as_ptr()) };
            unsafe { api::umbra_pushinteger(U, 1) };
            1
        }

        let outer = api::umbra_newstate();
        let inner = api::umbra_newstate();
        unsafe { INNER = inner };
        let name = CString::new("cross").unwrap();
        unsafe { api::umbra_register(outer, name.as_ptr(), Some(cross)) };
        let src = CString::new("v = cross()\ns = tostring(123)").unwrap();
        assert_eq!(unsafe { api::umbra_dostring(outer, src.as_ptr()) }, 0);
        // Inner state collects: it must not sweep the string the outer VM owns.
        unsafe { api::umbra_gc_collect(inner) };
        let gname = CString::new("s").unwrap();
        unsafe { api::umbra_getglobal(outer, gname.as_ptr()) };
        let s = unsafe { CStr::from_ptr(api::umbra_tostring(outer, -1)) };
        assert_eq!(s.to_str().unwrap(), "123");
        unsafe { api::umbra_close(inner) };
        unsafe { api::umbra_close(outer) };
    }
}
