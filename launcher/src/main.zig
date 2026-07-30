//! panini self-extracting launcher (Burrito-style), for Zig 0.16.
//!
//! Two compressed payloads are embedded at build time via @embedFile:
//!   * otp.pack — a minimal, relocatable OTP runtime (erts + boot + lib apps)
//!   * app.pack — the compiled Gleam .beam plus the boot script (run.sh)
//!
//! On first run each is extracted to a cache dir. The runtime is keyed by a hash
//! of its *composition* (otp_tag), so different apps that bundle the same runtime
//! share a single extraction under ~/.cache/panini/rt/<otp_tag>; the app lives
//! under ~/.cache/panini/apps/<app_tag>. run.sh is then handed the runtime root
//! as its first argument and execs the BEAM VM.

const std = @import("std");

const otp_payload = @embedFile("otp.pack");
const app_payload = @embedFile("app.pack");
const gen = @import("gen.zig");

pub fn main(init: std.process.Init) !u8 {
    const io = init.io;
    const gpa = init.gpa;
    const arena = init.arena.allocator();

    const home = init.environ_map.get("HOME") orelse "/tmp";
    const base = try std.fmt.allocPrint(arena, "{s}/.cache/panini", .{home});

    // Shared, content-addressed runtime — extracted once across every app that
    // bundles an identical runtime.
    const rt_dir = try std.fmt.allocPrint(arena, "{s}/rt/{s}", .{ base, gen.otp_tag });
    if (!try ensureExtracted(io, gpa, arena, rt_dir, otp_payload)) {
        std.log.err("panini: failed to extract runtime", .{});
        return 1;
    }

    // Per-app files (compiled .beam + run.sh).
    const app_dir = try std.fmt.allocPrint(arena, "{s}/apps/{s}", .{ base, gen.app_tag });
    if (!try ensureExtracted(io, gpa, arena, app_dir, app_payload)) {
        std.log.err("panini: failed to extract app", .{});
        return 1;
    }

    const otp_root = try std.fmt.allocPrint(arena, "{s}/otp", .{rt_dir});
    const run_sh = try std.fmt.allocPrint(arena, "{s}/run.sh", .{app_dir});

    // argv: run.sh <otp_root> <user args…>. run.sh consumes the root ($1) and
    // forwards the rest to the app.
    const args = try init.minimal.args.toSlice(arena);
    const argv = try arena.alloc([]const u8, args.len + 1);
    argv[0] = run_sh;
    argv[1] = otp_root;
    for (args[1..], 2..) |a, i| argv[i] = a;

    return std.process.replace(io, .{ .argv = argv });
}

/// Ensure `data` (a compressed tarball) is extracted into `dir`, exactly once.
/// A `.extracted` marker records completion; returns false on extraction failure.
fn ensureExtracted(
    io: std.Io,
    gpa: std.mem.Allocator,
    arena: std.mem.Allocator,
    dir: []const u8,
    data: []const u8,
) !bool {
    const cwd = std.Io.Dir.cwd();
    const marker = try std.fmt.allocPrint(arena, "{s}/.extracted", .{dir});

    const already = blk: {
        cwd.access(io, marker, .{}) catch break :blk false;
        break :blk true;
    };
    if (already) return true;

    // Delegate dir creation + extraction to the system mkdir/tar, present on
    // every macOS/Linux target and independent of libc details. `tar -xf`
    // autodetects the compression format (gz/xz/zst).
    const pack = try std.fmt.allocPrint(arena, "{s}/pack", .{dir});
    _ = try std.process.run(gpa, io, .{ .argv = &.{ "mkdir", "-p", dir } });
    try cwd.writeFile(io, .{ .sub_path = pack, .data = data });
    const res = try std.process.run(gpa, io, .{ .argv = &.{ "tar", "-xf", pack, "-C", dir } });
    if (res.term != .exited or res.term.exited != 0) return false;

    // The compressed pack is only needed for this one extraction; drop it so the
    // cache holds just the unpacked tree (best-effort — a leftover is harmless).
    cwd.deleteFile(io, pack) catch {};
    try cwd.writeFile(io, .{ .sub_path = marker, .data = "" });
    return true;
}
