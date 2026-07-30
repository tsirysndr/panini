//! panini — press a Gleam (Erlang/BEAM) app into a single self-contained binary.
//!
//! Pipeline (host-target v0):
//!   1. `gleam export erlang-shipment`   -> compiled .beam + .app files (no runtime)
//!   2. assemble a minimal, relocatable OTP runtime (erts + boot + needed lib apps)
//!   3. generate run.sh that sets ERL_ROOTDIR and boots the app
//!   4. compress the runtime + app into two content-addressed packs and embed
//!      them in a Zig self-extracting launcher (runtime is shared across apps)
//!
//! std-only: shells out to `gleam`, `erl`, `tar`, `cp`, and `zig`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod otp;
mod pipeline;
mod target;
mod zig;

fn main() {
    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");

    let result = match cmd {
        "build" => cmd_build(&args[2..]),
        "info" => otp::print_info(),
        "doctor" => cmd_doctor(),
        "otp-versions" => otp::list_versions(),
        "targets" => {
            cmd_targets();
            Ok(())
        }
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        "version" | "--version" | "-V" => {
            // Compiled in from Cargo.toml at build time.
            println!("panini {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        other => {
            eprintln!("panini: unknown command '{other}'\n");
            print_help();
            std::process::exit(2);
        }
    };

    if let Err(e) = result {
        eprintln!("\x1b[31merror:\x1b[0m {e}");
        std::process::exit(1);
    }
}

fn print_help() {
    println!(
        "panini — a Burrito for Gleam 🥪\n\n\
         USAGE:\n  \
           panini build [PROJECT_DIR] [OPTIONS]     Press a Gleam app into one binary\n  \
           panini doctor                            Check the toolchain is ready\n  \
           panini targets                           List supported build targets\n  \
           panini otp-versions                      List OTP versions usable with --otp\n  \
           panini info                              Show detected Gleam/OTP toolchain\n  \
           panini version                           Show the panini version\n  \
           panini help                              Show this help\n\n\
         BUILD OPTIONS:\n  \
           -o, --output PATH     Output binary (default: <project>/<app>)\n  \
           --otp VERSION         Bundle a precompiled OTP version, e.g. 27.2 (downloaded)\n  \
           --target LIST         Comma-separated targets, or 'all' (default: host)\n  \
           --compression KIND    Payload compressor: gz (default), xz, or zst.\n                         \
                                 xz/zst yield smaller binaries but need that\n                         \
                                 decompressor present on Linux targets at run time.\n\n\
         EXAMPLES:\n  \
           panini build ./examples/hello -o ./hello\n  \
           panini build ./examples/hello --otp 27.2\n  \
           panini build ./examples/hello --target x86_64-linux,aarch64-linux --otp 27.2\n  \
           panini build ./examples/hello --target all --otp 27.2"
    );
}

/// Diagnose whether the toolchain needed to build is present.
fn cmd_doctor() -> Result<(), String> {
    println!("panini doctor\n");
    let mut ok = true;

    // Fatal: without these, panini can't build at all.
    ok &= check(
        "gleam",
        &["--version"],
        "install from https://gleam.run/getting-started/",
    );
    ok &= check(
        "curl",
        &["--version"],
        "needed to fetch OTP runtimes and Zig",
    );
    ok &= check("tar", &["--version"], "needed to pack/unpack payloads");

    // Non-fatal: a host `erl` is only needed for host-OTP builds (no --otp);
    // a `--otp <v>` build downloads and compiles with its own OTP toolchain.
    match capture(
        "erl",
        &[
            "-noshell",
            "-eval",
            "io:format(\"OTP ~s\",[erlang:system_info(otp_release)]),halt().",
        ],
    ) {
        Ok(v) => println!("  \x1b[32m✓\x1b[0m erl: {}", v.trim()),
        Err(_) => {
            println!("  \x1b[33m•\x1b[0m erl: none (host-OTP builds unavailable; use --otp <v>)")
        }
    }

    // Zig is not fatal — panini auto-downloads 0.16.0 if missing (needs curl+tar).
    println!("  \x1b[36m•\x1b[0m zig: {}", zig::describe());

    match target::host() {
        Some(t) => println!("  {:<6} {}", "host:", t.name),
        None => {
            println!("  {:<6} UNSUPPORTED (see `panini targets`)", "host:");
            ok = false;
        }
    }

    if ok {
        println!("\n\x1b[32m✓ ready to build\x1b[0m");
        Ok(())
    } else {
        Err("some required tools are missing (see above)".into())
    }
}

/// Print a ✓/✗ line for a tool; return whether it ran successfully.
fn check(program: &str, args: &[&str], hint: &str) -> bool {
    match capture(program, args) {
        Ok(out) => {
            let ver = out.lines().next().unwrap_or("").trim();
            println!("  \x1b[32m✓\x1b[0m {program}: {ver}");
            true
        }
        Err(_) => {
            println!("  \x1b[31m✗\x1b[0m {program}: not found — {hint}");
            false
        }
    }
}

fn cmd_targets() {
    println!("supported targets:");
    let host = target::host();
    for t in target::ALL {
        let tag = if host.map(|h| h.name == t.name).unwrap_or(false) {
            "  (host)"
        } else {
            ""
        };
        println!("  {:<16} zig: {}{}", t.name, t.zig, tag);
    }
    println!("\nUse with: panini build --target <name>[,<name>...]  (or --target all)");
    println!("Cross targets require --otp <version>.");
}

/// Parse `build` args: project dir, `-o OUTPUT`, `--otp VERSION`, `--target LIST`.
fn cmd_build(args: &[String]) -> Result<(), String> {
    let mut project = PathBuf::from(".");
    let mut out: Option<PathBuf> = None;
    let mut otp_version: Option<String> = None;
    let mut target_spec: Option<String> = None;
    let mut compression = pipeline::Compression::Gz;
    let mut i = 0;
    let mut project_set = false;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                out = Some(PathBuf::from(
                    args.get(i).ok_or("-o requires a path argument")?,
                ));
            }
            "--otp" => {
                i += 1;
                otp_version = Some(
                    args.get(i)
                        .ok_or("--otp requires a version, e.g. 27.2")?
                        .clone(),
                );
            }
            "--target" => {
                i += 1;
                target_spec = Some(
                    args.get(i)
                        .ok_or("--target requires a value, e.g. all")?
                        .clone(),
                );
            }
            "--compression" | "--compress" => {
                i += 1;
                compression = pipeline::Compression::parse(
                    args.get(i)
                        .ok_or("--compression requires a value: gz, xz, or zst")?,
                )?;
            }
            p if !p.starts_with('-') && !project_set => {
                project = PathBuf::from(p);
                project_set = true;
            }
            other => return Err(format!("unexpected argument: {other}")),
        }
        i += 1;
    }

    let targets = resolve_targets(target_spec.as_deref())?;

    let project = project
        .canonicalize()
        .map_err(|e| format!("project dir {}: {e}", project.display()))?;

    pipeline::build(&project, out, otp_version, targets, compression)
}

/// Turn a `--target` spec into a target list. `None` => host; "all" => everything.
fn resolve_targets(spec: Option<&str>) -> Result<Vec<target::Target>, String> {
    match spec {
        None => Ok(vec![target::host().ok_or(
            "this host platform isn't a supported target; pass --target explicitly",
        )?]),
        Some("all") => Ok(target::ALL.to_vec()),
        Some(list) => {
            let mut out = Vec::new();
            for name in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let t = target::parse(name)?;
                if !out.contains(&t) {
                    out.push(t);
                }
            }
            if out.is_empty() {
                return Err("no targets given to --target".into());
            }
            Ok(out)
        }
    }
}

// ---- shared helpers used across modules ----

/// Run a command inheriting stdio; error if it exits non-zero.
pub fn sh(program: &str, args: &[&str], cwd: Option<&Path>) -> Result<(), String> {
    let mut c = Command::new(program);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let status = c
        .status()
        .map_err(|e| format!("failed to run `{program}`: {e} (is it installed?)"))?;
    if !status.success() {
        return Err(format!("`{program}` exited with {status}"));
    }
    Ok(())
}

/// Like `sh`, but prepends `extra_path` to `PATH` (used to compile with a
/// specific bundled OTP toolchain).
pub fn sh_env(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    extra_path: Option<&Path>,
) -> Result<(), String> {
    let mut c = Command::new(program);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    if let Some(p) = extra_path {
        let existing = std::env::var("PATH").unwrap_or_default();
        c.env("PATH", format!("{}:{}", p.display(), existing));
    }
    let status = c
        .status()
        .map_err(|e| format!("failed to run `{program}`: {e} (is it installed?)"))?;
    if !status.success() {
        return Err(format!("`{program}` exited with {status}"));
    }
    Ok(())
}

/// Run a command and capture trimmed stdout.
pub fn capture(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("failed to run `{program}`: {e}"))?;
    if !out.status.success() {
        return Err(format!("`{program}` exited with {}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Recursive copy via system `cp -R` (preserves perms + symlinks like the runtime needs).
pub fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    sh(
        "cp",
        &["-R", &src.to_string_lossy(), &dst.to_string_lossy()],
        None,
    )
}
