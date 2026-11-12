/**
 * char-rig — Tauri IPC leaf module
 *
 * Owns every access to `window.__TAURI__`. This module imports nothing, so
 * every other module can depend on it without creating a cycle.
 *
 * `__TAURI__` is injected by Tauri as an init script (withGlobalTauri), but we
 * still read it lazily on every call: a module-level `const TAURI = ...`
 * snapshot is captured before anyone can know whether injection happened, and
 * it silently pins the app to "not Tauri" forever if it had not.
 */

function tauri() {
  return typeof window !== "undefined" ? window.__TAURI__ : undefined;
}

/** True when running inside the Tauri webview (not a plain browser). */
export function isTauri() {
  const api = tauri();
  return !!(api && api.core && typeof api.core.invoke === "function");
}

/**
 * Invoke a Tauri command. Returns `null` on failure instead of throwing, so
 * callers can keep the `if (result)` idiom used throughout the UI.
 */
export async function call(cmd, args) {
  const api = tauri();
  if (!api || !api.core || typeof api.core.invoke !== "function") {
    return null;
  }
  try {
    return await api.core.invoke(cmd, args || {});
  } catch (err) {
    console.error("[avatar] command failed:", cmd, err);
    return { __error: String(err) };
  }
}

/** Subscribe to a Tauri event once. No-op outside Tauri. */
export async function once(event, handler) {
  const api = tauri();
  if (!api || !api.event) return;
  await api.event.once(event, handler);
}

/** Subscribe to a Tauri event. No-op outside Tauri. */
export async function listen(event, handler) {
  const api = tauri();
  if (!api || !api.event || typeof api.event.listen !== "function") return;
  await api.event.listen(event, handler);
}
