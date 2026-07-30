//! panini — press a Gleam (Erlang/BEAM) app into a single self-contained binary.
//!
//! Pipeline (host-target v0):
//!   1. `gleam export erlang-shipment`   -> compiled .beam + .app files (no runtime)
//!   2. assemble a minimal, relocatable OTP runtime (erts + boot + needed lib apps)
//!   3. generate run.sh that sets ERL_ROOTDIR and boots the app
//!   4. tar.gz the payload and embed it in a Zig self-extracting launcher
//!
//! std-only: shells out to `gleam`, `erl`, `tar`, `cp`, and `zig`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod otp;
mod pipeline;

fn main() {
    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");

    let result = match cmd {
        "build" => cmd_build(&args[2..]),
        "info" => otp::print_info(),
        "help" | "-h" | "--help" => {
            print_help();
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
           panini build [PROJECT_DIR] [-o OUTPUT]   Press a Gleam app into one binary\n  \
           panini info                              Show detected Gleam/OTP toolchain\n  \
           panini help                              Show this help\n\n\
         EXAMPLE:\n  \
           panini build ./examples/hello -o ./hello\n  \
           ./hello            # runs with nothing installed"
    );
}

/// Parse `build` args: an optional project dir and an optional `-o OUTPUT`.
fn cmd_build(args: &[String]) -> Result<(), String> {
    let mut project = PathBuf::from(".");
    let mut out: Option<PathBuf> = None;
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
            p if !p.starts_with('-') && !project_set => {
                project = PathBuf::from(p);
                project_set = true;
            }
            other => return Err(format!("unexpected argument: {other}")),
        }
        i += 1;
    }

    let project = project
        .canonicalize()
        .map_err(|e| format!("project dir {}: {e}", project.display()))?;

    pipeline::build(&project, out)
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
    sh("cp", &["-R", &src.to_string_lossy(), &dst.to_string_lossy()], None)
}
