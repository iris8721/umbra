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
  table constructor passes all of its values. `return f(x)` is a proper tail
  call — the frame is reused, so tail-recursive loops don't grow the stack
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
  getmetatable rawget rawset rawequal require load`)

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
- `nqueens.umbra` — recursive backtracking over a shared table
- `dijkstra.umbra` — shortest paths on a seeded graph with a hand-rolled
  binary-heap priority queue
- `json.umbra` — serializer + recursive-descent parser roundtrip; patterns,
  `__tostring`
- `mandel.umbra` — 200x200 ASCII Mandelbrot; float arithmetic in nested loops

## Tests

```sh
cargo test
```

The suite covers the lexer, parser, value representation, VM semantics,
GC behavior (finalizers, weak tables, coroutines), panic containment, the C
API, and stdlib edge cases.

## Performance

Release build, cycles on one pinned core (`perf stat -e cycles`, mean of
5), against PUC Lua 5.4:

| | umbra | lua 5.4 | |
|---|---|---|---|
| `fib(30)` — call overhead | 325M | 202M | 1.6× |
| 2M array writes + reads | 422M | 174M | 2.4× |
| 200k string concat + `gmatch` | 368M | 408M | 0.9× |
| 5M short-lived tables, 100k live | 3.24G | 1.69G | 1.9× |

The bytecode has the same superinstructions Lua 5.4 added (immediate-operand
arithmetic and compares, `GetField`/`SelfOp`, tail calls) plus a per-callsite
global cache. What's left is representation: NaN-boxing range-checks every
integer result against the 48-bit inline payload where Lua's 16-byte
`TValue` holds a full `i64`, and `match` dispatch can't do the computed-goto
threading a C interpreter gets. No JIT.

## Differences from Lua

These are deliberate and won't change:

- **Strings are UTF-8, not bytes.** `string.char(200)` produces a two-byte
  character; `string.sub`/`reverse` slice on bytes and re-validate. As a
  consequence `string.pack` returns its output hex-encoded rather than as
  raw bytes, alignment (`!`) is ignored, and native endianness is treated as
  little-endian — its output isn't interchangeable with real Lua's.
- **NaN is `none`.** The value representation is NaN-boxed, so the NaN bit
  patterns carry the other types; `0/0` yields `none`.
- **`none` instead of `nil`; `!=` is not-equal; `~=` is xor-assign.**
- **No `dofile`.** `load` compiles strings; `require` loads files. That's
  the whole filesystem surface for code.

## Limitations

- `yield` can't cross a `pcall`, metamethod or `table.sort` comparator
  boundary; it fails with "attempt to yield across a C-call boundary". Lua
  5.1 had the same restriction; 5.2 lifted it with continuation-passing
  (`lua_pcallk`), which this VM doesn't implement.
- The GC is stop-the-world. Large heaps will see pauses; Lua 5.4's
  generational collector doesn't have this problem.
- Expressions and blocks nest at most 100 levels; deeper sources are a
  parse error ("expected fewer nesting levels") rather than a stack overflow.

## Sandboxing

`require`, `io` and `os` do real filesystem and environment access, the same
as Lua's. To run untrusted code, clear them from the globals before
executing:

```c
umbra_pushnil(U); umbra_setglobal(U, "io");
umbra_pushnil(U); umbra_setglobal(U, "os");
umbra_pushnil(U); umbra_setglobal(U, "require");
```

and bound CPU and memory with `umbra_set_step_limit` and
`umbra_set_max_objects`. Panics inside the VM are caught and surfaced as
script errors; they don't unwind into the host.

## License

MIT — see [LICENSE](LICENSE).
