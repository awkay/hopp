import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import useStore from "@/store/store";
const isTauri = typeof window !== "undefined" && window.__TAURI_INTERNALS__ !== undefined;

export let appVersion: null | string = null;
getVersion().then((version) => {
  appVersion = version;
});

const getAvailableContent = async () => {
  if (isTauri) await invoke("get_available_content");
};

const closeCameraWindow = async () => {
  if (isTauri) {
    const cameraWindow = await WebviewWindow.getByLabel("camera");
    if (cameraWindow) {
      await cameraWindow.close();
    }
  }
};

const storeTokenBackend = async (token: string) => {
  if (isTauri) {
    try {
      await invoke("store_token_cmd", { token });
    } catch (err) {
      console.error("Failed to store token:", err);
    }
  }
};

const getStoredToken = async () => {
  return await invoke<string | null>("get_stored_token");
};

const getFavoriteTeammates = async (): Promise<string[]> => {
  try {
    return await invoke<string[]>("get_favorite_teammates");
  } catch (err) {
    console.error("Failed to load favorite teammates:", err);
    return [];
  }
};

let favoriteTeammateWrites: Promise<void> = Promise.resolve();

/**
 * Stars or unstars a teammate. The store changes at once; the write to the app state runs
 * after earlier ones finish, so writes land in toggle order. A failed write restores the
 * previous value.
 */
const toggleFavoriteTeammate = (userId: string) => {
  const { favoriteTeammateIds, setFavoriteTeammate } = useStore.getState();
  const favorite = !favoriteTeammateIds.includes(userId);
  setFavoriteTeammate(userId, favorite);
  favoriteTeammateWrites = favoriteTeammateWrites.then(async () => {
    try {
      await invoke("set_favorite_teammate", { userId, favorite });
    } catch (err) {
      console.error("Failed to save favorite teammate:", err);
      useStore.getState().setFavoriteTeammate(userId, !favorite);
    }
  });
};

/**
 * Removes the favorites whose user ID is not in `knownIds`, from the app state and the store.
 * `knownIds` must be the full teammate list of a successful fetch. Runs after earlier toggles
 * finish, so it never reorders with them.
 */
const pruneFavoriteTeammates = (knownIds: string[]) => {
  favoriteTeammateWrites = favoriteTeammateWrites.then(async () => {
    try {
      await invoke("retain_favorite_teammates", { knownIds });
      const known = new Set(knownIds);
      const { favoriteTeammateIds, setFavoriteTeammateIds } = useStore.getState();
      if (favoriteTeammateIds.some((id) => !known.has(id))) {
        setFavoriteTeammateIds(favoriteTeammateIds.filter((id) => known.has(id)));
      }
    } catch (err) {
      console.error("Failed to prune favorite teammates:", err);
    }
  });
};

const deleteStoredToken = async () => {
  if (isTauri) {
    try {
      await invoke("delete_stored_token");
    } catch (err) {
      console.error("Failed to delete stored token:", err);
    }
  }
};

const stopSharing = async () => {
  await invoke("stop_sharing");
};

const showWindow = async (windowLabel: string) => {
  if (isTauri) {
    const window = await WebviewWindow.getByLabel(windowLabel);
    if (window) {
      await window.show();
      await window.unminimize();
      await window.setFocus();
    }
  }
};

const closeScreenShareWindow = async () => {
  if (isTauri) {
    const screenShareWindow = await WebviewWindow.getByLabel("screenshare");
    if (screenShareWindow) {
      console.debug("Closing screen share window");
      await screenShareWindow.close();
    }
  }
};

/**
 * Ends call `callId` in Tauri and core. Core's CallEnd also stops screen sharing.
 * Without an id there is no call in core to end (it never got a CallStart), so nothing is
 * sent: an id-less CallEnd would end whatever call core has, possibly a newer one.
 */
const resetCoreProcess = async (callId?: number) => {
  if (callId === undefined) return;
  await invoke("reset_core_process", { callId });
};

const endCallCleanup = async (callId?: number) => {
  await resetCoreProcess(callId);
  await closeScreenShareWindow();
  await closeCameraWindow();
};

const getTokenParam = (param: string) => {
  const urlParams = new URLSearchParams(window.location.search);
  return urlParams.get(param);
};

const setControllerCursor = async (enabled: boolean) => {
  await invoke("set_controller_cursor", { enabled: enabled });
};

const openAccessibilitySettings = async () => {
  return await invoke("open_accessibility_settings");
};

const openMicrophoneSettings = async () => {
  return await invoke("open_microphone_settings");
};

const openCameraSettings = async () => {
  return await invoke("open_camera_settings");
};

const openScreenShareSettings = async () => {
  return await invoke("open_screenshare_settings");
};

const triggerScreenSharePermission = async () => {
  return await invoke<boolean>("trigger_screenshare_permission");
};

const getControlPermission = async () => {
  return await invoke<boolean>("get_control_permission");
};

const getMicPermission = async () => {
  return await invoke<boolean>("get_microphone_permission");
};

const getScreenSharePermission = async () => {
  return await invoke<boolean>("get_screenshare_permission");
};

const getCameraPermission = async () => {
  return await invoke<boolean>("get_camera_permission");
};

const hideTrayIconInstruction = async () => {
  await invoke("skip_tray_notification_selection_window");
};

const getLastUsedMic = async () => {
  return await invoke<string | null>("get_last_used_mic");
};

const setLastUsedMic = async (micId: string) => {
  return await invoke("set_last_used_mic", { mic: micId });
};

const getLastUsedCamera = async () => {
  return await invoke<string | null>("get_last_used_camera");
};

const setLastUsedCamera = async (camera: string) => {
  return await invoke("set_last_used_camera", { camera });
};

const getSharerDrawPersist = async (): Promise<boolean> => {
  return await invoke<boolean>("get_sharer_draw_persist");
};

const setSharerDrawPersist = async (persist: boolean): Promise<void> => {
  return await invoke("set_sharer_draw_persist", { persist });
};

const getDrawingEnabled = async (): Promise<boolean> => {
  return await invoke<boolean>("get_drawing_enabled");
};

const setDrawingEnabled = async (enabled: boolean, permanent: boolean): Promise<void> => {
  return await invoke("set_drawing_enabled", { enabled, permanent });
};

const getDrawingHintShown = async (): Promise<boolean> => {
  return await invoke<boolean>("get_drawing_hint_shown");
};

const setDrawingHintShown = async (shown: boolean): Promise<void> => {
  return await invoke("set_drawing_hint_shown", { shown });
};

const minimizeMainWindow = async () => {
  return await invoke("minimize_main_window");
};

const setLivekitUrl = async (url: string) => {
  return await invoke("set_livekit_url", { url });
};

const getLivekitUrl = async () => {
  return await invoke<string>("get_livekit_url");
};

const setSentryMetadata = async (userId: string) => {
  const appVersion = await getVersion();
  return await invoke("set_sentry_metadata", { userId, appVersion });
};

let lastCallId = 0;

/** Unique, increasing call id (safe integer, fits core's u64). */
const newCallId = () => {
  lastCallId = Math.max(lastCallId + 1, Date.now());
  return lastCallId;
};

/**
 * Starts the call whose tokens are in the store. Assigns it a fresh call id first, so any
 * event from an earlier call can be told apart. If starting fails, the call is also ended
 * in core (it may have started after Tauri gave up waiting).
 */
/** Thrown by `callStarted`; `callId` identifies the call that failed to start. */
export class CallStartError extends Error {
  constructor(
    readonly callId: number,
    readonly reason: unknown,
  ) {
    super(`Failed to start call ${callId}: ${String(reason)}`);
  }
}

/** `source` names the UI path starting the call; core logs it to trace duplicate starts. */
const callStarted = async (audioToken: string, videoToken: string, source: string): Promise<number> => {
  if (!useStore.getState().callTokens) {
    // The call was ended before it could start; don't start one in core with no UI.
    throw new Error("No call to start");
  }
  const callId = newCallId();
  useStore.getState().updateCallTokens({ callId });
  try {
    await invoke("call_started", { callId, audioToken, videoToken, source });
    return callId;
  } catch (error) {
    resetCoreProcess(callId).catch((e) => console.error("Failed to end call after start failure:", e));
    throw new CallStartError(callId, error);
  }
};

/**
 * True when `error` (from `callStarted`) belongs to the call the store still shows. A start
 * failure that arrives after the user already moved on to another call must not touch that
 * call. Callers only clear call state when this is true.
 */
const isFailedStartOfCurrentCall = (error: unknown): error is CallStartError =>
  error instanceof CallStartError && useStore.getState().callTokens?.callId === error.callId;

/** Clears the call from the store if `error` is the failed start of the current call. */
const clearFailedCall = (error: unknown): boolean => {
  if (!isFailedStartOfCurrentCall(error)) return false;
  useStore.getState().setCallTokens(null);
  return true;
};

/**
 * Loads the custom server URL from Tauri backend.
 */
const loadCustomServerUrl = async (): Promise<string | null> => {
  try {
    return await invoke<string | null>("get_hopp_server_url");
  } catch (error) {
    console.error("Failed to load custom server url from backend:", error);
  }
  return null;
};

/**
 * Sets a custom Hopp server URL.
 * Pass null to clear the custom URL and use the default.
 * Signs out the user when the URL changes.
 */
const setHoppServerUrl = async (url: string | null): Promise<void> => {
  try {
    await invoke("set_hopp_server_url", { url });
    // Sign out the user when changing the server URL
    await deleteStoredToken();
  } catch (error) {
    console.error("Failed to set hopp server url:", error);
    throw error;
  }
};

const setCallFeedbackPopup = async (enabled: boolean): Promise<void> => {
  await invoke("set_call_feedback_popup", { enabled });
};

const openSettingsWindow = async (): Promise<void> => {
  if (isTauri) {
    try {
      await invoke("create_settings_window");
      const windowHandle = await WebviewWindow.getByLabel("settings");
      if (windowHandle) {
        await windowHandle.setFocus();
      }
    } catch (error) {
      console.error("Failed to open settings window:", error);
    }
  }
};

const createFeedbackWindow = async (teamId: string, roomId: string, participantId: string): Promise<void> => {
  if (isTauri) {
    try {
      await invoke("create_feedback_window", { teamId, roomId, participantId });
      const windowHandle = await WebviewWindow.getByLabel("feedback");
      if (windowHandle) {
        await windowHandle.setFocus();
      }
    } catch (error) {
      console.error("Failed to create feedback window:", error);
    }
  } else {
    const URL = `feedback.html?teamId=${teamId}&roomId=${roomId}&participantId=${participantId}`;
    window.open(URL);
  }
};

const getUserSettings = async () => {
  return await invoke<{
    call_feedback_popup: boolean;
    show_dock_icon_in_call: boolean;
    start_camera_on_call: boolean;
    start_mic_on_call: boolean;
    noise_cancellation_enabled: boolean;
    screen_share_resolution: "P1080" | "P1440" | "P4K";
    hopp_server_url: string | null;
  }>("get_user_settings");
};

/**
 * Reads the user's "start mic/camera on call" preferences.
 * Falls back to safe defaults (both off) when settings can't be read,
 * so joining a call never fails because of a settings error.
 */
const getCallStartPreferences = async (): Promise<{ startMic: boolean; startCamera: boolean }> => {
  try {
    const settings = await getUserSettings();
    return { startMic: settings.start_mic_on_call, startCamera: settings.start_camera_on_call };
  } catch (error) {
    console.error("Failed to read call start preferences, falling back to defaults:", error);
    return { startMic: false, startCamera: false };
  }
};

const showFeedbackWindowIfEnabled = async (teamId: string, roomId: string, participantId: string): Promise<void> => {
  if (!isTauri) return;

  try {
    const settings = await invoke<{ call_feedback_popup: boolean }>("get_user_settings");
    if (settings.call_feedback_popup) {
      await createFeedbackWindow(teamId, roomId, participantId);
    }
  } catch (error) {
    console.error("Failed to check/show feedback window:", error);
  }
};

export const tauriUtils = {
  closeScreenShareWindow,
  getAvailableContent,
  showWindow,
  closeCameraWindow,
  storeTokenBackend,
  getStoredToken,
  getFavoriteTeammates,
  toggleFavoriteTeammate,
  pruneFavoriteTeammates,
  deleteStoredToken,
  stopSharing,
  endCallCleanup,
  hideTrayIconInstruction,
  setControllerCursor,
  getTokenParam,
  openAccessibilitySettings,
  openMicrophoneSettings,
  openScreenShareSettings,
  openCameraSettings,
  triggerScreenSharePermission,
  getControlPermission,
  getMicPermission,
  getScreenSharePermission,
  getCameraPermission,
  getLastUsedMic,
  setLastUsedMic,
  getLastUsedCamera,
  setLastUsedCamera,
  getSharerDrawPersist,
  setSharerDrawPersist,
  getDrawingEnabled,
  setDrawingEnabled,
  getDrawingHintShown,
  setDrawingHintShown,
  minimizeMainWindow,
  setLivekitUrl,
  getLivekitUrl,
  setSentryMetadata,
  callStarted,
  isFailedStartOfCurrentCall,
  clearFailedCall,
  loadCustomServerUrl,
  setHoppServerUrl,
  setCallFeedbackPopup,
  openSettingsWindow,
  createFeedbackWindow,
  showFeedbackWindowIfEnabled,
  getUserSettings,
  getCallStartPreferences,
};

/**
 * Whether this is the main window in the floating window style (macOS). The window's
 * initialization script sets the class before any app script runs.
 */
export const isFloatingMainWindow = () => document.documentElement.classList.contains("floating-window");

/** Whether this is the main window in the regular (titled) window style (macOS). */
export const isRegularMainWindow = () => document.documentElement.classList.contains("regular-window");
