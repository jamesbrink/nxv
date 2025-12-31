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
    {
      # Overlay for use in NixOS/home-manager configs
      overlays.default = final: prev: {
        nxv = self.packages.${prev.system}.nxv;
        nxv-indexer = self.packages.${prev.system}.nxv-indexer;
      };
    } // flake-utils.lib.eachDefaultSystem (system:
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

        # Read crate metadata from Cargo.toml
        crateInfo = craneLib.crateNameFromCargoToml { cargoToml = ./Cargo.toml; };

        # Common build arguments
        commonArgs = {
          inherit src;
          inherit (crateInfo) pname version;
          strictDeps = true;

          buildInputs = [
            pkgs.openssl
          ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.libiconv
            pkgs.darwin.libiconv
          ];

          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.installShellFiles
          ];
        };

        # Build dependencies only (for caching)
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Shell completions install script
        installCompletions = ''
          installShellCompletion --cmd nxv \
            --bash <($out/bin/nxv completions bash) \
            --zsh <($out/bin/nxv completions zsh) \
            --fish <($out/bin/nxv completions fish)
        '';

        # Build the main nxv package
        nxv = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;

          postInstall = installCompletions;

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
            pkgs.git
          ];

          postInstall = installCompletions;

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
          inputsFrom = [ nxv ];

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

          nxv-clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });

          nxv-test = craneLib.cargoTest (commonArgs // {
            inherit cargoArtifacts;
          });

          nxv-fmt = craneLib.cargoFmt {
            inherit src;
          };
        };

        # Apps (run with `nix run`)
        apps = {
          default = {
            type = "app";
            program = "${nxv}/bin/nxv";
            meta = nxv.meta;
          };
          nxv = {
            type = "app";
            program = "${nxv}/bin/nxv";
            meta = nxv.meta;
          };
          nxv-indexer = {
            type = "app";
            program = "${nxv-indexer}/bin/nxv";
            meta = nxv-indexer.meta;
          };
        };
      }
    );
}
