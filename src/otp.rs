//! Detection of the host Erlang/OTP installation and which OTP applications a
//! shipment actually needs, so we bundle a minimal (not full) runtime.

use crate::capture;
use crate::target::Target;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Absolute path to the host OTP root (e.g. .../lib/erlang), via `code:root_dir/0`.
pub fn root_dir() -> Result<PathBuf, String> {
    let out = capture(
        "erl",
        &[
            "-noshell",
            "-eval",
            "io:format(\"~s\", [code:root_dir()]), halt().",
        ],
    )?;
    Ok(PathBuf::from(out))
}

/// Find the single `erts-<vsn>` directory inside an OTP root.
pub fn find_erts_dir(root: &Path) -> Result<PathBuf, String> {
    fs::read_dir(root)
        .map_err(|e| format!("read {}: {e}", root.display()))?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("erts-"))
                    .unwrap_or(false)
        })
        .ok_or_else(|| format!("no erts-* dir in {}", root.display()))
}

/// Find the `releases/<name>/` directory inside an OTP root (holds start.boot).
pub fn find_release_dir(root: &Path) -> Result<PathBuf, String> {
    let releases = root.join("releases");
    fs::read_dir(&releases)
        .map_err(|e| format!("read {}: {e}", releases.display()))?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir() && p.join("start.boot").exists())
        .ok_or_else(|| format!("no releases/*/start.boot under {}", root.display()))
}

/// A ready-to-bundle OTP root for a precompiled version + target, downloading and
/// extracting into the cache on first use. Subsequent builds reuse the cache.
pub fn precompiled_root(version: &str, target: &Target, cache: &Path) -> Result<PathBuf, String> {
    let archives = cache.join("otp");
    fs::create_dir_all(&archives).map_err(|e| format!("mkdir {}: {e}", archives.display()))?;

    let name = target.otp_archive_name(version);
    let tgz = archives.join(&name);
    if !tgz.exists() {
        let url = target.otp_url(version);
        println!(
            "     downloading OTP {version} for {} (cached after first use)…",
            target.name
        );
        let part = tgz.with_extension("part");
        crate::sh(
            "curl",
            &["-fsSL", "--retry", "2", "-o", &part.to_string_lossy(), &url],
            None,
        )
        .map_err(|e| {
            format!("download failed: {e}\n  (is there a precompiled OTP {version} for {}? see https://github.com/erlef/otp_builds)", target.name)
        })?;
        fs::rename(&part, &tgz).map_err(|e| format!("finalize download: {e}"))?;
    }

    let stem = name.trim_end_matches(".tar.gz");
    let extract_dir = archives.join(stem);
    if !extract_dir.exists() {
        fs::create_dir_all(&extract_dir).ok();
        crate::sh(
            "tar",
            &[
                "-xzf",
                &tgz.to_string_lossy(),
                "-C",
                &extract_dir.to_string_lossy(),
            ],
            None,
        )?;
    }

    // Archives nest everything under a single top dir (e.g. otp_universal_apple_darwin_27.2).
    if find_erts_dir(&extract_dir).is_ok() {
        return Ok(extract_dir);
    }
    for e in fs::read_dir(&extract_dir)
        .map_err(|e| format!("read extract dir: {e}"))?
        .flatten()
    {
        let p = e.path();
        if p.is_dir() && find_erts_dir(&p).is_ok() {
            return Ok(p);
        }
    }
    Err(format!(
        "could not locate an OTP root inside {}",
        extract_dir.display()
    ))
}

/// The musl libc that BEAM Machine Linux builds are linked against. Their ELF
/// interpreter is a hardcoded `/tmp/libc-musl-<hash>.so`, so the `.so` must be
/// placed there before any OTP binary (or the produced app) runs.
pub struct Musl {
    /// Cached `.so` file to install at `/tmp/libc-musl-<hash>.so`.
    pub so: PathBuf,
    /// Content hash naming the interpreter file.
    pub hash: String,
}

impl Musl {
    /// The absolute path the OTP binaries expect their interpreter at.
    pub fn tmp_path(&self) -> String {
        format!("/tmp/libc-musl-{}.so", self.hash)
    }
}

/// For a Linux OTP root, fetch the matching musl libc runtime (hash read from the
/// build's `burrito_runtime_manifest.txt`), cached under `<cache>/musl`.
pub fn fetch_musl(root: &Path, cache: &Path) -> Result<Musl, String> {
    let manifest = fs::read_to_string(root.join("burrito_runtime_manifest.txt"))
        .map_err(|e| format!("read runtime manifest: {e}"))?;
    let hash = manifest
        .lines()
        .find_map(|l| l.strip_prefix("MUSL_HASH="))
        .map(|s| s.trim().to_string())
        .ok_or("no MUSL_HASH in runtime manifest")?;

    let dir = cache.join("musl");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let so = dir.join(format!("libc-musl-{hash}.so"));
    if !so.exists() {
        let url = format!(
            "https://beam-machine-universal.b-cdn.net/musl/libc-musl-{hash}.so\
             ?please-respect-my-bandwidth-costs=thank-you"
        );
        println!("     fetching musl runtime for Linux (cached after first use)…");
        let part = so.with_extension("part");
        crate::sh(
            "curl",
            &["-fsSL", "--retry", "2", "-o", &part.to_string_lossy(), &url],
            None,
        )?;
        fs::rename(&part, &so).map_err(|e| format!("finalize musl download: {e}"))?;
    }
    // The kernel requires the ELF interpreter itself to be executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&so, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod musl: {e}"))?;
    }
    Ok(Musl { so, hash })
}

/// ERTS version string (e.g. "17.0.3"); the runtime dir is `erts-<version>`.
pub fn erts_version() -> Result<String, String> {
    capture(
        "erl",
        &[
            "-noshell",
            "-eval",
            "io:format(\"~s\", [erlang:system_info(version)]), halt().",
        ],
    )
}

/// OTP release number (e.g. "29") — informational.
pub fn otp_release() -> Result<String, String> {
    capture(
        "erl",
        &[
            "-noshell",
            "-eval",
            "io:format(\"~s\", [erlang:system_info(otp_release)]), halt().",
        ],
    )
}

/// Determine which OTP `lib/<app>-<vsn>` directories are needed by scanning the
/// `{applications, [...]}` lists in every `.app` file in the shipment. Anything
/// that maps to a real dir under `<root>/lib` is included; kernel+stdlib always.
pub fn needed_lib_apps(shipment: &Path, root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    names.insert("kernel".into());
    names.insert("stdlib".into());

    for app_file in find_app_files(shipment) {
        let text = fs::read_to_string(&app_file).unwrap_or_default();
        for name in parse_applications(&text) {
            names.insert(name);
        }
    }

    let lib = root.join("lib");
    let mut dirs = Vec::new();
    for name in names {
        if let Some(dir) = newest_versioned_dir(&lib, &name) {
            dirs.push(dir);
        }
        // Names with no matching lib dir are non-OTP deps (e.g. gleam_stdlib);
        // those already ship as .beam under the app payload, so we skip them.
    }
    Ok(dirs)
}

fn find_app_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("app") {
                out.push(p);
            }
        }
    }
    out
}

/// Extract application names from a `{applications, [a, b, c]}` term.
fn parse_applications(text: &str) -> Vec<String> {
    let Some(start) = text.find("applications") else {
        return Vec::new();
    };
    let rest = &text[start..];
    let Some(open) = rest.find('[') else {
        return Vec::new();
    };
    let Some(close) = rest[open..].find(']') else {
        return Vec::new();
    };
    rest[open + 1..open + close]
        .split(',')
        .map(|s| s.trim().trim_matches('\'').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Newest `lib/<name>-<vsn>` directory for an app name, if any.
fn newest_versioned_dir(lib: &Path, name: &str) -> Option<PathBuf> {
    let prefix = format!("{name}-");
    let mut matches: Vec<PathBuf> = fs::read_dir(lib)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(&prefix))
                    .unwrap_or(false)
        })
        .collect();
    matches.sort();
    matches.pop()
}

/// List OTP versions that can be `--otp`'d, from the erlang/otp release tags.
/// Precompiled BEAM Machine builds exist for OTP 25.3+; older tags are hidden.
pub fn list_versions() -> Result<(), String> {
    let mut args = vec![
        "-fsSL",
        "https://api.github.com/repos/erlang/otp/tags?per_page=100",
        "-H",
        "Accept: application/vnd.github+json",
    ];
    // Use CI/user token if present to avoid unauthenticated rate limits.
    let auth;
    if let Ok(tok) = std::env::var("GITHUB_TOKEN") {
        if !tok.is_empty() {
            auth = format!("Authorization: Bearer {tok}");
            args.push("-H");
            args.push(&auth);
        }
    }
    let body = capture("curl", &args)?;

    let mut versions = extract_otp_versions(&body);
    versions.sort_by(|a, b| cmp_version(b, a)); // descending
    versions.dedup();

    println!("OTP versions available via --otp (newest first):\n");
    let mut shown = 0;
    for v in &versions {
        // BEAM Machine precompiled builds start at 25.3.
        if cmp_version(v, "25.3") == std::cmp::Ordering::Less {
            continue;
        }
        print!("  {v:<10}");
        shown += 1;
        if shown % 5 == 0 {
            println!();
        }
    }
    if shown % 5 != 0 {
        println!();
    }
    println!("\n  precompiled runtimes: https://github.com/erlef/otp_builds  (macOS builds are universal)");
    println!("  usage: panini build --otp <version> [--target <t>]");
    Ok(())
}

/// Scan a blob for `OTP-<major>[.<minor>...]` tag names.
fn extract_otp_versions(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let needle = b"OTP-";
    let mut i = 0;
    while i + needle.len() < bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let start = i + needle.len();
            let mut j = start;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b'.') {
                j += 1;
            }
            // Require a leading digit and at least one dot (skip old "OTP-R16B..").
            if j > start && bytes[start].is_ascii_digit() {
                let v = &text[start..j];
                if v.contains('.') && !v.ends_with('.') {
                    out.push(v.to_string());
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Compare dotted version strings numerically.
fn cmp_version(a: &str, b: &str) -> std::cmp::Ordering {
    let pa = a.split('.').map(|n| n.parse::<u32>().unwrap_or(0));
    let pb = b.split('.').map(|n| n.parse::<u32>().unwrap_or(0));
    pa.into_iter()
        .chain(std::iter::repeat(0))
        .zip(pb.into_iter().chain(std::iter::repeat(0)))
        .take(4)
        .map(|(x, y)| x.cmp(&y))
        .find(|o| *o != std::cmp::Ordering::Equal)
        .unwrap_or(std::cmp::Ordering::Equal)
}

pub fn print_info() -> Result<(), String> {
    let gleam = capture("gleam", &["--version"]).unwrap_or_else(|_| "not found".into());
    let root = root_dir()?;
    println!("panini toolchain");
    println!("  gleam:        {gleam}");
    println!("  otp release:  {}", otp_release()?);
    println!("  erts version: {}", erts_version()?);
    println!("  otp root:     {}", root.display());
    println!("  zig:          {}", crate::zig::describe());
    Ok(())
}
