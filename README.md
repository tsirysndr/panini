# panini 🥪

[![e2e](https://github.com/tsirysndr/panini/actions/workflows/e2e.yml/badge.svg)](https://github.com/tsirysndr/panini/actions/workflows/e2e.yml)

**Press a Gleam (Erlang/BEAM) app into a single, self-contained binary.**

A [Burrito](https://github.com/burrito-elixir/burrito) for Gleam. `panini` turns a
Gleam project that targets Erlang into one native executable that runs on a machine with
**nothing installed** — no Gleam, no Erlang, no `rebar3`. The BEAM runtime is bundled
inside.

```sh
panini build ./examples/hello -o ./hello
./hello                 # => Hello from hello!  (runs with nothing installed)
```

It can select the OTP version to bundle, and **cross-compile** binaries for other
platforms from a single machine.

```sh
panini build ./examples/hello --otp 27.2                       # pick the OTP version
panini build ./examples/hello --target all --otp 27.2          # every platform at once
```

---

## Commands

| Command | What it does |
|---|---|
| `panini build [DIR] [OPTIONS]` | Press a Gleam app into one binary |
| `panini doctor` | Check the toolchain is ready |
| `panini targets` | List supported build targets |
| `panini otp-versions` | List OTP versions usable with `--otp` |
| `panini info` | Show the detected Gleam / OTP / Zig toolchain |

### `build` options

| Option | Default | Meaning |
|---|---|---|
| `-o, --output PATH` | `<project>/<app>` | Output binary path |
| `--otp VERSION` | host's OTP | Bundle a specific OTP, e.g. `27.2` (downloaded) |
| `--target LIST` | host | Comma-separated targets, or `all` |

With multiple targets the output name gets a `-<target>` suffix (e.g. `hello-x86_64-linux`).

## Targets

```
aarch64-macos     x86_64-macos     aarch64-linux     x86_64-linux
```

macOS runtimes are universal (one archive covers both arches); Linux is per-arch and
statically linked (musl). Windows isn't supported yet (needs a different boot + launcher).

## OTP version selection & the BEAM compatibility rule

BEAM bytecode must be compiled by an OTP that is compatible with the runtime it runs on —
newer-compiler bytecode won't load on an older runtime. So `--otp` doesn't just swap the
runtime, it also controls how the app is compiled:

- **Native target** (building for this machine's OS): panini downloads the selected OTP and
  **compiles your app with it** (via PATH shims), so the bytecode always matches the bundled
  runtime. Any version works, and **no system Erlang is required** — `--otp` provides the
  whole toolchain.
- **Cross-OS target** (e.g. Linux from macOS): panini can't run the target's compiler, so the
  app is compiled by the **host** toolchain. The bundled OTP major must therefore equal the
  host's OTP major; panini checks this and tells you if it doesn't. (This is the same
  constraint Burrito has.)

`panini otp-versions` lists what you can pass to `--otp` (precompiled runtimes exist for OTP
25.3+; macOS builds are universal).

## How it works

```
gleam export erlang-shipment          .beam + .app files (portable bytecode)
        │
        ▼
minimal, relocatable OTP runtime      erts + boot files + only the OTP lib apps
   (host runtime or downloaded)        the app references (kernel, stdlib, …)
        │
        ▼
run.sh                                sets ROOTDIR/BINDIR and boots via `erlexec`
        │                             with an explicit -boot (no OTP `Install` needed)
        ▼
tar + gzip  ->  payload.tar.gz
        │
        ▼
Zig self-extracting launcher          @embedFile(payload); first run extracts to
   (cross-compiled per target)        ~/.cache/panini/<app>-<hash>/ then execs run.sh
```

- The **CLI** (`src/`) is Rust, std-only — it shells out to `gleam`, `erl`, `curl`, `tar`,
  and `zig`. No crate dependencies.
- The **launcher** (`launcher/`) is Zig (pinned to **0.16.0**), in the spirit of Burrito's
  wrapper: it embeds the compressed payload, self-extracts to a per-app cache dir on first
  run, and hands off via `process.replace` (exec). Cross-compiled with `zig build -Dtarget=…`.
- **BEAM bytecode is portable**, so the Gleam shipment is reused across targets — only the
  native runtime and launcher differ per platform.

## Zig is handled for you

The launcher uses Zig-0.16.0-only APIs, so panini needs exactly that version. It uses your
`zig` if it reports `0.16.0`; otherwise it **downloads Zig 0.16.0** into
`~/.cache/panini/zig/` once and uses that. You never have to install Zig yourself.

## Requirements

Build machine needs `gleam`, `curl`, and `tar`. A host `erl` is only needed for the default
host-OTP build; a `--otp <v>` build downloads and compiles with its own OTP. `zig` is
auto-provisioned. Run `panini doctor` to check.

The **target** machine needs only a POSIX `sh` and `tar` (universal on macOS/Linux).

## Usage

```sh
cargo build --release

./target/release/panini build ./examples/hello -o ./hello
./target/release/panini build ./examples/hello --otp 28.0
./target/release/panini build ./examples/hello --target x86_64-linux,aarch64-linux --otp 27.2
./target/release/panini doctor
```

## CI

`.github/workflows/e2e.yml` runs the real thing on every push:

- **bundle** — a matrix of {ubuntu x64/arm64, macOS x64/arm64} × {OTP 26, 27, 28}: builds a
  binary with a bundled OTP (no system Erlang; Zig auto-downloaded) and runs it.
- **native** — the host-OTP path via `erlef/setup-beam` across OTP 26/27/28.
- **cross-build → cross-run** — cross-compiles an `aarch64-linux` binary on an x86_64 runner
  and runs it on a real arm64 runner, proving cross-compiled binaries work end-to-end.
- **cli** — `clippy -D warnings`, `fmt --check`, and the CLI subcommands.

## Roadmap

- [x] Single self-contained binary from a Gleam app (host)
- [x] `--otp` version selection (downloaded runtime + matching compile)
- [x] Cross-compilation (`--target`, per-target OTP + cross-compiled launcher)
- [x] Auto-provision Zig 0.16.0
- [x] `doctor`, `targets`, `otp-versions`
- [x] GitHub Actions e2e matrix (platforms × OTP versions)
- [ ] Trim the runtime further (strip unused ERTS binaries / man pages)
- [ ] Proper OTP release via `relx` (release semantics for supervised apps)
- [ ] Windows target (`.exe` launcher + `.ps1` boot)
- [ ] Vendor Burrito's exact Zig wrapper as an alternative launcher backend

## Layout

```
src/            Rust CLI: main, pipeline, otp, target, zig
launcher/       Zig 0.16 self-extracting wrapper (build.zig, src/main.zig)
examples/hello/ a sample Gleam app to build
.github/        e2e workflow
```

## Name

A panini is a *pressed* sandwich — which is exactly the operation: press your app and its
runtime flat into one binary. Also a nod to [Pāṇini](https://en.wikipedia.org/wiki/P%C4%81%E1%B9%87ini),
who wrote the first formal grammar — fitting for a tool built on a typed, compiled language.

## License

MIT
