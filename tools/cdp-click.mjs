// Click at a viewport point over CDP, the way a mouse would.
//
// `cdp-eval.mjs` can call `.click()` on a DOM element, but that is not the same
// as a mouse: it skips the canvas hit tests, the click-through regions and any
// `mousedown` handler that inspects coordinates. Use this when the thing you are
// testing is *where* the press lands.
//
//   node tools/cdp-click.mjs <ws-url> <x> <y> [--double]
//
// The whole press/release pair is sent so `mousedown` handlers that arm a drag
// see a completed click, and `mouseMoved` runs first so hover state is honest.
//
// `WebSocket` is a Node global (22+), like the other scripts in this directory —
// there is no `ws` dependency in the repo.

const args = process.argv.slice(2);
const positional = args.filter((arg) => !arg.startsWith("--"));
const [wsUrl, rawX, rawY] = positional;
const double = args.includes("--double");

if (!wsUrl || rawX === undefined || rawY === undefined) {
  console.error("usage: node tools/cdp-click.mjs <ws-url> <x> <y> [--double]");
  process.exit(2);
}

const x = Number(rawX);
const y = Number(rawY);
if (!Number.isFinite(x) || !Number.isFinite(y)) {
  console.error(`bad coordinates: ${rawX}, ${rawY}`);
  process.exit(2);
}

// The DOM WebSocket API (Node's global), not `ws`: addEventListener, `event.data`.
const socket = new WebSocket(wsUrl);
let nextId = 0;

function send(method, params) {
  return new Promise((resolve, reject) => {
    const id = ++nextId;
    const onMessage = (event) => {
      let message;
      try {
        message = JSON.parse(event.data);
      } catch {
        return;
      }
      if (message.id !== id) return;
      socket.removeEventListener("message", onMessage);
      if (message.error) reject(new Error(JSON.stringify(message.error)));
      else resolve(message.result);
    };
    socket.addEventListener("message", onMessage);
    socket.send(JSON.stringify({ id, method, params }));
  });
}

socket.addEventListener("error", () => {
  console.error("socket error");
  process.exit(1);
});

socket.addEventListener("open", async () => {
  try {
    const clickCount = double ? 2 : 1;
    await send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y, button: "none" });
    for (let n = 1; n <= clickCount; n += 1) {
      await send("Input.dispatchMouseEvent", {
        type: "mousePressed", x, y, button: "left", clickCount: n,
      });
      await send("Input.dispatchMouseEvent", {
        type: "mouseReleased", x, y, button: "left", clickCount: n,
      });
      if (double && n < clickCount) await new Promise((r) => setTimeout(r, 40));
    }
    console.log(JSON.stringify({ clicked: { x, y }, double }));
    socket.close();
  } catch (err) {
    console.error("click failed:", err.message);
    process.exit(1);
  }
});
