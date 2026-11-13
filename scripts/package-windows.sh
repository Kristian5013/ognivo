#!/usr/bin/env bash
# The Windows packages of a release (docs/architecture.md §13, M6), in out/, built on
# Windows in Git Bash (CI: windows-2025):
#
#   ognivo-<version>-win32-x64.vsix           the VS Code extension, with ognivo.exe in it
#   ognivo-<version>-x86_64-windows.zip       ognivo.exe alone: the CLI, and the core it runs
#   SHA256SUMS
#
# Before it: npm ci. It builds ognivo (release) and the extension.
#
#   OGNIVO_BIN=path      package this ognivo.exe instead of building one
set -euo pipefail
cd "$(dirname "$0")/.."

# Windows' own bsdtar, which writes zip; Git Bash's tar does not
ZIP_TAR=/c/Windows/System32/tar.exe

fail() {
    echo "package-windows: $*" >&2
    exit 1
}

[[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* ]] || fail "this is for Windows, in Git Bash; scripts/package.sh is Linux's"
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[[ -n "$version" ]] || fail "no version in Cargo.toml"
v=$(node -p "require('./apps/vscode/package.json').version")
[[ "$v" == "$version" ]] || fail "apps/vscode/package.json has version $v, Cargo.toml $version: scripts/version.sh sets both"
[[ -f "$ZIP_TAR" ]] || fail "no $ZIP_TAR (Windows 10 and newer have it)"

if [[ -n "${OGNIVO_BIN:-}" ]]; then
    bin=$OGNIVO_BIN
else
    cargo build --release --locked -p ognivo-cli
    bin=target/release/ognivo.exe
fi

out=out
stage=$out/stage-windows
vsix=ognivo-${version}-win32-x64.vsix
zip=ognivo-${version}-x86_64-windows.zip
rm -rf "$stage"
rm -f "$out/$vsix" "$out/$zip" "$out/SHA256SUMS"
mkdir -p "$stage"

# the extension, with ognivo.exe in it (apps/vscode/scripts/package.mjs)
OGNIVO_BIN="$(cygpath -w "$bin")" TARGET=win32-x64 npm run -s package -w apps/vscode

# ognivo.exe alone
notices=$stage/THIRD-PARTY-LICENSES.txt
node scripts/licenses.mjs x86_64-pc-windows-msvc > "$notices"
dir=$stage/ognivo-$version
mkdir -p "$dir"
cp "$bin" "$dir/ognivo.exe"
cp LICENSE README.md "$notices" "$dir/"
(cd "$stage" && "$ZIP_TAR" -a -c -f "../$zip" "ognivo-$version") || fail "the zip of ognivo failed"
rm -rf "$stage"

cd "$out"
sha256sum -- "$vsix" "$zip" > SHA256SUMS
echo "ognivo $version for Windows:"
ls -l --block-size=M -- "$vsix" "$zip" SHA256SUMS | awk '{print "  " $5 "  " $9}'
