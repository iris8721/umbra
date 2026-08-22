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
- Coroutines (`coroutine.create/resume/status/wrap/isyieldable`, `yield()`)
- `switch`/`case`, ternary `cond ? a : b`, `++`/`--`, compound assignment
  (`+=`, `-=`, …); note `!=` is not-equal and `~=` is xor-assign
- String interpolation: `"balance: ${account.balance}"`
- `<close>` variable attribute: `close()` runs when the scope exits, whether
  by fall-through, `break`, `continue`, `return` or an error unwinding
  through it (the error is passed as close's second argument)
- `none` instead of `nil`; `not`/`and`/`or`; `@` line comments and
  `/* */` block comments
- Integers and floats as distinct types; integers are full 64-bit (values
  outside the NaN-box's 48-bit payload are heap-boxed transparently) and
  wrap on overflow like Lua's
- Multiple returns; a trailing call or `...` in a return, argument list or
  table constructor passes all of its values
- `require` module system (`require "foo.bar"` → `foo/bar.umbra`, cached)
- `goto`/labels, `pcall`/`xpcall` error handling with line-attributed tracebacks

## Standard library

- `string` — `len sub rep upper lower reverse byte char format`, plus
  Lua-style pattern matching: `find match gmatch gsub`
- `string.pack` / `string.unpack` — a subset of Lua 5.3/5.4's format
  language (fixed-width ints, floats, length-prefixed strings, endianness,
  padding); see limitations below
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
- `src/gc.rs` — stop-the-world tri-color mark-and-sweep over strings, tables,
  closures, and bigints; allocation-count threshold plus a host-settable hard
  object ceiling
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
cargo build --release  # produces target/release/libumbra.so
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

The suite covers the lexer, parser, value representation, VM semantics,
GC behavior (finalizers, weak tables, coroutines), panic containment, the C
API, and stdlib edge cases.

## Known limitations

- `require` and `io`/`os` do real filesystem and environment access — don't
  expose them to untrusted scripts (the step limit and object ceiling are the
  intended sandboxing knobs)
- `string.pack` returns the packed bytes hex-encoded (strings must be valid
  UTF-8), so its output isn't interchangeable with real Lua; alignment (`!`)
  is ignored and native endianness is treated as little-endian
- `yield` can't cross a `pcall`, metamethod or `table.sort` comparator
  boundary; it fails with "attempt to yield across a C-call boundary"
- Floating-point NaN is represented as `none` (the NaN bit patterns are the
  value encoding), so `0/0` yields `none`
- Strings are UTF-8, not byte strings: `string.char(200)` produces a two-byte
  character and `string.sub`/`reverse` slice on bytes but re-validate
- No `load`/`dofile` beyond `require`
- Expressions and blocks nest at most 100 levels; deeper sources are a
  parse error ("expected fewer nesting levels"), not a crash

## License

MIT — see [LICENSE](LICENSE).
