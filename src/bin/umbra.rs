// umbra — command-line driver: `umbra file.umbra [args...]`, `umbra -e 'code'`,
// or a stdin REPL with no arguments.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use umbra::embed::Ctx;
use umbra::value::Value;
use umbra::vm::Vm;

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
        if let Err(e) = run_src(&mut vm, src, None) {
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
        if let Err(e) = run_src(&mut vm, &src, Some(path)) {
            eprintln!("umbra: {e}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

// A script file's errors cite its path (`scripts/x.umbra:3: ...`); a -e
// string or REPL line has none.
fn run_src(vm: &mut Vm, src: &str, chunk_name: Option<&str>) -> Result<Vec<Value>, String> {
    let (block, errs) = umbra::parse(src);
    if !errs.is_empty() {
        let msg = errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
        return Err(match chunk_name {
            Some(name) => format!("{name}: {msg}"),
            None => msg,
        });
    }
    let chunk = umbra::compile(block, chunk_name.map(str::to_owned)).map_err(|e| e.to_string())?;
    vm.exec(chunk).map_err(|e| e.to_string())
}

// Lua's convention: arg[0] is the script path, arg[1..] its arguments, and
// arg[-1] the interpreter itself.
fn install_arg(vm: &mut Vm, prog: &str, script: Option<&str>, args: &[String]) {
    let mut cx = vm.ctx();
    let t = cx.new_table();
    let set = |cx: &mut Ctx<'_>, i: i64, s: &str| {
        let v = cx.new_string(s);
        cx.raw_set(t, Value::int(i), v).expect("arg is a table");
    };
    set(&mut cx, -1, prog);
    if let Some(s) = script {
        set(&mut cx, 0, s);
    }
    for (i, a) in args.iter().enumerate() {
        set(&mut cx, i as i64 + 1, a);
    }
    cx.set_global("arg", t);
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
        if src.is_empty() {
            continue;
        }
        // A bare expression isn't a statement; retry it as `return <line>`
        // so `1+1` prints 2 instead of a parse error.
        let src = match umbra::parse(src) {
            (_, errs) if errs.is_empty() => src.to_owned(),
            _ => format!("return {src}"),
        };
        match run_src(&mut vm, &src, None) {
            Ok(vals) if !vals.is_empty() => {
                let mut cx = vm.ctx();
                // The chunk's results are reachable from nothing once it
                // returned, and a __tostring can collect: root them first.
                for &v in &vals {
                    cx.root(v);
                }
                let tostring = cx.get_global("tostring");
                let mut out = Vec::with_capacity(vals.len());
                for v in vals {
                    out.push(stringify(&mut cx, tostring, v));
                }
                println!("{}", out.join("\t"));
            }
            Ok(_) => {}
            Err(e) => eprintln!("{e}"),
        }
    }
}

// tostring() honors __tostring; fall back to the plain Display form.
fn stringify(cx: &mut Ctx<'_>, tostring: Value, v: Value) -> String {
    let s = cx.call(tostring, &[v]).ok().and_then(|r| r.into_iter().next());
    match s.and_then(|s| cx.to_str(s)) {
        Some(s) => s.to_owned(),
        None => format!("{v}"),
    }
}
