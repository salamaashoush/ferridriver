#!/usr/bin/env bash
set -euo pipefail

# ferridriver install script
# Installs system dependencies and the ferridriver CLI binary.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/salamaashoush/ferridriver/main/install.sh | bash
#   # or with options:
#   curl -fsSL ... | bash -s -- --no-browser    # skip browser download
#   curl -fsSL ... | bash -s -- --deps-only     # only install system deps

REPO="salamaashoush/ferridriver"
INSTALL_DIR="${FERRIDRIVER_INSTALL_DIR:-$HOME/.ferridriver/bin}"
NO_BROWSER=false
DEPS_ONLY=false

for arg in "$@"; do
  case "$arg" in
    --no-browser) NO_BROWSER=true ;;
    --deps-only)  DEPS_ONLY=true ;;
    --help|-h)
      echo "Usage: install.sh [OPTIONS]"
      echo ""
      echo "Options:"
      echo "  --no-browser   Skip downloading Chromium after install"
      echo "  --deps-only    Only install system dependencies, skip binary"
      echo "  --help         Show this help"
      exit 0
      ;;
  esac
done

info()  { echo "  [*] $*"; }
warn()  { echo "  [!] $*" >&2; }
error() { echo "  [x] $*" >&2; exit 1; }

# --- Detect platform ---

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)  PLATFORM="linux" ;;
  Darwin) PLATFORM="macos" ;;
  *)      error "Unsupported OS: $OS" ;;
esac

case "$ARCH" in
  x86_64|amd64)  ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *)             error "Unsupported architecture: $ARCH" ;;
esac

# --- Detect Linux distro ---

detect_distro() {
  if [ ! -f /etc/os-release ]; then
    echo "unknown"
    return
  fi
  # shellcheck disable=SC1091
  . /etc/os-release
  echo "${ID:-unknown}"
}

# --- Install system dependencies ---

install_deps_apt() {
  local distro_version="${1:-}"
  info "Installing system dependencies via apt-get..."

  local pkgs=(
    # ffmpeg (video recording)
    ffmpeg libavutil-dev libavformat-dev libavfilter-dev libavdevice-dev
    libswscale-dev libswresample-dev libavcodec-dev
    # build tools (needed for some native deps)
    pkg-config libclang-dev
  )

  if command -v sudo &>/dev/null && [ "$(id -u)" -ne 0 ]; then
    sudo apt-get update -qq
    sudo apt-get install -y --no-install-recommends "${pkgs[@]}"
  elif [ "$(id -u)" -eq 0 ]; then
    apt-get update -qq
    apt-get install -y --no-install-recommends "${pkgs[@]}"
  else
    error "Need root or sudo to install system dependencies"
  fi
}

install_deps_pacman() {
  info "Installing system dependencies via pacman..."

  local pkgs=(
    # ffmpeg (video recording)
    ffmpeg
    # build tools
    pkgconf clang
  )

  if command -v sudo &>/dev/null && [ "$(id -u)" -ne 0 ]; then
    sudo pacman -Sy --noconfirm --needed "${pkgs[@]}"
  elif [ "$(id -u)" -eq 0 ]; then
    pacman -Sy --noconfirm --needed "${pkgs[@]}"
  else
    error "Need root or sudo to install system dependencies"
  fi
}

install_deps_brew() {
  info "Installing system dependencies via Homebrew..."
  brew install ffmpeg pkg-config
}

install_system_deps() {
  case "$PLATFORM" in
    macos)
      if command -v brew &>/dev/null; then
        install_deps_brew
      else
        warn "Homebrew not found. Install ffmpeg manually: https://ffmpeg.org/download.html"
      fi
      ;;
    linux)
      local distro
      distro="$(detect_distro)"
      case "$distro" in
        ubuntu|debian|pop|neon|tuxedo|linuxmint|raspbian)
          install_deps_apt "$distro"
          ;;
        arch|manjaro|endeavouros|garuda|artix|cachyos)
          install_deps_pacman
          ;;
        fedora|rhel|centos|rocky|alma)
          info "Installing system dependencies via dnf..."
          if command -v sudo &>/dev/null && [ "$(id -u)" -ne 0 ]; then
            sudo dnf install -y ffmpeg-devel clang-devel pkgconfig
          else
            dnf install -y ffmpeg-devel clang-devel pkgconfig
          fi
          ;;
        *)
          warn "Unknown distro: $distro"
          warn "Please install manually: ffmpeg (dev libs), pkg-config, clang"
          ;;
      esac
      ;;
  esac
}

# --- Download and install binary ---

install_binary() {
  info "Fetching latest release..."
  local tag
  tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"

  if [ -z "$tag" ]; then
    error "Could not determine latest release"
  fi
  info "Latest release: $tag"

  # Map to release artifact target triple
  local target
  case "${PLATFORM}-${ARCH}" in
    linux-x86_64)   target="x86_64-unknown-linux-musl" ;;
    linux-aarch64)  target="aarch64-unknown-linux-musl" ;;
    macos-x86_64)   target="x86_64-apple-darwin" ;;
    macos-aarch64)  target="aarch64-apple-darwin" ;;
    *)              error "No pre-built binary for ${PLATFORM}-${ARCH}" ;;
  esac

  local version="${tag#v}"
  local archive="ferridriver-${version}-${target}.tar.gz"
  local url="https://github.com/$REPO/releases/download/$tag/$archive"

  info "Downloading $archive..."
  local tmpdir
  tmpdir="$(mktemp -d)"
  trap 'rm -rf "$tmpdir"' EXIT

  curl -fsSL "$url" -o "$tmpdir/$archive"

  info "Extracting to $INSTALL_DIR..."
  mkdir -p "$INSTALL_DIR"
  tar xzf "$tmpdir/$archive" -C "$INSTALL_DIR"
  chmod +x "$INSTALL_DIR/ferridriver"

  # Check if fd_webkit_host was included (macOS only)
  if [ -f "$INSTALL_DIR/fd_webkit_host" ]; then
    chmod +x "$INSTALL_DIR/fd_webkit_host"
  fi

  info "Installed ferridriver to $INSTALL_DIR/ferridriver"

  # Verify
  if "$INSTALL_DIR/ferridriver" --version &>/dev/null; then
    info "Verified: $("$INSTALL_DIR/ferridriver" --version)"
  else
    warn "Binary installed but could not verify. You may be missing system dependencies."
    warn "Run: $0 --deps-only"
  fi
}

# --- PATH setup hint ---

check_path() {
  if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    echo ""
    info "Add ferridriver to your PATH:"
    echo ""
    echo "    export PATH=\"$INSTALL_DIR:\$PATH\""
    echo ""
    echo "  Add that line to your ~/.bashrc, ~/.zshrc, or ~/.profile"
  fi
}

# --- Main ---

echo ""
echo "  ferridriver installer"
echo "  Platform: $PLATFORM/$ARCH"
echo ""

info "Installing system dependencies..."
install_system_deps

if [ "$DEPS_ONLY" = true ]; then
  info "Done (deps only)."
  exit 0
fi

install_binary

if [ "$NO_BROWSER" = false ]; then
  info "Installing Chromium browser..."
  "$INSTALL_DIR/ferridriver" install chromium || warn "Browser install failed. Run 'ferridriver install chromium' manually."
fi

check_path

echo ""
info "Done. Run 'ferridriver --help' to get started."
echo ""
