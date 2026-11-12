/**
 * Free-identifier scan: finds identifiers used in a module that are neither
 * declared in it, imported, nor a browser/JS global. This catches the class of
 * bug a "functions moved out of global scope" refactor produces.
 *
 * Heuristic (flat scope) — it errs toward false negatives (a param declared in
 * one function hides the same name elsewhere), never toward false positives
 * for the constructs this codebase uses.
 */
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const dir = process.argv[2] || "ui/js";
const files = readdirSync(dir).filter((f) => f.endsWith(".js")).sort();

const KEYWORDS = new Set(("break case catch class const continue debugger default delete do else export extends " +
  "finally for function if import in instanceof let new return super switch this throw try typeof var void while " +
  "with yield async await of from as static get set true false null undefined").split(" "));

const GLOBALS = new Set(("window document console Math JSON Object Array Number String Boolean Symbol Date RegExp " +
  "Error TypeError Promise Map Set WeakMap WeakSet Image Path2D DOMParser Infinity NaN performance " +
  "requestAnimationFrame cancelAnimationFrame setInterval clearInterval setTimeout clearTimeout queueMicrotask " +
  "localStorage sessionStorage navigator location fetch isNaN isFinite parseFloat parseInt encodeURIComponent " +
  "decodeURIComponent structuredClone devicePixelRatio getComputedStyle alert confirm prompt history screen " +
  "ShadowRoot CustomEvent Event KeyboardEvent MouseEvent WheelEvent URL URLSearchParams Blob File FileReader " +
  "AudioContext Uint8Array Float32Array Int32Array ArrayBuffer DataView globalThis").split(" "));

let total = 0;

for (const file of files) {
  const raw = readFileSync(join(dir, file), "utf8");
  // strip block comments, line comments, strings, template literals, regex-ish
  const src = raw
    .replace(/\/\*[\s\S]*?\*\//g, " ")
    .replace(/(^|[^:\\])\/\/[^\n]*/g, "$1 ")
    .replace(/'(?:\\.|[^'\\])*'/g, "''")
    .replace(/"(?:\\.|[^"\\])*"/g, '""')
    .replace(/`(?:\\.|[^`\\])*`/g, "``");

  const declared = new Set();

  // imports: default, {a as b}, * as ns
  for (const m of raw.matchAll(/import\s+([\s\S]*?)\s+from\s*["'][^"']*["']/g)) {
    const clause = m[1];
    for (const t of clause.replace(/[{}*]/g, " ").split(/[\s,]+/)) {
      if (!t || t === "as") continue;
      declared.add(t);
    }
  }
  // declarations
  for (const m of src.matchAll(/\b(?:function|class)\s+([A-Za-z_$][\w$]*)/g)) declared.add(m[1]);
  for (const m of src.matchAll(/\b(?:const|let|var)\s+([^;\n]*)/g)) {
    for (const t of m[1].split(",")) {
      const n = t.trim().replace(/^[{[].*$/, "").split(/[=:]/)[0].trim();
      if (/^[A-Za-z_$][\w$]*$/.test(n)) declared.add(n);
    }
  }
  // destructuring declarations: const { a, b: c } = / const [a, b] =
  for (const m of src.matchAll(/\b(?:const|let|var)\s*[{[][\s\S]*?[}\]]\s*=/g)) {
    for (const t of m[0].replace(/[{}[\]:]/g, " ").split(/[\s,]+/)) {
      if (/^[A-Za-z_$][\w$]*$/.test(t)) declared.add(t);
    }
  }
  // params: (...) => and function (...)
  for (const m of src.matchAll(/\(([^()]*)\)\s*=>/g)) {
    for (const t of m[1].split(",")) {
      const n = t.trim().split(/[=\s]/)[0];
      if (/^[A-Za-z_$][\w$]*$/.test(n)) declared.add(n);
    }
  }
  for (const m of src.matchAll(/(?:function\s*[A-Za-z_$\w]*\s*|catch\s*)\(([^()]*)\)/g)) {
    for (const t of m[1].split(",")) {
      const n = t.trim().split(/[=\s]/)[0];
      if (/^[A-Za-z_$][\w$]*$/.test(n)) declared.add(n);
    }
  }
  // single-identifier arrow params: x => 
  for (const m of src.matchAll(/(?:^|[,(\s!?&|])([A-Za-z_$][\w$]*)\s*=>/g)) declared.add(m[1]);
  // labels / object shorthand keys we may have missed, plus loop bindings
  for (const m of src.matchAll(/\bfor\s*\(\s*(?:const|let|var)\s+([A-Za-z_$][\w$]*)/g)) declared.add(m[1]);

  const undefinedNames = new Map();
  const re = /[A-Za-z_$][\w$]*/g;
  let match;
  while ((match = re.exec(src))) {
    const name = match[0];
    if (KEYWORDS.has(name) || GLOBALS.has(name) || declared.has(name)) continue;
    // skip property access (a.b) and object keys (a: 1)
    const before = src.slice(Math.max(0, match.index - 2), match.index);
    if (/\.\s*$/.test(before)) continue;
    const after = src.slice(match.index + name.length, match.index + name.length + 2);
    if (/^\s*:/.test(after) && !/^\s*::$/.test(after)) continue;
    const line = src.slice(0, match.index).split("\n").length;
    if (!undefinedNames.has(name)) undefinedNames.set(name, line);
  }

  if (undefinedNames.size) {
    total += undefinedNames.size;
    console.log(`\n${file}:`);
    for (const [name, line] of undefinedNames) console.log(`  ${name}  (first seen ~line ${line})`);
  }
}

console.log(total ? `\n${total} possible undefined reference(s).` : "\nNo undefined references.");
