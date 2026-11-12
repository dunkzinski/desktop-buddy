/**
 * Static ESM audit for ui/js: named-import ↔ export matching + cycle detection.
 * Regex-based (no bundler deps), deliberately strict about the constructs this
 * codebase uses.
 */
import { readFileSync, readdirSync } from "node:fs";
import { join, dirname, resolve, basename } from "node:path";

const dir = process.argv[2] || "ui/js";
const files = readdirSync(dir).filter((f) => f.endsWith(".js")).sort();

const mods = new Map();

function stripComments(src) {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/(^|[^:])\/\/[^\n]*/g, "$1");
}

for (const file of files) {
  const raw = readFileSync(join(dir, file), "utf8");
  const src = stripComments(raw);
  const exportsSet = new Set();
  const reexports = [];
  // export const|let|var|function|async function|class NAME
  for (const m of src.matchAll(/^\s*export\s+(?:async\s+)?(?:const|let|var|function|class)\s+([A-Za-z_$][\w$]*)/gm)) {
    exportsSet.add(m[1]);
  }
  // export { a, b as c } [from "x"]
  for (const m of src.matchAll(/^\s*export\s*\{([^}]*)\}\s*(?:from\s*["']([^"']+)["'])?/gm)) {
    const from = m[2];
    for (const piece of m[1].split(",")) {
      const t = piece.trim();
      if (!t) continue;
      const [orig, alias] = t.split(/\s+as\s+/).map((s) => s.trim());
      if (from) reexports.push({ from, name: orig, alias: alias || orig });
      else exportsSet.add(alias || orig);
    }
  }
  // imports
  const imports = [];
  for (const m of src.matchAll(/import\s+([\s\S]*?)\s+from\s*["']([^"']+)["']/g)) {
    const clause = m[1].trim();
    const from = m[2];
    if (clause.startsWith("{")) {
      const names = clause.replace(/[{}]/g, "").split(",").map((s) => s.trim()).filter(Boolean)
        .map((t) => { const [o, a] = t.split(/\s+as\s+/).map((s) => s.trim()); return { name: o, local: a || o }; });
      imports.push({ from, names, ns: null });
    } else if (clause.startsWith("*")) {
      imports.push({ from, names: [], ns: clause.replace(/^\*\s+as\s+/, "").trim() });
    } else {
      const def = clause.split(",")[0].trim();
      imports.push({ from, names: [{ name: "default", local: def }], ns: null });
    }
  }
  mods.set(file, { file, exportsSet, reexports, imports });
}

let problems = 0;

// 1. named imports must exist
for (const [file, mod] of mods) {
  for (const imp of mod.imports) {
    if (!imp.from.startsWith("./") && !imp.from.startsWith("../")) continue;
    const target = resolve(dir, imp.from);
    const tfile = basename(target);
    if (!mods.has(tfile)) {
      console.log(`MISSING MODULE  ${file} -> ${imp.from}`);
      problems += 1;
      continue;
    }
    const t = mods.get(tfile);
    for (const n of imp.names) {
      if (n.name === "default") continue;
      if (!t.exportsSet.has(n.name) && !t.reexports.some((r) => r.name === n.name)) {
        console.log(`MISSING EXPORT  ${file} imports { ${n.name} } from ${imp.from} (not exported)`);
        problems += 1;
      }
    }
    if (imp.ns) {
      // namespace import: flag if used for assignment (illegal)
      const body = readFileSync(join(dir, file), "utf8");
      const re = new RegExp(`${imp.ns}\\.[A-Za-z_$][\\w$]*\\s*=[^=]`, "g");
      const hits = body.match(re);
      if (hits) {
        console.log(`NAMESPACE WRITE ${file}: ${hits.join(", ")} (TypeError at runtime)`);
        problems += 1;
      }
    }
  }
}

// 2. cycles
const WHITE = 0, GREY = 1, BLACK = 2;
const color = new Map([...mods.keys()].map((k) => [k, WHITE]));
const stack = [];
const cycles = new Set();

function dfs(file) {
  color.set(file, GREY);
  stack.push(file);
  const mod = mods.get(file);
  if (mod) {
    for (const imp of mod.imports) {
      if (!imp.from.startsWith("./") && !imp.from.startsWith("../")) continue;
      const t = basename(resolve(dir, imp.from));
      if (!mods.has(t)) continue;
      if (color.get(t) === GREY) {
        const at = stack.indexOf(t);
        cycles.add(stack.slice(at).concat(t).join(" -> "));
      } else if (color.get(t) === WHITE) {
        dfs(t);
      }
    }
  }
  stack.pop();
  color.set(file, BLACK);
}

for (const f of mods.keys()) if (color.get(f) === WHITE) dfs(f);

if (cycles.size) {
  for (const c of cycles) console.log(`CYCLE  ${c}`);
  problems += cycles.size;
} else {
  console.log("No circular dependencies.");
}

// 3. dangling "export let foo = () => {}" placeholders (the old broken pattern)
for (const [file] of mods) {
  const src = readFileSync(join(dir, file), "utf8");
  for (const m of src.matchAll(/export let ([A-Za-z_$][\w$]*)\s*=/g)) {
    console.log(`HOLLOW EXPORT  ${file}: "export let ${m[1]}" — reassignment from outside a module is impossible`);
    problems += 1;
  }
}

console.log(problems ? `\n${problems} problem(s) found.` : "\nAll modules clean.");
process.exit(problems ? 1 : 0);
