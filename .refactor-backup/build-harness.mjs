/**
 * Builds a browser test harness for ui/:
 *  - copies ui/ into the scratch dir
 *  - generates harness.html = index.html + a stub window.__TAURI__ injected
 *    BEFORE the module script (mirrors withGlobalTauri injection order)
 */
import { readFileSync, writeFileSync, cpSync, rmSync, mkdirSync } from "node:fs";
import { join } from "node:path";

const src = process.argv[2];
const out = process.argv[3];
const modelPath = process.argv[4];

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
cpSync(src, out, { recursive: true });

const model = JSON.parse(readFileSync(modelPath, "utf8"));
const rig = { format: model.format, name: model.name, label: model.label, notes: model.notes, bones: model.bones };
const payload = { name: model.name, label: model.label, rig, images: {} };

const stub = `<script>
/* ---- test stub: mimics withGlobalTauri's injected API surface ---- */
(function () {
  window.__calls = [];
  var listeners = {};
  var state = {
    model: ${JSON.stringify(payload)},
    rig: ${JSON.stringify(rig)},
    position: [0, 0],
    expression: "neutral",
    avatarState: "idle",
    hitRegions: [],
    clickThrough: false
  };
  window.__stub = state;
  function reply(cmd, args) {
    switch (cmd) {
      case "get_model": return state.model;
      case "get_runtime_info": return { api_url: "http://127.0.0.1:8765", version: "stub" };
      case "list_models": return [{ name: state.model.name, bones: state.rig.bones.length, parts: state.rig.bones.reduce(function (n, b) { return n + b.parts.length; }, 0) }];
      case "load_model": return state.model;
      case "update_hit_regions": state.hitRegions = args.regions || []; return null;
      case "set_click_through": state.clickThrough = !!args.enabled; return null;
      case "save_rig": state.rig = args.rig; return "C:/stub/" + args.rig.name + "/rig.json";
      case "reset_rig": return state.model;
      case "import_asset": return { name: "staged", staged: { name: "blob.png", image: "parts/blob.png", data_url: "data:image/png;base64,iVBORw0KGgo=", size: [40, 40] } };
      case "pick_import_file": return { path: "C:/stub/pick.png", kind: "image" };
      case "report_state": state.position = args.position; state.expression = args.expression; state.avatarState = args.state; return null;
      default: return null;
    }
  }
  window.__TAURI__ = {
    core: { invoke: function (cmd, args) { window.__calls.push({ cmd: cmd, args: args }); return Promise.resolve(reply(cmd, args)); } },
    event: {
      listen: function (name, handler) { (listeners[name] = listeners[name] || []).push(handler); return Promise.resolve(function () {}); },
      once: function (name, handler) { return Promise.resolve(function () {}); },
      emit: function (name, payload) { (listeners[name] || []).forEach(function (h) { h({ event: name, payload: payload }); }); }
    }
  };
  window.__emit = function (name, payload) { (listeners[name] || []).forEach(function (h) { h({ event: name, payload: payload }); }); };
  window.__errors = [];
  window.addEventListener("error", function (e) { window.__errors.push(String(e.message)); });
  window.addEventListener("unhandledrejection", function (e) { window.__errors.push("unhandled rejection: " + String(e.reason)); });
})();
<\/script>`;

let html = readFileSync(join(out, "index.html"), "utf8");
if (!html.includes('<script type="module" src="./js/main.js"></script>')) {
  throw new Error("anchor script tag not found");
}
html = html.replace('<script type="module" src="./js/main.js"></script>', stub + '\n<script type="module" src="./js/main.js"></script>');
writeFileSync(join(out, "harness.html"), html);
console.log("harness written:", join(out, "harness.html"));
