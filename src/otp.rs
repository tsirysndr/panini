//! Detection of the host Erlang/OTP installation and which OTP applications a
//! shipment actually needs, so we bundle a minimal (not full) runtime.

use crate::capture;
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
        let Ok(entries) = fs::read_dir(&d) else { continue };
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

pub fn print_info() -> Result<(), String> {
    let gleam = capture("gleam", &["--version"]).unwrap_or_else(|_| "not found".into());
    let root = root_dir()?;
    println!("panini toolchain");
    println!("  gleam:        {gleam}");
    println!("  otp release:  {}", otp_release()?);
    println!("  erts version: {}", erts_version()?);
    println!("  otp root:     {}", root.display());
    Ok(())
}
