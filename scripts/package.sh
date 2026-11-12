#!/usr/bin/env bash
# The packages of a release (docs/architecture.md §13, M6), in out/:
#
#   ognivo_<version>_amd64.deb             the window and ognivo in /usr/lib/ognivo, and
#                                          ognivo and ognivo-window in /usr/bin
#   ognivo-<version>-x86_64.AppImage       the same in one file; with one of ognivo's
#                                          commands (chat, run, doctor, ...) it is ognivo
#   ognivo-<version>-x86_64-linux.tar.gz   ognivo alone
#   SHA256SUMS
#
# Before it: npm ci, and Electron's binary (npm run install-electron -w @ognivo/desktop).
# It builds ognivo (release) and the UI, and fetches the pinned appimagetool and AppImage
# runtime once, checked by their hashes, into ~/.cache/ognivo/package. Build a release on
# the oldest system it is for (CI: Ubuntu 22.04): glibc is what ognivo needs of a system.
#
#   OGNIVO_BIN=path      package this ognivo instead of building one
#   ONLY="deb tar"       some of deb, appimage and tar
set -euo pipefail
cd "$(dirname "$0")/.."
# what the packages hold is the owner's to write, root's once installed
umask 022

APPIMAGETOOL=https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage
APPIMAGETOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
RUNTIME=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
MAINTAINER="Kristian Pilatovich <302522959+Kristian5013@users.noreply.github.com>"
HOMEPAGE=https://github.com/Kristian5013/ognivo
ONLY=${ONLY:-deb appimage tar}

fail() {
    echo "package: $*" >&2
    exit 1
}
wants() { [[ " $ONLY " == *" $1 "* ]]; }

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
    chmod 0755 "$path.part"
    mv "$path.part" "$path"
}

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[[ -n "$version" ]] || fail "no version in Cargo.toml"
for p in apps/desktop/package.json apps/ui/package.json; do
    v=$(node -p "require('./$p').version")
    [[ "$v" == "$version" ]] || fail "$p has version $v, Cargo.toml $version: scripts/version.sh sets them all"
done
electron=node_modules/electron/dist
[[ -x "$electron/electron" ]] || fail "no Electron in $electron: npm run install-electron -w @ognivo/desktop"

if [[ -n "${OGNIVO_BIN:-}" ]]; then
    bin=$OGNIVO_BIN
else
    cargo build --release --locked -p ognivo-cli
    bin=target/release/ognivo
fi
npm run -s build -w @ognivo/ui

out=out
stage=$out/stage
deb=ognivo_${version}_amd64.deb
appimage=ognivo-${version}-x86_64.AppImage
tarball=ognivo-${version}-x86_64-linux.tar.gz
rm -rf "$stage"
rm -f "$out/$deb" "$out/$appimage" "$out/$tarball" "$out/SHA256SUMS"
mkdir -p "$stage"

notices=$stage/THIRD-PARTY-LICENSES.txt
node scripts/licenses.mjs > "$notices"

# The window: Electron renamed, the window's code and the UI as its app, ognivo beside them
app=$stage/ognivo
cp -a "$electron/." "$app"
mv "$app/electron" "$app/ognivo-window"
rm -f "$app/resources/default_app.asar"
mkdir -p "$app/resources/app/desktop" "$app/resources/app/ui"
cp apps/desktop/main.js apps/desktop/core.js apps/desktop/gpu.js apps/desktop/preload.cjs "$app/resources/app/desktop/"
cp -a apps/ui/dist "$app/resources/app/ui/dist"
node -e '
const [version, homepage] = process.argv.slice(1);
const pkg = {
  name: "ognivo",
  productName: "ognivo",
  desktopName: "ognivo.desktop",
  version,
  description: "A coding agent for local models served by llama.cpp",
  homepage,
  license: "MIT",
  type: "module",
  main: "desktop/main.js",
};
process.stdout.write(`${JSON.stringify(pkg, null, 2)}\n`);
' "$version" "$HOMEPAGE" > "$app/resources/app/package.json"
install -m 0755 "$bin" "$app/resources/ognivo"
install -m 0755 apps/desktop/packaging/launch "$app/launch"
# as npm left Electron's files: writable by the group too
chmod -R go-w "$app"

# The newest glibc symbol that any of it needs
glibc=$(objdump -T "$app/ognivo-window" "$app/resources/ognivo" "$app/chrome_crashpad_handler" \
    "$app/chrome-sandbox" "$app"/*.so* 2> /dev/null |
    grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -u -V | tail -n 1)
[[ -n "$glibc" ]] || fail "no glibc version found in the binaries (objdump)"

if wants deb; then
    root=$stage/deb
    doc=$root/usr/share/doc/ognivo
    mkdir -p "$root/DEBIAN" "$root/usr/lib" "$root/usr/bin" "$root/usr/share/applications" \
        "$root/usr/share/icons/hicolor/scalable/apps" "$doc"
    cp -a "$app" "$root/usr/lib/ognivo"
    # Chromium's setuid sandbox helper, for where user namespaces are kept from it
    # (Ubuntu 23.10 and newer)
    chmod 4755 "$root/usr/lib/ognivo/chrome-sandbox"
    ln -s ../lib/ognivo/resources/ognivo "$root/usr/bin/ognivo"
    ln -s ../lib/ognivo/launch "$root/usr/bin/ognivo-window"
    install -m 0644 apps/desktop/packaging/ognivo.desktop "$root/usr/share/applications/ognivo.desktop"
    install -m 0644 apps/desktop/packaging/ognivo.svg "$root/usr/share/icons/hicolor/scalable/apps/ognivo.svg"
    install -m 0644 "$notices" "$doc/THIRD-PARTY-LICENSES.txt"
    {
        echo "ognivo: a coding agent for local models served by llama.cpp"
        echo "$HOMEPAGE"
        echo
        cat LICENSE
        echo
        echo "ognivo's window is Electron, with Chromium: their notices are in"
        echo "/usr/lib/ognivo/LICENSE and /usr/lib/ognivo/LICENSES.chromium.html. What ognivo"
        echo "links of others' code and what its window bundles, with their licenses, is in"
        echo "THIRD-PARTY-LICENSES.txt beside this file."
    } > "$doc/copyright"
    size=$(du -sk --exclude=DEBIAN "$root" | cut -f 1)
    cat > "$root/DEBIAN/control" << EOF
Package: ognivo
Version: $version
Architecture: amd64
Maintainer: $MAINTAINER
Installed-Size: $size
Depends: libc6 (>= $glibc), libgcc-s1, libasound2t64 | libasound2, libatk-bridge2.0-0t64 | libatk-bridge2.0-0, libatk1.0-0t64 | libatk1.0-0, libatspi2.0-0t64 | libatspi2.0-0, libcairo2, libcups2t64 | libcups2, libdbus-1-3, libexpat1, libgbm1, libglib2.0-0t64 | libglib2.0-0, libgtk-3-0t64 | libgtk-3-0, libnspr4, libnss3, libpango-1.0-0, libudev1, libx11-6, libxcb1, libxcomposite1, libxdamage1, libxext6, libxfixes3, libxkbcommon0, libxrandr2
Recommends: xdg-utils
Section: devel
Priority: optional
Homepage: $HOMEPAGE
Description: coding agent for local models served by llama.cpp
 ognivo works in a project from the terminal (ognivo chat, ognivo run) or in its
 window (ognivo-window), on a llama-server that you run. It owns each model's native
 format: prompt rendering, tokenization, tool-call parsing, reasoning and sampling
 are tuned per model family instead of being squeezed through a generic chat API.
EOF
    dpkg-deb --root-owner-group -Zxz --build "$root" "$out/$deb" > /dev/null
    rm -rf "$root"
fi

if wants appimage; then
    tools=${XDG_CACHE_HOME:-$HOME/.cache}/ognivo/package
    tool=$tools/appimagetool-1.9.1-x86_64.AppImage
    runtime=$tools/runtime-20251108-x86_64
    fetch "$APPIMAGETOOL" "$APPIMAGETOOL_SHA256" "$tool"
    fetch "$RUNTIME" "$RUNTIME_SHA256" "$runtime"
    dir=$stage/AppDir
    mkdir -p "$dir/usr/lib" "$dir/usr/share/doc/ognivo"
    cp -a "$app" "$dir/usr/lib/ognivo"
    install -m 0755 apps/desktop/packaging/AppRun "$dir/AppRun"
    install -m 0644 apps/desktop/packaging/ognivo.desktop "$dir/ognivo.desktop"
    install -m 0644 apps/desktop/packaging/ognivo.svg "$dir/ognivo.svg"
    ln -s ognivo.svg "$dir/.DirIcon"
    install -m 0644 LICENSE "$notices" "$dir/usr/share/doc/ognivo/"
    # the tool is an AppImage too: extracted, it needs no FUSE
    ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$tool" --no-appstream --runtime-file "$runtime" \
        "$dir" "$out/$appimage" > "$stage/appimagetool.log" 2>&1 ||
        fail "appimagetool failed: $stage/appimagetool.log"
    rm -rf "$dir"
fi

if wants tar; then
    dir=$stage/ognivo-$version
    mkdir -p "$dir"
    install -m 0755 "$bin" "$dir/ognivo"
    install -m 0644 LICENSE README.md "$notices" "$dir/"
    tar -C "$stage" --owner=0 --group=0 --numeric-owner -czf "$out/$tarball" "ognivo-$version"
    rm -rf "$dir"
fi

cd "$out"
made=()
for f in "$deb" "$appimage" "$tarball"; do
    [[ -f "$f" ]] && made+=("$f")
done
sha256sum -- "${made[@]}" > SHA256SUMS
echo "ognivo $version, glibc $glibc and newer:"
ls -l --block-size=M -- "${made[@]}" SHA256SUMS | awk '{print "  " $5 "  " $9}'
