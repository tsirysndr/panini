//! Build targets: the (os, arch) tuples panini can produce binaries for, and how
//! each maps to a Zig cross-compilation triple and a precompiled-OTP archive.

/// A supported build target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    /// panini's canonical name, e.g. "aarch64-macos".
    pub name: &'static str,
    /// "macos", "linux", or a BSD ("freebsd"/"netbsd"/"openbsd").
    pub os: &'static str,
    /// "x86_64" or "aarch64".
    pub arch: &'static str,
    /// Zig target triple for cross-compiling the launcher.
    pub zig: &'static str,
    /// Buildable only natively on a host of the same OS: no precompiled OTP
    /// runtime exists to cross-compile/bundle for it, so panini must use the
    /// host's own Erlang. The BSDs are host-only for this reason.
    pub host_only: bool,
}

/// Every target panini knows how to build. (Windows is not yet supported: it
/// needs a .ps1 boot path and a different launcher exec model.)
///
/// macOS/Linux ship precompiled OTP runtimes, so they can be cross-built from
/// any supported host. The BSDs are `host_only`: there are no precompiled BSD
/// OTP builds to fetch, so they can only be built on a machine of that OS using
/// its installed Erlang (and are excluded from `--target all`).
pub const ALL: &[Target] = &[
    Target {
        name: "aarch64-macos",
        os: "macos",
        arch: "aarch64",
        zig: "aarch64-macos",
        host_only: false,
    },
    Target {
        name: "x86_64-macos",
        os: "macos",
        arch: "x86_64",
        zig: "x86_64-macos",
        host_only: false,
    },
    Target {
        name: "aarch64-linux",
        os: "linux",
        arch: "aarch64",
        zig: "aarch64-linux-musl",
        host_only: false,
    },
    Target {
        name: "x86_64-linux",
        os: "linux",
        arch: "x86_64",
        zig: "x86_64-linux-musl",
        host_only: false,
    },
    Target {
        name: "x86_64-freebsd",
        os: "freebsd",
        arch: "x86_64",
        zig: "x86_64-freebsd",
        host_only: true,
    },
    Target {
        name: "x86_64-netbsd",
        os: "netbsd",
        arch: "x86_64",
        zig: "x86_64-netbsd",
        host_only: true,
    },
    Target {
        name: "x86_64-openbsd",
        os: "openbsd",
        arch: "x86_64",
        zig: "x86_64-openbsd",
        host_only: true,
    },
];

/// The macOS/Linux subset that can be cross-built (i.e. what `--target all`
/// expands to). BSD host-only targets are excluded.
pub fn cross_capable() -> impl Iterator<Item = Target> {
    ALL.iter().copied().filter(|t| !t.host_only)
}

/// The target matching the machine panini is running on, if supported.
pub fn host() -> Option<Target> {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        "freebsd" => "freebsd",
        "netbsd" => "netbsd",
        "openbsd" => "openbsd",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        _ => return None,
    };
    ALL.iter().copied().find(|t| t.os == os && t.arch == arch)
}

/// Resolve a target by name, or "host" for the current machine.
pub fn parse(name: &str) -> Result<Target, String> {
    if name == "host" {
        return host().ok_or_else(|| "this host platform is not a supported target".to_string());
    }
    ALL.iter()
        .copied()
        .find(|t| t.name == name)
        .ok_or_else(|| format!("unknown target '{name}' (see `panini targets`)"))
}

impl Target {
    pub fn is_host(&self) -> bool {
        host().map(|h| h.name == self.name).unwrap_or(false)
    }

    /// BEAM Machine download URL for a given OTP version.
    /// macOS ships a single universal (x86_64+aarch64) archive; Linux is per-arch.
    pub fn otp_url(&self, version: &str) -> String {
        // The query string is a bandwidth-courtesy marker requested by the host.
        const POLITE: &str = "?please-respect-my-bandwidth-costs=thank-you";
        const BASE: &str = "https://beam-machine-universal.b-cdn.net";
        match self.os {
            "macos" => format!(
                "{BASE}/OTP-{version}/macos/universal/otp_{version}_macos_universal.tar.gz{POLITE}"
            ),
            _ => format!(
                "{BASE}/OTP-{version}/linux/{arch}/any/otp_{version}_linux_any_{arch}.tar.gz{POLITE}",
                arch = self.arch
            ),
        }
    }

    /// Stable cache filename for the archive (macOS is arch-independent).
    pub fn otp_archive_name(&self, version: &str) -> String {
        match self.os {
            "macos" => format!("otp_{version}_macos_universal.tar.gz"),
            _ => format!("otp_{version}_linux_any_{arch}.tar.gz", arch = self.arch),
        }
    }
}
