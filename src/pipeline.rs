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

struct Plan {
    /// OTP root to bundle for this target.
    root: PathBuf,
    /// If set, compile the shipment with this toolchain dir on PATH.
    toolbin: Option<PathBuf>,
}

pub fn build(
    project: &Path,
    out: Option<PathBuf>,
    otp_version: Option<String>,
    targets: Vec<Target>,
) -> Result<(), String> {
    let app = read_app_name(project)?;
    let multi = targets.len() > 1;
    println!(
        "\x1b[1m🥪 panini\x1b[0m  pressing '{app}' → {} target{}\n",
        targets.len(),
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
        println!("\n\x1b[1m▸ {}\x1b[0m  {compiled_with}", target.name);

        // 1. Gleam shipment, using the selected toolchain.
        compile_shipment(project, plan.toolbin.as_deref())?;
        let shipment = project.join("build/erlang-shipment");
        if !shipment.is_dir() {
            return Err(format!("expected shipment at {}", shipment.display()));
        }

        // 2. Assemble a minimal, relocatable runtime + the compiled app.
        let payload = staging.join("payload");
        fs::create_dir_all(&payload).map_err(|e| format!("mkdir {}: {e}", payload.display()))?;
        assemble_runtime(&plan.root, &shipment, &payload)?;
        copy_tree(&shipment, &payload.join("app"))?;
        write_run_sh(&payload, &app)?;

        // 3. Compress + embed into a (cross-compiled) self-extracting launcher.
        let tarball = staging.join("payload.tar.gz");
        sh(
            "tar",
            &[
                "-czf",
                &tarball.to_string_lossy(),
                "-C",
                &staging.to_string_lossy(),
                "payload",
            ],
            None,
        )?;
        let payload_bytes = fs::read(&tarball).map_err(|e| format!("read tarball: {e}"))?;
        println!("     payload: {} MB", payload_bytes.len() / 1_048_576);

        build_launcher(
            &zig_bin,
            &staging,
            &tarball,
            &app,
            &payload_bytes,
            target,
            &output,
        )?;
        println!("     \x1b[32m✓\x1b[0m {}", output.display());
    }

    println!("\n\x1b[32m✓ done\x1b[0m");
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
            if runnable_on_host(target) {
                // We can execute this OTP: compile the app with it (correct beam).
                let toolbin = make_toolbin(&root, staging)?;
                Ok(Plan {
                    root,
                    toolbin: Some(toolbin),
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
                })
            }
        }
        None if target.is_host() => Ok(Plan {
            root: otp::root_dir()?,
            toolbin: None,
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
    println!("     gleam export erlang-shipment");
    sh_env(
        "gleam",
        &["export", "erlang-shipment"],
        Some(project),
        toolbin,
    )
}

/// Copy the erts dir, boot/releases metadata, and only the OTP lib apps the app
/// actually references, into `<payload>/otp`.
fn assemble_runtime(root: &Path, shipment: &Path, payload: &Path) -> Result<(), String> {
    let otp_dst = payload.join("otp");
    let erts = otp::find_erts_dir(root)?;
    copy_tree(&erts, &otp_dst.join(erts.file_name().unwrap()))?;
    copy_tree(&root.join("releases"), &otp_dst.join("releases"))?;

    let lib_apps = otp::needed_lib_apps(shipment, root)?;
    for app_dir in &lib_apps {
        copy_tree(
            app_dir,
            &otp_dst.join("lib").join(app_dir.file_name().unwrap()),
        )?;
    }
    println!(
        "     runtime: {} + {} OTP lib apps",
        erts.file_name().unwrap().to_string_lossy(),
        lib_apps.len()
    );
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
fn write_run_sh(payload: &Path, app: &str) -> Result<(), String> {
    let script = format!(
        "#!/bin/sh\n\
         set -eu\n\
         HERE=$(CDPATH= cd \"$(dirname \"$0\")\" && pwd)\n\
         ROOT=\"$HERE/otp\"\n\
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

/// Stage the Zig launcher sources, embed the payload, and (cross-)compile it.
fn build_launcher(
    zig_bin: &Path,
    staging: &Path,
    tarball: &Path,
    app: &str,
    payload_bytes: &[u8],
    target: &Target,
    out: &Path,
) -> Result<(), String> {
    let template = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("launcher");
    if !template.join("build.zig").exists() {
        return Err(format!(
            "launcher template not found at {} (run panini from its repo for v0)",
            template.display()
        ));
    }

    let build_dir = staging.join("launcher");
    let _ = fs::remove_dir_all(&build_dir);
    let src = build_dir.join("src");
    fs::create_dir_all(&src).map_err(|e| format!("mkdir {}: {e}", src.display()))?;

    // Copy only the launcher sources — never the template's .zig-cache/zig-out.
    fs::copy(template.join("build.zig"), build_dir.join("build.zig"))
        .map_err(|e| format!("copy build.zig: {e}"))?;
    fs::copy(template.join("src/main.zig"), src.join("main.zig"))
        .map_err(|e| format!("copy main.zig: {e}"))?;
    fs::copy(tarball, src.join("payload.tar.gz")).map_err(|e| format!("stage payload: {e}"))?;
    let tag = format!("{app}-{:08x}", djb2(payload_bytes));
    fs::write(
        src.join("gen.zig"),
        format!("pub const app_tag = \"{tag}\";\n"),
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
