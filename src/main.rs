//! panini — press a Gleam (Erlang/BEAM) app into a single self-contained binary.
//!
//! Pipeline (host-target v0):
//!   1. `gleam export erlang-shipment`   -> compiled .beam + .app files (no runtime)
//!   2. assemble a minimal, relocatable OTP runtime (erts + boot + needed lib apps)
//!   3. generate run.sh that sets ERL_ROOTDIR and boots the app
//!   4. compress the runtime + app into two content-addressed packs and embed
//!      them in a Zig self-extracting launcher (runtime is shared across apps)
//!
//! Shells out to `gleam`, `erl`, `tar`, `cp`, and `zig`; `clap` drives the CLI.

use clap::{Args, Parser, Subcommand};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod otp;
mod pipeline;
mod style;
mod target;
mod zig;

/// panini — press a Gleam (Erlang/BEAM) app into a single self-contained binary. 🥪
#[derive(Parser)]
#[command(
    name = "panini",
    version,
    about = "🥪 panini — bundle a Gleam app and the Erlang/BEAM runtime into one self-contained binary that runs with nothing installed",
    styles = style::clap_styles(),
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Press a Gleam app into one self-contained binary
    Build(BuildArgs),
    /// Check the toolchain is ready
    Doctor,
    /// List supported build targets
    Targets,
    /// List OTP versions usable with --otp
    OtpVersions,
    /// Show the detected Gleam/OTP toolchain
    Info,
}

/// Options for `panini build`.
#[derive(Args)]
#[command(after_help = "\
EXAMPLES:
  panini build ./examples/hello -o ./hello
  panini build ./examples/hello --otp 27.2
  panini build ./examples/hello --target x86_64-linux,aarch64-linux --otp 27.2
  panini build ./examples/hello --target all --otp 27.2 --compression xz")]
struct BuildArgs {
    /// Gleam project directory
    #[arg(value_name = "PROJECT_DIR", default_value = ".")]
    project: PathBuf,

    /// Output binary (default: <project>/<app>)
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// Bundle a precompiled OTP version, e.g. 27.2 (downloaded)
    #[arg(long, value_name = "VERSION")]
    otp: Option<String>,

    /// Comma-separated targets, or 'all' (default: host)
    #[arg(long, value_name = "LIST")]
    target: Option<String>,

    /// Payload compressor. xz/zst make smaller binaries but need that
    /// decompressor present on Linux targets at run time
    #[arg(long, value_name = "KIND", default_value_t = pipeline::Compression::Gz)]
    compression: pipeline::Compression,
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Build(args) => cmd_build(args),
        Commands::Info => otp::print_info(),
        Commands::Doctor => cmd_doctor(),
        Commands::OtpVersions => otp::list_versions(),
        Commands::Targets => {
            cmd_targets();
            Ok(())
        }
    };

    if let Err(e) = result {
        eprintln!("{} {e}", style::error("error:"));
        std::process::exit(1);
    }
}

/// Diagnose whether the toolchain needed to build is present.
fn cmd_doctor() -> Result<(), String> {
    println!("{}\n", style::header("panini doctor"));
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
        Ok(v) => println!("  {} erl: {}", style::ok("✓"), style::cyan(v.trim())),
        Err(_) => println!(
            "  {} erl: {}",
            style::warn("•"),
            style::muted("none (host-OTP builds unavailable; use --otp <v>)")
        ),
    }

    // Zig is not fatal — panini auto-downloads 0.16.0 if missing (needs curl+tar).
    println!("  {} zig: {}", style::cyan("•"), zig::describe());

    match target::host() {
        Some(t) => println!("  {} {}", style::muted("host:"), style::cyan(t.name)),
        None => {
            println!(
                "  {} {}",
                style::muted("host:"),
                style::warn("UNSUPPORTED (see `panini targets`)")
            );
            ok = false;
        }
    }

    if ok {
        println!("\n{}", style::ok("✓ ready to build"));
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
            println!("  {} {program}: {}", style::ok("✓"), style::cyan(ver));
            true
        }
        Err(_) => {
            println!(
                "  {} {program}: {}",
                style::error("✗"),
                style::muted(&format!("not found — {hint}"))
            );
            false
        }
    }
}

fn cmd_targets() {
    println!("{}", style::header("supported targets"));
    let host = target::host();
    for t in target::ALL {
        let is_host = host.map(|h| h.name == t.name).unwrap_or(false);
        let tag = if is_host {
            style::teal("  (host)")
        } else {
            String::new()
        };
        // Pad before coloring so ANSI codes don't throw off the column width.
        let name = format!("{:<16}", t.name);
        println!(
            "  {} {} {}{}",
            style::cyan(&name),
            style::muted("zig:"),
            t.zig,
            tag
        );
    }
    println!(
        "\n{} panini build --target <name>[,<name>...]  (or --target all)",
        style::muted("use with:")
    );
    println!("{}", style::muted("cross targets require --otp <version>."));
}

/// Run a `panini build` from its parsed arguments.
fn cmd_build(args: BuildArgs) -> Result<(), String> {
    let targets = resolve_targets(args.target.as_deref())?;

    let project = args
        .project
        .canonicalize()
        .map_err(|e| format!("project dir {}: {e}", args.project.display()))?;

    pipeline::build(&project, args.output, args.otp, targets, args.compression)
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
