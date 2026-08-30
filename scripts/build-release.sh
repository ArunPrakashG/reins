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

echo "==> Done"
echo "Version:  $NEW_VERSION"
echo "Binaries: $BUILD_DIR/reins, $BUILD_DIR/reinsd"
