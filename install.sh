#!/bin/sh
# nxv installer - installs the latest release of nxv
#
# Usage:
#   curl -sSfL https://raw.githubusercontent.com/jamesbrink/nxv/main/install.sh | sh
#
# Options (via environment variables):
#   NXV_INSTALL_DIR  - Installation directory (default: ~/.local/bin or /usr/local/bin)
#   NXV_VERSION      - Specific version to install (default: latest)

set -e

REPO="jamesbrink/nxv"
BINARY_NAME="nxv"

# Colors (disabled if not a terminal)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BLUE='\033[0;34m'
    BOLD='\033[1m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    BOLD=''
    NC=''
fi

info() {
    printf "${BLUE}info:${NC} %s\n" "$1"
}

success() {
    printf "${GREEN}success:${NC} %s\n" "$1"
}

warn() {
    printf "${YELLOW}warning:${NC} %s\n" "$1"
}

error() {
    printf "${RED}error:${NC} %s\n" "$1" >&2
    exit 1
}

# Detect OS
detect_os() {
    case "$(uname -s)" in
        Linux*)  echo "linux" ;;
        Darwin*) echo "darwin" ;;
        *)       error "Unsupported operating system: $(uname -s)" ;;
    esac
}

# Detect architecture
detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)  echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *)             error "Unsupported architecture: $(uname -m)" ;;
    esac
}

# Get the latest release version from GitHub
get_latest_version() {
    if command -v curl > /dev/null 2>&1; then
        curl -sSfL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/'
    elif command -v wget > /dev/null 2>&1; then
        wget -qO- "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/'
    else
        error "Neither curl nor wget found. Please install one of them."
    fi
}

# Download a file
download() {
    url="$1"
    output="$2"

    if command -v curl > /dev/null 2>&1; then
        curl -sSfL "$url" -o "$output"
    elif command -v wget > /dev/null 2>&1; then
        wget -q "$url" -O "$output"
    else
        error "Neither curl nor wget found. Please install one of them."
    fi
}

# Determine install directory
get_install_dir() {
    if [ -n "${NXV_INSTALL_DIR:-}" ]; then
        echo "$NXV_INSTALL_DIR"
    elif [ -w "/usr/local/bin" ]; then
        echo "/usr/local/bin"
    else
        echo "${HOME}/.local/bin"
    fi
}

# Check if directory is in PATH
check_path() {
    dir="$1"
    case ":${PATH}:" in
        *":${dir}:"*) return 0 ;;
        *)            return 1 ;;
    esac
}

# Build artifact name based on OS and arch
get_artifact_name() {
    os="$1"
    arch="$2"

    case "$os" in
        linux)  echo "nxv-${arch}-linux-musl" ;;
        darwin) echo "nxv-${arch}-apple-darwin" ;;
    esac
}

main() {
    printf '\n%bnxv installer%b\n\n' "$BOLD" "$NC"

    OS=$(detect_os)
    ARCH=$(detect_arch)

    info "Detected platform: ${ARCH}-${OS}"

    # Determine version
    if [ -n "${NXV_VERSION:-}" ]; then
        VERSION="$NXV_VERSION"
        info "Using specified version: ${VERSION}"
    else
        info "Fetching latest release..."
        VERSION=$(get_latest_version)
        if [ -z "$VERSION" ]; then
            error "Failed to determine latest version. Check https://github.com/${REPO}/releases"
        fi
        info "Latest version: ${VERSION}"
    fi

    # Build artifact name and download URL
    ARTIFACT=$(get_artifact_name "$OS" "$ARCH")
    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARTIFACT}"

    # Determine install directory
    INSTALL_DIR=$(get_install_dir)
    INSTALL_PATH="${INSTALL_DIR}/${BINARY_NAME}"

    info "Installing to: ${INSTALL_PATH}"

    # Create install directory if needed
    if [ ! -d "$INSTALL_DIR" ]; then
        info "Creating directory: ${INSTALL_DIR}"
        mkdir -p "$INSTALL_DIR"
    fi

    # Download binary
    info "Downloading ${ARTIFACT}..."
    TMPFILE=$(mktemp)
    trap 'rm -f "$TMPFILE"' EXIT

    if ! download "$DOWNLOAD_URL" "$TMPFILE"; then
        error "Failed to download ${DOWNLOAD_URL}\nCheck if the release exists: https://github.com/${REPO}/releases"
    fi

    # Install binary
    mv "$TMPFILE" "$INSTALL_PATH"
    chmod +x "$INSTALL_PATH"

    success "Installed ${BINARY_NAME} ${VERSION} to ${INSTALL_PATH}"

    # Check if install directory is in PATH
    if ! check_path "$INSTALL_DIR"; then
        printf '\n'
        warn "Installation directory is not in your PATH."
        printf '\nAdd it by running:\n'
        # shellcheck disable=SC2016
        printf '  %bexport PATH="$PATH:%s"%b\n' "$BOLD" "$INSTALL_DIR" "$NC"
        printf '\nTo make it permanent, add the above line to your shell config:\n'
        printf '  %b~/.bashrc%b, %b~/.zshrc%b, or %b~/.config/fish/config.fish%b\n' "$BOLD" "$NC" "$BOLD" "$NC" "$BOLD" "$NC"
    fi

    printf '\n%bInstallation complete!%b\n' "$GREEN" "$NC"
    printf "Run '%bnxv --help%b' to get started.\n" "$BOLD" "$NC"
    printf "Run '%bnxv update%b' to download the package index.\n\n" "$BOLD" "$NC"
}

main "$@"
