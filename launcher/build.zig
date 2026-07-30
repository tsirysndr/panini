const std = @import("std");

// The self-extracting launcher. `panini` injects src/payload.tar.gz and
// src/gen.zig before invoking `zig build -Doptimize=ReleaseFast`.
pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const exe = b.addExecutable(.{
        .name = "panini-app",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/main.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    b.installArtifact(exe);
}
