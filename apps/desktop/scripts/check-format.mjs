// Exercises the message formatter. `parseMessage` is deliberately free of any
// DOM, so it can be run here as plain data in and plain data out.
//
// Run: node scripts/check-format.mjs

import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { transformSync } from "esbuild";

const here = dirname(fileURLToPath(import.meta.url));
const source = resolve(here, "../src/format.ts");

// The module is TypeScript; strip the types and import what is left. esbuild
// is already here as part of the build, so this costs no new dependency.
const js = transformSync(readFileSync(source, "utf8"), {
  loader: "ts",
  format: "esm",
}).code;
const dir = mkdtempSync(join(tmpdir(), "hush-format-"));
const module = join(dir, "format.mjs");
writeFileSync(module, js);
const { parseMessage, plainText } = await import(pathToFileURL(module).href);
rmSync(dir, { recursive: true, force: true });

let failures = 0;

/// Compares against a compact shorthand: "b[…]" bold, "i[…]" italic,
/// "s[…]" strike, "m[…]" monospace, "B[…]" block, "¶" a line break.
function shorthand(spans) {
  return spans
    .map((span) => {
      switch (span.kind) {
        case "text":
          return span.text;
        case "break":
          return "¶";
        case "mono":
          return `m[${span.text}]`;
        case "block":
          return `B[${span.text}]`;
        case "bold":
          return `b[${shorthand(span.spans)}]`;
        case "italic":
          return `i[${shorthand(span.spans)}]`;
        case "strike":
          return `s[${shorthand(span.spans)}]`;
        default:
          throw new Error(`unknown span ${span.kind}`);
      }
    })
    .join("");
}

function check(input, want) {
  const got = shorthand(parseMessage(input));
  if (got !== want) {
    failures += 1;
    console.error(`  ${JSON.stringify(input)}\n    want ${want}\n    got  ${got}`);
  }
}

// The four kinds, on their own.
check("*negrita*", "b[negrita]");
check("_cursiva_", "i[cursiva]");
check("~tachado~", "s[tachado]");
check("`mono`", "m[mono]");
check("```en bloque```", "B[en bloque]");

// In the middle of a sentence, and more than once.
check("esto es *muy* importante", "esto es b[muy] importante");
check("*a* y *b*", "b[a] y b[b]");

// Nesting, which WhatsApp allows.
check("*_las dos_*", "b[i[las dos]]");
check("_*y al revés*_", "i[b[y al revés]]");

// Backticks are literal inside: no formatting, no surprises.
check("`*no es negrita*`", "m[*no es negrita*]");
check("```*tampoco* aquí```", "B[*tampoco* aquí]");

// Line breaks.
check("una\ndos", "una¶dos");
check("*a*\n_b_", "b[a]¶i[b]");

// What must NOT be formatting, because it is how people write.
check("2 * 3 * 4", "2 * 3 * 4");
check("* con espacio*", "* con espacio*");
check("*sin cerrar", "*sin cerrar");
check("**", "**");
check("a * b", "a * b");

// Markers work mid-word, which is what WhatsApp does and therefore what
// people expect — even though it means snake_case comes out italic. Written
// out rather than described, because it is the surprising case.
check("nada_de_esto", "nadai[de]esto");
check("snake_case y otro_mas", "snakei[case y otro]mas");

// A marker that never closes has to survive as itself, not eat the message.
check("precio: 5*", "precio: 5*");
check("_", "_");

// Text with no markers at all is one plain span.
check("hola qué tal", "hola qué tal");

// The plain form, for notifications.
const plain = plainText("*hola* _mundo_\n`code`");
if (plain !== "hola mundo code") {
  failures += 1;
  console.error(`  plainText -> ${JSON.stringify(plain)}`);
}

if (failures) {
  console.error(`\n${failures} formatting check(s) failed`);
  process.exit(1);
}
console.log("formatting: all checks passed");
