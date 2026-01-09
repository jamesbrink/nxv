#!/usr/bin/env bash
# Verify nix build in an isolated Docker container
# This ensures the flake builds correctly without any local state

set -euo pipefail

echo "==> Verifying nix build in isolated Docker container..."
echo "    This archives HEAD and builds inside nixos/nix"

git archive HEAD | docker run --rm -i nixos/nix sh -c "
    mkdir /app && cd /app && tar x &&
    nix build --extra-experimental-features 'nix-command flakes' \
        --override-input nixpkgs github:NixOS/nixpkgs/nixos-unstable . -L
"

echo "==> Isolated nix build successful!"
