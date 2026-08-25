#!/bin/sh
# One-line installer for the nirdosha CLI (macOS / Linux).
#
#   curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/arunsoman/nirdosha/main/scripts/install.sh | sh
#
# Downloads the right prebuilt binary from GitHub Releases, verifies its
# sha256 checksum, and installs it to $NIRDOSHA_INSTALL_DIR (default
# ~/.local/bin). No Rust, clang, or z3 required — those binaries have Z3
# statically vendored (see .github/workflows/release.yml). `clang` is
# still needed on this machine if you later run `nirdosha build`
# (native codegen); interpreting/`emit-ui`/`serve` work with no extra
# install.
#
# Windows: use scripts/install.ps1 instead.
set -eu

repo="arunsoman/nirdosha"
install_dir="${NIRDOSHA_INSTALL_DIR:-$HOME/.local/bin}"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)
    case "$arch" in
      x86_64) asset="nirdosha-x86_64-unknown-linux-gnu.tar.gz" ;;
      *) echo "error: no prebuilt nirdosha binary for Linux/$arch yet. Build from source: see README.md #10." >&2; exit 1 ;;
    esac
    ;;
  Darwin)
    case "$arch" in
      arm64)  asset="nirdosha-aarch64-apple-darwin.tar.gz" ;;
      x86_64) asset="nirdosha-x86_64-apple-darwin.tar.gz" ;;
      *) echo "error: no prebuilt nirdosha binary for macOS/$arch yet. Build from source: see README.md #10." >&2; exit 1 ;;
    esac
    ;;
  *)
    echo "error: unsupported OS '$os'. On Windows, use scripts/install.ps1 instead. Otherwise build from source: see README.md #10." >&2
    exit 1
    ;;
esac

version="${NIRDOSHA_VERSION:-latest}"
if [ "$version" = "latest" ]; then
  url="https://github.com/$repo/releases/latest/download/$asset"
  checksum_url="https://github.com/$repo/releases/latest/download/$asset.sha256"
else
  url="https://github.com/$repo/releases/download/$version/$asset"
  checksum_url="https://github.com/$repo/releases/download/$version/$asset.sha256"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $asset ($version)..."
curl --proto '=https' --tlsv1.2 -fsSL "$url" -o "$tmp/$asset"
curl --proto '=https' --tlsv1.2 -fsSL "$checksum_url" -o "$tmp/$asset.sha256" 2>/dev/null || true

if [ -s "$tmp/$asset.sha256" ]; then
  expected="$(awk '{print $1}' "$tmp/$asset.sha256")"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
  else
    actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
  fi
  if [ "$expected" != "$actual" ]; then
    echo "error: checksum mismatch for $asset (expected $expected, got $actual)" >&2
    exit 1
  fi
  echo "Checksum verified."
fi

mkdir -p "$install_dir"
tar xzf "$tmp/$asset" -C "$tmp"
install -m 755 "$tmp/nirdosha" "$install_dir/nirdosha"

echo "Installed nirdosha to $install_dir/nirdosha"
case ":$PATH:" in
  *":$install_dir:"*) ;;
  *) echo "Add it to your PATH: export PATH=\"$install_dir:\$PATH\"" ;;
esac
echo "Try it: nirdosha            # prints usage"
echo "        nirdosha hello.nir  # see README.md for a hello-world snippet to paste"
