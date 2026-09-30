import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { gzipSync } from "node:zlib";

// Count the complete static dependency graph; splitting a chunk alone must not look like a saving.
const output = resolve(process.argv[2] ?? "dist");
const html = readFileSync(join(output, "index.html"), "utf8");
const entry = html.match(/<script\b[^>]*\bsrc="([^"]+\.js)"/)[1];
const seen = new Map();
function visit(path) {
  if (seen.has(path)) return;
  const code = readFileSync(path);
  seen.set(path, { bytes: code.length, gzip: gzipSync(code).length });
  for (const match of code.toString().matchAll(/\b(?:from\s*|import\s*)["'](\.\/[^"']+\.js)["']/g)) {
    visit(resolve(dirname(path), match[1]));
  }
}
visit(join(output, "assets", entry.split("/").at(-1)));
const total = [...seen.values()].reduce((sum, item) => ({ bytes: sum.bytes + item.bytes, gzip: sum.gzip + item.gzip }), { bytes: 0, gzip: 0 });
console.log(JSON.stringify({ staticChunks: seen.size, bytes: total.bytes, gzipBytes: total.gzip }, null, 2));
