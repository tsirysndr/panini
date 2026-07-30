//! The build pipeline: Gleam shipment -> per-target runtime -> payload -> launcher.
//!
//! BEAM bytecode must be compiled by an OTP compatible with the runtime it will
//! run on. So when `--otp` selects a version we can execute on this host, we
//! compile the app *with that OTP* (via PATH shims) and bundle the matching
//! runtime. For cross-OS targets we can't run the target compiler, so the app is
//! compiled by the host toolchain and the bundled major must equal the host's.

use crate::target::Target;
use crate::{copy_tree, otp, sh, sh_env, zig};
use std::fs;
use std::path::{Path, PathBuf};

/// Which compressor to use for the embedded payloads.
///
/// `Gz` is the default because gzip decompression is effectively universal — a
/// gz payload keeps the "runs on a machine with nothing installed" promise. `Xz`
/// and `Zst` produce smaller binaries but require that decompressor to be present
/// at run time on Linux targets (macOS `tar` has both built in via libarchive).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Compression {
    /// gzip — decompresses everywhere (default)
    #[value(name = "gz", alias = "gzip")]
    Gz,
    /// xz — smallest binary, needs `xz` on Linux targets
    #[value(name = "xz")]
    Xz,
    /// zstd — fast, needs `zstd` on Linux targets
    #[value(name = "zst", alias = "zstd")]
    Zst,
}

impl Compression {
    /// Short name, used in messages and (informally) as the archive extension.
    /// Matches the clap value name so it round-trips as a `--compression` value.
    pub fn ext(self) -> &'static str {
        match self {
            Compression::Gz => "gz",
            Compression::Xz => "xz",
            Compression::Zst => "zst",
        }
    }

    /// `tar` flags to *create* an archive with this compressor. Extraction never
    /// needs these — the launcher uses `tar -xf`, which autodetects the format.
    fn create_flags(self) -> &'static [&'static str] {
        match self {
            Compression::Gz => &["-czf"],
            Compression::Xz => &["-cJf"],
            Compression::Zst => &["--zstd", "-cf"],
        }
    }
}

impl std::fmt::Display for Compression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.ext())
    }
}

struct Plan {
    /// OTP root to bundle for this target.
    root: PathBuf,
    /// If set, compile the shipment with this toolchain dir on PATH.
    toolbin: Option<PathBuf>,
    /// For Linux targets: the musl runtime that must live at /tmp/libc-musl-*.so.
    musl: Option<otp::Musl>,
}

pub fn build(
    project: &Path,
    out: Option<PathBuf>,
    otp_version: Option<String>,
    targets: Vec<Target>,
    compression: Compression,
) -> Result<(), String> {
    let app = read_app_name(project)?;
    let multi = targets.len() > 1;
    println!(
        "{} {}  {} {} → {} target{}\n",
        crate::style::gradient("🥪 panini"),
        crate::style::muted(&format!("v{}", env!("CARGO_PKG_VERSION"))),
        crate::style::muted("pressing"),
        crate::style::cyan(&format!("'{app}'")),
        crate::style::teal(&targets.len().to_string()),
        if multi { "s" } else { "" }
    );

    // A pinned Zig 0.16.0 (host or auto-downloaded) drives the launcher build.
    let zig_bin = zig::resolve()?;
    let host_major = otp::otp_release().ok().map(|v| major(&v).to_string());

    let staging_base = project.join("build/panini");
    let _ = fs::remove_dir_all(&staging_base);

    for target in &targets {
        let output = output_path(&out, project, &app, target, multi);
        let staging = staging_base.join(target.name);
        fs::create_dir_all(&staging).map_err(|e| format!("mkdir {}: {e}", staging.display()))?;

        // Resolve the runtime + compile toolchain for this target.
        let plan = prepare(&otp_version, target, &staging, host_major.as_deref())?;
        let compiled_with = match (&otp_version, &plan.toolbin) {
            (Some(v), Some(_)) => format!("compiled with OTP {v}"),
            (Some(v), None) => format!("bundling OTP {v}, compiled with host OTP"),
            (None, _) => "host OTP".into(),
        };
        println!(
            "\n{} {}  {}",
            crate::style::teal("▸"),
            crate::style::header(target.name),
            crate::style::muted(&compiled_with)
        );

        // The Linux OTP toolchain is musl-linked with a /tmp interpreter path;
        // install it before compiling with those binaries.
        if let (Some(_), Some(m)) = (&plan.toolbin, &plan.musl) {
            install_musl_tmp(m)?;
        }

        // 1. Gleam shipment, using the selected toolchain.
        compile_shipment(project, plan.toolbin.as_deref())?;
        let shipment = project.join("build/erlang-shipment");
        if !shipment.is_dir() {
            return Err(format!("expected shipment at {}", shipment.display()));
        }

        // 2. Assemble a minimal, relocatable runtime + the compiled app, split
        //    into two trees so they can be packed and cached independently.
        let payload = staging.join("payload");
        fs::create_dir_all(&payload).map_err(|e| format!("mkdir {}: {e}", payload.display()))?;
        let rt_desc = assemble_runtime(&plan.root, &shipment, &payload, plan.musl.as_ref())?;
        copy_tree(&shipment, &payload.join("app"))?;
        write_run_sh(&payload, &app, plan.musl.as_ref())?;

        // 3. Pack each tree separately. The runtime is content-addressed so that
        //    different apps sharing the same runtime extract it once, into a
        //    shared cache dir on the target; the app is keyed by its own hash.
        let ext = compression.ext();
        let otp_pack = staging.join(format!("otp.{ext}"));
        let app_pack = staging.join(format!("app.{ext}"));
        make_archive(&payload, &["otp"], &otp_pack, compression)?;
        make_archive(&payload, &["app", "run.sh"], &app_pack, compression)?;

        let otp_bytes = fs::read(&otp_pack).map_err(|e| format!("read otp pack: {e}"))?;
        let app_bytes = fs::read(&app_pack).map_err(|e| format!("read app pack: {e}"))?;
        // Runtime tag: a stable digest of the runtime *composition* (target, OTP
        // version, erts + lib apps, musl), independent of build timestamps — so
        // identical runtimes hash identically and share one extraction.
        let rt_key = format!(
            "{}-{}-{}-{}",
            target.os,
            target.arch,
            otp_version.as_deref().unwrap_or("host"),
            rt_desc
        );
        let otp_tag = format!("{:08x}", djb2(rt_key.as_bytes()));
        let app_tag = format!("{app}-{:08x}", djb2(&app_bytes));
        println!(
            "     {} otp {} + app {}  {}",
            crate::style::muted("payload:"),
            crate::style::cyan(&format!("{} MB", otp_bytes.len() / 1_048_576)),
            crate::style::cyan(&format!("{} KB", app_bytes.len() / 1024)),
            crate::style::muted(&format!("[{ext}, otp {otp_tag}]")),
        );

        build_launcher(
            &zig_bin,
            &staging,
            &Payloads {
                otp_pack: &otp_pack,
                app_pack: &app_pack,
                otp_tag: &otp_tag,
                app_tag: &app_tag,
            },
            target,
            &output,
        )?;
        println!(
            "     {} {}",
            crate::style::ok("✓"),
            crate::style::teal(&output.display().to_string())
        );
    }

    println!("\n{}", crate::style::gradient("✓ done"));
    Ok(())
}

/// Decide the OTP runtime + compile toolchain for a target.
fn prepare(
    otp_version: &Option<String>,
    target: &Target,
    staging: &Path,
    host_major: Option<&str>,
) -> Result<Plan, String> {
    match otp_version {
        Some(v) => {
            let root = otp::precompiled_root(v, target, &cache_dir())?;
            // BEAM Machine Linux builds are musl-linked and need their /tmp runtime.
            let musl = if target.os == "linux" {
                Some(otp::fetch_musl(&root, &cache_dir())?)
            } else {
                None
            };
            if runnable_on_host(target) {
                // We can execute this OTP: compile the app with it (correct beam).
                let toolbin = make_toolbin(&root, staging)?;
                Ok(Plan {
                    root,
                    toolbin: Some(toolbin),
                    musl,
                })
            } else {
                // Cross-OS: app is compiled by the host toolchain; majors must match.
                let hm = host_major.ok_or("cannot determine host OTP version")?;
                if major(v) != hm {
                    return Err(format!(
                        "cross-building '{}' with --otp {v}: the app is compiled by the host \
                         toolchain (OTP {hm}) but you're bundling OTP {v} — the majors differ so \
                         the binary won't load.\n  Fix: bundle a matching major (e.g. --otp {hm}.x) \
                         or build this target on a runner that has OTP {v} installed.",
                        target.name
                    ));
                }
                Ok(Plan {
                    root,
                    toolbin: None,
                    musl,
                })
            }
        }
        None if target.is_host() => Ok(Plan {
            root: otp::root_dir()?,
            toolbin: None,
            musl: None,
        }),
        None => Err(format!(
            "cross-building for '{}' needs an explicit OTP version — pass e.g. `--otp {}` \
             (the host runtime only works for the host platform)",
            target.name,
            host_major.unwrap_or("27.2")
        )),
    }
}

/// Can this host execute the target's OTP binaries (so we can compile with them)?
/// macOS builds are universal, so any mac runs them; Linux needs matching arch.
fn runnable_on_host(target: &Target) -> bool {
    match crate::target::host() {
        Some(h) => target.os == h.os && (target.os == "macos" || target.arch == h.arch),
        None => false,
    }
}

fn major(v: &str) -> &str {
    v.split('.').next().unwrap_or(v)
}

/// Write erl/erlc/escript shims that point at a downloaded OTP root, so Gleam
/// compiles the shipment with that exact OTP. Returns the shim directory.
fn make_toolbin(root: &Path, staging: &Path) -> Result<PathBuf, String> {
    let erts = otp::find_erts_dir(root)?;
    let rel = otp::find_release_dir(root)?;
    let dir = staging.join("toolbin");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;

    let root_s = root.display();
    let erts_s = erts.display();
    let common =
        format!("export ROOTDIR=\"{root_s}\"\nexport BINDIR=\"{erts_s}/bin\"\nexport EMU=beam\n");
    // erl is normally generated by OTP's Install; reconstruct it via erlexec.
    let erl = format!(
        "#!/bin/sh\n{common}export PROGNAME=erl\nexec \"{erts_s}/bin/erlexec\" -boot \"{}/start\" \"$@\"\n",
        rel.display()
    );
    write_exec(&dir.join("erl"), &erl)?;
    for tool in ["erlc", "escript"] {
        let s = format!("#!/bin/sh\n{common}exec \"{erts_s}/bin/{tool}\" \"$@\"\n");
        write_exec(&dir.join(tool), &s)?;
    }
    Ok(dir)
}

fn write_exec(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Compile the Gleam shipment, optionally with a specific OTP toolchain on PATH.
fn compile_shipment(project: &Path, toolbin: Option<&Path>) -> Result<(), String> {
    let _ = fs::remove_dir_all(project.join("build/erlang-shipment"));
    // Changing the compiler requires discarding Gleam's cached BEAM output.
    if toolbin.is_some() {
        let _ = fs::remove_dir_all(project.join("build/dev/erlang"));
    }
    println!(
        "     {}",
        crate::style::muted("gleam export erlang-shipment")
    );
    sh_env(
        "gleam",
        &["export", "erlang-shipment"],
        Some(project),
        toolbin,
    )
}

/// Copy the erts dir, boot/releases metadata, and only the OTP lib apps the app
/// actually references, into `<payload>/otp`. For Linux, also bundle the musl
/// runtime so `run.sh` can install it at /tmp on the target.
///
/// Returns a descriptor of the runtime composition (erts + sorted lib apps +
/// musl) that uniquely identifies its content, used to content-address the
/// runtime cache so identical runtimes are extracted only once on the target.
fn assemble_runtime(
    root: &Path,
    shipment: &Path,
    payload: &Path,
    musl: Option<&otp::Musl>,
) -> Result<String, String> {
    let otp_dst = payload.join("otp");
    let erts = otp::find_erts_dir(root)?;
    let erts_name = erts.file_name().unwrap().to_string_lossy().into_owned();
    let erts_dst = otp_dst.join(&erts_name);
    copy_tree(&erts, &erts_dst)?;
    make_bin_executable(&erts_dst.join("bin"))?;
    copy_tree(&root.join("releases"), &otp_dst.join("releases"))?;

    let lib_apps = otp::needed_lib_apps(shipment, root)?;
    let mut app_names: Vec<String> = Vec::new();
    for app_dir in &lib_apps {
        let name = app_dir.file_name().unwrap().to_string_lossy().into_owned();
        copy_tree(app_dir, &otp_dst.join("lib").join(&name))?;
        app_names.push(name);
    }
    app_names.sort();
    if let Some(m) = musl {
        fs::create_dir_all(&otp_dst).ok();
        fs::copy(&m.so, otp_dst.join("musl-runtime.so"))
            .map_err(|e| format!("bundle musl runtime: {e}"))?;
    }
    println!(
        "     {} {} {}",
        crate::style::muted("runtime:"),
        crate::style::cyan(&erts_name),
        crate::style::muted(&format!(
            "+ {} OTP lib apps{}",
            lib_apps.len(),
            if musl.is_some() { " + musl" } else { "" }
        )),
    );
    let musl_part = musl.map(|m| m.hash.as_str()).unwrap_or("");
    Ok(format!("{erts_name}|{}|{musl_part}", app_names.join(",")))
}

/// Install the musl runtime at its hardcoded /tmp interpreter path (idempotent).
fn install_musl_tmp(m: &otp::Musl) -> Result<(), String> {
    let dst = PathBuf::from(m.tmp_path());
    if !dst.exists() {
        fs::copy(&m.so, &dst).map_err(|e| format!("install musl to {}: {e}", dst.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The kernel requires the ELF interpreter to be executable.
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o755)).ok();
    }
    Ok(())
}

/// Make every file directly inside a `bin` dir executable (0755). Harmless for
/// data files, and belt-and-suspenders against any perm loss in copy/extract.
fn make_bin_executable(bin: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(entries) = fs::read_dir(bin) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_file() {
                    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).ok();
                }
            }
        }
    }
    let _ = bin;
    Ok(())
}

fn cache_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".cache/panini")
}

fn output_path(
    out: &Option<PathBuf>,
    project: &Path,
    app: &str,
    target: &Target,
    multi: bool,
) -> PathBuf {
    let base = out.clone().unwrap_or_else(|| project.join(app));
    if multi {
        let mut s = base.into_os_string();
        s.push(format!("-{}", target.name));
        PathBuf::from(s)
    } else {
        base
    }
}

/// Read `name = "..."` from the project's gleam.toml.
fn read_app_name(project: &Path) -> Result<String, String> {
    let toml = fs::read_to_string(project.join("gleam.toml")).map_err(|_| {
        format!(
            "no gleam.toml in {} — is this a Gleam project?",
            project.display()
        )
    })?;
    for line in toml.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name") {
            if let Some(eq) = rest.find('=') {
                let val = rest[eq + 1..].trim().trim_matches('"').trim();
                if !val.is_empty() {
                    return Ok(val.to_string());
                }
            }
        }
    }
    Err("could not find `name` in gleam.toml".into())
}

/// A self-locating boot script: sets the runtime env and boots via `erlexec`
/// with an explicit boot file — no `Install` step, works for host + cross runtimes.
///
/// The OTP root is passed as `$1` by the launcher (it lives in a shared,
/// content-addressed cache dir, separate from this app's own files) and consumed
/// here; `$HERE` still locates this app's compiled `.beam` under `app/`.
fn write_run_sh(payload: &Path, app: &str, musl: Option<&otp::Musl>) -> Result<(), String> {
    // Linux runtimes are musl-linked with a hardcoded /tmp interpreter path;
    // install the bundled libc there on first run before booting the VM.
    let musl_step = match musl {
        Some(m) => format!(
            "MUSL=\"{}\"\n\
             [ -f \"$MUSL\" ] || {{ cp \"$ROOT/musl-runtime.so\" \"$MUSL\"; chmod +x \"$MUSL\"; }}\n",
            m.tmp_path()
        ),
        None => String::new(),
    };
    let script = format!(
        "#!/bin/sh\n\
         set -eu\n\
         HERE=$(CDPATH= cd \"$(dirname \"$0\")\" && pwd)\n\
         ROOT=\"$1\"; shift\n\
         {musl_step}\
         for d in \"$ROOT\"/erts-*/; do ERTS=\"${{d%/}}\"; done\n\
         for d in \"$ROOT\"/releases/*/; do REL=\"${{d%/}}\"; done\n\
         export ROOTDIR=\"$ROOT\"\n\
         export BINDIR=\"$ERTS/bin\"\n\
         export EMU=beam\n\
         export PROGNAME=erl\n\
         exec \"$ERTS/bin/erlexec\" \\\n  \
           -boot \"$REL/start\" \\\n  \
           -pa \"$HERE\"/app/*/ebin \\\n  \
           -noshell \\\n  \
           -eval '{app}@@main:run({app})' \\\n  \
           -extra \"$@\"\n",
    );
    let path = payload.join("run.sh");
    fs::write(&path, script).map_err(|e| format!("write run.sh: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod run.sh: {e}"))?;
    }
    Ok(())
}

/// Create a compressed tar of `members` (relative to `cwd`) at `out`.
fn make_archive(
    cwd: &Path,
    members: &[&str],
    out: &Path,
    compression: Compression,
) -> Result<(), String> {
    let mut args: Vec<String> = compression
        .create_flags()
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.push(out.to_string_lossy().into_owned());
    args.push("-C".into());
    args.push(cwd.to_string_lossy().into_owned());
    for m in members {
        args.push((*m).to_string());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    sh("tar", &refs, None)
}

/// The compressed payloads and their cache tags to embed in the launcher.
struct Payloads<'a> {
    otp_pack: &'a Path,
    app_pack: &'a Path,
    otp_tag: &'a str,
    app_tag: &'a str,
}

/// Stage the Zig launcher sources, embed both payloads, and (cross-)compile it.
fn build_launcher(
    zig_bin: &Path,
    staging: &Path,
    payloads: &Payloads,
    target: &Target,
    out: &Path,
) -> Result<(), String> {
    // Launcher sources are embedded at compile time, so a distributed panini
    // binary is self-contained and doesn't depend on the repo layout.
    const BUILD_ZIG: &str = include_str!("../launcher/build.zig");
    const MAIN_ZIG: &str = include_str!("../launcher/src/main.zig");

    let build_dir = staging.join("launcher");
    let _ = fs::remove_dir_all(&build_dir);
    let src = build_dir.join("src");
    fs::create_dir_all(&src).map_err(|e| format!("mkdir {}: {e}", src.display()))?;

    fs::write(build_dir.join("build.zig"), BUILD_ZIG)
        .map_err(|e| format!("write build.zig: {e}"))?;
    fs::write(src.join("main.zig"), MAIN_ZIG).map_err(|e| format!("write main.zig: {e}"))?;
    // The launcher extracts with `tar -xf` (format autodetected), so the packs
    // are embedded under fixed names regardless of which compressor produced them.
    fs::copy(payloads.otp_pack, src.join("otp.pack"))
        .map_err(|e| format!("stage otp pack: {e}"))?;
    fs::copy(payloads.app_pack, src.join("app.pack"))
        .map_err(|e| format!("stage app pack: {e}"))?;
    fs::write(
        src.join("gen.zig"),
        format!(
            "pub const otp_tag = \"{}\";\npub const app_tag = \"{}\";\n",
            payloads.otp_tag, payloads.app_tag
        ),
    )
    .map_err(|e| format!("write gen.zig: {e}"))?;

    // Cross-compile the launcher; native builds skip -Dtarget.
    let mut args: Vec<String> = vec!["build".into(), "-Doptimize=ReleaseFast".into()];
    if !target.is_host() {
        args.push(format!("-Dtarget={}", target.zig));
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    sh(&zig_bin.to_string_lossy(), &arg_refs, Some(&build_dir))?;

    let produced = build_dir.join("zig-out/bin/panini-app");
    fs::copy(&produced, out)
        .map_err(|e| format!("copy {} -> {}: {e}", produced.display(), out.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(out, fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

/// Small stable hash for cache-busting the extraction dir.
fn djb2(bytes: &[u8]) -> u32 {
    let mut h: u32 = 5381;
    for &b in bytes {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    h
}
