#!/usr/bin/env bash
# Sets the version of ognivo: the workspace's crates and the VS Code extension, with their
# lockfiles. A release is then a commit of it and the tag v<version>, pushed: CI
# builds the packages into a draft release (.github/workflows/release.yml).
#
#   scripts/version.sh 0.1.0
set -euo pipefail
cd "$(dirname "$0")/.."

new=${1:?usage: scripts/version.sh X.Y.Z}
[[ $new =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || {
    echo "version: not a version: $new" >&2
    exit 1
}
old=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
# the first `version = ` is [workspace.package]'s
sed -i "0,/^version = \".*\"$/s//version = \"$new\"/" Cargo.toml
npm version "$new" --workspaces --no-git-tag-version --allow-same-version > /dev/null
cargo update --workspace --offline --quiet
echo "ognivo $old -> $new"
git status --short -- Cargo.toml Cargo.lock package-lock.json apps/vscode/package.json
