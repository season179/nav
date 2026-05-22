#!/usr/bin/env bash
set -euo pipefail

# nav installer — Apple Silicon macOS
# Usage: curl -fsSL https://raw.githubusercontent.com/season179/nav/main/install.sh | bash
#
# The SHA-256 checksum protects against transport-level corruption only.
# Both the tarball and checksum are fetched from the same GitHub release,
# so a compromised release would evade detection.

REPO="season179/nav"
INSTALL_DIR="$HOME/.nav/bin"
BINARY="$INSTALL_DIR/nav"
PATH_SENTINEL='# >>> nav-path >>>'

# ── helpers ──────────────────────────────────────────────────────────────────

info()  { printf '%s\n' "$*"; }
err()   { printf '%s\n' "$*" >&2; }
die()   { err "$@"; exit 1; }

# ── dependency check ─────────────────────────────────────────────────────────

for cmd in curl tar; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    die "nav installer: required command not found: $cmd"
  fi
done

# ── platform guard ───────────────────────────────────────────────────────────

os="$(uname -s)"
arch="$(uname -m)"

if [[ "$os" != "Darwin" || "$arch" != "arm64" ]]; then
  die "nav installer: unsupported platform: $os $arch" \
      "nav currently supports Apple Silicon (arm64) macOS only."
fi

# ── temp directory with cleanup trap ─────────────────────────────────────────

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

# ── detect latest release ────────────────────────────────────────────────────

info "nav installer: checking for the latest release..."

release_json="$WORK_DIR/release.json"
if ! curl -fsSL -o "$release_json" "https://api.github.com/repos/${REPO}/releases/latest"; then
  die "nav installer: failed to query GitHub Releases API." \
      "You may have hit the GitHub API rate limit — wait a few minutes and try again." \
      "Download manually from https://github.com/${REPO}/releases"
fi

tag="$(grep -m1 '"tag_name"' "$release_json" | sed -E 's/.*"tag_name"\s*:\s*"([^"]+)".*/\1/')"
if [[ -z "$tag" ]]; then
  die "nav installer: could not parse tag_name from release response."
fi

version="${tag#v}"
info "nav installer: latest version is ${version}."

# ── existing install detection ───────────────────────────────────────────────

if [[ -x "$BINARY" ]]; then
  existing_version="$("$BINARY" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
  if [[ "$existing_version" == "$version" ]]; then
    info "nav ${version} is already installed."
    exit 0
  fi
  info "nav installer: upgrading from ${existing_version:-unknown} to ${version}..."
fi

# ── download tarball + checksum ──────────────────────────────────────────────

base="https://github.com/${REPO}/releases/download/${tag}"
target="nav-${version}-aarch64-apple-darwin"
tarball="${target}.tar.gz"
checksum="${tarball}.sha256"

info "nav installer: downloading ${tarball}..."

curl -fsSL -o "$WORK_DIR/$tarball"   "${base}/${tarball}"   || die "nav installer: failed to download tarball."
curl -fsSL -o "$WORK_DIR/$checksum"  "${base}/${checksum}"  || die "nav installer: failed to download checksum."

# ── verify checksum ──────────────────────────────────────────────────────────

expected="$(awk '{print $1}' "$WORK_DIR/$checksum")"
actual="$(shasum -a 256 "$WORK_DIR/$tarball" | awk '{print $1}')"

if [[ "$actual" != "$expected" ]]; then
  die "nav installer: checksum verification failed — aborting" \
      "  expected: ${expected}" \
      "  actual:   ${actual}"
fi

info "nav installer: checksum verified."

# ── install the binary ───────────────────────────────────────────────────────

mkdir -p "$INSTALL_DIR"
tar -xzf "$WORK_DIR/$tarball" -C "$WORK_DIR"

src="$(find "$WORK_DIR" -name nav -type f -print 2>/dev/null | head -n 1)" || true
[[ -n "$src" ]] || die "nav installer: could not find nav binary in archive."

mv -f "$src" "$BINARY"
chmod +x "$BINARY"

# ── add to PATH ──────────────────────────────────────────────────────────────

shell_name="$(basename "${SHELL:-}")"
rc_file=""

case "$shell_name" in
  zsh)  rc_file="$HOME/.zshrc" ;;
  bash) rc_file="$HOME/.bash_profile" ;;
esac

if [[ -n "$rc_file" ]]; then
  path_line="export PATH=\"\$HOME/.nav/bin:\$PATH\" ${PATH_SENTINEL}"
  if [[ -f "$rc_file" ]] && grep -qF "$PATH_SENTINEL" "$rc_file" 2>/dev/null; then
    info "nav installer: PATH entry already present in ${rc_file}."
  else
    printf '\n%s\n' "$path_line" >> "$rc_file"
    info "nav installer: added PATH entry to ${rc_file}."
  fi
else
  info "nav installer: unknown shell (${SHELL:-unset}). Add this line to your profile:"
  info '  export PATH="$HOME/.nav/bin:$PATH"'
fi

# ── success ──────────────────────────────────────────────────────────────────

info ""
info "✓ nav ${version} installed to ${BINARY}"
info ""
info "Run \`nav\` to get started."
info "If this is your first install, restart your terminal or run:"
info "  export PATH=\"\$HOME/.nav/bin:\$PATH\""
