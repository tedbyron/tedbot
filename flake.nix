{
  inputs = {
    nixpkgs.url = "nixpkgs/nixpkgs-unstable";
    flake-utils.url = "flake-utils";
    crane.url = "github:ipetkov/crane";
    rust-overlay.url = "github:oxalica/rust-overlay";
    treefmt.url = "github:/numtide/treefmt-nix";
    treefmt.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { self, ... }@inputs:
    inputs.flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [ (import inputs.rust-overlay) ];
        };
        rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rust;
        commonArgs = {
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
          inherit (craneLib.crateNameFromCargoToml { cargoToml = ./tedbot/Cargo.toml; }) pname version;
        };
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        tedbot = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            doCheck = false;
            meta.mainProgram = "tedbot";
          }
        );
        treefmt =
          (inputs.treefmt.lib.evalModule pkgs {
            programs = {
              deadnix.enable = true;
              nixfmt.enable = true;
              rustfmt.enable = true;
              statix.enable = true;
              taplo.enable = true;
              prettier = {
                enable = true;
                settings = {
                  proseWrap = "always";
                  semi = false;
                  singleQuote = true;
                  trailingComma = "all";
                  overrides = [
                    {
                      files = "*.yml";
                      options.singleQuote = false;
                    }
                  ];
                };
              };
            };
            settings.excludes = [
              ".gitignore"
              "LICENSE-*"
              "flake.lock"
            ];
          }).config.build;
      in
      with pkgs;
      {
        checks = {
          inherit tedbot;
          formatting = treefmt.check self;
          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            }
          );
          test = craneLib.cargoTest (commonArgs // { inherit cargoArtifacts; });
          # Advisories need the network; run `cargo deny check` in the dev shell for them
          deny = craneLib.cargoDeny (
            commonArgs
            // {
              src = lib.cleanSourceWith {
                src = ./.;
                filter = path: type: craneLib.filterCargoSources path type || baseNameOf path == "deny.toml";
              };
              cargoDenyChecks = "bans licenses sources";
            }
          );
        };
        formatter = treefmt.wrapper;

        devShells.default = mkShellNoCC {
          packages = [
            rust
            bacon
            cargo-deny
          ];
        };

        packages.default = tedbot;
      }
    );
}
