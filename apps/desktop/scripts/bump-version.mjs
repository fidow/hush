// Advances the version *after* packaging, so the artefact just produced
// carries the current number and the repo moves on to the next one. Run by
// `npm run tauri:build`, not by the plain `build` script, which is also used
// for type-checking during development and would inflate the number.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const appDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const tauriConf = join(appDir, "src-tauri", "tauri.conf.json");
const packageJson = join(appDir, "package.json");
const workspaceToml = join(appDir, "..", "..", "Cargo.toml");

// Only the patch moves on its own. Major and minor are deliberate calls, so
// they change only when asked for explicitly:
//   node scripts/bump-version.mjs minor
//   node scripts/bump-version.mjs major
const part = process.argv[2] ?? "patch";
if (!["patch", "minor", "major"].includes(part)) {
  console.error(`Unknown version part "${part}"; use patch, minor or major.`);
  process.exit(1);
}

const conf = JSON.parse(readFileSync(tauriConf, "utf8"));
const [major, minor, patch] = conf.version.split(".").map(Number);
const next = {
  major: `${major + 1}.0.0`,
  minor: `${major}.${minor + 1}.0`,
  patch: `${major}.${minor}.${patch + 1}`,
}[part];

conf.version = next;
writeFileSync(tauriConf, `${JSON.stringify(conf, null, 2)}\n`);

const pkg = JSON.parse(readFileSync(packageJson, "utf8"));
pkg.version = next;
writeFileSync(packageJson, `${JSON.stringify(pkg, null, 2)}\n`);

// The Rust crates inherit the workspace version; keep it in step so the
// binaries and the installer agree.
const toml = readFileSync(workspaceToml, "utf8");
writeFileSync(
  workspaceToml,
  toml.replace(/^version = "\d+\.\d+\.\d+"$/m, `version = "${next}"`),
);

console.log(`Hush ${next}`);
