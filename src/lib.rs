pub mod api;
pub mod ast;
pub mod chunk;
pub mod compiler;
pub mod embed;
pub mod gc;
mod heap;
pub mod lexer;
pub mod pack;
pub mod parser;
pub mod pattern;
pub mod value;
pub mod vm;

pub use compiler::compile;
pub use parser::parse;

pub fn run(src: &str) -> Result<(), String> {
    run_with_vm(src, &mut vm::Vm::new())
}

pub fn run_with_vm(src: &str, vm: &mut vm::Vm) -> Result<(), String> {
    let (block, parse_errs) = parse(src);
    if !parse_errs.is_empty() {
        return Err(parse_errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"));
    }
    let chunk = compile(block, None).map_err(|e| e.to_string())?;
    vm.exec(chunk).map(|_| ()).map_err(|e| e.to_string())
}

pub fn run_capture(src: &str) -> Result<Vec<String>, String> {
    use std::sync::{Arc, Mutex};
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let cap2 = captured.clone();

    let (block, parse_errs) = parse(src);
    if !parse_errs.is_empty() {
        return Err(parse_errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"));
    }
    let chunk = compile(block, None).map_err(|e| e.to_string())?;
    let mut vm = vm::Vm::new_with_print(move |line, _newline| {
        cap2.lock().unwrap().push(line.to_owned());
    });
    let result = vm.exec(chunk).map(|_| ()).map_err(|e| e.to_string());
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
        use ast::{Binop, Expr};
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
        for n in [0i64, 1, -1, 42, -42, i32::MAX as i64, i32::MIN as i64, (1i64 << 47) - 1, -(1i64 << 47)] {
            let v = Value::int(n);
            assert!(v.is_int(), "n={n}");
            assert_eq!(v.as_int(), Some(n), "n={n}");
        }
    }

    #[test]
    fn value_float_roundtrip() {
        use value::Value;
        for f in [0.0f64, 1.0, -1.0, 3.14, f64::INFINITY, f64::NEG_INFINITY, f64::MAX, f64::MIN_POSITIVE] {
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
        // Only the address goes through the box here; nothing dereferences it.
        let x: usize = 0x0000_DEAD_BEEF_0010;
        let ptr = std::ptr::without_provenance_mut::<u8>(x);
        let v = Value::table(ptr);
        assert!(v.is_table());
        assert_eq!(v.as_table().unwrap().addr(), x);
    }

    #[test]
    fn value_type_names() {
        use value::Value;
        assert_eq!(Value::nil().type_name(), "none");
        assert_eq!(Value::bool(true).type_name(), "boolean");
        assert_eq!(Value::int(0).type_name(), "integer");
        assert_eq!(Value::float(1.0).type_name(), "float");
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
        assert_output("print(1 + 2)", &["3"]);
        assert_output("print(10 - 3)", &["7"]);
        assert_output("print(2 * 6)", &["12"]);
        assert_output("print(7 / 2)", &["3.5"]);
        assert_output("print(10 % 3)", &["1"]);
        assert_output("print(2 ^ 10)", &["1024.0"]);
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
    fn stdlib_string_format_rejects_huge_width() {
        // Lua's get2digits caps width/precision at two digits.
        assert!(run("string.format(\"%999999999999d\", 1)").is_err());
        assert!(run("string.format(\"%.100f\", 1.5)").is_err());
        assert_output(
            "print(string.format(\"%99d\", 1))",
            &["                                                                                                  1"],
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
        assert_eq!(e, "[string \"...\"]:3: boom");
    }

    #[test]
    fn vm_type_errors_carry_line_numbers() {
        let e = run("let x = none\nprint(1)\nlet y = x + 1").unwrap_err();
        assert!(e.starts_with("[string \"...\"]:3:"), "expected source:line prefix, got: {e}");
    }

    #[test]
    fn vm_error_level_2_attributes_to_caller_line() {
        let e = run("fn f() {\n    error(\"boom\", 2)\n}\nf()").unwrap_err();
        assert_eq!(e, "[string \"...\"]:4: boom");
    }

    #[test]
    fn vm_error_level_1_is_same_as_default() {
        let e = run("fn f() {\n    error(\"boom\", 1)\n}\nf()").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: boom");
    }

    #[test]
    fn vm_pcall_error_message_carries_line_number() {
        assert_output(
            "let ok, msg = pcall(fn() {
                print(\"a\")
                error(\"deep boom\")
            })
            print(msg)",
            &["a", "[string \"...\"]:3: deep boom"],
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
            &["false", "handled: [string \"...\"]:1: boom"],
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
        assert_output("let a, b = string.find(\"abc\", \"^a\")\nprint(a)\nprint(b)", &["1", "1"]);
        assert_output(r#"print(string.find("abc", "^b"))"#, &["none"]);
        assert_output("let a, b = string.find(\"abc\", \"c$\")\nprint(a)\nprint(b)", &["3", "3"]);
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
        assert_output("let a, b = string.match(\"hello\", \"()ll()\")\nprint(a)\nprint(b)", &["3", "5"]);
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

    // Stress-test finding 36: the print hook hears whether a newline
    // follows, so io.write keeps its no-newline semantics through it.
    #[test]
    fn the_print_hook_gets_the_newline_flag() {
        use std::sync::{Arc, Mutex};
        let log: Arc<Mutex<Vec<(String, bool)>>> = Arc::default();
        let log2 = log.clone();
        let mut vm = vm::Vm::new_with_print(move |text, newline| log2.lock().unwrap().push((text.to_owned(), newline)));
        run_with_vm("io.write(\"a\", 1)\nio.write(\"b\\n\")\nprint(\"c\", 2)\nio.write()", &mut vm).unwrap();
        let got = log.lock().unwrap().clone();
        assert_eq!(got, [("a1".to_owned(), false), ("b\n".to_owned(), false), ("c\t2".to_owned(), true), (String::new(), false),]);
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
        assert_output("print(os.getenv(\"UMBRA_DEFINITELY_UNSET_VAR_XYZ\"))", &["none"]);
    }

    #[test]
    fn os_date_from_script_with_explicit_epoch() {
        assert_output("print(os.date(\"%Y-%m-%d\", 0))", &["1970-01-01"]);
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
            }",
        )
        .unwrap();
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
    fn load_compiles_source_into_a_callable() {
        assert_output(
            r#"print(load("return 1+1")())
print(load("return ...")(7, 8))
print(type(load("let x = 1")))"#,
            &["2", "7\t8", "function"],
        );
    }

    #[test]
    fn load_returns_none_and_message_on_syntax_error() {
        assert_output(
            r#"let f, msg = load("syntax error here")
print(f, type(msg))"#,
            &["none\tstring"],
        );
    }

    #[test]
    fn load_chunkname_appears_in_error_lines() {
        assert_output(
            r#"let f = load("error('boom')", "mychunk")
print(pcall(f))"#,
            &["false\tmychunk:1: boom"],
        );
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
        assert_eq!(result.unwrap(), vec!["line one", "line two", "none"]);
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
            &["none", "true"],
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
            }",
        )
        .unwrap();
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
            &["15", "12", "24", "6.0", "3", "3", "256.0"],
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
        assert_eq!(vm::format_civil_time(0, "%Y-%m-%d %H:%M:%S").unwrap(), "1970-01-01 00:00:00");
        assert_eq!(vm::format_civil_time(1700000000, "%Y-%m-%d %H:%M:%S").unwrap(), "2023-11-14 22:13:20");
        assert_eq!(vm::format_civil_time(1000000000, "%Y-%m-%d %H:%M:%S").unwrap(), "2001-09-09 01:46:40");
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
        assert_output("print(7.5 // 2)", &["3.0"]);
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
        assert_output("var i = 0 var s = 0 while i < 5 { i = i + 1 s = s + i } print(s)", &["15"]);
    }

    #[test]
    fn vm_for_numeric() {
        assert_output("var s = 0 for i = 1, 10 { s = s + i } print(s)", &["55"]);
    }

    #[test]
    fn vm_for_step() {
        assert_output("var s = 0 for i = 0, 10, 2 { s = s + i } print(s)", &["30"]);
    }

    #[test]
    fn vm_function_call_and_return() {
        assert_output("let fn add(a, b) { return a + b } print(add(3, 4))", &["7"]);
    }

    #[test]
    fn vm_implicit_return() {
        assert_output("fn double(x) { x * 2 } print(double(21))", &["42"]);
    }

    #[test]
    fn vm_closure_shorthand() {
        assert_output("let sq = |x| x * x print(sq(9))", &["81"]);
    }

    #[test]
    fn vm_recursion() {
        assert_output("fn fact(n) { if n <= 1 { return 1 } return n * fact(n - 1) } print(fact(10))", &["3628800"]);
    }

    #[test]
    fn vm_tail_call_reuses_frame() {
        // 100k-deep tail recursion would overflow the 200-frame call stack
        // without tail calls.
        assert_output(
            "fn loop_(n, acc) { if n == 0 { return acc } return loop_(n - 1, acc + n) }
            print(loop_(100000, 0))",
            &["5000050000"],
        );
    }

    #[test]
    fn vm_tail_call_preserves_multi_results() {
        assert_output(
            "fn two() { return 7, 8 }
            fn f() { return two() }
            print(f())",
            &["7\t8"],
        );
    }

    #[test]
    fn vm_tail_call_through_cfn_and_method() {
        // Host-function tail calls still return through the reused frame.
        assert_output(
            "fn f() { return tostring(42) }
            print(f())",
            &["42"],
        );
        assert_output(
            "let o = { v = 3 }
            fn o:m(k) { return self.v + k }
            fn g() { return o:m(4) }
            print(g())",
            &["7"],
        );
    }

    #[test]
    fn vm_tail_call_runs_pending_close() {
        // A <close> local in scope makes `return f()` a normal call: f runs
        // first, then close() as g's frame unwinds.
        assert_output(
            "var log = {}
            fn f() { log[#log + 1] = \"f\" return 1 }
            fn g() { let c <close> = { close = fn() { log[#log + 1] = \"c\" } } return f() }
            g()
            print(table.concat(log, \",\"))",
            &["f,c"],
        );
    }

    #[test]
    fn vm_superinstructions_emitted() {
        let (block, errs) = parse("fn f(n) { if n < 2 { return n } return f(n - 1) }");
        assert!(errs.is_empty());
        let chunk = compile(block, None).unwrap();
        let dump = disasm(&chunk);
        assert!(dump.contains("LtI"), "{dump}");
        assert!(dump.contains("SubI"), "{dump}");
        assert!(dump.contains("TailCall"), "{dump}");
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
        assert_output("let t = {} t.x = 10 t.y = 20 print(t.x + t.y)", &["30"]);
    }

    #[test]
    fn vm_table_array() {
        assert_output("let t = {10, 20, 30} print(t[1] + t[2] + t[3])", &["60"]);
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
            print(le[1], le[2])
            print(be[1], be[2])",
            &["1\t0", "0\t1"],
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
            print(bytes.tostring(s))
            print(pos)"#,
            &["hi", "4"],
        );
    }

    #[test]
    fn stdlib_string_pack_fixed_string_pads_with_zeros() {
        assert_output(
            r#"let packed = string.pack("c5", "ab")
            print(#packed)"#,
            &["5"],
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
            &["1", "none"],
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
        assert_output("let fn make() { let fn f(x) { return x * 2 } return f } let g = make() print(g(21))", &["42"]);
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

        let src = CString::new(r#"let a = "gc_test_alpha" let b = "gc_test_beta" let c = a .. b"#).unwrap();
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
        // Computed strings (Vm::alloc_str) must be GC-tracked and freed.
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
    fn coroutine_wrap_accepts_host_functions() {
        // A host function can't yield, so it runs to completion on the first
        // resume and the coroutine ends dead.
        assert_output(
            r#"
let w = coroutine.wrap(print)
w("a", "b")
let co = coroutine.create(print)
print(coroutine.status(co))
print(coroutine.resume(co, "x"))
print(coroutine.status(co))
print(coroutine.resume(co))
let w2 = coroutine.wrap(tostring)
print(w2(42))
"#,
            &["a\tb", "suspended", "x", "true", "dead", "false\tcannot resume dead coroutine", "42"],
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
            &["true", "none"],
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
            &["hello", "none"],
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
        use std::sync::{Arc, Mutex};
        use vm::Vm;

        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();

        let mut vm = Vm::new_with_print(move |l, _| log2.lock().unwrap().push(l.to_owned()));

        run_with_vm(
            r#"gcfired = false
gcobj = setmetatable({}, { __gc = fn(self) { gcfired = true } })"#,
            &mut vm,
        )
        .unwrap();

        run_with_vm("gcobj = none", &mut vm).unwrap();

        vm.gc_collect();

        run_with_vm("print(gcfired)", &mut vm).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["true"]);
    }

    #[test]
    fn api_register_cfn() {
        use std::ffi::CString;
        use std::sync::{Arc, Mutex};

        static CALLED_WITH: std::sync::OnceLock<Arc<Mutex<Vec<f64>>>> = std::sync::OnceLock::new();
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
        assert_output(
            r#"let s, e = string.find("hello world", "world")
print(s, e)"#,
            &["7\t11"],
        );
        assert_output(r#"print(string.find("hello", "xyz"))"#, &["none"]);
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
        assert_output(
            r#"let s = "hello"
print(s:len())"#,
            &["5"],
        );
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
        assert_output(r#"print(math.sqrt(4.0))"#, &["2.0"]);
    }

    #[test]
    fn stdlib_math_type() {
        assert_output(r#"print(math.type(1))"#, &["integer"]);
        assert_output(r#"print(math.type(1.0))"#, &["float"]);
        assert_output(r#"print(math.type("x"))"#, &["none"]);
    }

    #[test]
    fn stdlib_table_insert() {
        assert_output(
            r#"let t = {1, 2, 3}
table.insert(t, 4)
print(#t, t[4])"#,
            &["4\t4"],
        );
        assert_output(
            r#"let t = {1, 2, 3}
table.insert(t, 2, 10)
print(t[1], t[2], t[3], t[4])"#,
            &["1\t10\t2\t3"],
        );
    }

    #[test]
    fn stdlib_table_remove() {
        assert_output(
            r#"let t = {10, 20, 30}
let v = table.remove(t)
print(v, #t)"#,
            &["30\t2"],
        );
        assert_output(
            r#"let t = {10, 20, 30}
let v = table.remove(t, 2)
print(v, t[1], t[2])"#,
            &["20\t10\t30"],
        );
    }

    #[test]
    fn stdlib_table_concat() {
        assert_output(
            r#"let t = {"a", "b", "c"}
print(table.concat(t, "-"))"#,
            &["a-b-c"],
        );
    }

    #[test]
    fn stdlib_table_sort() {
        assert_output(
            r#"let t = {3, 1, 4, 1, 5, 9, 2, 6}
table.sort(t)
print(t[1], t[2], t[3], t[8])"#,
            &["1\t1\t2\t9"],
        );
        assert_output(
            r#"let t = {3, 1, 2}
table.sort(t, fn(a, b) { a > b })
print(t[1], t[2], t[3])"#,
            &["3\t2\t1"],
        );
    }

    #[test]
    fn coroutine_resume_yield_values() {
        assert_output(
            r#"let co = coroutine.create(fn() {
    let x = yield(1)
    let y = yield(2)
    print(x, y)
})
coroutine.resume(co)
coroutine.resume(co, 10)
coroutine.resume(co, 20)
"#,
            &["10\t20"],
        );
    }

    #[test]
    fn gc_finalizer_closure_and_fields_survive_until_it_runs() {
        use std::sync::{Arc, Mutex};
        use vm::Vm;

        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l, _| log2.lock().unwrap().push(l.to_owned()));

        // __gc is a real closure (captures `tag`) and reads the dying table's
        // own fields: both must still be alive when the finalizer runs.
        run_with_vm(
            r#"seen = none
let tag = "tagged"
gcobj = setmetatable({ payload = {1, 2, 3} }, { __gc = fn(self) { seen = tag .. ":" .. #self.payload } })"#,
            &mut vm,
        )
        .unwrap();
        run_with_vm("gcobj = none", &mut vm).unwrap();
        vm.gc_collect();
        run_with_vm("print(seen)", &mut vm).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["tagged:3"]);
    }

    #[test]
    fn gc_finalizer_runs_once_per_object_with_shared_metatable() {
        use std::sync::{Arc, Mutex};
        use vm::Vm;

        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l, _| log2.lock().unwrap().push(l.to_owned()));

        run_with_vm(
            r#"count = 0
let mt = { __gc = fn(self) { count = count + 1 } }
objs = {}
for i = 1, 5 { objs[i] = setmetatable({}, mt) }"#,
            &mut vm,
        )
        .unwrap();
        run_with_vm("objs = none", &mut vm).unwrap();
        vm.gc_collect();
        vm.gc_collect();
        run_with_vm("print(count)", &mut vm).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["5"]);
    }

    #[test]
    fn gc_nested_collection_inside_a_finalizer_keeps_pending_finalizers_alive() {
        use std::sync::{Arc, Mutex};
        use vm::Vm;

        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l, _| log2.lock().unwrap().push(l.to_owned()));
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
        )
        .unwrap();
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
            co = none",
        )
        .unwrap();
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
        )
        .unwrap();
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
        use std::sync::{Arc, Mutex};
        use vm::Vm;
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l, _| log2.lock().unwrap().push(l.to_owned()));
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
        )
        .unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["7", "0"]);
    }

    #[test]
    fn gc_frame_top_results_are_roots() {
        use std::sync::{Arc, Mutex};
        use vm::Vm;
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l, _| log2.lock().unwrap().push(l.to_owned()));
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
        )
        .unwrap();
        let lines = log.lock().unwrap().clone();
        assert_eq!(lines.last().unwrap(), "0", "live call results were finalized early: {lines:?}");
    }

    #[test]
    fn gc_weak_key_entry_whose_value_keeps_its_key_is_collected() {
        use std::sync::{Arc, Mutex};
        use vm::Vm;
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l, _| log2.lock().unwrap().push(l.to_owned()));
        run_with_vm(
            r#"wt = setmetatable({}, { __mode = "k" })
var k = {}
wt[k] = { owner = k }
k = none"#,
            &mut vm,
        )
        .unwrap();
        vm.gc_collect();
        run_with_vm(
            "var n = 0
for k, v in pairs(wt) { n = n + 1 }
print(n)",
            &mut vm,
        )
        .unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["0"]);
    }

    #[test]
    fn gc_finalizers_queued_during_a_drain_run_on_later_collects() {
        use std::sync::{Arc, Mutex};
        use vm::Vm;
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l, _| log2.lock().unwrap().push(l.to_owned()));
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
        )
        .unwrap();
        for _ in 0..8 {
            vm.gc_collect();
        }
        run_with_vm("print(depth, made)", &mut vm).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["13\t12"]);
    }

    #[test]
    fn gc_varargs_survive_collection_inside_pcall() {
        use std::sync::{Arc, Mutex};
        use vm::Vm;
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l, _| log2.lock().unwrap().push(l.to_owned()));
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
        )
        .unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["7", "0"]);
    }

    #[test]
    fn gc_finalizer_can_resurrect_its_object() {
        use std::sync::{Arc, Mutex};
        use vm::Vm;
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l, _| log2.lock().unwrap().push(l.to_owned()));
        run_with_vm(
            r#"saved = none
var t = setmetatable({ x = 42 }, { __gc = fn(self) { saved = self } })
t = none"#,
            &mut vm,
        )
        .unwrap();
        vm.gc_collect();
        vm.gc_collect();
        run_with_vm("print(saved.x)", &mut vm).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["42"]);
    }

    #[test]
    fn gc_suspended_coroutine_varargs_are_roots() {
        use std::sync::{Arc, Mutex};
        use vm::Vm;
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l, _| log2.lock().unwrap().push(l.to_owned()));
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
        )
        .unwrap();
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
        )
        .unwrap();
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
    fn gc_byte_pacing_collects_large_dead_strings() {
        // A large live set pushes the object-count trigger past the number of
        // allocations a later phase makes, so only byte pacing can fire here:
        // 2000 discarded 4KB strings (~8MB) must be collected even though
        // 2000 < live_after_collect.
        use vm::Vm;
        let mut vm = Vm::new();
        run_with_vm(
            r#"keep = {}
for i = 1, 20000 { keep[i] = "k" .. i }
for i = 1, 2000 { let junk = string.rep("x", 4096) }"#,
            &mut vm,
        )
        .unwrap();
        let live = vm.gc.live_count();
        assert!(live < 21500, "dead large strings should have been collected, live={live}");
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
        )
        .unwrap();
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
            &["b\tb\td\td\tz", "none\tz"],
        );
    }

    #[test]
    fn gc_unpack_spill_results_are_roots() {
        use std::sync::{Arc, Mutex};
        use vm::Vm;
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let mut vm = Vm::new_with_print(move |l, _| log2.lock().unwrap().push(l.to_owned()));
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
        )
        .unwrap();
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
        assert_output(&format!("let t = {{ {} }}\nprint(#t, t[50], t[51], t[100], t[101], t[120])", items.join(", ")), &["120\t50\t51\t100\t101\t120"]);
    }

    #[test]
    fn many_constants_still_compile_correctly() {
        // More string constants than the 7-bit RK operand can address: the
        // extras must be loaded through a register rather than aliased.
        let mut src = String::from("let t = {}\n");
        for i in 1..=300 {
            src.push_str(&format!("t.k{i} = {i}\n"));
        }
        src.push_str("print(t.k1, t.k129, t.k200, t.k300)");
        assert_output(&src, &["1\t129\t200\t300"]);
    }

    #[test]
    fn too_many_registers_is_a_compile_error() {
        let mut src = String::new();
        for i in 1..=140 {
            src.push_str(&format!("let v{i} = {i}\n"));
        }
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
    fn close_runs_when_error_unwinds_scope() {
        // The pending error reaches close() as its second argument, and
        // nested scopes close innermost-first.
        assert_output(
            r#"fn res(name) { return { close = fn(self, e) { print("close " .. name .. " got " .. tostring(e)) } } }
let ok, err = pcall(fn() {
    let a <close> = res("a")
    let b <close> = res("b")
    error("x")
})
print(ok, err)"#,
            &["close b got [string \"...\"]:5: x", "close a got [string \"...\"]:5: x", "false\t[string \"...\"]:5: x"],
        );
    }

    #[test]
    fn close_error_replaces_pending_error() {
        // Lua 5.4: a close() that throws replaces the error being unwound.
        assert_output(
            r#"let ok, err = pcall(fn() {
    let a <close> = { close = fn(self, e) { error("close-fail") } }
    error("orig")
})
print(ok, err)"#,
            &["false\t[string \"...\"]:2: close-fail"],
        );
    }

    #[test]
    fn close_runs_before_error_reaches_host() {
        // No pcall at all: closes still run before the error surfaces.
        let mut vm = vm::Vm::new();
        let err = run_with_vm(
            r#"let f <close> = { close = fn(self, e) { seen = tostring(e) } }
error("toplevel")"#,
            &mut vm,
        )
        .unwrap_err();
        assert!(err.contains("toplevel"), "{err}");
        let key = vm.intern_pub("seen");
        let seen = vm.globals.raw_get(key);
        assert_eq!(vm.gc.str(seen), "[string \"...\"]:2: toplevel");
    }

    #[test]
    fn close_runs_when_coroutine_dies() {
        assert_output(
            r#"let co = coroutine.create(fn() {
    let r <close> = { close = fn(self, e) { print("closed " .. tostring(e)) } }
    error("co-boom")
})
print(coroutine.resume(co))"#,
            &["closed [string \"...\"]:3: co-boom", "false\t[string \"...\"]:3: co-boom"],
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
        assert_output("print(1 << 64, 1 << -1, 8 >> -2, -1 >> 63, 1 << 63)", &["0\t0\t32\t1\t-9223372036854775808"]);
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
        assert_output("let t = {}\nt[math.maxinteger] = \"int\"\nt[9223372036854775808.0] = \"float\"\nprint(t[math.maxinteger])", &["int"]);
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
    fn table_remove_out_of_range_errors() {
        // Lua: pos must satisfy 1 <= pos <= n+1 unless pos == n.
        assert!(run("let t = {1, 2, 3}\ntable.remove(t, 7)").is_err());
        assert!(run("let t = {1, 2, 3}\ntable.remove(t, 0)").is_err());
        assert_output("let t = {1, 2, 3}\nprint(table.remove(t, 4), #t)", &["none\t3"]);
        assert_output("print(table.remove({}), table.remove({}, 0))", &["none\tnone"]);
    }

    #[test]
    fn string_find_plain_inside_multibyte_text() {
        assert_output(r#"print(string.find("héllo", "x", 3, true))"#, &["none"]);
        assert_output(r#"print(string.find("héllo", "llo", 3, true))"#, &["4\t6"]);
    }

    #[test]
    fn anchored_gsub_and_gmatch_match_once() {
        assert_output(r#"print(string.gsub("aaa", "^a", "b"))"#, &["baa\t1"]);
        assert_output(
            r#"var n = 0
for m in string.gmatch("aaa", "^a") { n = n + 1 }
print(n)"#,
            &["1"],
        );
    }

    #[test]
    fn utf8_codes_skips_continuation_positions() {
        assert_output(
            r#"var n = 0
for p, c in utf8.codes("héllo") { n = n + 1 }
print(n)"#,
            &["5"],
        );
        // Like Lua's iter_aux: a control value inside a character skips
        // forward to the next character rather than erroring.
        assert_output(
            r#"let it = utf8.codes("héllo")
print(it("héllo", 3))"#,
            &["4\t108"],
        );
    }

    #[test]
    fn tonumber_handles_hex_and_bases() {
        assert_output(r#"print(tonumber("0x10"), tonumber("ff", 16), tonumber("z", 36), tonumber("12", 2))"#, &["16\t255\t35\tnone"]);
    }

    #[test]
    fn math_floor_of_huge_float_stays_float() {
        assert_output("print(math.floor(1e300) == 1e300, math.floor(-2.5), math.ceil(2.5))", &["true\t-3\t3"]);
    }

    #[test]
    fn math_random_near_integer_limits() {
        assert_output("let r = math.random(math.maxinteger - 1, math.maxinteger)\nprint(r >= math.maxinteger - 1 and r <= math.maxinteger)", &["true"]);
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
        for i in 0..5 {
            unsafe { api::umbra_pushinteger(U, i) };
        }
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
        assert_output("let x = 5\nlet big = \"B\"\nlet small = \"S\"\nprint(x > 3 ? big : small)", &["B"]);
        assert_output("let t = {m = fn(self) { return \"M\" }}\nprint(true ? t:m() : \"no\")", &["M"]);
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
        assert_output(&format!("print({}1{})", "(".repeat(50), ")".repeat(50)), &["1"]);
    }

    #[test]
    fn parse_interp_rejects_trailing_tokens() {
        assert!(run("print(\"${a b}\")").is_err());
        assert!(run("print(\"${{1,2}[1]}\")").is_err());
        // Interpolated strings work as call arguments too.
        assert_output("fn f(s) { return s }\nlet n = \"x\"\nprint(f\"${n}\")", &["x"]);
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
            assert(inner == 1)",
        )
        .unwrap();
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
        assert_eq!(unsafe { api::umbra_dostring(U, bad.as_ptr()) }, api::UmbraStatus::SyntaxError as std::ffi::c_int);
        let good = CString::new("x = 1").unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, good.as_ptr()) }, 0);
        let boom = CString::new("error(\"boom\")").unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, boom.as_ptr()) }, api::UmbraStatus::RuntimeError as std::ffi::c_int);
        unsafe { api::umbra_close(U) };
    }

    #[test]
    fn api_pcall_invokes_callable_tables() {
        use std::ffi::CString;

        let U = api::umbra_newstate();
        let src = CString::new("t = setmetatable({}, {__call = fn(self, x) { return x * 2 }})").unwrap();
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

        // A registered fn that runs a script on a *second* state: the outer
        // script's later allocations must still land on the outer VM's GC,
        // or they dangle after the inner one collects.
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

    #[test]
    fn unop_on_constant_does_not_clobber_result() {
        // Regression: Expr::Unop freed the result register after emitting,
        // so the next temporary overwrote it (-7 + -3 evaluated to 0).
        assert_output(
            r#"print(-7 + -3)
print(-7 * -3)
print(-7 // -3)
print(-7 - -3)
print(#"abc" + 0)
print(- -3)
var x = 5
print(-x + -x)"#,
            &["-10", "21", "2", "-4", "3", "3", "-10"],
        );
    }

    #[test]
    fn too_many_locals_is_compile_error_not_panic() {
        // Regression: 256+ names in one declaration overflowed the u8
        // register field and panicked instead of erroring.
        let names: Vec<String> = (1..=256).map(|i| format!("v{i}")).collect();
        let src = format!("let {} = 1", names.join(","));
        assert!(run(&src).unwrap_err().contains("too many registers"));

        let targets: Vec<String> = (1..=256).map(|i| format!("v{i}")).collect();
        let src = format!("{} = 1", targets.join(","));
        assert!(run(&src).unwrap_err().contains("too many registers"));
    }

    #[test]
    fn oversized_table_constructor_is_compile_error() {
        // Regression: SetList's batch operand is a u8; >255 batches of 50
        // wrapped it to 0 and the VM panicked on (c - 1) * 50.
        let items: Vec<String> = (1..=12800).map(|i| i.to_string()).collect();
        let src = format!("let t = {{{}}}", items.join(","));
        assert!(run(&src).unwrap_err().contains("table constructor too large"));
    }

    #[test]
    fn jump_past_sbx_range_is_compile_error() {
        // Regression: an if-body longer than the signed 16-bit jump range
        // silently truncated the offset and executed the block anyway.
        let body = "  x = x + 1\n".repeat(33000);
        let src = format!("var x = 0\nif x == 1 {{\n{body}}}\nprint(x)");
        assert!(run(&src).unwrap_err().contains("jump out of range"));
    }

    #[test]
    fn goto_into_nested_block_is_rejected() {
        // Lua: a label is only visible inside its own block (plus the
        // enclosing block when it's the block's last statement).
        assert!(run("goto inner\nif true {\n  ::inner::\n}\nprint(1)").unwrap_err().contains("no visible label"));
        // The end-of-block label idiom stays legal.
        assert_output("var n = 0\nfor i = 1, 5 {\n  if i == 3 { goto cont }\n  n = n + i\n  ::cont::\n}\nprint(n)", &["12"]);
    }

    #[test]
    fn goto_out_of_scope_runs_close() {
        // A goto that leaves a <close> local's scope must run close() first.
        assert_output(
            r#"fn res(name) { return { close = fn(self) { print("close " .. name) } } }
if true {
    let r <close> = res("x")
    goto out
}
::out::
print("after")"#,
            &["close x", "after"],
        );
    }

    #[test]
    fn vararg_outside_vararg_function_is_compile_error() {
        assert!(run("fn f() { return ... }").unwrap_err().contains("vararg"));
        assert!(run("fn f() { let a = ... }").unwrap_err().contains("vararg"));
        assert!(run("fn f() { return {...} }").unwrap_err().contains("vararg"));
        assert!(run("fn f() { return g(...) }").unwrap_err().contains("vararg"));
        // Vararg functions still work.
        assert_output("fn f(...) { return ... }\nprint(f(1, 2))", &["1\t2"]);
    }

    #[test]
    fn vm_error_level_0_has_no_line_prefix() {
        assert_output(
            "let ok, msg = pcall(fn() { error(\"bare\", 0) })
            print(ok)
            print(msg)",
            &["false", "bare"],
        );
    }

    #[test]
    fn vm_error_non_string_values_reach_pcall_unchanged() {
        // Lua: error(v) propagates v itself, so pcall returns the object.
        assert_output(
            "let ok, err = pcall(fn() { error({code = 42}) })
            print(ok)
            print(type(err))
            print(err.code)",
            &["false", "table", "42"],
        );
        assert_output(
            "let ok, err = pcall(fn() { error(42) })
            print(ok, err)",
            &["false\t42"],
        );
    }

    #[test]
    fn vm_xpcall_handler_receives_error_object() {
        assert_output(
            "let ok, code = xpcall(fn() { error({code = 7}) }, fn(m) { return m.code })
            print(ok, code)",
            &["false\t7"],
        );
    }

    #[test]
    fn vm_coroutine_error_object_reaches_resume() {
        assert_output(
            "let co = coroutine.create(fn() { error({why = \"x\"}) })
            let ok, err = coroutine.resume(co)
            print(ok, type(err), err.why)",
            &["false\ttable\tx"],
        );
    }

    #[test]
    fn vm_assert_throws_non_string_message() {
        assert_output(
            "let ok, err = pcall(fn() { assert(false, {n = 1}) })
            print(ok, type(err), err.n)",
            &["false\ttable\t1"],
        );
    }

    #[test]
    fn vm_math_tointeger_rejects_out_of_range_floats() {
        // i64::MAX as f64 is exactly 2^63, so a bare `f as i64` round-trip
        // check would wrongly accept 2^63; the bounds check must run first.
        assert_output("print(math.tointeger(2^63))", &["none"]);
        assert_output("print(math.tointeger(0.0 - 9223372036854777856.0))", &["none"]);
        assert_output("print(math.tointeger(3.0))", &["3"]);
        assert_output("print(math.tointeger(3.5))", &["none"]);
    }

    #[test]
    fn vm_for_num_integer_overflow_terminates() {
        // The counter must not wrap i64::MAX back to i64::MIN (infinite loop).
        assert_output(
            "var n = 0
            for i = math.maxinteger - 2, math.maxinteger { n += 1 }
            print(n)",
            &["3"],
        );
        assert_output(
            "var n = 0
            for i = math.mininteger + 2, math.mininteger, -1 { n += 1 }
            print(n)",
            &["3"],
        );
        // An integer counter against a float limit above i64::MAX still stops.
        assert_output(
            "var n = 0
            for i = math.maxinteger - 1, 2^63 { n += 1 }
            print(n)",
            &["2"],
        );
    }

    #[test]
    fn vm_unpack_honors_range_args() {
        assert_output("print(unpack({1,2,3}, 2, 3))", &["2\t3"]);
        assert_output("print(unpack({1,2,3}, 2))", &["2\t3"]);
        assert_output("print(select('#', unpack({1,2,3}, 3, 1)))", &["0"]);
    }
}

#[cfg(test)]
pub fn disasm(c: &compiler::Compiled) -> String {
    let mut out = String::new();
    fn dump(p: &chunk::Proto, depth: usize, out: &mut String) {
        use chunk::*;
        let pad = "  ".repeat(depth);
        out.push_str(&format!("{pad}proto params={} vararg={} regs={} consts={}\n", p.params, p.is_vararg, p.max_regs, p.consts.len()));
        for (i, &ins) in p.code.iter().enumerate() {
            let op = Op::from_u8(iop(ins)).map(|o| format!("{o:?}")).unwrap_or_else(|| "?".into());
            out.push_str(&format!("{pad}{i:4} {op:10} a={} b={} c={} bx={} sbx={}\n", ia(ins), ib(ins), ic(ins), ibx(ins), isbx(ins)));
        }
        // SAFETY: the children of a Compiled's tree, which it owns.
        for &c in p.protos.iter() {
            dump(unsafe { &(*c).proto }, depth + 1, out);
        }
    }
    dump(c.proto(), 0, &mut out);
    out
}

#[cfg(test)]
mod stdlib_second_pass_tests {
    use super::*;

    fn assert_output(src: &str, expected: &[&str]) {
        assert_eq!(run_capture(src).unwrap(), expected, "script: {src}");
    }

    #[test]
    fn string_find_init_past_end_finds_nothing() {
        // Lua: init > len+1 fails even for an empty pattern.
        assert_output(r#"print(string.find("abc", "", 5))"#, &["none"]);
        assert_output(r#"print(string.find("abc", "", 4))"#, &["4\t3"]);
        assert_output(r#"print(string.find("abc", "", 10, true))"#, &["none"]);
        assert_output(r#"print(string.match("abc", "", 5))"#, &["none"]);
        assert_output(r#"print(string.find("abc", "b", 5))"#, &["none"]);
    }

    #[test]
    fn string_gmatch_honors_init_argument() {
        assert_output(r#"for w in string.gmatch("a1b2", "%a", 3) { print(w) }"#, &["b"]);
        assert_output(r#"for w in string.gmatch("abc", "%a", 10) { print(w) }"#, &[]);
    }

    #[test]
    fn string_gsub_rejects_trailing_percent_in_replacement() {
        assert!(run(r#"string.gsub("abc", "%w", "abc%")"#).is_err());
        assert!(run(r#"string.gsub("abc", "%w", "%")"#).is_err());
        assert_output(r#"print(string.gsub("abc", "%w", "%%"))"#, &["%%%\t3"]);
    }

    #[test]
    fn pattern_backreference_to_position_capture_fails_match() {
        // Lua: %1 against a () capture fails the match rather than erroring.
        assert_output(r#"print(string.find("aa", "()a%1"))"#, &["none"]);
        assert_output(r#"print(string.find("aa", "(a)%1"))"#, &["1\t2\ta"]);
    }

    #[test]
    fn string_format_rejects_non_integral_and_unknown() {
        assert!(run(r#"string.format("%d", 3.7)"#).is_err());
        assert!(run(r#"string.format("%z", 1)"#).is_err());
        assert!(run(r#"string.format("abc%")"#).is_err());
        assert!(run(r#"string.format("%s")"#).is_err());
        assert_output(r#"print(string.format("%d", 3.0))"#, &["3"]);
        assert_output(r#"print(string.format("%.3d", 5))"#, &["005"]);
        assert_output(r#"print(string.format("%#08x", 255))"#, &["0x0000ff"]);
    }

    #[test]
    fn string_format_float_specifiers() {
        assert_output(r#"print(string.format("%e", 12345.6789))"#, &["1.234568e+04"]);
        assert_output(r#"print(string.format("%g", 0.00001))"#, &["1e-05"]);
        assert_output(r#"print(string.format("%.3g", 3.14159))"#, &["3.14"]);
        assert_output(r#"print(string.format("%a", 1.5))"#, &["0x1.8p+0"]);
        assert_output(r#"print(string.format("%.3a", 1.5))"#, &["0x1.800p+0"]);
    }

    #[test]
    fn string_format_q_quotes_only_literal_forms() {
        assert_output(r#"print(string.format("%q", 42))"#, &["42"]);
        // Lua quotes floats as %a hex so they re-read exactly.
        assert_output(r#"print(string.format("%q", 4.5))"#, &["0x1.2p+2"]);
        assert_output(r#"print(string.format("%q", "tab\there"))"#, &[r#""tab\9here""#]);
        assert_output(r#"print(string.format("%q", true), string.format("%q", none))"#, &["true\tnone"]);
        assert!(run(r#"string.format("%q", {})"#).is_err());
        assert!(run(r#"string.format("%5q", "x")"#).is_err());
    }

    #[test]
    fn tonumber_wraps_hex_and_reads_hex_floats() {
        assert_output(r#"print(tonumber("0xFFFFFFFFFFFFFFFF"))"#, &["-1"]);
        assert_output(r#"print(tonumber("0x8000000000000000"))"#, &["-9223372036854775808"]);
        assert_output(r#"print(tonumber("0x1.8p1"))"#, &["3.0"]);
        assert_output(r#"print(tonumber("0x.8"))"#, &["0.5"]);
        // With an explicit base Lua parses digits only: no 0x prefix, no
        // floats, no decimal points.
        assert_output(r#"print(tonumber("0x10", 16), tonumber("1.5", 10))"#, &["none\tnone"]);
        assert_output(r#"print(tonumber("ff ", 16))"#, &["255"]);
        assert!(run(r#"tonumber(10, 16)"#).is_err());
        assert!(run(r#"tonumber("ff", 1.5)"#).is_err());
    }

    #[test]
    fn string_pack_extra_formats_and_strict_ints() {
        assert_output(r#"print(string.unpack("j", string.pack("j", -5)))"#, &["-5\t9"]);
        assert_output(r#"print(string.unpack("n", string.pack("n", 1.5)))"#, &["1.5\t9"]);
        assert_output(r#"print(bytes.tostring(string.unpack("z", string.pack("z", "abc"))))"#, &["abc"]);
        assert!(run(r#"string.pack("z", "a\0b")"#).is_err());
        assert!(run(r#"string.pack("b", 1.9)"#).is_err());
        assert_output(r#"print(string.pack("b", 1.0)[1])"#, &["1"]);
        assert_output(r#"print(string.unpack("Xi4", string.pack("i4", 7)))"#, &["1"]);
    }

    #[test]
    fn string_unpack_position_argument_bounds() {
        assert_output(r#"print(string.unpack("b", string.pack("b", 1), -1))"#, &["1\t2"]);
        assert_output(r#"print(string.unpack("b", string.pack("b", 1), 0))"#, &["1\t2"]);
        assert_output(r#"print(string.unpack("b", string.pack("b", 1), -5))"#, &["1\t2"]);
        assert!(run(r#"string.unpack("b", string.pack("b", 1), 3)"#).is_err());
        assert_output(r#"print(string.unpack("", string.pack("b", 1), 2))"#, &["2"]);
    }

    #[test]
    fn math_fmod_and_ult() {
        assert_output("print(math.fmod(5, 3))", &["2"]);
        assert_output("print(math.fmod(-5, 3))", &["-2"]);
        assert_output("print(math.fmod(5.5, 2))", &["1.5"]);
        assert!(run("math.fmod(5, 0)").is_err());
        assert_output("print(math.ult(1, 2), math.ult(-1, 2), math.ult(2, -1))", &["true\tfalse\ttrue"]);
    }

    #[test]
    fn math_random_full_range_and_arg_errors() {
        assert_output("let r = math.random(math.mininteger, math.maxinteger)\nprint(math.type(r))", &["integer"]);
        // random(0) is a full-range integer, like Lua.
        assert_output("print(math.type(math.random(0)))", &["integer"]);
        assert!(run("math.random(1, 2, 3)").is_err());
        assert!(run("math.random(1.5)").is_err());
        assert!(run("math.random(-1)").is_err());
    }

    #[test]
    fn table_sort_runs_comparator() {
        // Lua detects invalid order functions only incidentally during
        // partitioning; an always-true comparator on an ordered table is
        // not an error there, so it isn't one here either.
        assert_output(
            r#"let t = {3, 1, 2}
            table.sort(t, fn(a, b) { return a < b })
            print(table.concat(t, ","))"#,
            &["1,2,3"],
        );
        assert_output(
            r#"let t = {3, 1, 2}
            table.sort(t, fn(a, b) { return a > b })
            print(table.concat(t, ","))"#,
            &["3,2,1"],
        );
    }

    #[test]
    fn table_unpack_requires_table() {
        assert!(run(r#"table.unpack("x")"#).is_err());
        assert!(run(r#"unpack("x")"#).is_err());
        assert!(run(r#"table.move({1, 2}, 1, 2, 1, "x")"#).is_err());
        assert!(run(r#"table.insert({1, 2}, 1.5, "x")"#).is_err());
    }

    #[test]
    fn os_time_from_table_and_date_table_form() {
        assert_output("print(os.time({year=1970, month=1, day=1, hour=0, min=0, sec=0}))", &["0"]);
        assert_output("print(os.time({year=1970, month=1, day=1}))", &["43200"]);
        assert!(run("os.time({})").is_err());
        assert_output(
            r#"let t = os.date("*t", 0)
            print(t.year, t.month, t.day, t.wday, t.yday)"#,
            &["1970\t1\t1\t5\t1"],
        );
        assert_output(r#"print(os.date("%A %j", 0))"#, &["Thursday 001"]);
        assert_output(r#"print(os.date("%I %p", 43200))"#, &["12 PM"]);
    }

    #[test]
    fn io_file_write_returns_handle_and_read_formats() {
        assert_output(
            r#"let f = io.open("/tmp/umbra_test_handle.txt", "w")
            print(f:write("3.5 42 rest\nlast") == f)
            f:close()
            let r = io.open("/tmp/umbra_test_handle.txt", "r")
            print(r:read("n"))
            print(r:read("n"))
            print(r:read("l"))
            print(r:read("l"))
            print(r:read("l"))
            r:close()"#,
            &["true", "3.5", "42", " rest", "last", "none"],
        );
        assert_output(
            r#"let r = io.open("/tmp/umbra_test_handle.txt", "r")
            print(r:read("L"))
            r:close()
            let r2 = io.open("/tmp/umbra_test_handle.txt", "r")
            print(r2:read(3))
            r2:close()"#,
            &["3.5 42 rest\n", "3.5"],
        );
        assert_output(r#"for l in io.lines("/tmp/umbra_test_handle.txt") { print(l) }"#, &["3.5 42 rest", "last"]);
        assert!(run(r#"io.lines("/nonexistent_umbra_path")"#).is_err());
    }

    #[test]
    fn utf8_len_empty_range_and_strict_args() {
        assert_output(r#"print(utf8.len("héllo", 3, 1))"#, &["0"]);
        assert_output(r#"print(utf8.codepoint("héllo", 3, 1))"#, &[""]);
        assert!(run(r#"utf8.len("héllo", 1.5)"#).is_err());
        assert!(run(r#"utf8.char(65.5)"#).is_err());
    }

    #[test]
    fn tostring_floats_print_lua_style() {
        assert_output("print(3.0)", &["3.0"]);
        assert_output("print(0.5)", &["0.5"]);
        assert_output("print(-0.0)", &["-0.0"]);
        assert_output("print(1/0)", &["inf"]);
        assert_output("print(2.5 .. '')", &["2.5"]);
        assert_output("print(1e300)", &["1e+300"]);
    }

    #[test]
    fn stdlib_integer_args_reject_non_integral() {
        assert!(run(r#"string.sub("abc", 1.5)"#).is_err());
        assert!(run(r#"string.find("abc", "b", 2.5)"#).is_err());
        assert!(run(r#"string.rep("x", 2.5)"#).is_err());
        assert!(run(r#"string.char(65.5)"#).is_err());
        assert!(run(r#"table.remove({1, 2}, 1.5)"#).is_err());
        assert_output(r#"print(string.sub("abc", 2.0))"#, &["bc"]);
    }
}

// bytes type + Lua-5.4-exact string.pack/unpack (byte-level compatibility
// with real Lua was verified against lua 5.4.7 during development).
#[cfg(test)]
mod bytes_and_pack_tests {
    use super::*;

    fn assert_output(src: &str, expected: &[&str]) {
        assert_eq!(run_capture(src).unwrap(), expected, "script: {src}");
    }

    #[test]
    fn bytes_new_from_index_and_len() {
        assert_output(
            "let b = bytes.new(4)
            print(#b, type(b))
            b[1] = 65
            b[4] = 255
            print(b[1], b[2], b[4])
            print(b[5], b[0])",
            &["4\tbytes", "65\t0\t255", "none\tnone"],
        );
    }

    #[test]
    fn bytes_index_bounds_and_value_range() {
        assert!(run("let b = bytes.new(2) b[3] = 1").is_err());
        assert!(run("let b = bytes.new(2) b[0] = 1").is_err());
        assert!(run("let b = bytes.new(2) b[1] = 256").is_err());
        assert!(run("let b = bytes.new(2) b[1] = -1").is_err());
        assert!(run("let b = bytes.new(2) b[1] = \"x\"").is_err());
        assert!(run("let b = bytes.new(2) b.x = 1").is_err());
    }

    #[test]
    fn bytes_from_tostring_concat() {
        assert_output(
            r#"let b = bytes.from("hi")
            print(#b, b[1], b[2])
            print(bytes.tostring(b))
            print(bytes.tostring(bytes.concat(b, "!", b)))
            print(bytes.len(b))"#,
            &["2\t104\t105", "hi", "hi!hi", "2"],
        );
        assert!(run(r#"bytes.tostring(string.pack("B", 200))"#).is_err());
        assert!(run("bytes.tostring(\"str\")").is_err());
        assert!(run("bytes.from(5)").is_err());
    }

    #[test]
    fn bytes_method_call_via_index() {
        assert_output(
            r#"let b = bytes.from("ab")
            print(b:tostring())
            print(b:len())"#,
            &["ab", "2"],
        );
    }

    #[test]
    fn pack_returns_bytes_matching_lua() {
        // Expected bytes produced by lua 5.4.7 string.pack on x86-64.
        assert_output(
            r#"let b = string.pack("<i4", 0x01020304)
            print(b[1], b[2], b[3], b[4], #b)
            let be = string.pack(">i4", 0x01020304)
            print(be[1], be[2], be[3], be[4])
            let n = string.pack("=i4", 0x01020304)
            print(n[1], n[4])"#,
            &["4\t3\t2\t1\t4", "1\t2\t3\t4", "4\t1"],
        );
    }

    #[test]
    fn pack_alignment_bang_and_x() {
        // lua: pack("!4bi4",1,2) = 01 000000 02000000; pack("!4bXdd",1,2.5)
        // pads the double to 4 (maxalign), not 8.
        assert_output(
            r#"let b = string.pack("!4bi4", 1, 2)
            print(#b, b[1], b[5])
            let d = string.pack("!4bXdd", 1, 2.5)
            print(#d, d[5], d[12])
            print(string.packsize("!4bi4"), string.packsize("bXhh"))"#,
            &["8\t1\t2", "12\t0\t64", "8\t3"],
        );
        assert!(run(r#"string.pack("Xc4", "ab")"#).is_err());
        assert!(run(r#"string.pack("Xz", "ab")"#).is_err());
        assert!(run(r#"string.pack("!3i4", 5)"#).is_err());
        assert!(run(r#"string.packsize("i4s1")"#).is_err());
    }

    #[test]
    fn pack_wide_ints_and_sign_extension() {
        // lua: pack("i16",-1) = ff*16; pack("I16",-1) = ff*8 then 00*8.
        assert_output(
            r#"let a = string.pack("i16", -1)
            print(#a, a[1], a[9], a[16])
            let u = string.pack("I16", -1)
            print(u[1], u[9], u[16])
            print(string.unpack("i16", a))"#,
            &["16\t255\t255\t255", "255\t0\t0", "-1\t17"],
        );
        assert!(run(r#"string.unpack("i16", string.pack("I16", -1))"#).is_err());
        assert!(run(r#"string.pack("i17", 1)"#).is_err());
        assert!(run(r#"string.pack("i0", 1)"#).is_err());
    }

    #[test]
    fn pack_string_formats() {
        assert_output(
            r#"let p = string.pack("s2", "hi")
            print(#p, p[1], p[2], p[3], p[4])
            print(bytes.tostring(string.unpack("s2", p)))
            print(#string.unpack("c5", string.pack("c5", "ab")))"#,
            &["4\t2\t0\t104\t105", "hi", "5"],
        );
        assert!(run(r#"string.pack("s1", string.rep("x", 300))"#).is_err());
        assert!(run(r#"string.pack("c3", "abcd")"#).is_err());
    }

    #[test]
    fn unpack_accepts_string_and_bytes() {
        assert_output(
            r#"let b = string.pack("<i2", 258)
            print(string.unpack("<i2", b))
            print(string.unpack("<i2", bytes.tostring(b)))"#,
            &["258\t3", "258\t3"],
        );
    }

    #[test]
    fn io_binary_mode_roundtrip() {
        let path = "umbra_test_bytes_io_xyz.bin";
        let result = run_capture(&format!(
            "let f = io.open(\"{path}\", \"wb\")
            f:write(string.pack(\"<i4\", 0x01020304))
            f:write(string.pack(\"BB\", 0, 255))
            f:close()
            let g = io.open(\"{path}\", \"rb\")
            let d = g:read(6)
            print(type(d), #d, d[1], d[4], d[5], d[6])
            print(string.unpack(\"<i4\", d))
            g:close()"
        ));
        std::fs::remove_file(path).ok();
        assert_eq!(result.unwrap(), vec!["bytes\t6\t4\t1\t0\t255", "16909060\t5"]);
    }

    #[test]
    fn bytes_are_gc_tracked_and_mutable() {
        // Survives collection and stays mutable afterwards.
        assert_output(
            r#"let b = bytes.new(8)
            b[1] = 7
            for i = 1, 2000 { let t = {i, i + 1} }
            b[8] = 9
            print(b[1], b[8], #b)"#,
            &["7\t9\t8"],
        );
    }
}

// Error-message naming (Lua's varinfo) and source:line tracebacks.
#[cfg(test)]
mod error_naming_tests {
    use super::*;

    fn assert_output(src: &str, expected: &[&str]) {
        assert_eq!(run_capture(src).unwrap(), expected, "script: {src}");
    }

    #[test]
    fn index_error_names_local_global_field_upvalue() {
        let e = run("let x = none\nreturn x.f").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: attempt to index a none value (local 'x')");
        let e = run("return foo.bar").unwrap_err();
        assert_eq!(e, "[string \"...\"]:1: attempt to index a none value (global 'foo')");
        let e = run("let t = {}\nreturn t.a.b").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: attempt to index a none value (field 'a')");
        let e = run("let x = none\nfn f() { return x.f }\nf()").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: attempt to index a none value (upvalue 'x')");
    }

    #[test]
    fn index_error_names_set_and_method_receiver() {
        let e = run("let x = none\nx.f = 1").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: attempt to index a none value (local 'x')");
        let e = run("let t = none\nt:m()").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: attempt to index a none value (local 't')");
    }

    #[test]
    fn call_error_names_callee() {
        let e = run("let x = none\nx()").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: attempt to call a none value (local 'x')");
        let e = run("foo()").unwrap_err();
        assert_eq!(e, "[string \"...\"]:1: attempt to call a none value (global 'foo')");
        let e = run("let t = {}\nt.m()").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: attempt to call a none value (field 'm')");
        let e = run("let t = {}\nt:m()").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: attempt to call a none value (method 'm')");
    }

    #[test]
    fn arith_concat_compare_len_errors_name_operand() {
        let e = run("let x = none\nreturn x + 1").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: attempt to perform arithmetic on a none value (local 'x')");
        let e = run("return \"a\" + 1").unwrap_err();
        assert_eq!(e, "[string \"...\"]:1: attempt to perform arithmetic on a string value (constant 'a')");
        let e = run("let x = none\nreturn x .. \"a\"").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: attempt to concatenate a none value (local 'x')");
        let e = run("let x = none\nreturn x < {}").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: attempt to compare none (local 'x') with table");
        let e = run("let x = none\nreturn #x").unwrap_err();
        assert_eq!(e, "[string \"...\"]:2: attempt to get length of a none value (local 'x')");
    }

    #[test]
    fn traceback_names_functions_and_main_chunk() {
        assert_output(
            r#"fn deep() { return debug.traceback("msg") }
fn mid() { let t = deep() return t }
fn outer() { let t = mid() return t }
print(outer())"#,
            &[
                "msg\nstack traceback:\n\t[string \"...\"]:1: in function 'deep'\n\t[string \"...\"]:2: in function 'mid'\n\t[string \"...\"]:3: in function 'outer'\n\t[string \"...\"]:4: in main chunk",
            ],
        );
    }

    #[test]
    fn traceback_marks_tail_calls() {
        assert_output(
            r#"fn tail() { error("boom") }
fn f() { return tail() }
let ok, tb = xpcall(f, debug.traceback)
print(tb)"#,
            &[
                "[string \"...\"]:1: boom\nstack traceback:\n\t[string \"...\"]:1: in function 'tail'\n\t(...tail calls...)\n\t[string \"...\"]:3: in main chunk",
            ],
        );
    }

    #[test]
    fn type_is_number_and_math_type_splits() {
        assert_output("print(type(1), type(1.5), type(\"s\"), type(none), type({}))", &["number\tnumber\tstring\tnone\ttable"]);
        assert_output("print(math.type(1), math.type(1.5), math.type(\"s\"))", &["integer\tfloat\tnone"]);
    }
}

#[cfg(test)]
mod gc_incremental_tests {
    use super::*;
    use crate::gc::{GcColor, GcHeader, GcPhase};

    // Storing a white object into a black (already-scanned) table mid-cycle
    // must mark it through the write barrier; without it the sweep frees a
    // reachable object and t.x dangles.
    #[test]
    fn gc_write_barrier_keeps_white_stored_into_black_table() {
        let mut vm = vm::Vm::new();
        run_with_vm("t = {}", &mut vm).unwrap();
        let t_val = vm.globals.get_str("t").unwrap();
        let t_hdr = t_val.as_table().unwrap() as *const GcHeader;

        // Step the collector until the cycle is in flight and t is black.
        let mut guard = 0;
        loop {
            vm.gc_step();
            guard += 1;
            assert!(guard < 1_000_000, "cycle never reached a state with t black");
            if vm.gc.phase() == GcPhase::Pause {
                break;
            }
            if unsafe { (*t_hdr).color } == GcColor::Black {
                break;
            }
        }
        assert_ne!(vm.gc.phase(), GcPhase::Pause, "cycle finished before t blackened");
        assert_eq!(unsafe { (*t_hdr).color }, GcColor::Black);

        run_with_vm("t.x = { y = 7 }", &mut vm).unwrap();
        vm.gc_collect();
        // Reuse the freed slot if the barrier missed the store.
        run_with_vm("for i = 1, 2000 { let g = { i } }", &mut vm).unwrap();
        run_with_vm("assert(t.x.y == 7)", &mut vm).unwrap();
    }

    // The incremental collector must bound each step: in a churn loop the
    // largest single step's charged work stays a small fraction of a whole
    // cycle's work (a stop-the-world collect would be nearly all of it).
    #[test]
    fn gc_incremental_steps_bound_max_pause() {
        let mut vm = vm::Vm::new();
        run_with_vm(
            r#"let keep = {}
for i = 1, 100000 { keep[i] = { i } }
for i = 1, 5000000 { let t = { i } }"#,
            &mut vm,
        )
        .unwrap();
        let max_step = vm.gc.max_step_work();
        let cycle = vm.gc.last_cycle_work();
        assert!(cycle > 0, "no incremental cycle ran during churn");
        assert!(max_step * 10 <= cycle, "max step {max_step} exceeds 10% of cycle work {cycle}");
    }
}

// stdlib format coverage
#[cfg(test)]
mod stdlib_format_coverage_tests {
    use super::*;

    fn assert_output(src: &str, expected: &[&str]) {
        assert_eq!(run_capture(src).unwrap(), expected, "script: {src}");
    }

    #[test]
    fn string_format_matches_lua_specifiers() {
        // All values verified against Lua 5.4.7.
        assert_output(r#"print(string.format("%d %i %u %c %x %X %o", 42, -7, 300, 65, 255, 255, 8))"#, &["42 -7 300 A ff FF 10"]);
        assert_output(
            r#"print(string.format("%e %E %f %g %G", 1234.5678, 1234.5678, 1234.5678, 1234.5678, 1234.5678))"#,
            &["1.234568e+03 1.234568E+03 1234.567800 1234.57 1234.57"],
        );
        assert_output(r#"print(string.format("%a %A", 1.0, 1.0))"#, &["0x1p+0 0X1P+0"]);
        assert_output(r#"print(string.format("%-5d|%+d|% d|%#x|%05d", 42, 42, 42, 255, 42))"#, &["42   |+42| 42|0xff|00042"]);
        assert_output(r#"print(string.format("%5.2f|%-10s|%.3s|%05d", 3.14159, "hi", "hello", 42))"#, &[" 3.14|hi        |hel|00042"]);
        assert_output(r#"print(string.format("%x %o %u", -1, -1, -1))"#, &["ffffffffffffffff 1777777777777777777777 18446744073709551615"]);
        assert_output(r#"print(string.format("%#o %#x %#X", 8, 255, 255))"#, &["010 0xff 0XFF"]);
        assert_output(r#"print(string.format("%.0f %.0f %.0f %.0f", -0.5, 0.5, 1.5, 2.5))"#, &["-0 0 2 2"]);
        assert_output(r#"print(string.format("%.0a %.3a", 1.5, 1.0))"#, &["0x2p+0 0x1.000p+0"]);
        assert_output(r#"print(string.format("%#g %#.0f %#.0e", 1.0, 2.0, 100.0))"#, &["1.00000 2. 1.e+02"]);
        assert_output(r#"print(string.format("%g %g %g %g", 100000, 1000000, 1e-4, 1e-5))"#, &["100000 1e+06 0.0001 1e-05"]);
        assert_output(r#"print(string.format("%d", 3.0))"#, &["3"]);
        assert_output(r#"print(string.format("%d", "0x1F"), string.format("%d", "  42  "))"#, &["31\t42"]);
        assert_output(r#"print(string.format("%s", setmetatable({}, {__tostring = fn() { return "MT!" }})))"#, &["MT!"]);
        assert_output(r#"print(string.format("%s", none), string.format("%s", true))"#, &["none\ttrue"]);
        assert_output(r#"print(string.format("%c", 0) == "\0")"#, &["true"]);
        assert_output(r#"print(string.format("%.3s", "héllo"))"#, &["hé"]);
        assert_output(r#"print(string.format("%p", none), string.format("%p", 5))"#, &["(null)\t(null)"]);
    }

    #[test]
    fn string_format_q_matches_lua() {
        assert_output(r#"print(string.format("%q", 1/3))"#, &["0x1.5555555555555p-2"]);
        assert_output(r#"print(string.format("%q", 3.5), string.format("%q", 3.0))"#, &["0x1.cp+1\t0x1.8p+1"]);
        assert_output(r#"print(string.format("%q", -0.0))"#, &["-0x0p+0"]);
        assert_output(r#"print(string.format("%q", 1/0), string.format("%q", -1/0))"#, &["1e9999\t-1e9999"]);
        assert_output(r#"print(string.format("%q", math.mininteger))"#, &["0x8000000000000000"]);
        assert_output(r#"print(string.format("%q", "a\0b"))"#, &[r#""a\0b""#]);
        assert_output(r#"print(string.format("%q", "\1\31\127"))"#, &[r#""\1\31\127""#]);
        assert_output(r#"print(string.format("%q", "\r\n"))"#, &["\"\\13\\\n\""]);
        assert_output(r#"print(string.format("%q", "back\\slash"))"#, &[r#""back\\slash""#]);
        // %q output re-reads as the same value.
        assert_output(
            r#"let s = string.format("%q", "a\"b\nc")
print(load("return " .. s)() == "a\"b\nc")"#,
            &["true"],
        );
        assert_output(r#"print(load("return " .. string.format("%q", 1/3))() == 1/3)"#, &["true"]);
    }

    #[test]
    fn string_format_rejects_like_lua() {
        assert!(run(r#"string.format("%F", 1.5)"#).is_err());
        assert!(run(r#"string.format("%y", 1)"#).is_err());
        assert!(run(r#"string.format("%d", 3.5)"#).is_err());
        assert!(run(r#"string.format("%d")"#).is_err());
        assert!(run(r#"string.format("%d %d", 1)"#).is_err());
        assert!(run(r#"string.format("%#d", 5)"#).is_err());
        assert!(run(r#"string.format("%+u", 5)"#).is_err());
        assert!(run(r#"string.format("%#s", "x")"#).is_err());
        assert!(run(r#"string.format("%.3c", 65)"#).is_err());
        assert!(run(r#"string.format("%5q", "x")"#).is_err());
        assert!(run(r#"string.format("%5s", "a\0b")"#).is_err());
        assert!(run(r#"string.format("%")"#).is_err());
        assert!(run(r#"string.format("abc%")"#).is_err());
        assert!(run(r#"string.format("%d", {})"#).is_err());
        assert!(run(r#"string.format("%d", "42a")"#).is_err());
        // Extra arguments are ignored.
        assert_output(r#"print(string.format("%d", 1, 2, 3))"#, &["1"]);
    }

    #[test]
    fn os_date_time_match_lua() {
        // Umbra is UTC-only; these are the TZ=UTC Lua values.
        assert_output(r#"print(os.date("!%Y-%m-%d %H:%M:%S", 1700000000))"#, &["2023-11-14 22:13:20"]);
        assert_output(r#"print(os.date("!%a %A %b %B", 1700000000))"#, &["Tue Tuesday Nov November"]);
        assert_output(r#"print(os.date("!%c", 1700000000))"#, &["Tue Nov 14 22:13:20 2023"]);
        assert_output(
            r#"print(os.date("!%d %H %I %j %m %M %p %S %U %w %W %x %X %y %Y %%", 1700000000))"#,
            &["14 22 10 318 11 13 PM 20 46 2 46 11/14/23 22:13:20 23 2023 %"],
        );
        assert_output(r#"print(os.date("!%u %V %g %G %C %z %r", 1700000000))"#, &["2 46 23 2023 20 +0000 10:13:20 PM"]);
        assert_output(r#"print(os.date("!%D %F %R %T %e %h", 1700000000))"#, &["11/14/23 2023-11-14 22:13 22:13:20 14 Nov"]);
        assert_output(r#"print(os.date("!%Y-%m-%d", -1))"#, &["1969-12-31"]);
        assert_output(r#"print(os.date("!%Y-%m-%d", -62135596800))"#, &["1-01-01"]);
        assert_output(
            r#"let t = os.date("!*t", 1700000000)
print(t.year, t.month, t.day, t.hour, t.min, t.sec, t.wday, t.yday, t.isdst)"#,
            &["2023\t11\t14\t22\t13\t20\t3\t318\tfalse"],
        );
        // os.time normalizes out-of-range fields like mktime.
        assert_output(r#"print(os.time({year=2024, month=1, day=1}))"#, &["1704110400"]);
        assert_output(r#"print(os.time({year=2024, month=13, day=1, hour=0}))"#, &["1735689600"]);
        assert_output(r#"print(os.time({year=2024, month=1, day=32, hour=0}))"#, &["1706745600"]);
        assert_output(r#"print(os.time({year=2024, month=0, day=1, hour=0}))"#, &["1701388800"]);
        assert_output(r#"print(os.time({year=2024, month=1, day=0, hour=0}))"#, &["1703980800"]);
        assert_output(r#"print(os.time({year=2024, month=1, day=-1, hour=0}))"#, &["1703894400"]);
        assert_output(r#"print(os.time({year=1970, month=1, day=1, hour=0}))"#, &["0"]);
        assert_output(r#"print(os.time({year=1969, month=12, day=31, hour=0}))"#, &["-86400"]);
        // mktime writes the normalized fields back into the table.
        assert_output(
            r#"let tt = {year=2024, month=3, day=1}
os.time(tt)
print(tt.wday, tt.yday, tt.isdst, tt.hour)"#,
            &["6\t61\tfalse\t12"],
        );
        assert!(run(r#"os.time({})"#).is_err());
        assert!(run(r#"os.time({year=2024, month=1})"#).is_err());
        assert!(run(r#"os.time("x")"#).is_err());
        assert!(run(r#"os.date("!%q", 0)"#).is_err());
        assert!(run(r#"os.date("!%", 0)"#).is_err());
        assert_output(r#"print(os.date(42, 0))"#, &["42"]);
        assert_output(r#"print(os.time(none) != none, os.date() != none)"#, &["true\ttrue"]);
    }

    #[test]
    fn tonumber_tostring_match_lua() {
        assert_output(
            r#"print(tostring(1e15), tostring(1e16), tostring(2^53), tostring(-0.0), tostring(1/0), tostring(3.0), tostring(0.1))"#,
            &["1e+15\t1e+16\t9.007199254741e+15\t-0.0\tinf\t3.0\t0.1"],
        );
        assert_output(r#"print(tonumber("0x10"), tonumber("1e2"), tonumber(" 12 "), tonumber("12a"), tonumber("z", 36))"#, &["16\t100.0\t12\tnone\t35"]);
        assert_output(
            r#"print(tonumber("0x1.8p1"), tonumber("  0x10  "), tonumber("-0x10"), tonumber("10", 2), tonumber("-z", 36))"#,
            &["3.0\t16\t-16\t2\t-35"],
        );
        assert_output(r#"print(tonumber("0x10000000000000000"), tonumber("0xFFFFFFFFFFFFFFFFF"))"#, &["0\t-1"]);
        assert_output(r#"print(tonumber("1e999"), tonumber("nan"), tonumber("inf"), tonumber("0x"), tonumber("."))"#, &["inf\tnone\tnone\tnone\tnone"]);
        assert_output(r#"print(tonumber("1."), tonumber(".5"), tonumber("1e"), tonumber(""))"#, &["1.0\t0.5\tnone\tnone"]);
        assert_output(r#"print(tonumber(42), tonumber(4.5), tonumber(none))"#, &["42\t4.5\tnone"]);
        assert_output(r#"print(math.type(tonumber("0x10")), math.type(tonumber("1e2")), math.type(tonumber("10.0")))"#, &["integer\tfloat\tfloat"]);
        assert!(run("tonumber()").is_err());
        assert!(run("tonumber(42, 10)").is_err());
        assert!(run("tonumber(42, 16)").is_err());
        assert!(run(r#"tonumber("10", 1)"#).is_err());
        assert!(run(r#"tonumber("10", 37)"#).is_err());
        assert!(run("tostring()").is_err());
    }

    #[test]
    fn string_edge_cases_match_lua() {
        assert_output(
            r#"let s = "hello"
print(string.sub(s, -1), string.sub(s, 0), string.sub(s, 100), string.sub(s, -100), string.sub(s, 2, -2), string.sub(s, -3, -1), string.sub(s, 0, 0), string.sub(s, 3, 2), string.sub(s, -100, 100))"#,
            &["o\thello\t\thello\tell\tllo\t\t\thello"],
        );
        assert_output(
            r#"print(string.rep("ab", 0), "|", string.rep("ab", -1), "|", string.rep("ab", 3, "-"), "|", string.rep("", 5))"#,
            &["\t|\t\t|\tab-ab-ab\t|\t"],
        );
        assert_output(r#"print(string.byte("ABC", 1, 3), string.byte("ABC", -1), string.byte("ABC", 0))"#, &["65\t67"]);
        assert_output(r#"print(string.byte("ABC", 2, 1) == none)"#, &["true"]);
        assert_output(r#"print(string.char(72, 105), #string.char(0), string.byte(string.char(0)))"#, &["Hi\t1\t0"]);
        // C-locale case mapping: ASCII only, like Lua.
        assert_output(r#"print(string.upper("héllo"), string.lower("HÉLLO"))"#, &["HéLLO\thÉllo"]);
        assert_output(r#"print(string.upper("ß"), string.lower("İ"))"#, &["ß\tİ"]);
        // Byte reversal like Lua; mid-character cuts re-validate.
        assert_output(r#"print(string.reverse("héllo") == "oll\169\195h")"#, &["true"]);
        assert_output(r##"print(string.len("héllo"), #"héllo", utf8.len("héllo"))"##, &["6\t6\t5"]);
        assert_output(r#"print(string.byte("héllo", 1, 4))"#, &["104\t195\t169\t108"]);
        assert_output(r#"print(string.byte("abc", 1, 1e18))"#, &["97\t98\t99"]);
        assert!(run(r#"string.rep("x")"#).is_err());
        assert!(run(r#"string.char(256)"#).is_err());
        assert!(run(r#"string.char(-1)"#).is_err());
        assert!(run(r#"string.rep("x", 1e18)"#).is_err());
    }

    #[test]
    fn table_edge_cases_match_lua() {
        assert_output(r#"print(table.concat({1,2,3}), table.concat({1,2,3}, "-"), table.concat({1,2.5,"x"}, ","))"#, &["123\t1-2-3\t1,2.5,x"]);
        assert_output(r#"print(table.concat({1,2,3}, "-", 2, 3), table.concat({1,2,3}, "-", 3, 2), table.concat({}, "-"))"#, &["2-3\t\t"]);
        assert!(run(r#"table.concat({{}})"#).is_err());
        assert_output(
            r#"let t = {10,20,30}
table.insert(t, 4)
table.insert(t, 1, 5)
print(table.concat(t, ","))"#,
            &["5,10,20,30,4"],
        );
        assert!(run(r#"table.insert({1,2,3}, 6, 99)"#).is_err());
        assert!(run(r#"table.insert({1,2,3}, 0, 99)"#).is_err());
        assert!(run(r#"table.insert({1,2,3})"#).is_err());
        assert_output(r#"print(table.remove({1,2,3}), table.remove({1,2,3}, 1))"#, &["3\t1"]);
        assert_output(r#"print(table.concat(table.move({1,2,3,4,5}, 2, 4, 1), ","))"#, &["2,3,4,4,5"]);
        assert_output(r#"print(table.concat(table.move({1,2,3,4,5}, 1, 3, 3), ","))"#, &["1,2,1,2,3"]);
        assert_output(r#"print(table.concat(table.move({1,2,3}, 1, 3, 2, {9,9,9,9,9}), ","))"#, &["9,1,2,3,9"]);
        assert_output(r#"print(table.unpack({1,2,3}, 0, 2))"#, &["none\t1\t2"]);
        assert_output(r##"print(select("#", table.unpack({1,2,3}, 4, 5)))"##, &["2"]);
        assert_output(
            r#"let pk = table.pack(1, none, 3)
print(pk.n, pk[1], pk[2], pk[3])"#,
            &["3\t1\tnone\t3"],
        );
        assert_output(r#"print(table.pack().n)"#, &["0"]);
        assert!(run(r#"table.unpack({1,2,3}, 1, 1e18)"#).is_err());
        assert!(run(r#"table.move({1,2,3}, 1, 2, math.maxinteger)"#).is_err());
        assert!(run(r#"table.sort({1,"x"})"#).is_err());
        // An always-true comparator is not an error (Lua detects invalid
        // order functions only incidentally during partitioning).
        assert_output(
            r#"let t = {1,2,3}
print(pcall(table.sort, t, fn(a,b) { return true }))"#,
            &["true"],
        );
        assert_output(r#"print(pcall(table.sort, {1,2,3}, fn(a,b) { return 1 }))"#, &["true"]);
    }

    #[test]
    fn math_edge_cases_match_lua() {
        assert_output(r#"print(math.floor(3.7), math.floor(-3.7), math.type(math.floor(3.7)))"#, &["3\t-4\tinteger"]);
        assert_output(r#"print(math.ceil(3.2), math.ceil(-3.2), math.type(math.ceil(3.2)))"#, &["4\t-3\tinteger"]);
        assert_output(r#"print(math.floor(2^63), math.type(math.floor(2^63)))"#, &["9.2233720368548e+18\tfloat"]);
        assert_output(r#"print(math.floor(-2^63), math.type(math.floor(-2^63)))"#, &["-9223372036854775808\tinteger"]);
        assert_output(r#"print(math.abs(-5), math.abs(5.5), math.abs(math.mininteger))"#, &["5\t5.5\t-9223372036854775808"]);
        assert_output(r#"print(math.fmod(-7, 3), math.fmod(7, -3), -7 % 3, 7 % -3)"#, &["-1\t1\t2\t-2"]);
        assert_output(r#"print(math.fmod(5.5, 2), math.fmod(-5.5, 2))"#, &["1.5\t-1.5"]);
        assert!(run("math.fmod(1, 0)").is_err());
        assert_output(
            r#"print(math.tointeger("8"), math.tointeger("8.0"), math.tointeger("0x10"), math.tointeger(8.5), math.tointeger(2^63))"#,
            &["8\t8\t16\tnone\tnone"],
        );
        assert_output(r#"print(math.max(1, 2.5), math.type(math.max(1, 2.5)), math.min(1, 2.5), math.type(math.min(1, 2.5)))"#, &["2.5\tfloat\t1\tinteger"]);
        assert_output(r#"print(math.max(3, 3.0), math.type(math.max(3, 3.0)), math.min(3, 3.0), math.type(math.min(3, 3.0)))"#, &["3\tinteger\t3\tinteger"]);
        assert_output(r#"print(math.maxinteger + 1 == math.mininteger, math.mininteger - 1 == math.maxinteger)"#, &["true\ttrue"]);
        assert_output(r#"print(math.ult(1, 2), math.ult(-1, 2), math.ult(2, -1), math.ult(math.mininteger, 0))"#, &["true\tfalse\ttrue\tfalse"]);
        assert_output(r#"print(math.type(math.random(0)))"#, &["integer"]);
        assert!(run("math.random(3, 1)").is_err());
        assert!(run("math.random(-5)").is_err());
        assert_output(r#"print(math.type(math.random()), math.type(math.random(5)), math.type(math.random(1,5)))"#, &["float\tinteger\tinteger"]);
        assert_output(r#"print(math.modf(3.7))"#, &["3\t0.7"]);
        assert_output(r#"print(math.modf(-3.7))"#, &["-3\t-0.7"]);
        assert_output(r#"print(math.modf(5), math.type(math.modf(5)))"#, &["5\tinteger"]);
        assert_output(r#"print(math.log(8, 2), math.log(100, 10), math.log(math.exp(1)))"#, &["3.0\t2.0\t1.0"]);
        assert_output(r#"print(math.log(0), math.log(-1))"#, &["-inf\tnone"]);
        // Numeric strings coerce like luaL_checknumber/luaL_checkinteger.
        assert_output(r#"print(math.floor("3.5"), math.floor("3"), math.abs("-5"))"#, &["3\t3\t5.0"]);
        assert!(run(r#"math.floor("x")"#).is_err());
        assert!(run(r#"math.abs("x")"#).is_err());
        assert!(run("math.tointeger()").is_err());
        assert_output(r#"print(math.type(1), math.type(1.0), math.type("x"), math.type(none))"#, &["integer\tfloat\tnone\tnone"]);
    }

    #[test]
    fn utf8_edge_cases_match_lua() {
        assert_output(r#"print(utf8.char(0x1F600), #utf8.char(0x1F600))"#, &["😀\t4"]);
        assert_output(r#"print(utf8.codepoint("héllo", 1, -1))"#, &["104\t233\t108\t108\t111"]);
        assert_output(r#"print(utf8.codepoint("héllo"), utf8.codepoint("héllo", 2, 3))"#, &["104\t233"]);
        assert_output(r#"print(utf8.codepoint("héllo", 3, 2) == none)"#, &["true"]);
        assert_output(r#"print(utf8.len("héllo"), utf8.len(""), utf8.len("😀"))"#, &["5\t0\t1"]);
        // j may sit mid-character; i may not.
        assert_output(r#"print(utf8.len("héllo", 1, 2))"#, &["2"]);
        assert_output(r#"print(utf8.len("héllo", 3))"#, &["none\t3"]);
        assert_output(r#"print(utf8.len("abc", 2, 1), utf8.len("abc", 1, 3), utf8.len("abc", -1), utf8.len("abc", 4))"#, &["0\t3\t1\t0"]);
        assert!(run(r#"utf8.len("abc", 0)"#).is_err());
        assert!(run(r#"utf8.len("abc", 1, 5)"#).is_err());
        assert_output(
            r#"print(utf8.offset("héllo", 1), utf8.offset("héllo", 2), utf8.offset("héllo", 3), utf8.offset("héllo", 5), utf8.offset("héllo", 6))"#,
            &["1\t2\t4\t6\t7"],
        );
        assert_output(r#"print(utf8.offset("héllo", -1), utf8.offset("héllo", -2), utf8.offset("héllo", 0))"#, &["6\t5\t1"]);
        assert_output(r#"print(utf8.offset("héllo", 1, 2), utf8.offset("héllo", -1, 4))"#, &["2\t2"]);
        assert_output(r#"print(utf8.offset("abc", 4), utf8.offset("abc", 5), utf8.offset("abc", -4))"#, &["4\tnone\tnone"]);
        assert_output(r#"print(utf8.offset("abc", 1, 4), utf8.offset("abc", 2, 4), utf8.offset("abc", 0, 4))"#, &["4\tnone\t4"]);
        assert_output(r#"print(utf8.offset("abc", -1, 4), utf8.offset("abc", -4, 4))"#, &["3\tnone"]);
        assert_output(r#"print(utf8.offset("", 1), utf8.offset("", -1), utf8.offset("", 0))"#, &["1\tnone\t1"]);
        assert_output(r#"print(utf8.offset("héllo", 2, -1), utf8.offset("héllo", -1, -1))"#, &["7\t5"]);
        assert_output(
            r#"print(utf8.offset("héllo", 1, 2), utf8.offset("héllo", 0, 2), utf8.offset("héllo", 0, 3), utf8.offset("héllo", 0, 4))"#,
            &["2\t2\t2\t4"],
        );
        assert_output(r#"print(utf8.offset("héllo", -1, 6), utf8.offset("héllo", 1, 6), utf8.offset("héllo", 2, 6))"#, &["5\t6\t7"]);
        assert!(run(r#"utf8.offset("abc", 1, 5)"#).is_err());
        assert!(run(r#"utf8.offset("abc", 1, 0)"#).is_err());
        assert!(run(r#"utf8.offset("héllo", 1, 3)"#).is_err());
        assert_output(
            r#"var out = ""
for p, c in utf8.codes("a😀b") { out = out .. p .. ":" .. c .. " " }
print(out)"#,
            &["1:97 2:128512 6:98 "],
        );
        assert_output(r#"print(utf8.codepoint("😀"), utf8.codepoint("😀", 1, 4))"#, &["128512\t128512"]);
        assert!(run(r#"utf8.codepoint("😀", 2)"#).is_err());
        assert_output(r#"print(utf8.offset("😀x", 2), utf8.offset("😀x", -1), utf8.offset("😀x", 1, 5))"#, &["5\t5\t5"]);
        assert!(run(r#"utf8.char(0x110000)"#).is_err());
        assert!(run(r#"utf8.char(-1)"#).is_err());
        assert!(run(r#"utf8.char(0xD800)"#).is_err());
    }

    #[test]
    fn table_sort_10k_is_not_quadratic() {
        // An already-sorted 10k array must not degenerate; Rust's sort is
        // O(n log n) worst case. This just checks it completes quickly.
        let start = std::time::Instant::now();
        run(r#"let t = {}
for i = 1, 10000 { t[i] = i }
table.sort(t)
table.sort(t, fn(a, b) { return a < b })
print(t[1], t[10000])"#)
        .unwrap();
        assert!(start.elapsed().as_secs() < 10);
    }
}

// Rust embedding API (embed.rs): natives with VM access, host context,
// rooted refs, host-driven calls, module loading.
#[cfg(test)]
#[allow(non_snake_case)]
mod embed_tests {
    use super::*;
    use crate::embed::{Ctx, Module};
    use crate::value::Value;
    use crate::vm::{Vm, VmError};

    // Compiles and runs `src`, panicking with the script error on failure.
    fn exec(cx: &mut Ctx<'_>, src: &str) -> Vec<Value> {
        let f = cx.load_chunk(src, "test.umbra").unwrap_or_else(|e| panic!("{e}"));
        cx.call(f, &[]).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn native_builds_and_returns_a_table_and_a_string() {
        let mut vm = Vm::new();
        vm.set_global_cfn("make_point", |cx, args| {
            let x = args.first().copied().unwrap_or(Value::nil());
            let t = cx.new_table();
            cx.set_field(t, "x", x)?;
            let label = cx.new_string("point");
            cx.set_field(t, "label", label)?;
            let desc = cx.new_string(&format!("point at {x}"));
            Ok(vec![t, desc])
        });
        // A threshold of 1 keeps the collector cycling through the churn
        // below, so a result that wasn't handed back rooted would be swept.
        vm.gc.threshold = 1;
        run_with_vm(
            r#"let p, d = make_point(3)
let junk = {}
for i = 1, 2000 { junk[i % 10] = { i } }
assert(p.x == 3 and p.label == "point")
assert(d == "point at 3")"#,
            &mut vm,
        )
        .unwrap();
    }

    #[test]
    fn native_calls_back_into_script() {
        let mut vm = Vm::new();
        vm.set_global_cfn("map_twice", |cx, args| {
            let f = args[0];
            // Held only by this local (and the Ctx's roots) while the
            // callbacks churn the heap.
            let out = cx.new_table();
            for i in 1..=2 {
                let r = cx.call(f, &[Value::int(i)])?;
                cx.raw_set(out, Value::int(i), r[0])?;
            }
            Ok(vec![out])
        });
        vm.gc.threshold = 1;
        run_with_vm(
            r#"let t = map_twice(fn(x) {
    var junk = {}
    for i = 1, 500 { junk = { i, tostring(i) } }
    return "v" .. x
})
assert(t[1] == "v1" and t[2] == "v2")
let ok, err = pcall(map_twice, fn(x) { error("boom") })
assert(not ok)
assert(err == "[string \"...\"]:7: boom", err)"#,
            &mut vm,
        )
        .unwrap();
    }

    struct Score {
        points: i64,
    }
    struct Other;

    #[test]
    fn natives_reach_the_host_only_inside_with_host() {
        let mut vm = Vm::new();
        vm.set_global_cfn("add_points", |cx, args| {
            let n = args.first().and_then(|v| v.as_int()).unwrap_or(0);
            match cx.host::<Score>() {
                Some(score) => {
                    score.points += n;
                    Ok(vec![Value::int(score.points)])
                }
                None => Err(VmError::RuntimeError("no score host".into())),
            }
        });
        let mut score = Score { points: 0 };
        let mut cx = vm.ctx();
        let f = cx.load_chunk("return add_points(5) + add_points(2)", "host.umbra").unwrap();
        let r = cx
            .with_host(&mut score, |cx| {
                // A loan of another type doesn't hide Score.
                cx.with_host(&mut Other, |cx| cx.call(f, &[]))
            })
            .unwrap();
        assert_eq!(r[0].as_int(), Some(12));

        // The loan ended with with_host: the same native finds nothing.
        let err = cx.call(f, &[]).unwrap_err();
        assert_eq!(err.message, "host.umbra:1: no score host");
        assert!(cx.host::<Score>().is_none());

        // A panic out of the closure ends the loan too.
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cx.with_host(&mut score, |_cx| panic!("host code failed"));
        }));
        assert!(unwound.is_err());
        assert!(cx.host::<Score>().is_none());
        drop(cx);
        assert_eq!(score.points, 7);
    }

    #[test]
    fn values_a_ctx_returns_survive_collections_until_it_drops() {
        let mut vm = Vm::new();
        vm.gc.threshold = 1;
        let mut cx = vm.ctx();
        let t = cx.new_table();
        let s = cx.new_string("kept");
        cx.raw_set(t, Value::int(1), s).unwrap();
        let churn = cx.load_chunk("for i = 1, 5000 { let x = { tostring(i) } }", "churn.umbra").unwrap();
        cx.call(churn, &[]).unwrap();
        cx.call(churn, &[]).unwrap();
        let v = cx.raw_get(t, Value::int(1)).unwrap();
        assert_eq!(cx.to_str(v), Some("kept"));
    }

    #[test]
    fn ref_keeps_a_table_alive_until_released() {
        let mut vm = Vm::new();
        run_with_vm("collected = false", &mut vm).unwrap();
        let r = {
            let mut cx = vm.ctx();
            let t = exec(&mut cx, "return setmetatable({ hp = 42 }, { __gc = fn(self) { collected = true } })")[0];
            cx.make_ref(t)
        };
        // Only the Ref reaches the table now; collect and churn so a missed
        // root would be swept and its memory reused.
        for _ in 0..3 {
            vm.gc_collect();
            run_with_vm("for i = 1, 5000 { let t = { hp = i } }", &mut vm).unwrap();
        }
        {
            let mut cx = vm.ctx();
            let t = cx.get_ref(&r);
            assert_eq!(cx.get_field(t, "hp").unwrap().as_int(), Some(42));
            assert_eq!(cx.get_global("collected").as_bool(), Some(false));
            cx.release_ref(r);
        }
        vm.gc_collect();
        assert_eq!(vm.ctx().get_global("collected").as_bool(), Some(true));
    }

    #[test]
    fn ref_from_another_vm_is_refused_and_one_outliving_its_vm_is_inert() {
        let mut a = Vm::new();
        let mut b = Vm::new();
        let r = {
            let mut cx = a.ctx();
            let t = cx.new_table();
            cx.make_ref(t)
        };
        let wrong_vm = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            b.ctx().get_ref(&r);
        }));
        assert!(wrong_vm.is_err());
        drop(a);
        // r outlives `a` here and goes out of scope after it. It holds an
        // index and a VM id, no pointer, so there is nothing to dangle.
        assert!(format!("{r:?}").contains("slot"));
    }

    #[test]
    fn host_drives_metatable_class_objects() {
        let mut vm = Vm::new();
        let mut cx = vm.ctx();
        let class = exec(
            &mut cx,
            r#"let Spinner = {}
Spinner.__index = Spinner
fn Spinner.new(speed) { return setmetatable({ angle = 0, speed = speed }, Spinner) }
fn Spinner:update(dt) { self.angle = self.angle + self.speed * dt
    return self.angle }
fn Spinner:fail() { error("spinner jammed") }
return Spinner"#,
        )[0];

        // One instance built by script...
        let new = cx.get_field(class, "new").unwrap();
        let a = cx.call(new, &[Value::float(2.0)]).unwrap()[0];
        // ...and one built by the host, the way setmetatable would.
        let b = cx.new_table();
        cx.set_field(b, "angle", Value::float(10.0)).unwrap();
        cx.set_field(b, "speed", Value::float(-1.0)).unwrap();
        cx.set_metatable(b, Some(class)).unwrap();
        assert_eq!(cx.metatable(b).map(|m| m.raw_bits()), Some(class.raw_bits()));

        for _ in 0..3 {
            cx.call_method(a, "update", &[Value::float(0.5)]).unwrap();
            cx.call_method(b, "update", &[Value::float(0.5)]).unwrap();
        }
        assert_eq!(cx.get_field(a, "angle").unwrap().as_float(), Some(3.0));
        assert_eq!(cx.get_field(b, "angle").unwrap().as_float(), Some(8.5));
        // Script treats the host-built object as an ordinary instance.
        cx.set_global("b", b);
        exec(&mut cx, "assert(b:update(1) == 7.5)");

        let err = cx.call_method(a, "explode", &[]).unwrap_err();
        assert_eq!(err.message, "attempt to call a none value (method 'explode')");
        let err = cx.call_method(a, "fail", &[]).unwrap_err();
        assert_eq!(err.message, "test.umbra:6: spinner jammed");
        assert!(err.traceback.contains("test.umbra:6: in method 'fail'"), "{}", err.traceback);
        assert!(err.to_string().contains("\nstack traceback:\n"));

        // Strings resolve methods through the string library.
        let s = cx.new_string("spin");
        let up = cx.call_method(s, "upper", &[]).unwrap()[0];
        assert_eq!(cx.to_str(up), Some("SPIN"));
    }

    #[test]
    fn host_iterates_tables_and_bypasses_metamethods_with_raw_access() {
        let mut vm = Vm::new();
        let mut cx = vm.ctx();
        let t = exec(
            &mut cx,
            r#"return setmetatable({ 10, 20, 30, name = "x" }, {
    __index = fn(t, k) { return "default" },
    __newindex = fn(t, k, v) { rawset(t, k, v * 2) } })"#,
        )[0];
        assert_eq!(cx.raw_len(t).unwrap(), 3);

        let mut sum = 0;
        let mut names = Vec::new();
        let mut k = Value::nil();
        while let Some((nk, v)) = cx.next(t, k).unwrap() {
            match nk.as_int() {
                Some(_) => sum += v.as_int().unwrap(),
                None => names.push(cx.to_str(nk).unwrap().to_owned()),
            }
            k = nk;
        }
        assert_eq!((sum, names), (60, vec!["name".to_owned()]));
        // Same walk from script: a hash key used to restart the array part,
        // so pairs over a mixed table never ended.
        exec(
            &mut cx,
            r#"var n = 0
for k, v in pairs({ 10, 20, name = "x", [100] = 1 }) { n = n + 1
    assert(n <= 4, "pairs revisited a key") }
assert(n == 4)"#,
        );

        let missing = cx.get_field(t, "missing").unwrap();
        assert_eq!(cx.to_str(missing), Some("default"));
        let key = cx.new_string("missing");
        assert!(cx.raw_get(t, key).unwrap().is_nil());

        cx.set_field(t, "a", Value::int(5)).unwrap();
        let a = cx.new_string("a");
        assert_eq!(cx.raw_get(t, a).unwrap().as_int(), Some(10));
        cx.raw_set(t, a, Value::int(5)).unwrap();
        assert_eq!(cx.get(t, a).unwrap().as_int(), Some(5));

        cx.set_metatable(t, None).unwrap();
        assert!(cx.metatable(t).is_none());
        assert!(cx.get_field(t, "missing").unwrap().is_nil());

        // Wrong types are errors, not panics.
        assert!(cx.raw_get(Value::int(1), a).is_err());
        assert!(cx.next(Value::nil(), Value::nil()).is_err());
        assert!(cx.set_metatable(t, Some(Value::int(1))).is_err());
        assert!(cx.get(Value::bool(true), a).is_err());
    }

    struct Assets(std::collections::HashMap<String, String>);

    #[test]
    fn require_goes_through_the_host_loader_and_errors_name_the_asset() {
        let mut vm = Vm::new();
        vm.set_module_loader(|cx, name| {
            let path = format!("scripts/{}.umbra", name.replace('.', "/"));
            let assets = cx.host::<Assets>().ok_or("no asset store")?;
            match assets.0.get(&path) {
                Some(src) => Ok(Module { source: src.clone(), chunk_name: path }),
                None => Err(format!("module '{name}' not found in assets")),
            }
        });
        let mut assets = Assets(
            [
                ("scripts/util/math.umbra", "return { double = fn(x) { return x * 2 } }"),
                ("scripts/broken.umbra", "let x = 1\nlet y = none\nreturn y.z"),
                ("scripts/bad_syntax.umbra", "let = 1"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
        );

        let mut cx = vm.ctx();
        cx.with_host(&mut assets, |cx| {
            let r = exec(cx, "let m = require(\"util.math\")\nassert(require(\"util.math\") == m)\nreturn m.double(21)");
            assert_eq!(r[0].as_int(), Some(42));

            let main = cx.load_chunk("return require(\"broken\")", "main.umbra").unwrap();
            let err = cx.call(main, &[]).unwrap_err();
            assert_eq!(err.message, "scripts/broken.umbra:3: attempt to index a none value (local 'y')");

            let main = cx.load_chunk("require(\"bad_syntax\")", "main.umbra").unwrap();
            let err = cx.call(main, &[]).unwrap_err();
            assert!(err.message.starts_with("main.umbra:1: require: scripts/bad_syntax.umbra: line 1:"), "{}", err.message);

            let main = cx.load_chunk("require(\"nope\")", "main.umbra").unwrap();
            let err = cx.call(main, &[]).unwrap_err();
            assert_eq!(err.message, "main.umbra:1: require: module 'nope' not found in assets");
        });
        // Without the asset store lent, the loader has nothing to read from.
        let main = cx.load_chunk("require(\"other\")", "main.umbra").unwrap();
        let err = cx.call(main, &[]).unwrap_err();
        assert_eq!(err.message, "main.umbra:1: require: no asset store");
    }

    #[test]
    fn load_chunk_errors_name_the_chunk() {
        let mut vm = Vm::new();
        let mut cx = vm.ctx();
        let src = format!("{}fn spin(t) {{ error(\"spin failed\") }}\nspin(1)", "\n".repeat(11));
        let f = cx.load_chunk(&src, "scripts/spin.umbra").unwrap();
        let err = cx.call(f, &[]).unwrap_err();
        assert_eq!(err.message, "scripts/spin.umbra:12: spin failed");
        assert!(err.traceback.contains("scripts/spin.umbra:12: in function 'spin'"), "{}", err.traceback);
        assert!(err.traceback.contains("scripts/spin.umbra:13: in main chunk"), "{}", err.traceback);

        let err = cx.load_chunk("let = 1", "scripts/spin.umbra").unwrap_err();
        assert!(err.message.starts_with("scripts/spin.umbra: line 1:"), "{}", err.message);
    }

    #[test]
    fn host_calls_keep_the_step_limit_object_cap_and_panic_containment() {
        let mut vm = Vm::new();
        vm.set_step_limit(10_000);
        {
            let mut cx = vm.ctx();
            let f = cx.load_chunk("while true { }", "spin.umbra").unwrap();
            let err = cx.call(f, &[]).unwrap_err();
            assert!(err.message.ends_with("instruction budget exceeded"), "{err}");
        }
        vm.set_step_limit(0);
        vm.set_max_objects(vm.gc.live_count() + 50);
        {
            let mut cx = vm.ctx();
            let f = cx.load_chunk("let t = {}\nfor i = 1, 1000 { t[i] = {} }", "hog.umbra").unwrap();
            let err = cx.call(f, &[]).unwrap_err();
            assert!(err.message.ends_with("memory limit exceeded"), "{err}");
        }
        vm.set_max_objects(0);

        // A panicking native is contained, then the VM refuses further work.
        vm.set_global_cfn("boom", |_cx, _args| panic!("native bug"));
        let mut cx = vm.ctx();
        let boom = cx.get_global("boom");
        let err = cx.call(boom, &[]).unwrap_err();
        assert_eq!(err.message, "internal error (panic): native bug");
        let err = cx.call(boom, &[]).unwrap_err();
        assert!(err.message.contains("poisoned"), "{err}");
    }

    // umbra_register used to capture the state pointer at registration and
    // hand that to the C function while the interpreter was still holding
    // &mut Vm, so every umbra_* call inside the callback aliased a live
    // mutable borrow. The pointer is now made from the &mut Vm the
    // interpreter lends the native: the callback's accesses are reborrows
    // nested inside it, finished before the interpreter touches the VM
    // again. This drives the heaviest state use from inside a callback:
    // allocation, a nested pcall into script, and a full collection.
    #[test]
    fn api_callback_state_is_derived_from_the_natives_borrow() {
        use std::ffi::{CStr, CString};
        use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
        static STATE: AtomicPtr<api::UmbraState> = AtomicPtr::new(std::ptr::null_mut());
        static SAME_STATE: AtomicBool = AtomicBool::new(false);

        unsafe extern "C" fn churn(U: *mut api::UmbraState) -> std::ffi::c_int {
            // Same address the host created, so C hosts that key data by
            // state pointer keep working.
            SAME_STATE.store(U == STATE.load(Ordering::Relaxed), Ordering::Relaxed);
            let x = unsafe { api::umbra_tointeger(U, 1) };
            let helper = CString::new("helper").unwrap();
            unsafe { api::umbra_getglobal(U, helper.as_ptr()) };
            unsafe { api::umbra_pushinteger(U, x) };
            if unsafe { api::umbra_pcall(U, 1, 1) } != 0 {
                return -1;
            }
            unsafe { api::umbra_gc_collect(U) };
            let done = CString::new("done").unwrap();
            unsafe { api::umbra_pushstring(U, done.as_ptr()) };
            2
        }

        let U = api::umbra_newstate();
        STATE.store(U, Ordering::Relaxed);
        let name = CString::new("churn").unwrap();
        unsafe { api::umbra_register(U, name.as_ptr(), Some(churn)) };
        let src = CString::new(
            r#"fn helper(n) {
    let t = {}
    for i = 1, 100 { t[i] = tostring(i) }
    return #t + n
}
let a, b = churn(5)
result = tostring(a) .. " " .. b"#,
        )
        .unwrap();
        assert_eq!(unsafe { api::umbra_dostring(U, src.as_ptr()) }, 0);
        assert!(SAME_STATE.load(Ordering::Relaxed));
        let g = CString::new("result").unwrap();
        unsafe { api::umbra_getglobal(U, g.as_ptr()) };
        let s = unsafe { CStr::from_ptr(api::umbra_tostring(U, -1)) };
        assert_eq!(s.to_str().unwrap(), "105 done");
        unsafe { api::umbra_close(U) };
    }
}

// Natives as collected objects, and values that must stay reachable across
// a collection. Small enough to run under Miri, which is what catches a
// use-after-free here; a native run may read freed memory that still holds
// the old bytes.
#[cfg(test)]
mod heap_safety_tests {
    use super::*;
    use crate::value::Value;
    use crate::vm::Vm;

    #[test]
    fn per_call_natives_are_collected() {
        let mut vm = Vm::new();
        run_with_vm("x = 1", &mut vm).unwrap();
        vm.gc_collect();
        let baseline = vm.gc.live_count();
        run_with_vm(
            r#"keep = {}
for i = 1, 200 { keep[i] = string.gmatch("a b", "%a") }"#,
            &mut vm,
        )
        .unwrap();
        vm.gc_collect();
        let held = vm.gc.live_count();
        assert!(held >= baseline + 200, "iterators aren't heap objects: baseline {baseline} held {held}");
        run_with_vm("keep = none", &mut vm).unwrap();
        vm.gc_collect();
        vm.gc_collect();
        let after = vm.gc.live_count();
        assert!(after <= baseline + 4, "iterators leaked: baseline {baseline} after {after}");
    }

    #[test]
    fn native_upvalues_keep_their_values_alive() {
        let mut vm = Vm::new();
        let t = {
            let mut cx = vm.ctx();
            let t = cx.new_table();
            cx.set_field(t, "x", Value::int(7)).unwrap();
            t
        };
        // Past this point only the native's upvalue reaches the table.
        let f = vm.make_cfn_closure(&[t], |cx, _args, up| Ok(vec![cx.get_field(up[0], "x")?]));
        vm.set_global("get_x", f);
        vm.gc_collect();
        vm.gc_collect();
        run_with_vm("for i = 1, 100 { let junk = { i } }\nassert(get_x() == 7)", &mut vm).unwrap();
    }

    #[test]
    fn file_write_returns_its_handle_after_the_handle_is_dropped() {
        // Relative to the crate root (the test's working directory), so the
        // test also runs under Miri on an emulated target.
        let path = std::path::PathBuf::from(format!("target/umbra-write-{}.txt", std::process::id()));
        let mut vm = Vm::new();
        let src = format!("w = io.open({:?}, \"w\").write", path.to_str().unwrap());
        run_with_vm(&src, &mut vm).unwrap();
        vm.gc_collect();
        vm.gc_collect();
        run_with_vm(
            r#"for i = 1, 100 { let junk = { i } }
let h = w(none, "abc")
assert(type(h) == "table" and h.write == w)
h:close()"#,
            &mut vm,
        )
        .unwrap();
        drop(vm);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "abc");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pairs_and_ipairs_keep_their_iterators() {
        let mut vm = Vm::new();
        run_with_vm("next = none", &mut vm).unwrap();
        vm.gc_collect();
        vm.gc_collect();
        run_with_vm(
            r#"for i = 1, 100 { let junk = { i } }
var n = 0
for k, v in pairs({1, 2, x = 3}) { n = n + v }
for i, v in ipairs({4, 5}) { n = n + v }
assert(n == 15)"#,
            &mut vm,
        )
        .unwrap();
    }

    // A VM where a collector step is due right after any of its natives
    // return: each allocates 256KB, past the 4KB byte trigger set here and
    // past the step granule, whatever phase the collector is in. `fresh`
    // returns a new table {n = 2}, `fresh_fn` a new native returning 2,
    // `fresh_str` the string "fresh", `yes` true, and `collect` runs a full
    // collection.
    fn vm_with_due_steps() -> Vm {
        fn pad(cx: &mut crate::embed::Ctx<'_>) {
            cx.vm.alloc_str(&"x".repeat(256 * 1024));
        }
        let mut vm = Vm::new();
        vm.gc.byte_threshold = 4096;
        vm.set_global_cfn("fresh", |cx, _args| {
            pad(cx);
            let t = cx.new_table();
            cx.set_field(t, "n", Value::int(2))?;
            Ok(vec![t])
        });
        vm.set_global_cfn("fresh_fn", |cx, _args| {
            pad(cx);
            Ok(vec![cx.vm.make_cfn_val(|_cx, _args| Ok(vec![Value::int(2)]))])
        });
        vm.set_global_cfn("fresh_str", |cx, _args| {
            pad(cx);
            Ok(vec![cx.vm.alloc_str("fresh")])
        });
        vm.set_global_cfn("yes", |cx, _args| {
            pad(cx);
            Ok(vec![Value::bool(true)])
        });
        vm.set_global_cfn("collect", |cx, _args| {
            cx.vm.gc_collect();
            Ok(vec![])
        });
        vm
    }

    // Stress-test finding 27: GetField's __index result was dropped when a
    // step was due on return, leaving the register's old value in place.
    #[test]
    fn index_function_result_survives_a_due_step() {
        let mut vm = vm_with_due_steps();
        run_with_vm(
            r#"let mt = {__index = fresh}
for i = 1, 3 {
    let p = setmetatable({}, mt).transform
    assert(rawget(p, "n") == 2, "iteration ${tostring(i)}: p is not the __index result")
    let q = setmetatable({}, {__index = fresh_fn})
    assert(q:method() == 2)
    let r = setmetatable({}, mt)["k"]
    assert(rawget(r, "n") == 2)
}"#,
            &mut vm,
        )
        .unwrap();
    }

    // The same pattern in the compares: a checkpoint before the fused Jmp
    // was consumed ran that Jmp unconditionally.
    #[test]
    fn compare_metamethod_results_survive_a_due_step() {
        let mut vm = vm_with_due_steps();
        run_with_vm(
            r#"let mt = {__lt = yes, __le = yes, __eq = yes}
let a = setmetatable({}, mt)
let b = setmetatable({}, mt)
var hits = 0
for i = 1, 3 {
    if a < b { hits = hits + 1 }
    if a <= b { hits = hits + 1 }
    if a == b { hits = hits + 1 }
    if a < 5 { hits = hits + 1 }
    if a <= 5 { hits = hits + 1 }
    if a > 5 { hits = hits + 1 }
    if a >= 5 { hits = hits + 1 }
}
assert(hits == 21, "hits ${tostring(hits)}")"#,
            &mut vm,
        )
        .unwrap();
    }

    #[test]
    fn other_metamethod_results_survive_a_due_step() {
        let mut vm = vm_with_due_steps();
        run_with_vm(
            r#"let mt = {__len = fresh, __concat = fresh, __unm = fresh, __add = fresh,
    __div = fresh, __call = fresh, __tostring = fresh_str}
let o = setmetatable({}, mt)
for i = 1, 3 {
    assert(rawget(#o, "n") == 2)
    assert(rawget(o .. "x", "n") == 2)
    assert(rawget("x" .. o, "n") == 2)
    assert(rawget(-o, "n") == 2)
    assert(rawget(o + 1, "n") == 2)
    assert(rawget(o / 2, "n") == 2)
    assert(rawget(o(), "n") == 2)
    assert(tostring(o) == "fresh")
}"#,
            &mut vm,
        )
        .unwrap();
    }

    // A __call handler that drops its own metatable was reachable from
    // nothing while it ran: the isolated-call path recursed without
    // rooting it, and its frame reads upvalues out of the closure.
    #[test]
    fn call_handler_stays_alive_after_dropping_its_metatable() {
        let mut vm = vm_with_due_steps();
        run_with_vm(
            r#"let t = {}
setmetatable(t, {__call = (fn() {
    let tag = { "up" }
    return fn(self) {
        setmetatable(self, none)
        collect()
        for i = 1, 50 { let junk = { i } }
        return tag[1]
    }
})()})
let ok, v = pcall(t)
assert(ok and v == "up", tostring(v))"#,
            &mut vm,
        )
        .unwrap();
    }

    // Stress-test finding 18: table.sort's working copy was unrooted while
    // the comparator ran script.
    #[test]
    fn sort_keeps_elements_the_comparator_removed() {
        let mut vm = vm_with_due_steps();
        run_with_vm(
            r#"let t = { {v = 3}, {v = 1}, {v = 2} }
var emptied = false
table.sort(t, fn(a, b) {
    if not emptied {
        emptied = true
        t[1] = none  t[2] = none  t[3] = none
        collect()
        for i = 1, 50 { let junk = { i } }
    }
    return a.v < b.v
})
assert(t[1].v == 1 and t[2].v == 2 and t[3].v == 3)"#,
            &mut vm,
        )
        .unwrap();
    }
}

// Stress-test finding 10: recursion through isolated calls (pcall,
// metamethods, natives, require, resume) overflowed the Rust stack and
// aborted the process. The nesting limit turns it into a script error.
#[cfg(test)]
mod nesting_tests {
    use super::*;
    use crate::embed::Module;
    use crate::value::Value;
    use crate::vm::{MAX_NESTING, Vm};

    // A debug build spends about 88 KB of Rust stack per nesting level
    // (vm.rs, MAX_NESTING), past the 2 MB a test thread gets.
    fn on_big_stack(f: impl FnOnce() + Send + 'static) {
        std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap();
    }

    #[test]
    fn pcall_recursion_stops_at_the_limit() {
        on_big_stack(|| {
            let mut vm = Vm::new();
            for _ in 0..2 {
                run_with_vm(
                    r#"fn f(n) { return pcall(f, n + 1) }
let r = table.pack(f(0))
assert(r[r.n] == "stack overflow" and r[r.n - 1] == false, tostring(r[r.n]))
depth = r.n - 2"#,
                    &mut vm,
                )
                .unwrap();
                assert_eq!(vm.nesting, 0);
                // The chunk runs one level deep and each pcall that got in
                // added one. The same depth both times: the count came
                // back down after the caught error.
                let depth = vm.ctx().get_global("depth").as_int();
                assert_eq!(depth, Some(MAX_NESTING as i64 - 1));
            }
        });
    }

    #[test]
    fn index_metamethod_recursion_is_an_error() {
        on_big_stack(|| {
            let mut vm = Vm::new();
            let err = run_with_vm("let t = setmetatable({}, {__index = fn(t, k) { return t[k] }})\nprint(t.x)", &mut vm).unwrap_err();
            assert_eq!(err, "[string \"...\"]:1: stack overflow");
            assert_eq!(vm.nesting, 0);
            run_with_vm("let ok, e = pcall(fn() { return setmetatable({}, {__index = fn(t, k) { return t[k] }}).x })\nassert(not ok and e == \"[string \\\"...\\\"]:1: stack overflow\", e)", &mut vm).unwrap();
            assert_eq!(vm.nesting, 0);
        });
    }

    #[test]
    fn circular_require_is_an_error() {
        on_big_stack(|| {
            let mut vm = Vm::new();
            vm.set_module_loader(|_cx, name| {
                let other = if name == "a" { "b" } else { "a" };
                Ok(Module { source: format!("return require(\"{other}\")"), chunk_name: format!("{name}.umbra") })
            });
            let mut cx = vm.ctx();
            let main = cx.load_chunk("return require(\"a\")", "main.umbra").unwrap();
            let err = cx.call(main, &[]).unwrap_err();
            // The loading sentinel (finding 47) stops it before the limit.
            assert_eq!(err.message, "b.umbra:1: loop or previous error loading module 'a'");
            drop(cx);
            assert_eq!(vm.nesting, 0);
        });
    }

    #[test]
    fn host_native_script_recursion_is_an_error() {
        on_big_stack(|| {
            let mut vm = Vm::new();
            vm.set_global_cfn("call", |cx, args| Ok(cx.call(args[0], &args[1..])?));
            let mut cx = vm.ctx();
            let f = cx.load_chunk("fn f(n) { return call(f, n + 1) }\nreturn f", "rec.umbra").unwrap();
            let f = cx.call(f, &[]).unwrap()[0];
            // The innermost Ctx::call fails before any script runs, so it
            // takes the position of the script that called the native, and
            // each native passes the error on as is.
            let err = cx.call(f, &[Value::int(0)]).unwrap_err();
            assert_eq!(err.message, "rec.umbra:1: stack overflow");
            // Unwound and balanced: the host can call again right away.
            let ok = cx.load_chunk("return 1", "ok.umbra").unwrap();
            assert_eq!(cx.call(ok, &[]).unwrap()[0].as_int(), Some(1));
            drop(cx);
            assert_eq!(vm.nesting, 0);
        });
    }

    #[test]
    fn coroutine_resume_recursion_is_an_error() {
        on_big_stack(|| {
            let mut vm = Vm::new();
            let err = run_with_vm("fn f(n) { return coroutine.wrap(f)(n + 1) }\nf(0)", &mut vm).unwrap_err();
            assert!(err.ends_with("stack overflow"), "{err}");
            assert_eq!(vm.nesting, 0);
            // Sort comparators and __tostring count too.
            run_with_vm(
                r#"fn s(n) { table.sort({2, 1}, fn(a, b) { s(n + 1) return a < b }) }
let ok, e = pcall(s, 0)
assert(not ok and e:sub(-14) == "stack overflow", e)
fn ts(n) { return tostring(setmetatable({}, {__tostring = fn() { return ts(n + 1) }})) }
let ok2, e2 = pcall(ts, 0)
assert(not ok2 and e2:sub(-14) == "stack overflow", e2)"#,
                &mut vm,
            )
            .unwrap();
            assert_eq!(vm.nesting, 0);
        });
    }

    // A panic unwinds past the decrements between it and the catch; the
    // catch puts the count back.
    #[test]
    fn caught_panic_restores_the_count() {
        let mut vm = Vm::new();
        vm.set_global_cfn("panic_nested", |cx, _args| cx.vm.nested(|_| -> crate::vm::VmResult<Vec<Value>> { panic!("deep") }));
        let err = run_with_vm("panic_nested()", &mut vm).unwrap_err();
        assert!(err.contains("internal error (panic): deep"), "{err}");
        assert_eq!(vm.nesting, 0);
        assert!(vm.poisoned);
    }
}

// Who owns compiled code, and the caches kept in it.
#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::value::Value;
    use crate::vm::Vm;

    // Stress-test finding 16: Vm::exec(&Proto) left function values
    // pointing into a Proto the caller could drop. exec now takes the
    // chunk; its functions outlive the source, the AST and the call.
    #[test]
    fn functions_outlive_the_exec_that_defined_them() {
        let mut vm = Vm::new();
        let r = {
            let src = String::from("let k = \"twice\"\nreturn fn(x) { return k .. x * 2 }, fn(x) { return x + 1 }");
            let (block, errs) = parse(&src);
            assert!(errs.is_empty());
            let results = vm.exec(compile(block, None).unwrap()).unwrap();
            let mut cx = vm.ctx();
            for &v in &results {
                cx.root(v);
            }
            let t = cx.new_table();
            cx.raw_set(t, Value::int(1), results[0]).unwrap();
            cx.raw_set(t, Value::int(2), results[1]).unwrap();
            cx.make_ref(t)
        };
        for _ in 0..3 {
            vm.gc_collect();
            run_with_vm("for i = 1, 2000 { let junk = { tostring(i) } }", &mut vm).unwrap();
        }
        let mut cx = vm.ctx();
        let t = cx.get_ref(&r);
        let f = cx.raw_get(t, Value::int(1)).unwrap();
        let g = cx.raw_get(t, Value::int(2)).unwrap();
        let out = cx.call(f, &[Value::int(21)]).unwrap()[0];
        assert_eq!(cx.to_str(out), Some("twice42"));
        assert_eq!(cx.call(g, &[Value::int(1)]).unwrap()[0].as_int(), Some(2));
    }

    // The filled cache entries of function `f`'s proto: (owner, bits) for
    // each string constant and each GetGlobal site.
    fn caches(vm: &mut Vm, f: Value) -> Vec<(u64, u64)> {
        let p = vm.gc.closure_proto(f).unwrap();
        // SAFETY: the proto of a function value the caller keeps rooted.
        let p = unsafe { crate::heap::proto(p) };
        let mut out: Vec<(u64, u64)> = p
            .consts
            .iter()
            .filter_map(|c| match c {
                chunk::Const::Str(sc) => Some(sc.cached()),
                _ => None,
            })
            .collect();
        out.extend(p.global_cache.iter().map(|c| {
            let (owner, _, bits) = c.get();
            (owner, bits)
        }));
        out.retain(|&(_, bits)| bits != 0);
        out
    }

    fn call_greet(vm: &mut Vm) -> (String, Vec<(u64, u64)>) {
        let mut cx = vm.ctx();
        let f = cx.get_global("greet");
        let r = cx.call(f, &[]).unwrap()[0];
        let s = cx.to_str(r).unwrap().to_owned();
        drop(cx);
        let f = vm.get_global("greet");
        (s, caches(vm, f))
    }

    // Stress-test finding 32: the caches were keyed on the Vm's address,
    // so a Vm that moved refilled them all.
    #[test]
    fn a_moved_vm_keeps_its_caches() {
        let mut vm = Vm::new();
        run_with_vm("who = \"world\"\nfn greet() { return \"hello \" .. who }", &mut vm).unwrap();
        let (s, before) = call_greet(&mut vm);
        assert_eq!(s, "hello world");
        assert!(before.len() >= 2 && before.iter().all(|&(owner, _)| owner == vm.id), "{before:?}");
        let mut moved = Box::new(vm);
        let (s, after) = call_greet(&mut moved);
        assert_eq!(s, "hello world");
        assert_eq!(before, after);
    }

    // Stress-test finding 15: a Vm made at a dropped Vm's address passed
    // the address check on the dropped one's cache entries.
    #[test]
    fn a_vm_at_a_dropped_vms_address_has_its_own_caches() {
        let mut vm = Vm::new();
        let (addr_a, id_a) = ((&vm as *const Vm).addr(), vm.id);
        run_with_vm("who = \"a\"\nfn greet() { return \"hello \" .. who }", &mut vm).unwrap();
        let (s, a) = call_greet(&mut vm);
        assert_eq!(s, "hello a");
        assert!(a.iter().all(|&(owner, _)| owner == id_a));
        // Drops the first VM and builds the second in the same place.
        vm = Vm::new();
        assert_eq!((&vm as *const Vm).addr(), addr_a);
        assert_ne!(vm.id, id_a);
        run_with_vm("who = \"b\"\nfn greet() { return \"hello \" .. who }", &mut vm).unwrap();
        let (s, b) = call_greet(&mut vm);
        assert_eq!(s, "hello b");
        assert!(b.iter().all(|&(owner, _)| owner == vm.id), "{b:?}");
    }
}

// Stress-test finding 13: coroutines lived until the Vm dropped. They are
// collected objects now. Sized to run under Miri.
#[cfg(test)]
mod coroutine_gc_tests {
    use super::*;
    use crate::vm::Vm;

    // `collect()` runs a full collection; `live()` is the live object count.
    fn vm_with_collect() -> Vm {
        let mut vm = Vm::new();
        vm.set_global_cfn("collect", |cx, _args| {
            cx.vm.gc_collect();
            Ok(vec![])
        });
        vm.set_global_cfn("live", |cx, _args| Ok(vec![value::Value::int(cx.vm.gc.live_count() as i64)]));
        vm
    }

    #[test]
    fn short_lived_coroutines_are_collected() {
        let n: i64 = if cfg!(miri) { 300 } else { 100_000 };
        let mut vm = vm_with_collect();
        run_with_vm("x = 1", &mut vm).unwrap();
        vm.gc_collect();
        let baseline = vm.gc.live_count();
        // They are heap objects: held ones count.
        run_with_vm("keep = {}\nfor i = 1, 100 { keep[i] = coroutine.create(fn() { yield(1) }) }", &mut vm).unwrap();
        vm.gc_collect();
        assert!(vm.gc.live_count() >= baseline + 100);
        run_with_vm("keep = none", &mut vm).unwrap();
        // Suspended, finished and wrapped ones, all dropped at once.
        let src = format!(
            r#"var peak = 0
for i = 1, {n} {{
    let co = coroutine.create(fn(a) {{ let t = {{ a }} yield(t) return a }})
    coroutine.resume(co, i)
    if i % 2 == 0 {{ coroutine.resume(co) }}
    let w = coroutine.wrap(fn() {{ yield(i) }})
    w()
    if i % 100 == 0 {{ let l = live() if l > peak {{ peak = l }} }}
}}
return peak"#
        );
        let mut cx = vm.ctx();
        let f = cx.load_chunk(&src, "churn.umbra").unwrap();
        let peak = cx.call(f, &[]).unwrap()[0].as_int().unwrap() as usize;
        drop(cx);
        // Before, each iteration left two coroutines that only a Vm drop freed.
        assert!(peak < baseline + 20_000, "live count grew with the churn: peak {peak}, baseline {baseline}");
        vm.gc_collect();
        vm.gc_collect();
        let after = vm.gc.live_count();
        assert!(after <= baseline + 8, "coroutines leaked: baseline {baseline} after {after}");
    }

    #[test]
    fn suspended_coroutine_held_by_a_table_survives_and_resumes() {
        let mut vm = vm_with_collect();
        run_with_vm(
            r#"holder = { co = coroutine.create(fn(a) {
    let t = { "x", "y" }
    let s = "str" .. a
    let b = yield(1)
    assert(t[1] == "x" and t[2] == "y" and s == "str5" and b == 7)
    let c = yield(2)
    return t[2] .. s .. c
}) }
let ok, v = coroutine.resume(holder.co, 5)
assert(ok and v == 1)"#,
            &mut vm,
        )
        .unwrap();
        let churn = "for i = 1, 200 { let junk = { tostring(i) } }";
        for step in [
            "let ok, v = coroutine.resume(holder.co, 7)\nassert(ok and v == 2, v)",
            "let ok, v = coroutine.resume(holder.co, \"!\")\nassert(ok and v == \"ystr5!\", v)\nassert(coroutine.status(holder.co) == \"dead\")",
        ] {
            vm.gc_collect();
            run_with_vm(churn, &mut vm).unwrap();
            vm.gc_collect();
            run_with_vm(step, &mut vm).unwrap();
        }
        run_with_vm(
            r#"let ok, e = coroutine.resume(holder.co)
assert(not ok and e == "cannot resume dead coroutine")
weak = setmetatable({ holder.co }, { __mode = "v" })
holder = none"#,
            &mut vm,
        )
        .unwrap();
        vm.gc_collect();
        run_with_vm("assert(weak[1] == none)", &mut vm).unwrap();
    }

    // While a coroutine runs, its object holds its resumer's stack. A full
    // collection two coroutines deep must keep all three stacks.
    #[test]
    fn the_resume_chain_and_its_parked_stacks_survive_a_collection() {
        let mut vm = vm_with_collect();
        run_with_vm(
            r#"let main_data = { "main" }
let r = coroutine.wrap(fn() {
    let outer_data = { "outer" }
    var inner = none
    inner = coroutine.create(fn() {
        let inner_data = { "inner" }
        collect()
        for i = 1, 200 { let junk = { i } }
        collect()
        assert(coroutine.status(inner) == "running")
        yield(inner_data[1])
    })
    let ok, v = coroutine.resume(inner)
    collect()
    assert(coroutine.status(inner) == "suspended")
    return v .. " " .. outer_data[1]
})()
assert(r == "inner outer" and main_data[1] == "main", r)"#,
            &mut vm,
        )
        .unwrap();
    }

    // A coroutine scanned early in an incremental cycle runs again before
    // the cycle ends and stores new objects only in its own registers. Only
    // the atomic re-scan finds them.
    #[test]
    fn incremental_cycles_rescan_coroutine_stacks() {
        let n: i64 = if cfg!(miri) { 200 } else { 3000 };
        let mut vm = Vm::new();
        vm.gc.threshold = 1;
        vm.gc.byte_threshold = 1024;
        run_with_vm(
            &format!(
                r#"let gen = coroutine.wrap(fn() {{
    var keep = none
    for i = 1, {n} {{ keep = {{ i, tostring(i), keep }} yield(i) }}
    var k = {n}
    while keep != none {{
        assert(keep[1] == k and keep[2] == tostring(k), "lost link " .. tostring(k))
        keep = keep[3]
        k = k - 1
    }}
    return "ok"
}})
for i = 1, {n} {{ assert(gen() == i) let junk = {{ {{}}, {{}} }} }}
assert(gen() == "ok")"#
            ),
            &mut vm,
        )
        .unwrap();
    }
}

// Stress-test finding 14: compiled code was never freed, so every load,
// require and load_chunk (hot reload) leaked. Protos are collected objects
// now. Sized to run under Miri.
#[cfg(test)]
mod proto_gc_tests {
    use super::*;
    use crate::value::Value;
    use crate::vm::Vm;

    fn vm_with_collect() -> Vm {
        let mut vm = Vm::new();
        vm.set_global_cfn("collect", |cx, _args| {
            cx.vm.gc_collect();
            Ok(vec![])
        });
        vm.set_global_cfn("live", |cx, _args| Ok(vec![Value::int(cx.vm.gc.live_count() as i64)]));
        vm
    }

    #[test]
    fn dropped_chunks_are_collected() {
        let n: i64 = if cfg!(miri) { 100 } else { 10_000 };
        let mut vm = vm_with_collect();
        run_with_vm("x = 1", &mut vm).unwrap();
        vm.gc_collect();
        let baseline = vm.gc.live_count();
        // Each chunk has a nested function, an upvalue-less one and a
        // string constant of its own, which its proto caches once run.
        let src = format!(
            r#"var peak = 0
for i = 1, {n} {{
    let f = load("let s = \"s" .. i .. "\"\nreturn fn() {{ return s }}, fn() {{ return " .. i .. " }}")
    let a, b = f()
    assert(a() == "s" .. i and b() == i)
    if i % 100 == 0 {{ let l = live() if l > peak {{ peak = l }} }}
}}
return peak"#
        );
        let mut cx = vm.ctx();
        let f = cx.load_chunk(&src, "loads.umbra").unwrap();
        let peak = cx.call(f, &[]).unwrap()[0].as_int().unwrap() as usize;
        drop(cx);
        assert!(peak < baseline + 20_000, "live count grew with the loads: peak {peak}, baseline {baseline}");
        vm.gc_collect();
        vm.gc_collect();
        let after = vm.gc.live_count();
        assert!(after <= baseline + 8, "chunks leaked: baseline {baseline} after {after}");
    }

    // Hot reload from the host: a fresh chunk per reload, the old one
    // dropped with its Ref.
    #[test]
    fn reloaded_chunks_are_collected() {
        let n = if cfg!(miri) { 50 } else { 2000 };
        let mut vm = Vm::new();
        vm.gc_collect();
        let baseline = vm.gc.live_count();
        let mut current = None;
        for i in 0..n {
            let mut cx = vm.ctx();
            let f = cx.load_chunk(&format!("return {{ update = fn(self, dt) {{ return dt * {i} }}, name = \"v{i}\" }}"), "spin.umbra").unwrap();
            let module = cx.call(f, &[]).unwrap()[0];
            let r = cx.call_method(module, "update", &[Value::int(2)]).unwrap()[0];
            assert_eq!(r.as_int(), Some(2 * i));
            if let Some(old) = current.replace(cx.make_ref(module)) {
                cx.release_ref(old);
            }
        }
        vm.gc_collect();
        vm.gc_collect();
        let after = vm.gc.live_count();
        assert!(after <= baseline + 12, "reloads leaked: baseline {baseline} after {after}");
    }

    // The chunk and its source are gone; the functions it made, and the
    // functions those make, still run.
    #[test]
    fn functions_from_a_dropped_chunk_keep_working() {
        let n = if cfg!(miri) { 5 } else { 50 };
        let mut vm = vm_with_collect();
        run_with_vm(&format!(r#"keep = {{}}
for i = 1, {n} {{
    let a, b, c = load("let k = \"c" .. i .. "\"\nreturn fn(x) {{ return k .. x }}, fn() {{ return \"plain" .. i .. "\" }}, fn() {{ return fn() {{ return \"deep" .. i .. "\" }} }}")()
    keep[i] = {{ a, b, c }}
}}"#), &mut vm).unwrap();
        for _ in 0..2 {
            vm.gc_collect();
            run_with_vm("for i = 1, 300 { let junk = { tostring(i) } }", &mut vm).unwrap();
        }
        run_with_vm(
            &format!(
                r#"for i = 1, {n} {{
    let f = keep[i]
    assert(f[1]("x") == "c" .. i .. "x")
    assert(f[2]() == "plain" .. i)
    let inner = f[3]()
    collect()
    assert(inner() == "deep" .. i)
}}"#
            ),
            &mut vm,
        )
        .unwrap();
    }

    #[test]
    fn required_module_functions_keep_working() {
        let mut vm = vm_with_collect();
        vm.set_module_loader(|_cx, name| {
            Ok(crate::embed::Module {
                source: "let tag = \"mod\"\nreturn { f = fn() { return tag }, g = fn() { return \"g\" } }".into(),
                chunk_name: format!("{name}.umbra"),
            })
        });
        run_with_vm("m = require(\"m\")", &mut vm).unwrap();
        vm.gc_collect();
        run_with_vm("for i = 1, 300 { let junk = { tostring(i) } }\ncollect()\nassert(m.f() == \"mod\" and m.g() == \"g\" and require(\"m\") == m)", &mut vm)
            .unwrap();
    }

    // A proto scanned early in an incremental cycle caches a string
    // constant on first use. Only the barrier in intern_const_str keeps the
    // string from being swept while the cache still points at it.
    #[test]
    fn constants_cached_mid_cycle_survive() {
        let n = if cfg!(miri) { 30 } else { 200 };
        let mut vm = Vm::new();
        vm.gc.threshold = 1;
        vm.gc.byte_threshold = 1024;
        let body: String = (1..=n).map(|j| format!("if i == {j} {{ return \"const_{j}\" }}\n")).collect();
        run_with_vm(
            &format!(
                r#"let f = load("return fn(i) {{\n{body}}}")()
for round = 1, 3 {{
    for i = 1, {n} {{
        assert(f(i) == "const_" .. i, "round " .. round .. ": constant " .. i)
        let junk = {{ {{}}, {{}}, tostring(i) }}
    }}
}}"#,
                body = body.replace('"', "\\\"").replace('\n', "\\n")
            ),
            &mut vm,
        )
        .unwrap();
    }
}

// Stress-test finding 31: natives couldn't make natives through a Ctx.
#[cfg(test)]
mod ctx_function_tests {
    use super::*;
    use crate::value::Value;
    use crate::vm::Vm;

    #[test]
    fn a_native_returns_a_bound_method() {
        let mut vm = Vm::new();
        vm.gc.threshold = 1;
        vm.set_global_cfn("bind", |cx, args| {
            let (obj, name) = (args[0], args[1]);
            let method = cx.get(obj, name)?;
            let bound = cx.new_closure(
                |cx, args, up| {
                    let mut full = vec![up[0]];
                    full.extend_from_slice(args);
                    Ok(cx.call(up[1], &full)?)
                },
                &[obj, method],
            );
            Ok(vec![bound])
        });
        vm.set_global_cfn("collect", |cx, _args| {
            cx.vm.gc_collect();
            Ok(vec![])
        });
        // Only the bound closure's upvalues reach the object and its method
        // once `make` returns.
        run_with_vm(
            r#"fn make(n) {
    let o = { n = n, add = fn(self, x) { return self.n + x } }
    return bind(o, "add")
}
let f = make(5)
collect()
for i = 1, 300 { let junk = { tostring(i) } }
collect()
assert(f(3) == 8)
assert(f(10) == 15)"#,
            &mut vm,
        )
        .unwrap();
    }

    // A host builds a module table entirely through one Ctx, with the
    // collector running script in between.
    #[test]
    fn host_builds_natives_through_a_ctx() {
        let mut vm = Vm::new();
        vm.gc.threshold = 1;
        let mut cx = vm.ctx();
        let module = cx.new_table();
        let answer = cx.new_function(|_cx, _args| Ok(vec![Value::int(42)]));
        let label = cx.new_string("count");
        let counter = cx.new_table();
        cx.set_field(counter, "n", Value::int(0)).unwrap();
        let next = cx.new_closure(
            |cx, _args, up| {
                let n = cx.get_field(up[0], "n")?.as_int().unwrap_or(0) + 1;
                cx.set_field(up[0], "n", Value::int(n))?;
                let s = cx.to_str(up[1]).unwrap_or("?").to_owned();
                Ok(vec![cx.new_string(&format!("{s} {n}"))])
            },
            &[counter, label],
        );
        let churn = cx.load_chunk("for i = 1, 500 { let junk = { tostring(i) } }", "churn.umbra").unwrap();
        cx.call(churn, &[]).unwrap();
        cx.set_field(module, "answer", answer).unwrap();
        cx.set_field(module, "next", next).unwrap();
        cx.set_global("m", module);
        drop(cx);
        vm.gc_collect();
        run_with_vm(
            r#"for i = 1, 300 { let junk = { tostring(i) } }
assert(m.answer() == 42)
assert(m.next() == "count 1")
assert(m.next() == "count 2")"#,
            &mut vm,
        )
        .unwrap();
    }
}

// Stress-test findings 28 and 29: string interpolation.
#[cfg(test)]
mod interp_tests {
    use super::*;
    use crate::vm::Vm;

    fn out(src: &str) -> Vec<String> {
        run_capture(src).unwrap_or_else(|e| panic!("{e}"))
    }

    // Each part is what tostring() makes of it, as in print.
    #[test]
    fn parts_use_tostring() {
        assert_eq!(
            out(r#"let v = setmetatable({}, {__tostring = fn(v) { return "V" }})
print("a ${v}")
print("${v}")
print("${v}${v} and ${v}!")"#),
            ["a V", "V", "VV and V!"]
        );
        assert_eq!(out(r#"print("${true} ${false} ${none}")"#), ["true false none"]);
        assert_eq!(out(r#"print("${1} ${2.5} ${3.0} ${-0.0} ${1 // 0.0}")"#), ["1 2.5 3.0 -0.0 inf"]);
        assert_eq!(
            out(r#"let t = {}
print("${t} ${print} ${"s"}" == tostring(t) .. " " .. tostring(print) .. " s")"#),
            ["true"]
        );
        // __tostring may return a number, like tostring() allows.
        assert_eq!(
            out(r#"let n = setmetatable({}, {__tostring = fn() { return 7 }})
print("n=${n}")"#),
            ["n=7"]
        );
        // __concat is not consulted: interpolation isn't `..`.
        assert_eq!(
            out(r#"let c = setmetatable({}, {__concat = fn() { return "cat" }, __tostring = fn() { return "str" }})
print("${c}")"#),
            ["str"]
        );
    }

    #[test]
    fn a_script_tostring_does_not_change_it() {
        assert_eq!(
            out(r#"tostring = fn(x) { return "global" }
print("${1} ${true}")
let tostring = fn(x) { return "local" }
print("${2} ${none}")"#),
            ["1 true", "2 none"]
        );
    }

    #[test]
    fn tostring_errors_propagate() {
        let err = run("let t = setmetatable({}, {__tostring = fn() { return {} }})\n\nprint(\"${t}\")").unwrap_err();
        assert_eq!(err, "[string \"...\"]:3: '__tostring' must return a string");
        let err = run("let t = setmetatable({}, {__tostring = fn() { error(\"boom\") }})\nprint(\"x ${t}\")").unwrap_err();
        assert_eq!(err, "[string \"...\"]:1: boom");
    }

    // The ${...} parser starts at the line the expression is on.
    #[test]
    fn syntax_errors_cite_the_line() {
        let err = run("let x = 1\nlet y = 2\nprint(\"a ${x +}\")").unwrap_err();
        assert!(err.starts_with("line 3:"), "{err}");
        // After an escaped newline the expression is a line further down.
        let err = run("let x = 1\nprint(\"a\\\n${x +}\")").unwrap_err();
        assert!(err.starts_with("line 3:"), "{err}");
        let err = run("\n\nprint(\"${a b}\")").unwrap_err();
        assert!(err.starts_with("line 3:"), "{err}");
        // A function body records its errors instead of returning them;
        // they were dropped and the broken function ran anyway.
        let err = run("\nprint(\"${(fn() { let = 1 return 2 })()}\")").unwrap_err();
        assert!(err.starts_with("line 2:"), "{err}");
    }

    #[test]
    fn runtime_errors_cite_the_line() {
        let mut vm = Vm::new();
        let mut cx = vm.ctx();
        let f = cx.load_chunk("let t = none\n\n\nprint(\"x ${t.field}\")", "interp.umbra").unwrap();
        let err = cx.call(f, &[]).unwrap_err();
        assert_eq!(err.message, "interp.umbra:4: attempt to index a none value (local 't')");
        let f = cx.load_chunk("fn g() { error(\"in g\") }\nlet s = \"a\\\n${g()}\"", "interp.umbra").unwrap();
        let err = cx.call(f, &[]).unwrap_err();
        assert_eq!(err.message, "interp.umbra:1: in g");
        assert!(err.traceback.contains("interp.umbra:3: in main chunk"), "{}", err.traceback);
    }
}

// Stress-test findings 19 and 46: tracebacks run across isolated calls.
#[cfg(test)]
mod traceback_tests {
    use super::*;
    use crate::embed::{Ctx, Module, ScriptError};
    use crate::value::Value;
    use crate::vm::Vm;

    fn fail(cx: &mut Ctx<'_>, src: &str, name: &str) -> ScriptError {
        let f = cx.load_chunk(src, name).unwrap_or_else(|e| panic!("{e}"));
        cx.call(f, &[]).unwrap_err()
    }

    // The module's frames, then the frames of the script that required it.
    #[test]
    fn a_required_module_error_shows_the_whole_chain() {
        let mut vm = Vm::new();
        vm.set_module_loader(|_cx, name| Ok(Module { source: "fn boom() { error(\"deep\") }\nboom()".to_owned(), chunk_name: format!("{name}.umbra") }));
        let mut cx = vm.ctx();
        let err = fail(&mut cx, "fn get() { let m = require(\"mod\") return m }\nget()", "main.umbra");
        assert_eq!(err.message, "mod.umbra:1: deep");
        assert_eq!(
            err.traceback,
            "\tmod.umbra:1: in function 'boom'\n\tmod.umbra:2: in main chunk\n\
            \tmain.umbra:1: in function 'get'\n\tmain.umbra:2: in main chunk"
        );
    }

    #[test]
    fn an_index_metamethod_error_shows_the_whole_chain() {
        let mut vm = Vm::new();
        let mut cx = vm.ctx();
        let err = fail(
            &mut cx,
            r#"let t = setmetatable({}, {__index = fn(t, k) { error("no field " .. k) }})
fn read() { let v = t.missing return v }
read()"#,
            "idx.umbra",
        );
        assert_eq!(err.message, "idx.umbra:1: no field missing");
        assert_eq!(
            err.traceback,
            "\tidx.umbra:1: in function <idx.umbra:1>\n\
            \tidx.umbra:2: in function 'read'\n\tidx.umbra:3: in main chunk"
        );
    }

    // A native that passes on a failed Ctx::call: the callback's frames,
    // then the script that called the native.
    #[test]
    fn an_error_through_a_hosts_call_shows_the_whole_chain() {
        let mut vm = Vm::new();
        vm.set_global_cfn("call", |cx, args| Ok(cx.call(args[0], &args[1..])?));
        let mut cx = vm.ctx();
        let err = fail(&mut cx, "fn inner() { error(\"in callback\") }\nfn outer() { call(inner) }\nouter()", "cb.umbra");
        assert_eq!(err.message, "cb.umbra:1: in callback");
        assert_eq!(
            err.traceback,
            "\tcb.umbra:1: in function 'inner'\n\
            \tcb.umbra:2: in function 'outer'\n\tcb.umbra:3: in main chunk"
        );
    }

    // Finding 46: an error raised before any script ran in a Ctx::call
    // takes the position of the script that called the native.
    #[test]
    fn an_error_before_any_script_ran_keeps_a_position() {
        let mut vm = Vm::new();
        vm.set_global_cfn("call", |cx, args| Ok(cx.call(args[0], &args[1..])?));
        let mut cx = vm.ctx();
        let err = fail(&mut cx, "let x = 1\ncall(none)", "n.umbra");
        assert_eq!(err.message, "n.umbra:2: attempt to call a none value");
        assert_eq!(err.traceback, "\tn.umbra:2: in main chunk");
        let f = cx.load_chunk("let ok, e = pcall(call, none)\nreturn e", "n.umbra").unwrap();
        let e = cx.call(f, &[]).unwrap()[0];
        assert_eq!(cx.to_str(e), Some("n.umbra:1: attempt to call a none value"));
        // The host itself has no script position to give.
        let err = cx.call(Value::nil(), &[]).unwrap_err();
        assert_eq!(err.message, "attempt to call a none value");
        assert_eq!(err.traceback, "");
    }

    #[test]
    fn a_wrapped_coroutine_error_shows_the_resumers_stack_too() {
        let mut vm = Vm::new();
        let mut cx = vm.ctx();
        let err = fail(&mut cx, "let gen = coroutine.wrap(fn() { error(\"in co\") })\nfn drive() { gen() }\ndrive()", "co.umbra");
        assert_eq!(err.message, "co.umbra:1: in co");
        assert_eq!(
            err.traceback,
            "\tco.umbra:1: in function <co.umbra:1>\n\
            \tco.umbra:2: in function 'drive'\n\tco.umbra:3: in main chunk"
        );
    }

    // An xpcall handler sees its own frames, then the stack the error was
    // raised on, even after a pcall of its own failed.
    #[test]
    fn debug_traceback_in_a_handler_shows_the_error_stack() {
        let out = run_capture(
            r#"fn fail() { error("x") }
fn handler(e) { pcall(fn() { error("inner") }) return debug.traceback(e) }
fn run() { let ok, tb = xpcall(fail, handler) return tb }
print(run())"#,
        )
        .unwrap();
        assert_eq!(
            out,
            ["[string \"...\"]:1: x\nstack traceback:\n\
            \t[string \"...\"]:2: in function 'handler'\n\
            \t[string \"...\"]:1: in function 'fail'\n\
            \t[string \"...\"]:3: in function 'run'\n\
            \t[string \"...\"]:4: in main chunk"]
        );
    }

    // Only the first enrichment records: an error that was handled leaves
    // nothing behind for debug.traceback or for the next error.
    #[test]
    fn a_handled_error_leaves_no_traceback_behind() {
        let out = run_capture("fn fail() { error(\"first\") }\npcall(fail)\nprint(debug.traceback(\"now\"))").unwrap();
        assert_eq!(out, ["now\nstack traceback:\n\t[string \"...\"]:3: in main chunk"]);
        let mut vm = Vm::new();
        let mut cx = vm.ctx();
        let err = fail(&mut cx, "fn fail() { error(\"first\") }\nfn again() { pcall(fail) error(\"second\") }\nagain()", "t.umbra");
        assert_eq!(err.message, "t.umbra:2: second");
        assert_eq!(err.traceback, "\tt.umbra:2: in function 'again'\n\tt.umbra:3: in main chunk");
    }

    // Stress-test finding 37: frames the call site can't name (here, host
    // calls) are named after their declaration.
    #[test]
    fn frames_are_named_after_their_declaration() {
        let mut vm = Vm::new();
        let mut cx = vm.ctx();
        let f = cx
            .load_chunk(
                r#"let A = {}
fn A:update() { error("e") }
fn A.make() { error("e") }
fn top() { error("e") }
let lf = fn() { error("e") }
var vf = fn() { error("e") }
let fn lfn() { error("e") }
let anon = { f = fn() { error("e") } }
let p, q = fn() { error("e") }, fn() { error("e") }
return A, top, lf, vf, lfn, anon.f, p, q"#,
                "n.umbra",
            )
            .unwrap();
        let r = cx.call(f, &[]).unwrap();
        let err = cx.call_method(r[0], "update", &[]).unwrap_err();
        assert_eq!(err.traceback, "\tn.umbra:2: in method 'update'");
        let make = cx.get_field(r[0], "make").unwrap();
        let mut tracebacks = vec![cx.call(make, &[]).unwrap_err().traceback];
        for &f in &r[1..] {
            tracebacks.push(cx.call(f, &[]).unwrap_err().traceback);
        }
        assert_eq!(
            tracebacks,
            [
                "\tn.umbra:3: in function 'A.make'",
                "\tn.umbra:4: in function 'top'",
                "\tn.umbra:5: in function 'lf'",
                "\tn.umbra:6: in function 'vf'",
                "\tn.umbra:7: in function 'lfn'",
                "\tn.umbra:8: in function <n.umbra:8>",
                "\tn.umbra:9: in function 'p'",
                "\tn.umbra:9: in function 'q'",
            ]
        );
        // Where the call site names the function, that name wins, as in Lua.
        let err = fail(&mut cx, "fn top() { error(\"e\") }\nlet g = top\nfn run() { g() }\nrun()", "c.umbra");
        assert_eq!(err.traceback, "\tc.umbra:1: in upvalue 'g'\n\tc.umbra:3: in function 'run'\n\tc.umbra:4: in main chunk");
    }

    // Stress-test finding 20: load's syntax errors name the chunk.
    #[test]
    fn load_syntax_errors_name_the_chunk() {
        let out = run_capture(
            r#"let f, e = load("let x = 1\nlet = 2", "cfg.umbra")
print(f, e)
let g, e2 = load("let = 1")
print(g, e2)"#,
        )
        .unwrap();
        assert_eq!(out, ["none\tcfg.umbra: line 2: expected identifier", "none\t[string \"...\"]: line 1: expected identifier"]);
    }

    // Nor does a finalizer's, though it runs in the middle of script.
    #[test]
    fn a_finalizer_error_leaves_no_traceback_behind() {
        let mut vm = Vm::new();
        vm.gc.threshold = 1;
        let mut cx = vm.ctx();
        let err = fail(
            &mut cx,
            r#"fn gc_fail() { ran = true error("in gc") }
setmetatable({}, {__gc = gc_fail})
for i = 1, 5000 { let junk = { i } }
if not ran { error("no finalizer ran") }
error("after")"#,
            "g.umbra",
        );
        assert_eq!(err.message, "g.umbra:5: after");
        assert_eq!(err.traceback, "\tg.umbra:5: in main chunk");
    }
}

// Stress-test finding 24: collectgarbage, and tostring identities.
#[cfg(test)]
mod collectgarbage_tests {
    use super::*;

    fn out(src: &str) -> Vec<String> {
        run_capture(src).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn collect_frees_and_count_reports_kilobytes() {
        assert_eq!(
            out(r#"let weak = setmetatable({}, {__mode = "v"})
weak[1] = {}
print(collectgarbage("collect"), weak[1])
weak[1] = {}
print(collectgarbage(), weak[1])
let before = collectgarbage("count")
print(math.type(before), before > 0)
var big = {}
for i = 1, 100000 { big[i] = i }
let grown = collectgarbage("count")
print(grown - before > 100000 * 8 / 1024)
big = none
collectgarbage()
print(collectgarbage("count") < grown)"#),
            ["0\tnone", "0\tnone", "float\ttrue", "true", "true"]
        );
    }

    #[test]
    fn step_reports_a_finished_cycle() {
        assert_eq!(
            out(r#"var steps = 0
while not collectgarbage("step") { steps = steps + 1 }
print(steps > 0, type(collectgarbage("step")))"#),
            ["true\tboolean"]
        );
    }

    #[test]
    fn other_options_are_errors() {
        let err = run("collectgarbage(\"stop\")").unwrap_err();
        assert_eq!(err, "[string \"...\"]:1: collectgarbage: invalid option 'stop'");
        let err = run("collectgarbage(1)").unwrap_err();
        assert_eq!(err, "[string \"...\"]:1: collectgarbage: string expected");
    }

    #[test]
    fn tostring_prints_a_stable_identity() {
        assert_eq!(
            out(r#"let t = {}
let co = coroutine.create(fn() {})
let b = bytes.new(4)
let up = 1
for _, v in ipairs({t, print, fn() {}, fn() { return up }, co, b}) {
    let s = tostring(v)
    print(string.match(s, "^(%a+): 0x%x+$"), s == tostring(v), string.format("%p", v) == string.sub(s, #type(v) + 3))
}
print(tostring({}) != tostring(t))
let named = setmetatable({}, {__tostring = fn() { return "named" }})
print(named, "${named}")"#),
            [
                "table\ttrue\ttrue",
                "function\ttrue\ttrue",
                "function\ttrue\ttrue",
                "function\ttrue\ttrue",
                "coroutine\ttrue\ttrue",
                "bytes\ttrue\ttrue",
                "true",
                "named\tnamed",
            ]
        );
        // print and interpolation show the same identity.
        assert_eq!(out("let t = {}\nprint(t == t, tostring(t) == \"${t}\")"), ["true\ttrue"]);
        let printed = out("let t = {}\nprint(t)\nprint(tostring(t))");
        assert_eq!(printed[0], printed[1]);
    }
}

// Stress-test findings 35 and 23: the nil value is spelled `none`
// everywhere a script or host sees it, and can't be a key to store under.
#[cfg(test)]
mod none_tests {
    use super::*;
    use crate::value::Value;
    use crate::vm::Vm;

    fn out(src: &str) -> Vec<String> {
        run_capture(src).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn none_is_spelled_none() {
        assert_eq!(
            out(r#"print(none, type(none), tostring(none), "${none}", math.type("x"))
print(string.format("%s %q", none, none))
print(load("return " .. string.format("%q", none))() == none)
let t = {}
let ok, e = pcall(fn() { return t.a.b })
print(e)"#),
            ["none\tnone\tnone\tnone\tnone", "none none", "true", "[string \"...\"]:5: attempt to index a none value (field 'a')"]
        );
        assert_eq!(Value::nil().to_string(), "none");
        assert_eq!(format!("{:?}", Value::nil()), "none");
        let err = run("let f = none\nf()").unwrap_err();
        assert_eq!(err, "[string \"...\"]:2: attempt to call a none value (local 'f')");
    }

    #[test]
    fn storing_under_a_none_key_is_an_error() {
        for src in [
            "let t = {}\nt[none] = 1",
            "let t = {}\nt[0/0] = 1",
            "let t = {}\nt[none] = none",
            "let k = none\nlet t = {[k] = 1}",
            "let t = {}\nrawset(t, none, 1)",
            // A __newindex table passes the key on to the store.
            "let t = setmetatable({}, {__newindex = {}})\nt[none] = 1",
        ] {
            assert_eq!(run(src).unwrap_err(), "[string \"...\"]:2: index is none", "{src}");
        }
        // Reading one is fine, as in Lua; and a __newindex function sees the
        // none key like any other.
        assert_eq!(
            out(r#"let t = {1}
print(t[none], rawget(t, none))
var seen = false
let p = setmetatable({}, {__newindex = fn(t, k, v) { seen = k == none and v == 2 }})
p[none] = 2
print(seen)"#),
            ["none\tnone", "true"]
        );
    }

    #[test]
    fn the_host_cant_store_under_a_none_key_either() {
        let mut vm = Vm::new();
        let mut cx = vm.ctx();
        let t = cx.new_table();
        assert_eq!(cx.raw_set(t, Value::nil(), Value::int(1)).unwrap_err().message, "index is none");
        assert_eq!(cx.set(t, Value::float(f64::NAN), Value::int(1)).unwrap_err().message, "index is none");
        assert!(cx.raw_get(t, Value::nil()).unwrap().is_nil());
        assert!(cx.next(t, Value::nil()).unwrap().is_none());
        // From a native the error carries the calling script's position.
        drop(cx);
        vm.set_global_cfn("store", |cx, args| {
            cx.raw_set(args[0], args[1], args[2])?;
            Ok(vec![])
        });
        let err = run_with_vm("let t = {}\nstore(t, none, 1)", &mut vm).unwrap_err();
        assert_eq!(err, "[string \"...\"]:2: index is none");
    }
}

// Stress-test finding 47: require's loading sentinel.
#[cfg(test)]
mod require_tests {
    use crate::embed::Module;
    use crate::vm::Vm;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    // A Vm whose loader serves `modules` and counts the loads.
    fn vm_with(modules: Rc<RefCell<Vec<(&'static str, String)>>>, loads: Rc<Cell<u32>>) -> Vm {
        let mut vm = Vm::new();
        vm.set_module_loader(move |_cx, name| {
            loads.set(loads.get() + 1);
            let src = modules.borrow().iter().find(|(n, _)| *n == name).map(|(_, s)| s.clone());
            let source = src.ok_or_else(|| format!("no module '{name}'"))?;
            Ok(Module { source, chunk_name: format!("{name}.umbra") })
        });
        vm
    }

    #[test]
    fn requiring_a_module_that_is_still_loading_is_an_error() {
        let modules = Rc::new(RefCell::new(vec![
            ("a", "return require(\"b\")".to_owned()),
            ("b", "return require(\"a\")".to_owned()),
            ("me", "let x = 1\nreturn require(\"me\")".to_owned()),
        ]));
        let loads = Rc::new(Cell::new(0));
        let mut vm = vm_with(modules, loads.clone());
        let mut cx = vm.ctx();
        let main = cx.load_chunk("return require(\"a\")", "main.umbra").unwrap();
        let err = cx.call(main, &[]).unwrap_err();
        assert_eq!(err.message, "b.umbra:1: loop or previous error loading module 'a'");
        // Each module loaded once, not once per nesting level.
        assert_eq!(loads.get(), 2);
        let main = cx.load_chunk("return require(\"me\")", "main.umbra").unwrap();
        let err = cx.call(main, &[]).unwrap_err();
        assert_eq!(err.message, "me.umbra:2: loop or previous error loading module 'me'");
        // Caught, the cycle leaves nothing marked as loading.
        let main = cx.load_chunk("let ok, e = pcall(require, \"a\")\nreturn ok", "main.umbra").unwrap();
        assert_eq!(cx.call(main, &[]).unwrap()[0].as_bool(), Some(false));
        let main = cx.load_chunk("let ok, e = pcall(require, \"a\")\nreturn e", "main.umbra").unwrap();
        let e = cx.call(main, &[]).unwrap()[0];
        assert_eq!(cx.to_str(e), Some("b.umbra:1: loop or previous error loading module 'a'"));
    }

    #[test]
    fn a_module_whose_load_failed_is_not_cached() {
        let modules = Rc::new(RefCell::new(vec![("m", "error(\"not yet\")".to_owned())]));
        let loads = Rc::new(Cell::new(0));
        let mut vm = vm_with(modules.clone(), loads.clone());
        let mut cx = vm.ctx();
        let main = cx.load_chunk("return require(\"m\")", "main.umbra").unwrap();
        assert_eq!(cx.call(main, &[]).unwrap_err().message, "m.umbra:1: not yet");
        // Neither cached nor left marked as loading: the next require loads
        // it again, and once it loads, it is cached.
        modules.borrow_mut()[0].1 = "return 42".to_owned();
        assert_eq!(cx.call(main, &[]).unwrap()[0].as_int(), Some(42));
        assert_eq!(cx.call(main, &[]).unwrap()[0].as_int(), Some(42));
        assert_eq!(loads.get(), 2);
        // A loader error caches nothing either.
        let main = cx.load_chunk("return require(\"gone\")", "main.umbra").unwrap();
        assert_eq!(cx.call(main, &[]).unwrap_err().message, "main.umbra:1: require: no module 'gone'");
        modules.borrow_mut().push(("gone", "return 7".to_owned()));
        assert_eq!(cx.call(main, &[]).unwrap()[0].as_int(), Some(7));
    }
}

// Stress-test finding 9: every pointer goes into the NaN box with its
// provenance exposed and comes back out with it (value.rs box_ptr and
// unbox_ptr). Each kind is unboxed and dereferenced here, which Miri checks.
#[cfg(test)]
mod provenance_tests {
    use crate::value::Value;
    use crate::vm::Vm;

    #[test]
    fn every_kind_of_boxed_pointer_comes_back_usable() {
        let mut vm = Vm::new();
        let mut cx = vm.ctx();
        let t = cx.new_table();
        let s = cx.new_string("abc");
        let big = cx.new_int(i64::MAX);
        let f = cx.new_function(|_cx, args| Ok(vec![args[0]]));
        cx.set_field(t, "s", s).unwrap();
        cx.set_field(t, "big", big).unwrap();
        let got = cx.get_field(t, "s").unwrap();
        assert_eq!(cx.to_str(got), Some("abc"));
        assert_eq!(cx.get_field(t, "big").unwrap().as_int(), Some(i64::MAX));
        assert_eq!(cx.call(f, &[Value::int(5)]).unwrap()[0].as_int(), Some(5));
        // The native's tag bit stays out of the pointer the collector and
        // the call path use.
        assert!(crate::heap::is_native(f));
        assert_eq!(f.as_userdata().unwrap().addr() as u64 & crate::heap::NATIVE_BIT, 0);
        assert_eq!(f.gc_ptr(), f.as_userdata());
        // Closures, upvalue-less functions, coroutines and bytes, from script.
        let chunk = cx
            .load_chunk(
                r#"let up = 2
let add = fn(x) { return x + up }
let plain = fn(x) { return x * 3 }
let co = coroutine.wrap(fn(a) { let b = yield(a + 1) return b * 2 })
let b = bytes.new(2)
b[1] = 9
return add(1), plain(2), co(1), co(5), b[1], #b"#,
                "p.umbra",
            )
            .unwrap();
        let r: Vec<i64> = cx.call(chunk, &[]).unwrap().iter().map(|v| v.as_int().unwrap()).collect();
        assert_eq!(r, [3, 6, 2, 10, 9, 2]);
        drop(cx);
        vm.gc_collect();
    }
}

// Stress-test findings 22 and 33: build.rs writes umbra.h only when its
// bytes change, with the LF line endings .gitattributes tells git to
// expect, so a build leaves the tracked header as git checked it out.
#[cfg(test)]
mod header_tests {
    #[test]
    fn the_header_has_the_line_endings_git_expects() {
        assert!(!include_bytes!("../umbra.h").contains(&b'\r'));
        assert!(include_str!("../.gitattributes").lines().any(|l| l.trim() == "umbra.h text eol=lf"));
    }
}

// Stress-test finding 21: math.random's state belongs to its VM.
#[cfg(test)]
mod random_tests {
    use super::*;
    use crate::vm::Vm;

    fn draws(vm: &mut Vm, n: usize) -> Vec<i64> {
        let mut cx = vm.ctx();
        let f = cx.load_chunk("return math.random(1, 1000000)", "r.umbra").unwrap();
        (0..n).map(|_| cx.call(f, &[]).unwrap()[0].as_int().unwrap()).collect()
    }

    #[test]
    fn vms_on_one_thread_have_their_own_sequences() {
        let mut a = Vm::new();
        let mut b = Vm::new();
        // Interleaved, each VM still draws the sequence it would alone.
        let (mut seq_a, mut seq_b) = (Vec::new(), Vec::new());
        for _ in 0..5 {
            seq_a.extend(draws(&mut a, 1));
            seq_b.extend(draws(&mut b, 1));
        }
        assert_eq!(seq_a, draws(&mut Vm::new(), 5));
        assert_eq!(seq_a, seq_b);
        // randomseed reseeds its own VM and no other.
        run_with_vm("math.randomseed(42)", &mut a).unwrap();
        let seeded = draws(&mut a, 3);
        assert_eq!(draws(&mut b, 3), draws(&mut Vm::new(), 8)[5..]);
        let mut c = Vm::new();
        run_with_vm("math.randomseed(42)", &mut c).unwrap();
        assert_eq!(seeded, draws(&mut c, 3));
    }
}
