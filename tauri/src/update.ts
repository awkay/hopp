import { check, Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { logWarn } from "@/lib/log";

// The plugin's HTTP client has no timeout unless given one, so a server or proxy that stops
// sending would leave a request pending forever. Both are total deadlines (connect to last byte).
const CHECK_TIMEOUT_MS = 30_000;
// Bounds the download (~40 MB), not the install that follows it.
const DOWNLOAD_TIMEOUT_MS = 15 * 60_000;

/** Where `downloadAndRelaunch` is. `percent` is null while the download size is unknown. */
export type UpdateProgress =
  | { phase: "downloading"; version: string; percent: number | null }
  | { phase: "installing"; version: string };

/** How `downloadAndRelaunch` ends when it doesn't throw. "no-update": the feed no longer offers one. */
export type UpdateResult = "relaunching" | "no-update";

// Every `Update` that `check()` returns holds a webview resource until it is closed. The plugin
// never closes it, not even after `downloadAndInstall`.
const closeUpdate = (update: Update) => update.close().catch((err) => logWarn("Failed to close the update", err));

/** The version the feed offers if it is newer than this one, or null. */
export async function checkForUpdates() {
  const update = await check({ timeout: CHECK_TIMEOUT_MS });
  if (!update) return null;

  console.debug(`found update ${update.version} from ${update.date} with notes ${update.body}`);
  const { version } = update;
  await closeUpdate(update);
  return version;
}

/**
 * Downloads and installs the latest update, then relaunches. Resolves to "no-update" without
 * downloading if the feed has none (the release was pulled since the last check). Throws if a
 * step fails or times out.
 */
export async function downloadAndRelaunch(onProgress: (progress: UpdateProgress) => void): Promise<UpdateResult> {
  const update = await check({ timeout: CHECK_TIMEOUT_MS });
  if (!update) return "no-update";

  const { version } = update;
  let downloaded = 0;
  let contentLength: number | undefined;
  const percent = () => (contentLength ? Math.min(100, Math.floor((downloaded / contentLength) * 100)) : null);
  // Progress events come by a different route than the command's result, so one can arrive after
  // a failure and put the tile back to "Updating".
  let failed = false;

  onProgress({ phase: "downloading", version, percent: null });
  try {
    await update.downloadAndInstall(
      (event) => {
        if (failed) return;
        switch (event.event) {
          case "Started":
            contentLength = event.data.contentLength;
            console.debug(`started downloading ${event.data.contentLength} bytes`);
            onProgress({ phase: "downloading", version, percent: percent() });
            break;
          case "Progress":
            downloaded += event.data.chunkLength;
            onProgress({ phase: "downloading", version, percent: percent() });
            break;
          case "Finished":
            console.debug("download finished");
            onProgress({ phase: "installing", version });
            break;
        }
      },
      { timeout: DOWNLOAD_TIMEOUT_MS },
    );
  } catch (err) {
    failed = true;
    throw err;
  } finally {
    await closeUpdate(update);
  }

  console.debug("update installed");
  await relaunch();
  return "relaunching";
}
