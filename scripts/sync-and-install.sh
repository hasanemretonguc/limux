#!/usr/bin/env bash
set -euo pipefail

REPO_SLUG="hasanemretonguc/limux"
ROOT_DIR="$(cd "$(dirname "$(realpath "$0")")/.." && pwd)"

install_pkg() {
  local pkg_file="$1"
  local remove_old=""
  if pacman -Q limux-bin >/dev/null 2>&1; then
    remove_old="pacman -Rdd --noconfirm limux-bin && "
  fi
  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    sh -c "${remove_old}pacman -U --noconfirm '$pkg_file'"
  elif [[ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]] && command -v zenity >/dev/null 2>&1; then
    local askpass
    askpass="$(mktemp /tmp/limux-askpass.XXXXXX)"
    trap 'rm -f "$askpass"' RETURN
    printf '#!/bin/sh\nexec zenity --password --title="Update limux-custom-git"\n' > "$askpass"
    chmod +x "$askpass"
    SUDO_ASKPASS="$askpass" sudo -A sh -c "${remove_old}pacman -U --noconfirm '$pkg_file'"
  elif command -v pkexec >/dev/null 2>&1; then
    pkexec sh -c "${remove_old}pacman -U --noconfirm '$pkg_file'"
  else
    sudo sh -c "${remove_old}pacman -U --noconfirm '$pkg_file'"
  fi
}

update_from_github_release() {
  echo "==> Checking latest GitHub release on ${REPO_SLUG}..."
  local release_json
  release_json="$(curl -fsSL "https://api.github.com/repos/${REPO_SLUG}/releases/latest")" || {
    echo "No published GitHub release found yet on ${REPO_SLUG}." >&2
    return 1
  }

  local pkg_url pkg_name sums_url latest_ver installed_ver
  pkg_url="$(printf '%s' "$release_json" | python3 -c "import sys,json; r=json.load(sys.stdin); print(next((a['browser_download_url'] for a in r.get('assets',[]) if a['name'].endswith('.pkg.tar.zst')), ''))")"
  pkg_name="$(printf '%s' "$release_json" | python3 -c "import sys,json; r=json.load(sys.stdin); print(next((a['name'] for a in r.get('assets',[]) if a['name'].endswith('.pkg.tar.zst')), ''))")"
  sums_url="$(printf '%s' "$release_json" | python3 -c "import sys,json; r=json.load(sys.stdin); print(next((a['browser_download_url'] for a in r.get('assets',[]) if a['name']=='SHA256SUMS'), ''))")"

  if [[ -z "$pkg_url" || -z "$pkg_name" ]]; then
    echo "Latest GitHub release has no .pkg.tar.zst asset." >&2
    return 1
  fi

  latest_ver="$(echo "$pkg_name" | sed 's/^limux-custom-git-\(.*\)-x86_64\.pkg\.tar\.zst$/\1/')"
  installed_ver="$(pacman -Q limux-custom-git 2>/dev/null | awk '{print $2}' || true)"

  if [[ "$installed_ver" == "$latest_ver" ]]; then
    echo "==> limux-custom-git is already up to date (${installed_ver})."
    return 0
  fi

  echo "==> Updating limux-custom-git: ${installed_ver:-none} -> ${latest_ver}"
  local tmp_dir
  tmp_dir="$(mktemp -d /tmp/limux-release.XXXXXX)"
  trap 'rm -rf "$tmp_dir"' RETURN

  curl -fSL "$pkg_url" -o "$tmp_dir/$pkg_name"
  if [[ -n "$sums_url" ]]; then
    curl -fSL "$sums_url" -o "$tmp_dir/SHA256SUMS"
    (cd "$tmp_dir" && sha256sum -c SHA256SUMS)
  fi

  install_pkg "$tmp_dir/$pkg_name"
  echo "==> Installed $(pacman -Q limux-custom-git). Restart Limux to use the new version."
}

update_from_local_source() {
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
    local behind
    behind="$(git rev-list --count HEAD..upstream/main)"
    if [[ "$behind" -gt 0 ]]; then
      echo "==> Merging $behind new commit(s) from upstream/main..."
      git merge --no-edit upstream/main
      git push origin main
    fi
  fi

  echo "==> Building release binaries locally..."
  cargo build --release --locked -p limux-host-linux -p limux-cli

  local pkg_stage
  pkg_stage="$(mktemp -d /tmp/limux-pkg.XXXXXX)"
  trap 'rm -rf "$pkg_stage"' RETURN
  cp "$ROOT_DIR/packaging/arch/PKGBUILD" "$pkg_stage/PKGBUILD"
  (cd "$pkg_stage" && LIMUX_REPO_ROOT="$ROOT_DIR" makepkg -f --noconfirm)
  local pkg_file
  pkg_file="$(ls -t "$pkg_stage"/limux-custom-git-*.pkg.tar.* | head -n 1)"
  install_pkg "$pkg_file"
  echo "==> Installed $(pacman -Q limux-custom-git). Restart Limux to use the updated build."
}

case "${1:-}" in
  --local)
    update_from_local_source
    ;;
  *)
    update_from_github_release
    ;;
esac
