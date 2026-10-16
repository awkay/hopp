# Changelog

What's new in each Hopp for Dataico release, newest first.

## 1.0.37 (2026-10-16)

- **Sharper screen sharing with HEVC.** On an M3 or newer Mac, your screen share now uses HEVC, which keeps text sharper on the same bandwidth. It needs everyone watching to be on 1.0.37 or later: if someone can't use HEVC (an older Hopp, the web app, or someone who turned it off), the share switches to H.264 for everyone after a brief freeze, and you keep sharing. Turn it off in Settings › Screen share settings › "Use HEVC for screen sharing".

## 1.0.36 (2026-10-16)

- **Sharing buttons in their own pill.** While you share your screen, mute, draw and stop sharing sit together on a blue pill in the menu bar, at the left end of the icons next to the purple screen-recording indicator. The mic button is new: it mutes and unmutes you. Turn the pill off in Settings › Call settings › "Show sharing buttons in menu bar".
- **Less voice delay.** Other people's audio no longer builds up extra delay during a call.
- **Lighter calls.** Hopp uses less CPU and battery in calls, especially with your camera off, your screen not shared or your mic muted, and its audio holds up better when your Mac is busy.
- **Smoother screen sharing.** The viewer's window shows each new frame right away, so the end of a scroll no longer lags.
- **No crash when you share while your displays sleep.**

## 1.0.35 (2026-10-08)

- **Update button.** When a new version is out, an "Update" tile appears in the sidebar, above your avatar. Click it and Hopp downloads the update and restarts. You stay logged in and keep your settings and permissions. During a call it waits until you hang up. This is the last version you install by hand.
- **What's new.** After an update, a "New" tile in the sidebar shows what changed. The notes are always in the avatar menu › What's new.
- **No more "Automatic updates" toggle in Settings.** Updates are offered by the Update tile instead.

## 1.0.34 (2026-10-08)

- **Sharing buttons in the menu bar.** While you share your screen, the Hopp menu-bar icon grows a draw button and a stop-sharing button next to it. Turn it off in Settings › Call settings › "Show sharing buttons in menu bar".
- **Favorite teammates.** Star a teammate (hover their row) to pin them in a Favorites section at the top of the list.
- **Window style.** Settings › Call settings › Window style: keep the menu-bar popup (default), or use a "Floating window" or "Regular window" that shows in the Dock and Cmd-Tab and works with window managers like AeroSpace. Applies after a restart.
- **New screen effect: Hop.**
- **Local drawing works every time.** Before, it failed on every second toggle.
- **Clicking the menu-bar icon during a call no longer switches AeroSpace workspaces.**
- **Esc closes the menu-bar popup.**
- **Profile opens your profile on our server** (it used to open Hopp's cloud site).
- **"Show settings file in Finder"** link at the bottom of Settings, for debugging.

## 1.0.33 (2026-09-30)

- **Screen effects.** While watching someone's screen, click the new wand button in the screen-share window to send an animated reaction over their screen: thumbs up, bouncing star, confetti, hello, ship it, this is fine, Jerry, raccoon, Indiana and panda flip. Everyone watching sees it, and it's never part of the video. Everyone in the call needs 1.0.33 to see effects.
- **Double-click while drawing** plays the click animation for everyone.
- **Calls connect on slow connections (3G).** Before, every connect attempt timed out.
- **Fewer freezes and wrong hang-ups.** Reworked communication between the app and its screen-sharing process fixes "Hopp is not responding" hangs, and a late hang-up from a previous call can no longer end the call you're in.
- **Lower CPU after a call.** Background work (screen picker, drawing, leftover windows) now stops when you hang up.

## 1.0.32 (2026-09-28)

- **First Hopp for Dataico release.**
