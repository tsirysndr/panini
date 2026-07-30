//! Ensure a Zig 0.16.0 toolchain is available. The launcher relies on APIs that
//! only exist in Zig 0.16.0, so we pin that exact version: use the host `zig` if
//! it matches, otherwise download 0.16.0 into the cache once and reuse it.

use std::path::PathBuf;

pub const REQUIRED: &str = "0.16.0";

/// Path to a usable Zig 0.16.0 (`"zig"` on PATH, or a cached download).
pub fn resolve() -> Result<PathBuf, String> {
    if let Ok(v) = crate::capture("zig", &["version"]) {
        if v.trim() == REQUIRED {
            return Ok(PathBuf::from("zig"));
        }
    }

    let (arch, os) = host_tuple()?;
    let cache = cache_root();
    let dir = cache.join(format!("zig-{arch}-{os}-{REQUIRED}"));
    let bin = dir.join("zig");
    if bin.exists() {
        return Ok(bin);
    }

    download(&cache, &arch, &os)?;
    if bin.exists() {
        Ok(bin)
    } else {
        Err(format!(
            "zig not found at {} after extraction",
            bin.display()
        ))
    }
}

/// Report which Zig will be used, for `panini info`.
pub fn describe() -> String {
    match crate::capture("zig", &["version"]) {
        Ok(v) if v.trim() == REQUIRED => format!("{v} (host)"),
        Ok(v) => format!("host has {v}; will fetch {REQUIRED}"),
        Err(_) => format!("not installed; will fetch {REQUIRED}"),
    }
}

fn download(cache: &std::path::Path, arch: &str, os: &str) -> Result<(), String> {
    std::fs::create_dir_all(cache).map_err(|e| format!("mkdir {}: {e}", cache.display()))?;
    let file = format!("zig-{arch}-{os}-{REQUIRED}.tar.xz");
    let url = format!("https://ziglang.org/download/{REQUIRED}/{file}");
    let tgz = cache.join(&file);
    println!("     fetching Zig {REQUIRED} for {arch}-{os} (one time)…");
    let part = tgz.with_extension("part");
    crate::sh(
        "curl",
        &["-fsSL", "--retry", "2", "-o", &part.to_string_lossy(), &url],
        None,
    )
    .map_err(|e| format!("zig download failed: {e}"))?;
    std::fs::rename(&part, &tgz).map_err(|e| format!("finalize zig download: {e}"))?;
    // The tarball unpacks to zig-<arch>-<os>-<ver>/ containing the `zig` binary.
    crate::sh(
        "tar",
        &[
            "-xf",
            &tgz.to_string_lossy(),
            "-C",
            &cache.to_string_lossy(),
        ],
        None,
    )?;
    Ok(())
}

fn host_tuple() -> Result<(String, String), String> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => return Err(format!("no Zig 0.16.0 mapping for host arch {other}")),
    };
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        other => return Err(format!("no Zig 0.16.0 mapping for host os {other}")),
    };
    Ok((arch.into(), os.into()))
}

fn cache_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".cache/panini/zig")
}
