//! The build pipeline: Gleam shipment -> minimal runtime -> payload -> launcher.

use crate::{copy_tree, otp, sh};
use std::fs;
use std::path::{Path, PathBuf};

pub fn build(project: &Path, out: Option<PathBuf>) -> Result<(), String> {
    let app = read_app_name(project)?;
    println!("\x1b[1m🥪 panini\x1b[0m  pressing '{app}' into a single binary\n");

    // 1. Gleam shipment (compiled .beam + .app files, no runtime).
    step(1, "gleam export erlang-shipment");
    sh("gleam", &["export", "erlang-shipment"], Some(project))?;
    let shipment = project.join("build/erlang-shipment");
    if !shipment.is_dir() {
        return Err(format!("expected shipment at {}", shipment.display()));
    }

    // Fresh staging area under the project's build dir.
    let staging = project.join("build/panini");
    let _ = fs::remove_dir_all(&staging);
    let payload = staging.join("payload");
    fs::create_dir_all(&payload).map_err(|e| format!("mkdir {}: {e}", payload.display()))?;

    // 2. Assemble a minimal, relocatable OTP runtime.
    step(2, "assembling minimal OTP runtime");
    let root = otp::root_dir()?;
    let erts = format!("erts-{}", otp::erts_version()?);
    let otp_dst = payload.join("otp");
    copy_tree(&root.join(&erts), &otp_dst.join(&erts))?;
    copy_tree(&root.join("bin"), &otp_dst.join("bin"))?;
    copy_tree(&root.join("releases"), &otp_dst.join("releases"))?;
    let lib_apps = otp::needed_lib_apps(&shipment, &root)?;
    for app_dir in &lib_apps {
        let name = app_dir.file_name().unwrap();
        copy_tree(app_dir, &otp_dst.join("lib").join(name))?;
    }
    println!(
        "     bundled {} + {} OTP lib apps",
        erts,
        lib_apps.len()
    );

    // Copy the compiled Gleam code (the shipment) alongside the runtime.
    copy_tree(&shipment, &payload.join("app"))?;

    // 3. Generate the relocatable boot script.
    write_run_sh(&payload, &app)?;

    // 4. Compress the payload and embed it in the Zig launcher.
    step(3, "compressing payload");
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

    step(4, "building self-extracting launcher (zig)");
    let out = out.unwrap_or_else(|| project.join(&app));
    build_launcher(&staging, &tarball, &app, &payload_bytes, &out)?;

    println!("\n\x1b[32m✓ done\x1b[0m  →  {}", out.display());
    println!("  run it with nothing installed:  {}", display_run(&out));
    Ok(())
}

fn step(n: u8, msg: &str) {
    println!("\x1b[36m[{n}/4]\x1b[0m {msg}");
}

fn display_run(out: &Path) -> String {
    let s = out.display().to_string();
    if s.contains('/') {
        s
    } else {
        format!("./{s}")
    }
}

/// Read `name = "..."` from the project's gleam.toml.
fn read_app_name(project: &Path) -> Result<String, String> {
    let toml = fs::read_to_string(project.join("gleam.toml"))
        .map_err(|_| format!("no gleam.toml in {} — is this a Gleam project?", project.display()))?;
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

/// A self-locating boot script: sets ERL_ROOTDIR to the extracted runtime and
/// boots the Gleam app's generated `@@main` entrypoint.
fn write_run_sh(payload: &Path, app: &str) -> Result<(), String> {
    let script = format!(
        "#!/bin/sh\n\
         set -eu\n\
         HERE=$(CDPATH= cd \"$(dirname \"$0\")\" && pwd)\n\
         export ERL_ROOTDIR=\"$HERE/otp\"\n\
         exec \"$HERE/otp/bin/erl\" \\\n  \
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

/// Stage the Zig launcher template, embed the payload, and compile it.
fn build_launcher(
    staging: &Path,
    tarball: &Path,
    app: &str,
    payload_bytes: &[u8],
    out: &Path,
) -> Result<(), String> {
    // The launcher template lives in the panini repo next to this source.
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

    // Copy only the launcher sources — never the template's .zig-cache/zig-out,
    // which would let zig treat a stale placeholder build as up-to-date.
    fs::copy(template.join("build.zig"), build_dir.join("build.zig"))
        .map_err(|e| format!("copy build.zig: {e}"))?;
    fs::copy(template.join("src/main.zig"), src.join("main.zig"))
        .map_err(|e| format!("copy main.zig: {e}"))?;

    // Embed payload + a per-app cache tag (name + content hash) for cache-busting.
    fs::copy(tarball, src.join("payload.tar.gz")).map_err(|e| format!("stage payload: {e}"))?;
    let tag = format!("{app}-{:08x}", djb2(payload_bytes));
    fs::write(
        src.join("gen.zig"),
        format!("pub const app_tag = \"{tag}\";\n"),
    )
    .map_err(|e| format!("write gen.zig: {e}"))?;

    sh(
        "zig",
        &["build", "-Doptimize=ReleaseFast"],
        Some(&build_dir),
    )?;

    let produced = build_dir.join("zig-out/bin/panini-app");
    fs::copy(&produced, out).map_err(|e| {
        format!(
            "copy {} -> {}: {e}",
            produced.display(),
            out.display()
        )
    })?;
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
