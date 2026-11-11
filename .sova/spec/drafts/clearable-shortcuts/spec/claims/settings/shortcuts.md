# §settings/shortcuts Settings › Shortcuts

The Shortcuts section of the Settings window lists four call shortcuts: Mute / unmute mic, Toggle
camera, Toggle screen share and End call. Each one is either a key combination or Not set. While
the user is in a call, Hopp listens system-wide for each shortcut that is set and does its action;
a shortcut that is Not set does nothing and leaves its keys to other apps.

## §settings.shortcuts/clear Clearing a shortcut

While a shortcut is set, a small × clear button is always visible next to its field, outside the
field's box, to its right. Clicking it makes that shortcut Not set: nothing fires for it in calls,
and its keys work in other apps again. The × is the only way to clear a shortcut. A cleared
shortcut stays Not set, also after Hopp restarts, until the user records a new one or restores the
defaults.

## §settings.shortcuts/not-set Not set display

A shortcut that is Not set shows "Not set" in grey in its field, and no × button; the four fields
stay aligned whether or not their shortcut is set.

## §settings.shortcuts/recording Recording a shortcut

Clicking a field starts recording: it shows "Recording..." until the user presses a key with ⌘,
Ctrl or ⌥ held, and that combination becomes the shortcut. A key pressed without one of those
modifiers is ignored, Backspace and Delete included: they never clear the shortcut. Esc cancels
recording and keeps the old value. Recording works the same for a field that is Not set. Only one
field records at a time: starting to record one stops any other. Leaving the Shortcuts section,
clicking an × or clicking Restore defaults stops recording.

## §settings.shortcuts/conflicts Recording keys another action uses

When the user records a combination that another call shortcut already uses, that other shortcut
becomes Not set, so no two actions share a combination.

## §settings.shortcuts/restore-defaults Restore defaults

A "Restore defaults" link under the four rows puts all four shortcuts back to their defaults in one
click, with no confirmation: ⌘⇧A for the mic, ⌘⇧V for the camera, ⌘⇧S for screen share and ⌘⇧W to
end the call.

## §settings.shortcuts/in-call Changes during a call

Clearing, recording or restoring shortcuts during a call takes effect at once: shortcuts that are
set work immediately, and cleared ones stop. The call window's mic, camera, screen share and End
call buttons show their shortcut in their hover hint only while it is set, and the hints update as
soon as a shortcut is cleared, recorded or restored, without the call window needing focus.

## §settings.shortcuts/saved-settings Saved settings

Settings files saved by earlier versions keep loading with their shortcuts unchanged, and a
shortcut missing from the file uses its default. A cleared shortcut is saved in the settings file
as `"none"`. An empty or cleared shortcut never makes startup or the Settings window fail.

## §settings.shortcuts/typed-symbol-limitation Known limitation: typed symbols

Recording reads the typed character, not the physical key, so a combination with ⌥, or ⇧ with a
digit, probably records the symbol the keys type ("å", "@") and then never fires in a call. Not
handled yet.
