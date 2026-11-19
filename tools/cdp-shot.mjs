// Capture the live WebView2 page (plus the avatar's real bounds) over CDP.
// Usage: node cdp-shot.mjs <wsUrl> <out.png>
import { writeFileSync } from "node:fs";

const [wsUrl, out] = process.argv.slice(2);
if (!wsUrl || !out) {
  console.error("usage: node cdp-shot.mjs <wsUrl> <out.png>");
  process.exit(2);
}

const ws = new WebSocket(wsUrl);
let id = 0;
const pending = new Map();
const send = (method, params = {}) =>
  new Promise((resolve, reject) => {
    const reqId = ++id;
    pending.set(reqId, { resolve, reject });
    ws.send(JSON.stringify({ id: reqId, method, params }));
  });

ws.onmessage = (event) => {
  const msg = JSON.parse(event.data);
  const slot = pending.get(msg.id);
  if (!slot) return;
  pending.delete(msg.id);
  if (msg.error) slot.reject(new Error(JSON.stringify(msg.error)));
  else slot.resolve(msg.result);
};

ws.onopen = async () => {
  try {
    await send("Page.enable");
    const shot = await send("Page.captureScreenshot", { format: "png" });
    writeFileSync(out, Buffer.from(shot.data, "base64"));
    const bounds = await send("Runtime.evaluate", {
      expression: "JSON.stringify(window.__avatarDebug.bounds())",
      returnByValue: true,
    });
    console.log(JSON.stringify({ out, bounds: JSON.parse(bounds.result.value) }));
    ws.close();
  } catch (err) {
    console.error(String(err));
    process.exit(4);
  }
};
