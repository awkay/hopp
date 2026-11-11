# 0010 notes: quirks, gotchas and bugs

Discoveries while building clearable call shortcuts (2026-11-10). Agents never start the app, so
the UI and the in-call behavior are unverified until the user runs the spec's manual check; add
what it finds here.

## Bugs fixed along the way

- **Recording another row's keys reset that row to its default.** The conflict loop in
  `commitShortcut` sent `""` to the other row, and `set_shortcut` stored `""` as `None`, which
  `resolve_shortcuts` turns back into the default. When the default was the very combination just
  recorded (record ⌘⇧V for the mic: the camera went back to ⌘⇧V), both rows showed the same keys
  and only one of them fired in a call. The other row now becomes Not set (`"none"`).
- **Only End call was guarded at registration.** `register_call_shortcuts` registered mic, camera
  and screen share unconditionally; only End call had an `is_empty()` check. All four now go
  through one helper that skips a shortcut that isn't set.
- **The recording listener outlived its row.** `ShortcutRow` kept its capture-phase `keydown`
  listener in a ref with no unmount cleanup, so switching Settings sections mid-recording left a
  listener that swallowed every key (`preventDefault` + `stopPropagation`) until a modifier combo
  arrived and was saved to a row no longer on screen. Two rows could also record at once. The
  listener now lives in an effect keyed on `recording`, and the parent owns the one recording row.
- **Call window hints went stale until focus.** The call window's `["user-settings"]` query only
  refetched on window focus, so a shortcut changed in Settings mid-call still showed the old hint.
  The backend now emits `user_settings_changed` after a shortcut change and `CallCenter`
  invalidates the query.

## Gotchas

- `UserSettings.shortcut_*` are `Option<String>` without `#[serde(default)]`; serde still reads a
  missing `Option` field as `None`. `None` means "use the default", so a cleared shortcut needs a
  value of its own: `"none"`. Don't change the field type: any serde failure on
  `AppStateInternal` makes `AppState::new` fall back to defaults and drop every setting and the JWT.
- `get_user_settings` returns resolved settings (defaults filled in), so the Settings window
  compares and shows real combinations, and `"none"` for a cleared one; the frontend helper
  `isShortcutSet` (`core_payloads.ts`) mirrors Rust's `is_shortcut_set`.
- An older Hopp build reading a file with `"none"` keeps loading it (still a string) and tries to
  register `"none"`: global-hotkey 0.7's `parse_key` has no such key and returns `UnsupportedKey`,
  so the old build logs a registration error and the shortcut stays unregistered there too.
- The main window gets `core:default` (event listening) from `capabilities/migrated.json`; a new
  app event needed no capability change.
- `CallCenter` is mounted for the whole call (hidden when another tab is shown), so its listener
  covers the call; outside a call, the query refetches on mount anyway.

## Remaining limitations

- Recording uses `KeyboardEvent.key`, the typed character: ⌥-combinations or ⇧ with a digit
  probably record "å", "@" and the like instead of the key, and then never fire. Out of scope here.
- No frontend test runner in `tauri/`, so the Settings UI and call-window refresh have only the
  manual check; the Rust tests cover storage, normalizing, reset and reload.
