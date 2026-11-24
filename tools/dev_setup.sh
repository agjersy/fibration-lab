#!/usr/bin/env bash
# Prepare a checkout of Fibration Lab for development and hands-on testing on Linux
# (Debian/Ubuntu or Fedora) or macOS: check the prerequisites, set up the Python environments
# and the renderer, build the Rust binaries, and print how to start the app.
# Safe to run again: every step is idempotent, and a second run only re-checks and rebuilds
# what changed.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: tools/dev_setup.sh [--install-system] [--with-serve] [--help]

Checks the prerequisites, then runs:
  uv sync          in python/kernel, present/python/kernel, present/python/sdk
  pnpm install     at the repository root
  pnpm build       in apps/desktop
  cargo build      -p fibration-cli -p fibration-desktop -p wb-sandbox (+ fibration-serve)

Options:
  --install-system  install missing system packages with apt-get, dnf or brew (uses sudo on
                    Linux). Without it the script only prints the command to run.
  --with-serve      also build fibration-serve (the renderer-in-a-browser bridge)
  -h, --help        show this help

Rust (rustup), uv and Node 22 are never installed by this script; it prints the official
install command for each when one is missing.
EOF
}

INSTALL_SYSTEM=0
WITH_SERVE=0
for arg in "$@"; do
  case "$arg" in
    --install-system) INSTALL_SYSTEM=1 ;;
    --with-serve) WITH_SERVE=1 ;;
    -h | --help) usage; exit 0 ;;
    *) echo "unknown option: $arg" >&2; usage >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

step() { printf '\n== %s\n' "$*"; }
fail() { printf '\nerror: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
# Run a step; on failure say which one and stop.
run() {
  echo "+ $*"
  "$@" || fail "step failed: $* (in $(pwd))"
}

# ---- platform ----------------------------------------------------------------------------

OS="$(uname -s)"
DISTRO=""
case "$OS" in
  Linux)
    if [ -r /etc/os-release ]; then
      # shellcheck disable=SC1091
      . /etc/os-release
      case " ${ID:-} ${ID_LIKE:-} " in
        *" debian "* | *" ubuntu "*) DISTRO=debian ;;
        *" fedora "* | *" rhel "*) DISTRO=fedora ;;
      esac
    fi
    [ -n "$DISTRO" ] || echo "warning: unrecognised Linux distribution; package names below are for Debian/Ubuntu"
    DISTRO="${DISTRO:-debian}"
    ;;
  Darwin) DISTRO=macos ;;
  *) fail "unsupported platform: $OS (Linux and macOS are supported)" ;;
esac

# System packages: the sandbox, the Tauri (WebKitGTK) build dependencies, git and ssh.
case "$DISTRO" in
  debian)
    SYS_PKGS=(build-essential pkg-config libssl-dev bubblewrap git openssh-client
      libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev
      libsoup-3.0-dev libxdo-dev patchelf file)
    OFFICE_PKGS=(libreoffice-impress)
    SYS_INSTALL="sudo apt-get update && sudo apt-get install -y"
    ;;
  fedora)
    SYS_PKGS=(gcc gcc-c++ make pkgconf-pkg-config openssl-devel bubblewrap git openssh-clients
      webkit2gtk4.1-devel gtk3-devel librsvg2-devel libappindicator-gtk3-devel
      libsoup3-devel libxdo-devel patchelf file)
    OFFICE_PKGS=(libreoffice-impress)
    SYS_INSTALL="sudo dnf install -y"
    ;;
  macos)
    SYS_PKGS=()
    OFFICE_PKGS=()
    SYS_INSTALL=""
    ;;
esac

install_system() {
  local sudo=()
  [ "$(id -u)" = 0 ] || sudo=(sudo)
  case "$DISTRO" in
    debian)
      run "${sudo[@]}" apt-get update
      run "${sudo[@]}" apt-get install -y "${SYS_PKGS[@]}"
      ;;
    fedora) run "${sudo[@]}" dnf install -y "${SYS_PKGS[@]}" ;;
    macos)
      if ! xcode-select -p >/dev/null 2>&1; then
        echo "+ xcode-select --install"
        xcode-select --install || true
        fail "finish the Xcode command line tools installer, then run this script again"
      fi
      ;;
  esac
}

# ---- prerequisites -----------------------------------------------------------------------

step "Checking prerequisites ($DISTRO)"
MISSING=()     # required: "<what>|<how to install>"
OPTIONAL=()    # recommended
SYSTEM_MISSING=0

need() { MISSING+=("$1|$2"); }

if [ "$DISTRO" = macos ]; then
  if ! xcode-select -p >/dev/null 2>&1; then
    SYSTEM_MISSING=1
    need "Xcode command line tools" "xcode-select --install"
  fi
else
  sys_hint="$SYS_INSTALL ${SYS_PKGS[*]}"
  for c in cc pkg-config bwrap; do
    have "$c" || { SYSTEM_MISSING=1; need "$c" "$sys_hint"; }
  done
  if have pkg-config; then
    for m in webkit2gtk-4.1 gtk+-3.0 librsvg-2.0 libsoup-3.0; do
      pkg-config --exists "$m" || { SYSTEM_MISSING=1; need "$m (development files)" "$sys_hint"; }
    done
  fi
  have ssh || OPTIONAL+=("OpenSSH client (remote targets)|$SYS_INSTALL ${SYS_PKGS[*]}")
fi

if [ "$SYSTEM_MISSING" = 1 ] && [ "$INSTALL_SYSTEM" = 1 ]; then
  step "Installing system packages"
  install_system
  # Check again from the start with the packages in place.
  exec "$0" "$@"
fi

have git || need git "$( [ "$DISTRO" = macos ] && echo "xcode-select --install" || echo "$SYS_INSTALL git")"

if ! have rustup; then
  need "Rust (rustup)" "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   (official installer, https://rustup.rs)"
fi

if ! have uv; then
  if [ "$DISTRO" = macos ]; then
    need uv "brew install uv   or   curl -LsSf https://astral.sh/uv/install.sh | sh   (https://docs.astral.sh/uv/)"
  else
    need uv "curl -LsSf https://astral.sh/uv/install.sh | sh   (official installer, https://docs.astral.sh/uv/)"
  fi
fi

NODE_HINT="install Node 22 from https://nodejs.org/en/download (or: $( [ "$DISTRO" = macos ] && echo "brew install node@22" || echo "your package manager, nvm or fnm"))"
if have node; then
  node_major="$(node -p 'process.versions.node.split(".")[0]' 2>/dev/null || echo 0)"
  [ "$node_major" -ge 22 ] 2>/dev/null || need "Node 22 or newer (found $(node --version))" "$NODE_HINT"
else
  need "Node 22" "$NODE_HINT"
fi

# pnpm: on PATH, else through corepack (which ships with Node and reads packageManager).
PNPM=()
if have pnpm; then
  PNPM=(pnpm)
elif have corepack; then
  export COREPACK_ENABLE_DOWNLOAD_PROMPT=0
  PNPM=(corepack pnpm)
elif have node; then
  need pnpm "corepack enable pnpm   (corepack ships with Node 22)"
fi

if ! have soffice && ! have libreoffice && [ ! -d "/Applications/LibreOffice.app" ]; then
  if [ "$DISTRO" = macos ]; then
    OPTIONAL+=("LibreOffice (PDF export and the sample deck's PDF)|brew install --cask libreoffice")
  else
    OPTIONAL+=("LibreOffice (PDF export and the sample deck's PDF)|$SYS_INSTALL ${OFFICE_PKGS[*]}")
  fi
fi

for o in ${OPTIONAL[@]+"${OPTIONAL[@]}"}; do
  printf 'optional, not found: %s\n    %s\n' "${o%%|*}" "${o#*|}"
done
if [ "${#MISSING[@]}" -gt 0 ]; then
  echo
  echo "Missing prerequisites:"
  for m in "${MISSING[@]}"; do
    printf '  - %s\n      %s\n' "${m%%|*}" "${m#*|}"
  done
  if [ "$SYSTEM_MISSING" = 1 ]; then
    echo
    echo "Run again with --install-system to install the system packages listed above."
  fi
  fail "install the missing prerequisites, then run tools/dev_setup.sh again"
fi
echo "all required tools found"

# ---- Rust toolchain (rust-toolchain.toml) ------------------------------------------------

step "Rust toolchain (pinned by rust-toolchain.toml)"
# rustup 1.28 and newer install the pinned toolchain with a bare `toolchain install`; older
# versions install it on first use.
rustup toolchain install >/dev/null 2>&1 || true
run rustup show active-toolchain

# ---- Python environments -----------------------------------------------------------------

for dir in python/kernel present/python/kernel present/python/sdk; do
  step "uv sync in $dir"
  (cd "$dir" && run uv sync)
done

# ---- renderer ----------------------------------------------------------------------------

step "pnpm install (workspace root)"
run "${PNPM[@]}" install --frozen-lockfile

step "pnpm build in apps/desktop"
run "${PNPM[@]}" --dir apps/desktop build

# ---- Rust binaries -----------------------------------------------------------------------

CRATES=(-p fibration-cli -p fibration-desktop -p wb-sandbox)
[ "$WITH_SERVE" = 1 ] && CRATES+=(-p fibration-serve)
step "cargo build ${CRATES[*]}"
run cargo build "${CRATES[@]}"

# ---- next steps --------------------------------------------------------------------------

HOME_DIR="${FIBRATION_HOME:-$HOME/.fibration}"
cat <<EOF

== Ready

Binaries: $ROOT/target/debug/fibration, $ROOT/target/debug/fibration-desktop

Run the desktop app in development (two terminals, from $ROOT):
  pnpm --dir apps/desktop dev            # Vite on http://localhost:5173
  target/debug/fibration-desktop           # the window loads the renderer from Vite
or as one binary with the built renderer embedded:
  cargo build -p fibration-desktop --features custom-protocol && target/debug/fibration-desktop
(\`cargo tauri dev\` in apps/desktop/src-tauri also works once \`cargo install tauri-cli --version '^2' --locked\`.)

FIBRATION_HOME is $HOME_DIR (override with FIBRATION_HOME or --home).
Add a model provider to $HOME_DIR/config.toml:

  [defaults]
  model = "anthropic/<model id>"        # "<provider name>/<model id>"

  [[providers]]
  name = "anthropic"
  kind = "anthropic"                    # or "openai-compatible" with base_url = ".../v1"
  api_key_env = "ANTHROPIC_API_KEY"

and its key to $HOME_DIR/.env with
  target/debug/fibration key set anthropic          (reads the key from stdin; writes the file with mode 0600)
or by hand, one line (then chmod 600 the file):
  ANTHROPIC_API_KEY=sk-ant-...

Quick self-checks (no model needed):
  tools/deck_walkthrough.sh              # every deck command against a scripted model
  tools/make_sample_deck.sh              # ./sample-deck/sample.pptx to open in PowerPoint
EOF
