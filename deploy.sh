#!/bin/bash
#
# Install the latest bricklayers release:
#
#     curl -fsSL https://raw.githubusercontent.com/erkexzcx/Bricklayers-rust/main/deploy.sh | bash
#
# Set BRICKLAYERS_DIR to install somewhere other than ~/BrickLayers, and GITHUB_TOKEN if the
# unauthenticated GitHub API rate limit gets in the way.

set -euo pipefail

repo="erkexzcx/Bricklayers-rust"
install_dir="${BRICKLAYERS_DIR:-${HOME}/BrickLayers}"

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

for tool in curl jq awk mktemp; do
    command -v "$tool" >/dev/null 2>&1 ||
        die "${tool} is required — install it with your package manager (apt/dnf/pacman/brew install ${tool})."
done

# macOS ships shasum where Linux ships coreutils' sha256sum.
if command -v sha256sum >/dev/null 2>&1; then
    verify_sums() { sha256sum --check --status -; }
elif command -v shasum >/dev/null 2>&1; then
    verify_sums() { shasum --algorithm 256 --check --status -; }
else
    die "sha256sum or shasum is required to verify the download."
fi

system="$(uname -s)"
machine="$(uname -m)"
case "${system}/${machine}" in
    Linux/x86_64 | Linux/amd64) platform="linux_amd64" ;;
    Linux/aarch64 | Linux/arm64) platform="linux_arm64" ;;
    Darwin/arm64 | Darwin/aarch64) platform="darwin_arm64" ;;
    Darwin/x86_64 | Darwin/amd64) platform="darwin_amd64" ;;
    *) die "no prebuilt binary for ${system} ${machine}. Build from source instead: cargo install --git https://github.com/${repo}" ;;
esac

curl_args=(--silent --show-error --location --retry 3)
if [[ -n "${GITHUB_TOKEN:-}" ]]; then
    curl_args+=(--header "Authorization: Bearer ${GITHUB_TOKEN}")
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/bricklayers.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

printf 'Looking up the latest release of %s...\n' "$repo"
status="$(curl "${curl_args[@]}" --header 'Accept: application/vnd.github+json' \
    --write-out '%{http_code}' --output "${tmp}/release.json" \
    "https://api.github.com/repos/${repo}/releases/latest")" ||
    die "could not reach the GitHub API."

case "$status" in
    200) ;;
    404) die "${repo} has no published release yet. Build from source: cargo install --git https://github.com/${repo}" ;;
    403 | 429) die "GitHub API rate limit reached — set GITHUB_TOKEN and try again." ;;
    *) die "the GitHub API answered HTTP ${status}." ;;
esac

release="$(cat "${tmp}/release.json")"
tag="$(jq -r '.tag_name // empty' <<<"$release")"
[[ -n "$tag" ]] || die "the latest release has no tag name."

asset="bricklayers_${tag}_${platform}"
asset_url="$(jq -r --arg name "$asset" '.assets[] | select(.name == $name) | .browser_download_url' <<<"$release")"
[[ -n "$asset_url" ]] || die "release ${tag} publishes no asset named ${asset}."

sums="bricklayers_${tag}_SHA256SUMS.txt"
sums_url="$(jq -r --arg name "$sums" '.assets[] | select(.name == $name) | .browser_download_url' <<<"$release")"

printf 'Downloading %s...\n' "$asset"
curl "${curl_args[@]}" --fail --output "${tmp}/${asset}" "$asset_url" || die "download failed."

if [[ -n "$sums_url" ]]; then
    curl "${curl_args[@]}" --fail --output "${tmp}/${sums}" "$sums_url" || die "checksum download failed."
    # An unmatched name leaves the checker with no input lines, so a missing entry also fails.
    (cd "$tmp" && awk -v name="$asset" '$2 == name' "$sums" | verify_sums) ||
        die "checksum mismatch — the download is corrupt or has been tampered with."
    printf 'Checksum verified.\n'
else
    printf 'warning: release %s publishes no checksum file, skipping verification.\n' "$tag" >&2
fi

mkdir -p "$install_dir"
binary="${install_dir}/bricklayers"
mv -f "${tmp}/${asset}" "$binary"
chmod 755 "$binary"

version="$("$binary" --version 2>/dev/null || printf 'bricklayers %s' "$tag")"

cat <<EOF

Installed ${version} to ${binary}

Add this to your slicer — PrusaSlicer: Print Settings -> Output options -> Post-processing
scripts, Orca/Bambu Studio: Others -> Post-processing Scripts:

    ${binary} brick --extrusion-multiplier 1.05

The slicer appends the G-code path itself. Use three walls or more, or there is nothing for a
raised loop to bond to. Run '${binary} brick --help' for the options.
EOF
