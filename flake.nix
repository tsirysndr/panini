{
  description = "panini - press a Gleam (Erlang/BEAM) app into a single self-contained binary";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    crane.url = "github:ipetkov/crane";

    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, crane, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        inherit (pkgs) lib;

        craneLib = crane.mkLib pkgs;

        # panini embeds the Zig launcher sources via include_str!, so the crate
        # won't compile without launcher/*.zig. crane's default source filter
        # keeps only cargo files, so we additionally keep any .zig file.
        src = lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            (craneLib.filterCargoSources path type) || (lib.hasSuffix ".zig" path);
          name = "source";
        };

        commonArgs = {
          inherit src;
          pname = "panini";
          version = "0.1.0";
          strictDeps = true;
        };

        # panini has no crate dependencies, but keeping the split lets CI cache
        # the (empty) dep build and mirrors the standard crane layout.
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Wrapping lives only on the final package: buildDepsOnly never produces
        # bin/panini, so a postInstall wrap in commonArgs would fail there.
        panini = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;

          # panini shells out to these at runtime; provide them as a fallback
          # (--suffix keeps any versions already on the user's PATH first).
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postInstall = ''
            wrapProgram $out/bin/panini \
              --suffix PATH : ${lib.makeBinPath [ pkgs.curl pkgs.gnutar pkgs.gzip pkgs.xz ]}
          '';

          meta = with lib; {
            description = "Press a Gleam (Erlang/BEAM) app into a single self-contained binary";
            homepage = "https://github.com/tsirysndr/panini";
            license = licenses.mit;
            mainProgram = "panini";
          };
        });
      in
      {
        checks = {
          inherit panini;

          panini-clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });

          panini-fmt = craneLib.cargoFmt { inherit src; };
        };

        packages.default = panini;
        packages.panini = panini;

        apps.default = flake-utils.lib.mkApp { drv = panini; };

        # `nix develop` — toolchain + everything to build Gleam apps.
        # (Zig is fetched by panini itself, pinned to 0.16.0, so it's not listed.)
        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          packages = with pkgs; [
            gleam
            erlang
            rebar3
            curl
            gnutar
            gzip
            xz
          ];
        };
      });
}
