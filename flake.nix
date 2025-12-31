{
  description = "nxv - Nix Versions CLI tool";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Use stable Rust toolchain
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };

        # Create crane lib with our toolchain
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Common source filtering
        src = craneLib.cleanCargoSource ./.;

        # Common build arguments
        commonArgs = {
          inherit src;
          strictDeps = true;

          buildInputs = [
            pkgs.openssl
          ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.libiconv
            pkgs.darwin.libiconv
          ];

          nativeBuildInputs = [
            pkgs.pkg-config
          ];
        };

        # Build dependencies only (for caching)
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Build the main nxv package
        nxv = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;

          meta = {
            description = "CLI tool for finding specific versions of Nix packages";
            homepage = "https://github.com/jamesbrink/nxv";
            license = pkgs.lib.licenses.mit;
            maintainers = [ ];
            mainProgram = "nxv";
          };
        });

        # Build nxv with indexer feature enabled
        nxv-indexer = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          cargoExtraArgs = "--features indexer";
          pname = "nxv-indexer";

          buildInputs = commonArgs.buildInputs ++ [
            pkgs.libgit2
          ];

          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
            pkgs.cmake
            pkgs.git  # Required for tests that create git repos
          ];

          meta = {
            description = "CLI tool for finding specific versions of Nix packages (with indexer)";
            homepage = "https://github.com/jamesbrink/nxv";
            license = pkgs.lib.licenses.mit;
            maintainers = [ ];
            mainProgram = "nxv";
          };
        });

      in
      {
        # Packages
        packages = {
          inherit nxv nxv-indexer;
          default = nxv;
        };

        # Development shell
        devShells.default = craneLib.devShell {
          # Include dependencies from the main package
          inputsFrom = [ nxv ];

          # Extra development tools
          packages = [
            pkgs.rust-analyzer
            pkgs.cargo-watch
            pkgs.cargo-edit
          ];

          RUST_BACKTRACE = "1";
        };

        # Checks (run with `nix flake check`)
        checks = {
          inherit nxv;

          # Run clippy
          nxv-clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });

          # Run tests
          nxv-test = craneLib.cargoTest (commonArgs // {
            inherit cargoArtifacts;
          });

          # Check formatting
          nxv-fmt = craneLib.cargoFmt {
            inherit src;
          };
        };

        # Apps (run with `nix run`)
        apps = {
          default = flake-utils.lib.mkApp {
            drv = nxv;
          };
          nxv = flake-utils.lib.mkApp {
            drv = nxv;
          };
          nxv-indexer = flake-utils.lib.mkApp {
            drv = nxv-indexer;
            name = "nxv";
          };
        };
      }
    );
}
