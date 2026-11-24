#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$(realpath "$0")")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$HOME/.local/share/mise/shims:$PATH"
export LD_LIBRARY_PATH="$ROOT_DIR/ghostty/zig-out/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

cd "$ROOT_DIR"

echo "==> Fetching origin & upstream..."
git fetch origin main --tags
if git remote get-url upstream >/dev/null 2>&1; then
  git fetch upstream main --tags
fi

git checkout main
git pull --ff-only origin main || true

if git remote get-url upstream >/dev/null 2>&1; then
  BEHIND=$(git rev-list --count HEAD..upstream/main)
  if [ "$BEHIND" -gt 0 ]; then
    echo "==> Merging $BEHIND new commit(s) from upstream/main..."
    git merge --no-edit upstream/main
    git push origin main
  else
    echo "==> Already up to date with upstream/main."
  fi
fi

echo "==> Building release binaries (limux-host-linux & limux-cli)..."
cargo build --release --locked -p limux-host-linux -p limux-cli

echo "==> Packaging Arch package (limux-custom-git)..."
PKG_STAGE="$(mktemp -d /tmp/limux-pkg.XXXXXX)"
trap 'rm -rf "$PKG_STAGE"' EXIT
cp "$ROOT_DIR/packaging/arch/PKGBUILD" "$PKG_STAGE/PKGBUILD"
(
  cd "$PKG_STAGE"
  LIMUX_REPO_ROOT="$ROOT_DIR" makepkg -f --noconfirm
)
PKG_FILE="$(ls -t "$PKG_STAGE"/limux-custom-git-*.pkg.tar.* | head -n 1)"

echo "==> Installing $PKG_FILE via pkexec pacman -U..."
pkexec pacman -U --noconfirm "$PKG_FILE"

echo "==> Done! Installed $(pacman -Q limux-custom-git). Restart Limux to use the updated build."
