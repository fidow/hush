// Cross-checks the translation dictionaries: every key defined in one language
// must exist in the other, every key the UI asks for must be defined, and every
// error code the server can return must have a message.
//
// Run: node scripts/check-i18n.mjs

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const desktop = resolve(here, "..");
const repo = resolve(desktop, "../..");

const read = (p) => readFileSync(resolve(repo, p), "utf8");

const i18n = read("apps/desktop/src/i18n.ts");
const main = read("apps/desktop/src/main.ts");
const html = read("apps/desktop/index.html");
const server = read("crates/hush-server/src/lib.rs");

// ---- keys defined per language ----------------------------------------------

function dictionary(lang) {
  const start = i18n.indexOf(`${lang}: {`);
  if (start < 0) throw new Error(`dictionary "${lang}" not found in i18n.ts`);
  // The dictionaries are the only top-level blocks, so the closing "  }," at
  // the start of a line ends this one.
  const end = i18n.indexOf("\n  },", start);
  const body = i18n.slice(start, end < 0 ? undefined : end);
  return new Set([...body.matchAll(/"([\w.]+)":/g)].map((m) => m[1]));
}

const es = dictionary("es");
const en = dictionary("en");

// ---- keys the UI asks for ----------------------------------------------------

const used = new Set([
  ...[...main.matchAll(/\bt\(\s*"([\w.]+)"/g)].map((m) => m[1]),
  ...[...html.matchAll(/data-i18n(?:-[\w-]+)?="([\w.]+)"/g)].map((m) => m[1]),
]);

// ---- error codes the server can return ---------------------------------------

// Codes come either from err(status, code, message) or from the shorthands
// built on top of it, such as bad_request(code, message).
const codes = new Set([
  ...[...server.matchAll(/\berr\(\s*StatusCode::[A-Z_]+\s*,\s*"(\w+)"/g)].map((m) => m[1]),
  ...[...server.matchAll(/\bbad_request\(\s*"(\w+)"/g)].map((m) => m[1]),
]);

// ---- report ------------------------------------------------------------------

const problems = [];
const report = (label, items) => {
  if (items.length) problems.push(`${label}:\n  ${items.sort().join("\n  ")}`);
};

report("defined in es but missing in en", [...es].filter((k) => !en.has(k)));
report("defined in en but missing in es", [...en].filter((k) => !es.has(k)));
report("used by the UI but not defined", [...used].filter((k) => !es.has(k) || !en.has(k)));
report(
  "server error codes without a message",
  [...codes].filter((c) => !es.has(`err.${c}`) || !en.has(`err.${c}`)),
);

if (problems.length) {
  console.error(problems.join("\n\n"));
  process.exit(1);
}
console.log(
  `i18n ok: ${es.size} keys in both languages, ${used.size} used by the UI, ${codes.size} server error codes covered`,
);
