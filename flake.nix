{
  description = "nxv - Nix Version Index";

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

      # NixOS module for running nxv as a service
      # The module is passed the flake's packages so it works without the overlay
      nixosModules.default = import ./nix/module.nix { flakePackages = self.packages; };
      nixosModules.nxv = self.nixosModules.default;
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

        # Common source filtering - include frontend and keys directories for embedded assets
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            (craneLib.filterCargoSources path type) ||
            (builtins.match ".*frontend.*" path != null) ||
            (builtins.match ".*keys/.*\\.pub$" path != null);
        };

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
            description = "Nix Version Index";
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
            description = "Nix Version Index (with indexer feature)";
            homepage = "https://github.com/jamesbrink/nxv";
            license = pkgs.lib.licenses.mit;
            maintainers = [ ];
            mainProgram = "nxv";
          };
        });

        # Static musl build (Linux only)
        # Uses cross-compilation approach to avoid build script crashes
        nxv-static = let
          # Only build static on Linux
          isLinux = pkgs.stdenv.isLinux;
          target = if system == "aarch64-linux"
                   then "aarch64-unknown-linux-musl"
                   else "x86_64-unknown-linux-musl";

          # musl cross-compilation pkgs
          pkgsMusl = if system == "aarch64-linux"
                     then pkgs.pkgsCross.aarch64-multiplatform-musl
                     else pkgs.pkgsCross.musl64;

          # Get the musl C compiler
          muslCC = "${pkgsMusl.stdenv.cc}/bin/${pkgsMusl.stdenv.cc.targetPrefix}cc";

          # Toolchain with musl target added
          rustToolchainMusl = pkgs.rust-bin.stable.latest.default.override {
            targets = [ target ];
          };

          # Crane lib with musl toolchain
          craneLibMusl = (crane.mkLib pkgs).overrideToolchain rustToolchainMusl;

          # Common musl build args
          muslBuildArgs = {
            inherit src;
            inherit (crateInfo) pname version;
            strictDeps = true;

            CARGO_BUILD_TARGET = target;
            CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static -C linker=${muslCC}";

            # C compiler configuration for musl
            # HOST_CC is for build scripts that run on the build machine
            # TARGET_CC/CC_x86_64_unknown_linux_musl is for code that runs on target
            HOST_CC = "${pkgs.stdenv.cc}/bin/cc";
            TARGET_CC = muslCC;
            CC_x86_64_unknown_linux_musl = muslCC;
            CC_aarch64_unknown_linux_musl = muslCC;

            # Disable glibc-specific hardening that breaks musl
            hardeningDisable = [ "fortify" ];

            nativeBuildInputs = [
              pkgs.pkg-config
              pkgsMusl.stdenv.cc  # musl cross-compiler
            ];

            # Add musl libc for static linking
            buildInputs = [ ];
          };

          cargoArtifactsMusl = craneLibMusl.buildDepsOnly muslBuildArgs;

        in if isLinux then craneLibMusl.buildPackage (muslBuildArgs // {
          pname = "nxv-static";
          cargoArtifacts = cargoArtifactsMusl;

          nativeBuildInputs = muslBuildArgs.nativeBuildInputs ++ [
            pkgs.installShellFiles
          ];

          # Shell completions still work - binary runs on host during build
          postInstall = installCompletions;

          meta = {
            description = "Nix Version Index (static musl binary)";
            homepage = "https://github.com/jamesbrink/nxv";
            license = pkgs.lib.licenses.mit;
            maintainers = [ ];
            mainProgram = "nxv";
            platforms = [ "x86_64-linux" "aarch64-linux" ];
          };
        }) else pkgs.runCommand "nxv-static-unavailable" {} ''
          echo "nxv-static is only available on Linux" >&2
          exit 1
        '';

        # Docker image for nxv-indexer (Linux only)
        nxv-docker = if pkgs.stdenv.isLinux then pkgs.dockerTools.buildLayeredImage {
          name = "nxv";
          tag = crateInfo.version;

          contents = [
            nxv-indexer
            pkgs.cacert        # CA certificates for HTTPS
            pkgs.tzdata        # Timezone data
            pkgs.git           # Required for indexing nixpkgs
          ];

          config = {
            Entrypoint = [ "${nxv-indexer}/bin/nxv" ];
            Cmd = [ "serve" ];
            ExposedPorts = {
              "8080/tcp" = {};
            };
            Env = [
              "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
              "TZ=UTC"
            ];
            Labels = {
              "org.opencontainers.image.title" = "nxv";
              "org.opencontainers.image.description" = "Nix Version Index - search nixpkgs package history";
              "org.opencontainers.image.source" = "https://github.com/jamesbrink/nxv";
              "org.opencontainers.image.version" = crateInfo.version;
            };
          };
        } else pkgs.runCommand "nxv-docker-unavailable" {} ''
          echo "Docker images are only available on Linux" >&2
          exit 1
        '';

      in
      {
        # Packages
        packages = {
          inherit nxv nxv-indexer nxv-static nxv-docker;
          default = nxv;
        };

        # Development shell
        devShells.default = craneLib.devShell {
          inputsFrom = [ nxv ];

          packages = [
            pkgs.rust-analyzer
            pkgs.cargo-watch
            pkgs.cargo-edit
            pkgs.miniserve  # Simple HTTP server for frontend dev
            pkgs.nodePackages.prettier  # HTML/JS/CSS formatter
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
