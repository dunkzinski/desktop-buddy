// Record the page as PNG frames over CDP (`Page.startScreencast`).
//
// Stills lie about motion: three screenshots a hundred milliseconds apart cannot
// show whether an arc is smooth, whether the arm leads the throw, or whether two
// objects overlap in flight. This captures what the compositor actually painted.
//
//   node tools/cdp-record.mjs <ws-url> <out-dir> <duration-ms> [fps-hint]
//
// Frames land as frame-00000.png … in `out-dir`; assemble with ffmpeg:
//   ffmpeg -framerate 15 -i frame-%05d.png -vf "scale=720:-1:flags=lanczos,split[a][b];
//     [a]palettegen[p];[b][p]paletteuse" -loop 0 out.gif
//
// `Page.screencastFrameAck` must be sent for every frame or the stream stalls
// after the first one — that is the whole trick.
const [wsUrl, outDir, durationRaw, fpsRaw] = process.argv.slice(2);
const duration = Number(durationRaw || 3000);
const fps = Number(fpsRaw || 15);

if (!wsUrl || !outDir) {
  console.error("usage: node tools/cdp-record.mjs <ws-url> <out-dir> <duration-ms> [fps]");
  process.exit(2);
}

const fs = await import("node:fs");
const path = await import("node:path");
fs.mkdirSync(outDir, { recursive: true });

const socket = new WebSocket(wsUrl);
let nextId = 0;
const pending = new Map();

function send(method, params) {
  return new Promise((resolve, reject) => {
    const id = ++nextId;
    pending.set(id, { resolve, reject });
    socket.send(JSON.stringify({ id, method, params }));
  });
}

socket.addEventListener("message", (event) => {
  const message = JSON.parse(event.data);
  if (message.id && pending.has(message.id)) {
    const { resolve, reject } = pending.get(message.id);
    pending.delete(message.id);
    if (message.error) reject(new Error(JSON.stringify(message.error)));
    else resolve(message.result);
    return;
  }
  if (message.method === "Page.screencastFrame") {
    const { data, sessionId, metadata } = message.params;
    const index = frames.length;
    frames.push(metadata.timestamp);
    fs.writeFileSync(
      path.join(outDir, `frame-${String(index).padStart(5, "0")}.png`),
      Buffer.from(data, "base64"),
    );
    // Acknowledge or the stream stops after one frame.
    socket.send(JSON.stringify({
      id: ++nextId,
      method: "Page.screencastFrameAck",
      params: { sessionId },
    }));
  }
});

const frames = [];

socket.addEventListener("error", () => {
  console.error("socket error");
  process.exit(1);
});

socket.addEventListener("open", async () => {
  try {
    await send("Page.enable", {});
    await send("Page.startScreencast", {
      format: "png",
      // Downscale at the source: a 1920-wide PNG per frame is slow enough to
      // distort the very timing we are trying to observe.
      maxWidth: 960,
      maxHeight: 540,
      everyNthFrame: 1,
    });
    await new Promise((resolve) => setTimeout(resolve, duration));
    await send("Page.stopScreencast", {});
    console.log(JSON.stringify({ frames: frames.length, dir: outDir }));
    socket.close();
  } catch (err) {
    console.error("record failed:", err.message);
    process.exit(1);
  }
});
