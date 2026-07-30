//! panini self-extracting launcher (Burrito-style), for Zig 0.16.
//!
//! The compressed payload (Gleam .beam + a minimal, relocatable OTP runtime) is
//! embedded at build time via @embedFile. On first run it is extracted to a
//! per-app cache dir; every run then execs the boot script, which sets
//! ERL_ROOTDIR and starts the BEAM VM.

const std = @import("std");

const payload = @embedFile("payload.tar.gz");
const app_tag = @import("gen.zig").app_tag; // "<name>-<payloadhash>"

pub fn main(init: std.process.Init) !u8 {
    const io = init.io;
    const gpa = init.gpa;
    const arena = init.arena.allocator();

    const home = init.environ_map.get("HOME") orelse "/tmp";

    const cache = try std.fmt.allocPrint(arena, "{s}/.cache/panini/{s}", .{ home, app_tag });
    const marker = try std.fmt.allocPrint(arena, "{s}/.extracted", .{cache});
    const tgz = try std.fmt.allocPrint(arena, "{s}/payload.tar.gz", .{cache});
    const run_sh = try std.fmt.allocPrint(arena, "{s}/payload/run.sh", .{cache});

    const cwd = std.Io.Dir.cwd();

    const already = blk: {
        cwd.access(io, marker, .{}) catch break :blk false;
        break :blk true;
    };

    if (!already) {
        // Delegate dir creation + extraction to the system tar/mkdir, which are
        // present on every macOS/Linux target and independent of libc details.
        _ = try std.process.run(gpa, io, .{ .argv = &.{ "mkdir", "-p", cache } });
        try cwd.writeFile(io, .{ .sub_path = tgz, .data = payload });
        const res = try std.process.run(gpa, io, .{ .argv = &.{ "tar", "-xzf", tgz, "-C", cache } });
        if (res.term != .exited or res.term.exited != 0) {
            std.log.err("panini: failed to extract payload", .{});
            return 1;
        }
        try cwd.writeFile(io, .{ .sub_path = marker, .data = "" });
    }

    // Forward our args (minus argv[0]) to the boot script and hand off the process.
    const args = try init.minimal.args.toSlice(arena);
    const argv = try arena.alloc([]const u8, args.len);
    argv[0] = run_sh;
    for (args[1..], 1..) |a, i| argv[i] = a;

    return std.process.replace(io, .{ .argv = argv });
}
