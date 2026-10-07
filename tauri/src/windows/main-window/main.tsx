import "@/services/sentry";
/**
 * Core JS polyfills to allow for compatibility with Safari
 * on cases like conditional spreading of elements in Array.
 * (Example in Sidebar.tsx)
 */
import "core-js/actual/iterator";
import "../../App.css";
import { OS } from "@/constants";
import { disableWebViewAppNap } from "@/lib/utils";
import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { QueryProvider } from "@/services/query";
import App from "./app";
import { Toaster } from "react-hot-toast";
import { PostHogProvider } from "posthog-js/react";
import { PostHogConfig } from "posthog-js";
import { BOTTOM_ARROW, POSTHOG_API_KEY, POSTHOG_HOST } from "@/constants";
import { typedInvoke } from "@/core_payloads";
import { listen } from "@tauri-apps/api/event";
import posthog from "posthog-js";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isFloatingMainWindow } from "@/windows/window-utils";

const options: Partial<PostHogConfig> = {
  api_host: POSTHOG_HOST,
  // Commenting out until we figure out WTF is going on
  // api_host: "https://webhook.site/4ce330a4-4bb3-497c-9cfe-997515e9093b",
  // autocapture: false,
  loaded: async function (ph) {
    if (import.meta.env.MODE == "development") {
      ph.opt_out_capturing(); // opts a user out of event capture
      ph.set_config({ disable_session_recording: true });
    } else {
      try {
        const settings = await typedInvoke("get_user_settings");
        if (!settings.telemetry_enabled) {
          ph.opt_out_capturing();
          ph.set_config({ disable_session_recording: true });
        }
      } catch {
        // telemetry enabled by default
      }
    }
  },
};

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: true,
    },
  },
});

if (BOTTOM_ARROW) {
  document.body.className = "arrow_bottom";
} else {
  document.body.className = "arrow";
}

if (OS === "macos") {
  disableWebViewAppNap();
}

/**
 * Floating window style: Escape hides the main window, unless something else uses it: a
 * menu, popover or dialog (Radix marks the event handled when it closes one, and any open one
 * also counts), a focused text field, or an IME composition.
 */
function hideOnEscape(event: KeyboardEvent) {
  if (event.key !== "Escape" || event.defaultPrevented || event.isComposing) return;
  if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
  const active = document.activeElement;
  if (active instanceof HTMLElement && (active.isContentEditable || active.closest("input, textarea, select"))) {
    return;
  }
  const openLayer = document.querySelector(
    '[data-radix-popper-content-wrapper], [role="dialog"], [role="alertdialog"], [role="menu"], [role="listbox"]',
  );
  if (openLayer) return;
  getCurrentWindow().hide();
}

if (isFloatingMainWindow()) {
  window.addEventListener("keydown", hideOnEscape);
}

listen<boolean>("telemetry_enabled_changed", (event) => {
  if (event.payload) {
    posthog.opt_in_capturing();
  } else {
    posthog.opt_out_capturing();
    posthog.set_config({ disable_session_recording: true });
  }
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <PostHogProvider apiKey={POSTHOG_API_KEY} options={options}>
      <QueryClientProvider client={queryClient}>
        {/* Custom type-safe provider */}
        <QueryProvider>
          <Toaster
            position="bottom-right"
            toastOptions={{
              duration: 1_500,
              loading: { duration: Infinity },
            }}
          />
          <App />
        </QueryProvider>
      </QueryClientProvider>
    </PostHogProvider>
  </React.StrictMode>,
);
