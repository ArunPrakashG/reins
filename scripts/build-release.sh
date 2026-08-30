#!/usr/bin/env bash
# Bumps the workspace version, builds reins/reinsd in release mode, and
# drops the binaries into builds/<version>/.
#
# Usage: scripts/build-release.sh [major|minor|patch]
#   Defaults to a patch bump. The version lives in one place —
#   [workspace.package].version in the root Cargo.toml — every crate
#   inherits it via `version.workspace = true`.

set -euo pipefail

BUMP="${1:-patch}"
case "$BUMP" in
    major|minor|patch) ;;
    *)
        echo "error: unknown bump type '$BUMP' (expected major, minor, or patch)" >&2
        exit 1
        ;;
esac

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

CARGO_TOML="Cargo.toml"

CURRENT_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$CARGO_TOML" | head -1)"
if [[ -z "$CURRENT_VERSION" ]]; then
    echo "error: could not find [workspace.package] version in $CARGO_TOML" >&2
    exit 1
fi

IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT_VERSION"
case "$BUMP" in
    major) MAJOR=$((MAJOR + 1)); MINOR=0; PATCH=0 ;;
    minor) MINOR=$((MINOR + 1)); PATCH=0 ;;
    patch) PATCH=$((PATCH + 1)) ;;
esac
NEW_VERSION="${MAJOR}.${MINOR}.${PATCH}"

echo "==> Bumping version: $CURRENT_VERSION -> $NEW_VERSION ($BUMP)"
sed -i "0,/^version = \"$CURRENT_VERSION\"/s//version = \"$NEW_VERSION\"/" "$CARGO_TOML"

echo "==> Building workspace (release)"
cargo build --workspace --release

BUILD_DIR="builds/$NEW_VERSION"
mkdir -p "$BUILD_DIR"

echo "==> Collecting binaries into $BUILD_DIR"
for bin in reins reinsd; do
    src="target/release/$bin"
    if [[ ! -f "$src" ]]; then
        echo "error: expected binary not found: $src" >&2
        exit 1
    fi
    cp "$src" "$BUILD_DIR/$bin"
done

echo "==> Packaging release asset"
PLATFORM_ARCH="$(uname -m)"
case "$(uname -s)" in
    Linux)  PLATFORM_OS="linux" ;;
    Darwin) PLATFORM_OS="macos" ;;
    *)
        echo "error: unsupported platform for release packaging: $(uname -s)" >&2
        exit 1
        ;;
esac
case "$PLATFORM_ARCH" in
    x86_64|amd64) PLATFORM_ARCH="x86_64" ;;
    arm64|aarch64) PLATFORM_ARCH="aarch64" ;;
    *)
        echo "error: unsupported architecture for release packaging: $PLATFORM_ARCH" >&2
        exit 1
        ;;
esac

ASSET_NAME="reins-${PLATFORM_OS}-${PLATFORM_ARCH}.tar.gz"
ASSET_PATH="$BUILD_DIR/$ASSET_NAME"
tar -czf "$ASSET_PATH" -C "$BUILD_DIR" reins reinsd

echo "==> Computing checksums"
SUMS_PATH="$BUILD_DIR/SHA256SUMS"
# sha256sum is GNU coreutils and isn't present on stock macOS (which ships
# `shasum -a 256` instead); prefer sha256sum where it exists, fall back otherwise.
# Both produce the same `<hex>  <filename>` line format the updater's checksum
# parser (apps/daemon/src/updater/mod.rs, run_update) expects.
if command -v sha256sum >/dev/null 2>&1; then
    SHA_CMD="sha256sum"
else
    SHA_CMD="shasum -a 256"
fi
(cd "$BUILD_DIR" && $SHA_CMD "$ASSET_NAME" > "SHA256SUMS")

if [[ "${SKIP_PUBLISH:-}" == "1" ]]; then
    echo "==> SKIP_PUBLISH=1 set, not publishing a GitHub release"
elif ! command -v gh >/dev/null 2>&1; then
    echo "==> 'gh' CLI not found, skipping GitHub release publish"
    echo "    (install it, or re-run with SKIP_PUBLISH=1 to silence this)"
elif gh release view "v$NEW_VERSION" >/dev/null 2>&1; then
    # Release already exists — this is a second (or later) platform's build for the
    # same version. Merge this platform's checksum line into the existing SHA256SUMS
    # asset rather than clobbering it, so the release ends up with one SHA256SUMS
    # file covering every platform built for this version, not just the most recent.
    echo "==> Release v$NEW_VERSION already exists, uploading additional platform asset"
    EXISTING_SUMS="$(mktemp)"
    if gh release download "v$NEW_VERSION" --pattern SHA256SUMS --output "$EXISTING_SUMS" --clobber 2>/dev/null; then
        MERGED_SUMS="$(mktemp)"
        cat "$EXISTING_SUMS" "$SUMS_PATH" | sort -u -k2 > "$MERGED_SUMS"
        mv "$MERGED_SUMS" "$SUMS_PATH"
    fi
    rm -f "$EXISTING_SUMS"
    gh release upload "v$NEW_VERSION" "$ASSET_PATH" "$SUMS_PATH" --clobber
else
    echo "==> Publishing GitHub release v$NEW_VERSION"
    gh release create "v$NEW_VERSION" \
        --title "v$NEW_VERSION" \
        --generate-notes \
        "$ASSET_PATH" \
        "$SUMS_PATH"
fi

echo "==> Done"
echo "Version:  $NEW_VERSION"
echo "Binaries: $BUILD_DIR/reins, $BUILD_DIR/reinsd"
echo "Asset:    $ASSET_PATH"
