#!/bin/sh
# One-line installer for the nirdosha CLI (macOS / Linux).
#
#   curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/kannamma-labs/nirdosha/main/scripts/install.sh | sh
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

repo="kannamma-labs/nirdosha"
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

# ── .nir file icon, so file managers show the Nirdosha mark instead of a
# generic text-file icon the moment this install finishes — best-effort,
# never fatal: a headless/server machine with no desktop environment (no
# xdg-mime, no icon cache) just silently skips this, same posture as the
# rest of this installer's optional steps.
if [ "$os" = "Linux" ] && [ -d "$tmp/linux-icons" ]; then
  icon_base="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
  mime_dir="${XDG_DATA_HOME:-$HOME/.local/share}/mime/packages"
  mkdir -p "$mime_dir"
  cp "$tmp/nirdosha-mime.xml" "$mime_dir/nirdosha.xml" 2>/dev/null || true
  for sizedir in "$tmp/linux-icons"/*/; do
    size="$(basename "$sizedir")"
    mkdir -p "$icon_base/$size/apps"
    cp "$sizedir/apps/nirdosha.png" "$icon_base/$size/apps/nirdosha.png" 2>/dev/null || true
  done
  if command -v update-mime-database >/dev/null 2>&1; then
    update-mime-database "${XDG_DATA_HOME:-$HOME/.local/share}/mime" >/dev/null 2>&1 || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$icon_base" >/dev/null 2>&1 || true
  fi
  echo "Registered the .nir file icon (Nautilus/Dolphin/Thunar etc. — may need a restart to pick it up)."
elif [ "$os" = "Darwin" ]; then
  app_asset="nirdosha-icon-registrar-macos.zip"
  app_url="https://github.com/$repo/releases/latest/download/$app_asset"
  if [ "$version" != "latest" ]; then
    app_url="https://github.com/$repo/releases/download/$version/$app_asset"
  fi
  apps_dir="${NIRDOSHA_APPS_DIR:-$HOME/Applications}"
  mkdir -p "$apps_dir"
  if curl --proto '=https' --tlsv1.2 -fsSL "$app_url" -o "$tmp/$app_asset" 2>/dev/null; then
    rm -rf "$apps_dir/Nirdosha.app"
    unzip -q "$tmp/$app_asset" -d "$tmp/app_extract" && cp -R "$tmp/app_extract/Nirdosha.app" "$apps_dir/Nirdosha.app"
    lsregister="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
    if [ -x "$lsregister" ]; then
      "$lsregister" -f "$apps_dir/Nirdosha.app" >/dev/null 2>&1 || true
      echo "Registered the .nir file icon with Finder (Nirdosha.app installed to $apps_dir, not a real app — see its Info.plist)."
    fi
  else
    echo "Note: couldn't fetch the Finder icon registrar ($app_asset) — nirdosha itself installed fine, .nir files just won't get a custom icon."
  fi
fi

echo "Try it: nirdosha            # prints usage"
echo "        nirdosha hello.nir  # see README.md for a hello-world snippet to paste"
