#!/usr/bin/env bash
# The Linux packages of a release (docs/architecture.md §13, M6), in out/:
#
#   ognivo-<version>-linux-x64.vsix        the VS Code extension, with ognivo in it
#   ognivo-<version>-x86_64-linux.tar.gz   ognivo alone: the CLI, and the core it runs
#   SHA256SUMS
#
# Before it: npm ci. It builds ognivo (release) and the extension. Build a release on the
# oldest system it is for (CI: Ubuntu 22.04): glibc is what ognivo needs of a system.
#
#   OGNIVO_BIN=path      package this ognivo instead of building one
set -euo pipefail
cd "$(dirname "$0")/.."
# what the packages hold is the owner's to write
umask 022

fail() {
    echo "package: $*" >&2
    exit 1
}

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[[ -n "$version" ]] || fail "no version in Cargo.toml"
v=$(node -p "require('./apps/vscode/package.json').version")
[[ "$v" == "$version" ]] || fail "apps/vscode/package.json has version $v, Cargo.toml $version: scripts/version.sh sets both"

if [[ -n "${OGNIVO_BIN:-}" ]]; then
    bin=$OGNIVO_BIN
else
    cargo build --release --locked -p ognivo-cli
    bin=target/release/ognivo
fi

out=out
stage=$out/stage
vsix=ognivo-${version}-linux-x64.vsix
tarball=ognivo-${version}-x86_64-linux.tar.gz
rm -rf "$stage"
rm -f "$out/$vsix" "$out/$tarball" "$out/SHA256SUMS"
mkdir -p "$stage"

# the extension, with ognivo in it (apps/vscode/scripts/package.mjs)
OGNIVO_BIN="$bin" TARGET=linux-x64 npm run -s package -w apps/vscode

# ognivo alone
notices=$stage/THIRD-PARTY-LICENSES.txt
node scripts/licenses.mjs > "$notices"
dir=$stage/ognivo-$version
mkdir -p "$dir"
install -m 0755 "$bin" "$dir/ognivo"
install -m 0644 LICENSE README.md "$notices" "$dir/"
tar -C "$stage" --owner=0 --group=0 --numeric-owner -czf "$out/$tarball" "ognivo-$version"
rm -rf "$stage"

# the newest glibc symbol that ognivo needs
glibc=$(objdump -T "$bin" 2> /dev/null | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -u -V | tail -n 1)
[[ -n "$glibc" ]] || fail "no glibc version found in $bin (objdump)"

cd "$out"
sha256sum -- "$vsix" "$tarball" > SHA256SUMS
echo "ognivo $version, glibc $glibc and newer:"
ls -l --block-size=M -- "$vsix" "$tarball" SHA256SUMS | awk '{print "  " $5 "  " $9}'
