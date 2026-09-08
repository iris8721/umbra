// umbra — command-line driver: `umbra file.umbra [args...]`, `umbra -e 'code'`,
// or a stdin REPL with no arguments.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use umbra::value::Value;
use umbra::vm::{RtString, Table, Vm};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let mut evals: Vec<String> = Vec::new();
    let mut script: Option<String> = None;
    let mut script_args: Vec<String> = Vec::new();

    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "-e" => {
                i += 1;
                if i >= argv.len() {
                    eprintln!("umbra: -e needs an argument");
                    return ExitCode::from(2);
                }
                evals.push(argv[i].clone());
            }
            "--" => {
                i += 1;
                if i < argv.len() {
                    script = Some(argv[i].clone());
                    script_args = argv[i + 1..].to_vec();
                }
                break;
            }
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("umbra: unknown option '{s}'");
                return ExitCode::from(2);
            }
            s => {
                script = Some(s.to_owned());
                script_args = argv[i + 1..].to_vec();
                break;
            }
        }
        i += 1;
    }

    if evals.is_empty() && script.is_none() {
        repl();
        return ExitCode::SUCCESS;
    }

    let mut vm = Vm::new();
    install_arg(&mut vm, &argv[0], script.as_deref(), &script_args);

    for src in &evals {
        if let Err(e) = run_src(&mut vm, src) {
            eprintln!("umbra: {e}");
            return ExitCode::FAILURE;
        }
    }
    if let Some(path) = &script {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("umbra: cannot read {path}: {e}");
                return ExitCode::FAILURE;
            }
        };
        if let Err(e) = run_src(&mut vm, &src) {
            eprintln!("umbra: {e}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

fn run_src(vm: &mut Vm, src: &str) -> Result<Vec<Value>, String> {
    let (block, errs) = umbra::parse(src);
    if !errs.is_empty() {
        return Err(errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"));
    }
    let proto = umbra::compile(block, None).map_err(|e| e.to_string())?;
    vm.exec_owned(proto).map_err(|e| e.to_string())
}

// Lua's convention: arg[0] is the script path, arg[1..] its arguments, and
// arg[-1] the interpreter itself.
fn install_arg(vm: &mut Vm, prog: &str, script: Option<&str>, args: &[String]) {
    let ptr = Box::into_raw(Box::new(Table::new()));
    vm.gc.register(ptr as *mut u8);
    let t = unsafe { &mut *ptr };
    t.raw_set(vm.make_int(-1), vm.intern_pub(prog));
    if let Some(s) = script {
        t.raw_set(Value::int(0), vm.intern_pub(s));
    }
    for (i, a) in args.iter().enumerate() {
        let v = vm.intern_pub(a);
        t.raw_set(vm.make_int(i as i64 + 1), v);
    }
    vm.set_global("arg", Value::table(ptr as *mut u8));
}

fn repl() {
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    let mut vm = Vm::new();
    let mut line = String::new();
    loop {
        if interactive {
            print!("> ");
            let _ = std::io::stdout().flush();
        }
        line.clear();
        match stdin.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let src = line.trim();
        if src.is_empty() { continue; }
        // A bare expression isn't a statement; retry it as `return <line>`
        // so `1+1` prints 2 instead of a parse error.
        let src = match umbra::parse(src) {
            (_, errs) if errs.is_empty() => src.to_owned(),
            _ => format!("return {src}"),
        };
        match run_src(&mut vm, &src) {
            Ok(vals) if !vals.is_empty() => {
                let tostring = vm.get_global("tostring");
                let mut out = Vec::with_capacity(vals.len());
                for v in vals {
                    out.push(stringify(&mut vm, tostring, v));
                }
                println!("{}", out.join("\t"));
            }
            Ok(_) => {}
            Err(e) => eprintln!("{e}"),
        }
    }
}

// tostring() honors __tostring; fall back to the plain Display form.
fn stringify(vm: &mut Vm, tostring: Value, v: Value) -> String {
    let s = vm
        .call_value_isolated(tostring, &[v])
        .ok()
        .and_then(|r| r.into_iter().next());
    match s {
        Some(s) if s.is_string() => unsafe {
            let r = &*s.as_string().unwrap().cast::<RtString>();
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(r.as_c_ptr(), r.len))
                .to_owned()
        },
        _ => format!("{v}"),
    }
}
