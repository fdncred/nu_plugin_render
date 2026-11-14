#!/usr/bin/env bash
# Package a release build of nu_plugin_render for `cargo binstall`.
#
#     .github/package.sh TARGET
#
# Run it from the repository's root after
# `cargo build --release --target TARGET`. It writes the archive to `dist/`
# and prints its path, relative to the root.
#
# The archive's name and layout must match `[package.metadata.binstall]` in
# Cargo.toml: `nu_plugin_render-TARGET-vVERSION.tgz`, or `.zip` on Windows,
# with the binary, README.md and LICENSE at the top.
set -euo pipefail

target=$1
name=nu_plugin_render
version=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' Cargo.toml | head -n 1)
if [ -z "$version" ]; then
    echo "package.sh: no version in Cargo.toml" >&2
    exit 1
fi
build=${CARGO_TARGET_DIR:-target}/$target/release

case $target in
*windows*) binary=$name.exe archive=dist/$name-$target-v$version.zip ;;
*) binary=$name archive=dist/$name-$target-v$version.tgz ;;
esac

# The archive is made in a fresh folder, so it never adds to an old one.
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/files"
cp "$build/$binary" README.md LICENSE "$stage/files/"
made=$stage/$(basename "$archive")
case $archive in
*.zip)
    # 7-Zip comes with GitHub's Windows runners; `zip` is for trying this
    # elsewhere.
    if command -v 7z >/dev/null; then
        (cd "$stage/files" && 7z a -tzip "$made" "$binary" README.md LICENSE >/dev/null)
    else
        (cd "$stage/files" && zip -q "$made" "$binary" README.md LICENSE)
    fi
    ;;
*) tar -C "$stage/files" -czf "$made" "$binary" README.md LICENSE ;;
esac
mkdir -p dist
mv -f "$made" "$archive"
echo "$archive"
