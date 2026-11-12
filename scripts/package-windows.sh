#!/usr/bin/env bash
# The Windows packages of a release (docs/architecture.md §13, M6), in out/, built on
# Windows in Git Bash (CI: windows-2025):
#
#   ognivo-<version>-x86_64-windows-setup.exe   the installer: the window and ognivo for
#                                               the user alone, with no administrator, a
#                                               Start menu entry, ognivo on the user's PATH
#   ognivo-<version>-x86_64-windows.zip         the same, to unpack anywhere
#   ognivo-<version>-x86_64-windows-cli.zip     ognivo.exe alone
#   SHA256SUMS
#
# Before it: npm ci, and Electron's binary (npm run install-electron -w @ognivo/desktop),
# and Inno Setup 6. It builds ognivo (release) and the UI, and fetches the pinned rcedit
# once, checked by its hash, into ~/.cache/ognivo/package, to give the window's .exe
# ognivo's icon and version.
#
#   OGNIVO_BIN=path      package this ognivo.exe instead of building one
#   ISCC=path            Inno Setup's compiler, if it is not where its installer puts it
set -euo pipefail
cd "$(dirname "$0")/.."

RCEDIT=https://github.com/electron/rcedit/releases/download/v2.0.0/rcedit-x64.exe
RCEDIT_SHA256=3e7801db1a5edbec91b49a24a094aad776cb4515488ea5a4ca2289c400eade2a
HOMEPAGE=https://github.com/Kristian5013/ognivo
ISCC=${ISCC:-"/c/Program Files (x86)/Inno Setup 6/ISCC.exe"}
# Windows' own bsdtar, which writes zip; Git Bash's tar does not
ZIP_TAR=/c/Windows/System32/tar.exe

fail() {
    echo "package-windows: $*" >&2
    exit 1
}

# url sha256 path: the file at path, downloaded once and checked by its hash
fetch() {
    local url=$1 sum=$2 path=$3
    if [[ -f "$path" ]] && echo "$sum  $path" | sha256sum -c --status; then
        return
    fi
    mkdir -p "$(dirname "$path")"
    curl -fsSL --retry 3 -o "$path.part" "$url"
    if ! echo "$sum  $path.part" | sha256sum -c --status; then
        rm -f "$path.part"
        fail "$url is not the file pinned here (sha256)"
    fi
    mv "$path.part" "$path"
}

[[ "$(uname -s)" == MINGW* || "$(uname -s)" == MSYS* ]] || fail "this is for Windows, in Git Bash; scripts/package.sh is Linux's"
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[[ -n "$version" ]] || fail "no version in Cargo.toml"
for p in apps/desktop/package.json apps/ui/package.json; do
    v=$(node -p "require('./$p').version")
    [[ "$v" == "$version" ]] || fail "$p has version $v, Cargo.toml $version: scripts/version.sh sets them all"
done
# a file's version is numbers alone: 0.1.0-rc.1 is 0.1.0
file_version=${version%%-*}
electron=node_modules/electron/dist
[[ -f "$electron/electron.exe" ]] || fail "no Electron in $electron: npm run install-electron -w @ognivo/desktop"
[[ -f "$ISCC" ]] || fail "no Inno Setup at $ISCC: install it, or set ISCC"
[[ -f "$ZIP_TAR" ]] || fail "no $ZIP_TAR (Windows 10 and newer have it)"

if [[ -n "${OGNIVO_BIN:-}" ]]; then
    bin=$OGNIVO_BIN
else
    cargo build --release --locked -p ognivo-cli
    bin=target/release/ognivo.exe
fi
npm run -s build -w @ognivo/ui

tools=${XDG_CACHE_HOME:-$HOME/.cache}/ognivo/package
rcedit=$tools/rcedit-2.0.0-x64.exe
fetch "$RCEDIT" "$RCEDIT_SHA256" "$rcedit"

out=out
stage=$out/stage-windows
setup=ognivo-${version}-x86_64-windows-setup.exe
zip=ognivo-${version}-x86_64-windows.zip
cli=ognivo-${version}-x86_64-windows-cli.zip
rm -rf "$stage"
rm -f "$out/$setup" "$out/$zip" "$out/$cli" "$out/SHA256SUMS"
mkdir -p "$stage"

notices=$stage/THIRD-PARTY-LICENSES.txt
node scripts/licenses.mjs x86_64-pc-windows-msvc > "$notices"

# The window: Electron renamed, with ognivo's icon and version; the window's code and the
# UI as its app; ognivo in bin, which the window finds there and the installer puts on
# PATH. Electron's LICENSE and LICENSES.chromium.html stay as they are, ognivo's beside.
app=$stage/ognivo
cp -a "$electron/." "$app"
mv "$app/electron.exe" "$app/ognivo-window.exe"
rm -f "$app/resources/default_app.asar"
mkdir -p "$app/resources/app/desktop" "$app/resources/app/ui" "$app/bin"
cp apps/desktop/main.js apps/desktop/core.js apps/desktop/gpu.js apps/desktop/preload.cjs "$app/resources/app/desktop/"
cp -a apps/ui/dist "$app/resources/app/ui/dist"
node -e '
const [version, homepage] = process.argv.slice(1);
const pkg = {
  name: "ognivo",
  productName: "ognivo",
  version,
  description: "A coding agent for local models served by llama.cpp",
  homepage,
  license: "MIT",
  type: "module",
  main: "desktop/main.js",
};
process.stdout.write(`${JSON.stringify(pkg, null, 2)}\n`);
' "$version" "$HOMEPAGE" > "$app/resources/app/package.json"
cp "$bin" "$app/bin/ognivo.exe"
cp LICENSE "$app/ognivo-LICENSE.txt"
cp "$notices" "$app/THIRD-PARTY-LICENSES.txt"
"$rcedit" "$app/ognivo-window.exe" \
    --set-icon apps/desktop/packaging/ognivo.ico \
    --set-file-version "$file_version" \
    --set-product-version "$version" \
    --set-version-string ProductName ognivo \
    --set-version-string FileDescription ognivo \
    --set-version-string CompanyName "Kristian Pilatovich" \
    --set-version-string LegalCopyright "MIT License" \
    --set-version-string OriginalFilename ognivo-window.exe \
    --set-version-string InternalName ognivo-window ||
    fail "rcedit failed"

"$ISCC" /Q "/DVersion=$version" "/DFileVersion=$file_version" \
    "/DSource=$(cygpath -w "$app")" "/DOutput=$(cygpath -w "$out")" \
    apps/desktop/packaging/ognivo.iss || fail "Inno Setup failed"

(cd "$stage" && "$ZIP_TAR" -a -c -f "../$zip" ognivo) || fail "the zip of the window failed"

dir=$stage/ognivo-$version
mkdir -p "$dir"
cp "$bin" "$dir/ognivo.exe"
cp LICENSE README.md "$notices" "$dir/"
(cd "$stage" && "$ZIP_TAR" -a -c -f "../$cli" "ognivo-$version") || fail "the zip of ognivo failed"
rm -rf "$dir"

cd "$out"
sha256sum -- "$setup" "$zip" "$cli" > SHA256SUMS
echo "ognivo $version for Windows:"
ls -l --block-size=M -- "$setup" "$zip" "$cli" SHA256SUMS | awk '{print "  " $5 "  " $9}'
