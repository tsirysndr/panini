# panini 🥪

**Press a Gleam (Erlang/BEAM) app into a single, self-contained binary.**

A [Burrito](https://github.com/burrito-elixir/burrito) for Gleam. `panini` takes a
Gleam project that targets Erlang and produces one native executable that runs on a
machine with **nothing installed** — no Gleam, no Erlang, no `rebar3`. The BEAM
runtime is bundled inside.

```sh
panini build ./examples/hello -o ./hello
./hello          # => Hello from hello!
```

The resulting `./hello` is a ~13 MB native binary. On first run it self-extracts a
minimal Erlang runtime + your compiled `.beam` files to `~/.cache/panini/`, then boots
the VM. Subsequent runs skip extraction (warm start ~90 ms here).

> Status: **working v0, host-target only.** Builds a binary for the platform you build
> on. Cross-compilation is designed but not yet wired — see [Roadmap](#roadmap).

---

## Why this works

Gleam compiles to BEAM bytecode but its `erlang-shipment` export still *assumes Erlang
is installed on the target*. Burrito solved the "bundle the runtime" problem for Elixir;
that machinery is BEAM-agnostic, so the same idea works for Gleam. `panini` is the glue.

Two facts make the host-target path simple and robust:

1. **The BEAM runtime is relocatable.** Erlang's `bin/erl` honors an `ERL_ROOTDIR`
   environment variable, so a copied runtime tree runs correctly from any location — no
   binary patching required.
2. **A minimal runtime is small.** We bundle only `erts-<vsn>`, the boot scripts, and the
   OTP `lib` apps the shipment actually references (kernel, stdlib, …). For a typical
   Gleam app that's ~28 MB uncompressed, ~12 MB gzipped.

## Pipeline

```
gleam export erlang-shipment          .beam + .app files (no runtime)
        │
        ▼
assemble a minimal, relocatable OTP   erts-<vsn> + bin + releases + needed lib/ apps
        │                             (needed apps discovered from .app files)
        ▼
generate run.sh                       sets ERL_ROOTDIR, boots <app>@@main:run(<app>)
        │
        ▼
tar + gzip  ->  payload.tar.gz
        │
        ▼
embed in a Zig self-extracting        @embedFile(payload); first run extracts to
launcher  ->  single binary           ~/.cache/panini/<app>-<hash>/ then execs run.sh
```

- The **CLI** (`src/`) is Rust, std-only — it shells out to `gleam`, `erl`, `tar`, and
  `zig`. No crate dependencies.
- The **launcher** (`launcher/`) is Zig (0.16), in the spirit of Burrito's wrapper: it
  embeds the compressed payload, self-extracts to a per-app cache dir on first run, and
  hands off the process to the boot script via `process.replace` (exec).

## Requirements (build machine)

| Tool     | Used for                                  |
|----------|-------------------------------------------|
| `gleam`  | compiling the app to a shipment           |
| `erl`    | locating the host OTP runtime to bundle   |
| `zig`    | compiling the self-extracting launcher    |
| `cargo`  | building `panini` itself                  |

The **target** machine needs only a POSIX `sh` and `tar` (both universal on macOS/Linux).

## Usage

```sh
cargo build --release          # build panini

./target/release/panini build [PROJECT_DIR] [-o OUTPUT]
./target/release/panini info   # show detected gleam/OTP toolchain
```

- `PROJECT_DIR` defaults to `.` and must contain a `gleam.toml`.
- `OUTPUT` defaults to `<PROJECT_DIR>/<app-name>`.

## Roadmap

- [ ] **Cross-compilation.** Fetch precompiled ERTS per target instead of copying the host
      runtime. Burrito already publishes precompiled OTP for
      `{darwin,linux}/{x86_64,aarch64}` and `windows/x86_64` — reuse those archives, and
      cross-compile the launcher with `zig build -Dtarget=<triple>` (Zig cross-compiles for
      free). This is the main thing standing between v0 and a real release.
- [ ] **Trim the runtime further** — strip unused `lib` apps / ERTS binaries.
- [ ] **Proper OTP release** via `relx` (boot script, config, releases metadata) instead of
      the plain-`erl` boot, so long-running/supervised apps get real release semantics.
- [ ] **Windows target** (`.ps1` boot path + `.exe` launcher).
- [ ] **Vendor Burrito's exact wrapper** as an alternative launcher backend.
- [ ] Compression/scrub options, custom VM flags, embedded env.

## Layout

```
src/            Rust CLI (main.rs, otp.rs, pipeline.rs)
launcher/       Zig self-extracting wrapper (build.zig, src/main.zig)
examples/hello/ a sample Gleam app to build
```

## Name

A panini is a *pressed* sandwich — which is exactly the operation: press your app and its
runtime flat into one binary. Also a nod to [Pāṇini](https://en.wikipedia.org/wiki/P%C4%81%E1%B9%87ini),
who wrote the first formal grammar — fitting for a tool built on a typed, compiled language.

## License

MIT
