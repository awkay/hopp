import { getVersion } from "@tauri-apps/api/app";
import toast from "react-hot-toast";
import { typedInvoke } from "@/core_payloads";
import { changelogSections } from "@/lib/changelog";
import { logWarn } from "@/lib/log";
import { compareVersions } from "@/lib/semver";
import useStore, { WhatsNewTile } from "@/store/store";
import { tauriUtils } from "@/windows/window-utils";

/*
 * What the app does after an update (spec 0004, B10–B12). The state lives in the main window's
 * localStorage, which persists across launches. Any access can throw (storage disabled or
 * full), so every one is guarded and a failure only loses the confirmation or the tile.
 */

// The version the user clicked the update tile for (B10).
const PENDING_UPDATE_KEY = "hopp.pendingUpdate";
// The version that ran last (B11).
const LAST_VERSION_KEY = "hopp.lastVersion";
// The "What's new" tile, as JSON (B11).
const WHATS_NEW_TILE_KEY = "hopp.whatsNewTile";

const WHATS_NEW_TILE_MS = 7 * 24 * 60 * 60 * 1000;

const getItem = (key: string) => {
  try {
    return localStorage.getItem(key);
  } catch (err) {
    logWarn(`Failed to read ${key}`, err);
    return null;
  }
};

const setItem = (key: string, value: string) => {
  try {
    localStorage.setItem(key, value);
  } catch (err) {
    logWarn(`Failed to write ${key}`, err);
  }
};

const removeItem = (key: string) => {
  try {
    localStorage.removeItem(key);
  } catch (err) {
    logWarn(`Failed to remove ${key}`, err);
  }
};

/** Remembers that the user is updating to `version`, for the confirmation after the relaunch (B10). */
export const setPendingUpdate = (version: string) => setItem(PENDING_UPDATE_KEY, version);

export const clearPendingUpdate = () => removeItem(PENDING_UPDATE_KEY);

const readWhatsNewTile = (): WhatsNewTile | null => {
  const raw = getItem(WHATS_NEW_TILE_KEY);
  if (!raw) return null;
  try {
    const tile = JSON.parse(raw);
    if (
      typeof tile?.version === "string" &&
      (tile.since === null || typeof tile.since === "string") &&
      typeof tile.until === "number" &&
      tile.until > Date.now()
    ) {
      return { version: tile.version, since: tile.since, until: tile.until };
    }
  } catch {
    // Malformed: dropped below.
  }
  return null;
};

let tileExpiry: ReturnType<typeof setTimeout> | undefined;

const setWhatsNewTile = (tile: WhatsNewTile | null) => {
  if (tile) {
    setItem(WHATS_NEW_TILE_KEY, JSON.stringify(tile));
  } else {
    removeItem(WHATS_NEW_TILE_KEY);
  }
  useStore.getState().setWhatsNewTile(tile);
  clearTimeout(tileExpiry);
  // A menu bar app runs for weeks, so the tile also expires while it runs.
  if (tile) tileExpiry = setTimeout(() => setWhatsNewTile(null), tile.until - Date.now());
};

const isSignedIn = async () => {
  try {
    return (await tauriUtils.getStoredToken()) !== null;
  } catch (err) {
    logWarn("Failed to read the stored token", err);
    return false;
  }
};

/** The newest release in the bundled changelog before `version`, or null. */
const previousRelease = (version: string) =>
  changelogSections.find((section) => compareVersions(section.version, version) < 0)?.version ?? null;

let started = false;

/**
 * Runs once when the main window starts. After any upgrade, offers the release notes with the
 * "What's new" tile (B11). After an update from the update tile, shows the main window once
 * with a confirmation (B10).
 */
export async function handleAppStart() {
  if (started) return;
  started = true;

  const version = await getVersion();
  const pendingUpdate = getItem(PENDING_UPDATE_KEY);
  clearPendingUpdate();
  // An update from the tile that didn't land: the app quit or crashed during it, or the feed
  // served an older signed build under a newer version number. Only logged.
  if (pendingUpdate && pendingUpdate !== version) {
    logWarn(`Expected ${pendingUpdate} after the update from the update tile, but ${version} is running`);
  }

  const lastVersion = getItem(LAST_VERSION_KEY);
  let tile = readWhatsNewTile();
  // Without a last version this is the first run of a build with this feature: an upgrade
  // only for a user who was already signed in, not a new install. The version they came from
  // is unknown; assume the previous release.
  const upgraded = lastVersion ? compareVersions(version, lastVersion) > 0 : await isSignedIn();
  if (upgraded) {
    tile = {
      version,
      // Notes left unread since an earlier upgrade stay new.
      since: tile?.since ?? lastVersion ?? previousRelease(version),
      until: Date.now() + WHATS_NEW_TILE_MS,
    };
  }
  setWhatsNewTile(tile);
  setItem(LAST_VERSION_KEY, version);

  if (pendingUpdate === version) {
    // In the menu bar style this waits until the popup is placed under the tray icon.
    try {
      await typedInvoke("show_main_window_when_placed");
    } catch (err) {
      logWarn("Failed to show the main window after the update", err);
    }
    toast.success(`Hopp updated to ${version}`, { duration: 6_000 });
  }
}

/**
 * Opens the "What's new" tab (B12). If the "What's new" tile is up, the versions after the
 * one the user came from are marked new, and the tile goes away.
 */
export function openWhatsNew() {
  const { whatsNewTile, setWhatsNewSince, setTab } = useStore.getState();
  setWhatsNewSince(whatsNewTile && whatsNewTile.until > Date.now() ? whatsNewTile.since : null);
  setWhatsNewTile(null);
  setTab("whats-new");
}
