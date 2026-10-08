import { error, warn } from "@tauri-apps/plugin-log";

/*
 * Writes to `hopp.log` as well as the webview console. The log plugin files frontend messages
 * under `webview:*` targets, which the plugin's global Warn level filters: only warnings and
 * errors reach the file. Main window only (its capability grants `log:default`).
 */

const describe = (message: string, err: unknown) =>
  err === undefined ? message
  : err instanceof Error ? `${message}: ${err.message}`
  : `${message}: ${String(err)}`;

export function logError(message: string, err?: unknown) {
  console.error(message, err ?? "");
  error(describe(message, err)).catch((e) => console.error("Failed to write to hopp.log:", e));
}

export function logWarn(message: string, err?: unknown) {
  console.warn(message, err ?? "");
  warn(describe(message, err)).catch((e) => console.error("Failed to write to hopp.log:", e));
}
