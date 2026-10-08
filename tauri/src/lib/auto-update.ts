import useStore from "@/store/store";
import { clearPendingUpdate, setPendingUpdate } from "@/lib/after-update";
import { logError, logWarn } from "@/lib/log";
import { checkForUpdates, downloadAndRelaunch, UpdateProgress, UpdateResult } from "@/update";

// The updater plugin's error when the build has no feed (ad-hoc or non-notarized builds):
// the updater is off, and `check()` fails without a request.
const NO_ENDPOINTS_ERROR = "does not have any endpoints set";
let updaterOff = false;

/** Refreshes the version the update tile offers. Downloads nothing; a failed check is only logged. */
export async function pollUpdates() {
  if (import.meta.env.DEV || updaterOff) return;
  // Leave the tile alone while it shows an update's progress.
  if (useStore.getState().updateInProgress) return;

  try {
    const version = await checkForUpdates();
    // The user may have clicked the tile while this check ran.
    if (useStore.getState().updateInProgress) return;
    useStore.getState().setUpdateVersion(version);
  } catch (err) {
    if (String(err).includes(NO_ENDPOINTS_ERROR)) {
      updaterOff = true;
      return;
    }
    logWarn("Update check failed", err);
  }
}

// The update didn't happen: no confirmation after the next launch (B10), and calls are accepted again.
const abandonUpdate = () => {
  clearPendingUpdate();
  useStore.getState().setUpdateInProgress(false);
};

/**
 * Downloads and installs the offered update, then relaunches. Incoming calls are rejected
 * meanwhile. If the feed no longer offers an update, hides the update tile and resolves to
 * "no-update". On failure, logs the error, accepts calls again and rethrows.
 */
export async function installUpdate(onProgress: (progress: UpdateProgress) => void): Promise<UpdateResult> {
  const { updateVersion, setUpdateInProgress } = useStore.getState();
  setUpdateInProgress(true);
  // Written now, so WebKit has the download's seconds to persist it before the relaunch.
  let pendingVersion = updateVersion;
  if (pendingVersion) setPendingUpdate(pendingVersion);

  let result: UpdateResult;
  try {
    result = await downloadAndRelaunch((progress) => {
      // A newer release may have come out since the last check.
      if (progress.version !== pendingVersion) {
        pendingVersion = progress.version;
        setPendingUpdate(pendingVersion);
        useStore.getState().setUpdateVersion(pendingVersion);
      }
      onProgress(progress);
    });
  } catch (err) {
    abandonUpdate();
    logError("Update failed", err);
    throw err;
  }

  if (result === "no-update") {
    // The release was pulled or rolled back since the last check.
    abandonUpdate();
    useStore.getState().setUpdateVersion(null);
    logWarn(`The update feed no longer offers ${updateVersion}; hiding the update tile`);
  }
  return result;
}
