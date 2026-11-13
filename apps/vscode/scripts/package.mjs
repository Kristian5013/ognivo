// The .vsix of a platform, with the core in it (docs/architecture.md §13):
// out/ognivo-<version>-<target>.vsix, for VS Code's win32-x64 or linux-x64. It builds the
// extension (scripts/build.mjs) and puts in it the ognivo binary, in bin/: OGNIVO_BIN, or
// the repo's release build (cargo build --release -p ognivo-cli, which it runs when there
// is none); and THIRD-PARTY-LICENSES.txt, the code of others the package carries
// (scripts/licenses.mjs of the repo). The extension runs the binary copied out of the
// extension (src/core.ts).
//
//   npm run package -w apps/vscode                 for this machine
//   OGNIVO_BIN=path TARGET=linux-x64 npm run package -w apps/vscode

import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const repo = join(root, "..", "..");
const target = process.env.TARGET || `${process.platform}-${process.arch}`;
/** The Rust target whose crates the binary of `target` links. */
const RUST = { "win32-x64": "x86_64-pc-windows-msvc", "linux-x64": "x86_64-unknown-linux-gnu" };
if (!(target in RUST)) throw new Error(`no package for ${target}: ${Object.keys(RUST).join(" or ")}`);
const exe = target.startsWith("win32") ? ".exe" : "";
const version = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version;

let bin = process.env.OGNIVO_BIN;
if (!bin) {
  bin = join(repo, "target", "release", `ognivo${exe}`);
  execFileSync("cargo", ["build", "--release", "--locked", "-p", "ognivo-cli"], { cwd: repo, stdio: "inherit" });
}
if (!existsSync(bin)) throw new Error(`no ognivo binary at ${bin}`);

execFileSync(process.execPath, [join(root, "scripts", "build.mjs")], { cwd: root, stdio: "inherit" });
const notices = execFileSync(process.execPath, [join(repo, "scripts", "licenses.mjs"), RUST[target]], {
  cwd: repo,
  encoding: "utf8",
  maxBuffer: 64 * 1024 * 1024,
});
rmSync(join(root, "bin"), { recursive: true, force: true });
mkdirSync(join(root, "bin"));
copyFileSync(bin, join(root, "bin", `ognivo${exe}`));
copyFileSync(join(repo, "LICENSE"), join(root, "LICENSE"));
writeFileSync(join(root, "THIRD-PARTY-LICENSES.txt"), notices);
mkdirSync(join(repo, "out"), { recursive: true });
const out = join(repo, "out", `ognivo-${version}-${target}.vsix`);
const vsce = join(repo, "node_modules", "@vscode", "vsce", "vsce");
try {
  execFileSync(process.execPath, [vsce, "package", "--no-dependencies", "--target", target, "--out", out], {
    cwd: root,
    stdio: "inherit",
  });
} finally {
  rmSync(join(root, "bin"), { recursive: true, force: true });
  rmSync(join(root, "LICENSE"), { force: true });
  rmSync(join(root, "THIRD-PARTY-LICENSES.txt"), { force: true });
}
console.log(`package: ${out}`);
