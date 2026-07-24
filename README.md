# umbra

A Lua-inspired embeddable scripting language implemented from scratch in Rust:
lexer, parser, bytecode compiler, register-based VM, and a tri-color
mark-and-sweep garbage collector — ~10k lines, no dependencies.

The goal is a small scripting layer a host application can embed through a C
API (`umbra.h`), in the same spirit as Lua but with a few conveniences added
and some common Lua annoyances removed.

## Language

Familiar to anyone who knows Lua, with a C-flavored syntax:

- `let` / `var` bindings, `fn` functions and closures with proper upvalue
  semantics, implicit returns
- Tables with metatables and the usual metamethods (`__index`, `__newindex`,
  `__add`, `__call`, `__gc`, `__tostring`, weak `__mode`, …) — enough for
  prototype-style OOP
- Coroutines (`coroutine.create/resume/status/wrap`, `yield` as a statement)
- `switch`/`case`, ternary `cond ? a : b`, `++`/`--`, compound assignment
  (`+=`, `-=`, …)
- String interpolation: `"balance: ${account.balance}"`
- `<close>` variable attribute for deterministic cleanup
  (`let f <close> = io.open(path)`)
- `none` instead of `nil`; `not`/`and`/`or`; `@` line comments and
  `/* */` block comments
- Integers and floats as distinct types, with automatic promotion to a
  heap-boxed bigint on `i64` overflow instead of silent truncation
- `require` module system (`require "foo.bar"` → `foo/bar.umbra`, cached)
- `pcall`/`xpcall` error handling with line-attributed tracebacks

## Standard library

- `string` — `len sub rep upper lower reverse byte char format`, plus
  Lua-style pattern matching: `find match gmatch gsub`
- `string.pack` / `string.unpack` — binary packing, a subset of Lua 5.3/5.4's
  format language (fixed-width ints, floats, length-prefixed strings,
  endianness, padding)
- `table` — `insert remove concat sort pack unpack move`
- `io` — `open read write lines close` on file handles
- `os` — `time clock date getenv`
- `utf8` — `char len codepoint codes`
- `math` — `floor ceil abs sqrt max min sin cos tan exp log modf random
  randomseed` and constants
- `debug.traceback`, plus the base functions (`print tostring tonumber type
  assert error pcall xpcall ipairs pairs unpack select setmetatable
  getmetatable rawget rawset rawequal require`)

## Architecture

```
source → lexer → parser (AST) → compiler → bytecode chunk → register VM → GC
```

- `src/lexer.rs` — tokenizer; interpolation is lexed into parts and stitched
  by the parser
- `src/parser.rs` — recursive descent with Pratt expression parsing;
  `switch` desugars to `if`/`else` at parse time
- `src/compiler.rs` — AST → bytecode for a register-based VM
- `src/chunk.rs` — bytecode chunk format (opcodes, constants, line table)
- `src/vm.rs` — interpreter loop, metatables, coroutines, stdlib
- `src/gc.rs` — tri-color mark-and-sweep over strings, tables, closures, and
  bigints; incremental threshold plus a host-settable hard object ceiling
- `src/value.rs` — NaN-boxed value representation
- `src/api.rs` + `umbra.h` — the C embedding surface

Host-facing safety knobs: an instruction-step budget
(`umbra_set_step_limit`), a hard cap on live GC objects
(`umbra_set_max_objects`), and panic containment — a Rust panic inside the VM
is caught and surfaced as a script error rather than unwinding into the host.

## Building

```sh
cargo build --release
```

Produces `libumbra` as both a `cdylib` (for C hosts) and an `rlib` (for Rust
hosts). `umbra.h` is emitted by `build.rs` — cbindgen doesn't yet handle Rust
2024's `#[unsafe(no_mangle)]`, so the header is generated programmatically.

*Note: build not verified in the environment this repo was staged in — no
Rust toolchain installed. Edition 2024 requires a recent stable rustc.*

## Embedding from C

`umbra.h` exposes a small stack-based API modeled on Lua's:

```c
umbra_State *U = umbra_newstate();
umbra_register(U, "print", my_print);          /* umbra_CFunction */

if (umbra_dostring(U, src) != UMBRA_OK)
    fprintf(stderr, "error: %s\n", umbra_tostring(U, -1));

umbra_pushstring(U, "arg");
umbra_getglobal(U, "my_fn");
umbra_pcall(U, 1, 0);                          /* fn("arg"), protected */

umbra_close(U);
```

`example/host.c` is a complete host: it registers a `print` function, loads a
script file, and runs it. `example/Makefile` builds it against the cdylib:

```sh
cargo build            # produces target/debug/libumbra.so
cd example && make run-word-count
```

## Example scripts

Runnable `.umbra` programs in `example/`:

- `bank_account.umbra` — prototype-based OOP: classes, inheritance, method
  calls, string interpolation
- `scheduler.umbra` — a cooperative round-robin task scheduler on coroutines
- `word_count.umbra` — `io.open` with `<close>`, `string.gmatch` patterns,
  `table.sort`

## Tests

```sh
cargo test
```

224 tests covering the lexer, parser, value representation, VM semantics, GC
behavior under coroutines, panic containment, and stdlib edge cases
(allocation-cap enforcement, out-of-range arguments, etc.).

## Known limitations

- `require` and `io`/`os` do real filesystem and environment access — don't
  expose them to untrusted scripts (the step limit and object ceiling are the
  intended sandboxing knobs)
- `string.pack` ignores alignment (`!`) and treats native endianness as
  little-endian
- No `goto` and no `load`/`dofile` beyond `require`

## Notes

Comments in the source were written by Claude Sonnet 5 with guidance, to
articulate some of the architecture decisions and make certain invariants
clear. The design and code are mine.

## License

MIT — see [LICENSE](LICENSE).
