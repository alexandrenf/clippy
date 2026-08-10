# Clippy — a polished capture companion for AI work

[![Release](https://img.shields.io/github/v/release/alexandrenf/clippy)](https://github.com/alexandrenf/clippy/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/alexandrenf/clippy/total)](https://github.com/alexandrenf/clippy/releases)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Build](https://github.com/alexandrenf/clippy/actions/workflows/build.yml/badge.svg)](https://github.com/alexandrenf/clippy/actions)

**Clippy** is a local, private capture panel for saving selected AI answers,
links, notes, and follow-up prompts without leaving your current app. It keeps
the useful parts of a to-do list, clipboard, and scratchpad one shortcut away.

> **Fork and credits:** Clippy is a fork of
> [Cooper by TouchMyBar](https://github.com/TouchMyBar/cooper), an open-source
> recreation inspired by shadcn's Copper app. Full credit goes to the Cooper
> maintainers and contributors for the cross-platform foundation. This fork
> keeps that workflow, adds macOS reliability and visual polish, and renames
> the app to suit my preference. Clippy is not affiliated with Cooper's
> maintainers, shadcn, Copper, or Microsoft.

**Local-first and private.** Everything lives in a SQLite file on your Mac.
Telemetry is off, and an account is needed only when you enable the optional
end-to-end encrypted iPhone sync.

<p align="center">
  <img src="docs/screenshot-dark.png" width="330" alt="Clippy dark theme — empty state with capture shortcuts" />
  <img src="docs/screenshot-glass.png" width="330" alt="Clippy glass theme — captured items with inline markdown formatting" />
</p>

## Download

Grab the latest build from **[Releases](https://github.com/alexandrenf/clippy/releases/latest)**:

| Mac | File to download |
| --- | --- |
| **Apple Silicon** (M-series) | `Clippy_x.y.z_aarch64.dmg` |

First-run notes:

- **macOS:** Right-click → *Open* the unsigned app the first time, or run
  `xattr -dr com.apple.quarantine /Applications/Clippy.app`. Grant
  **Accessibility** permission for double-shift capture.

## How it works

- Select text anywhere → **double-tap Left Shift** → captured.
- Think of a follow-up prompt → **double-tap Right Shift** → Clippy appears.
- Send items back into your AI apps and check them off as you go.

## Shortcuts

| Action | Shortcut |
| --- | --- |
| Capture selected text (any app) | `Left Shift` + `Left Shift` |
| Show / hide Clippy (any app) | `Right Shift` + `Right Shift` |
| Show / hide (fallback) | `Ctrl/Cmd` + `Shift` + `Space` |
| Capture (fallback) | `Ctrl/Cmd` + `Alt/Option` + `C` |
| Switch / create list | `Ctrl/Cmd` + `K` |
| Copy selected · Copy as list | `Ctrl/Cmd` + `C` · `Ctrl/Cmd` + `Shift` + `C` |
| Mark as done / Edit / Delete | `Space` / `Enter` / `Del` |
| Shortcut sheet | `Ctrl/Cmd` + `/` |

## Features

- **Capture panel** — frameless, always on top, and summoned from the keyboard.
- **Lists** — type `## Title` to group prompts, switch with `Ctrl/Cmd+K`, and use
  each list's `•••` menu to rename it, move its prompts to Inbox, or delete it.
- **File attachments** — drag files onto the composer or use `+`; Clippy keeps a
  private local copy with the prompt. Paste copied images directly into the
  composer, drag selected text into the same box, then copy or drag prompts and
  files back into another app.
- **Mixed selection** — select prompts and individual images together with
  `Cmd`-click, extend a range with `Shift`-click, or select the visible list with
  `Cmd+A` before copying or dragging it elsewhere.
- **Settled prompts** — completed work collapses into a compact block at the
  bottom. Clear Settled follows the current scope: Inbox, one list, or All.
- **Quiet capture feedback** — Double Shift shows a brief click-through preview
  when the panel is closed, without taking focus from the app you're using.
- **Local-first** — no registration is required for the Mac app; optional
  end-to-end encrypted iPhone sync is enabled from Settings with an in-app
  browser sign-in.
- **Agent access** — an optional local MCP companion lets configured agents read
  lists and, when you explicitly allow it, create or update lists and todos.
- **Custom shortcuts** — record your preferred global Show and Capture hotkeys
  in Settings; Double Shift capture stays available.
- **Inline formatting** — supports bold, italic, strikethrough, and code.
- **Copy as List** — sends selected items and their attached files back out as a
  numbered prompt list.
- **Themes** — System, Light, Dark, and a translucent Glass theme.
- **Tray app** — closes to the tray, supports start-at-login, and uses a
  monochrome macOS template icon that adapts to the menu-bar appearance.
- **Portable data** — one SQLite file plus one-click Markdown export.

## Building from source

Prerequisites: an Apple Silicon Mac, [Rust](https://rustup.rs), Node 18+, and the
[Tauri 2 platform prerequisites](https://tauri.app/start/prerequisites/).

```sh
npm install
npm run tauri dev
npm run tauri -- build --target aarch64-apple-darwin
```

Clippy uses a Rust core and React UI in the operating system webview—no
Electron.

## Platform notes

- Clippy reads the focused app's selected text through the native
  Accessibility API first, without touching the clipboard. Apps that do not
  expose selection use a copy/pasteboard compatibility path that preserves
  non-text clipboard data when capture is empty.

## Data

- Database: `clippy.db` in
  `~/Library/Application Support/app.clippy.desktop`.
- On first launch, Clippy copies an existing Cooper database into the new
  location when possible, leaving the original untouched.
- `⋯ → Export to Markdown` writes `clippy-export.md` to Documents.

## License and upstream

[Apache-2.0](LICENSE). Clippy preserves Cooper's license and contributor
credit. For the original project, its history, and upstream releases, visit
[TouchMyBar/cooper](https://github.com/TouchMyBar/cooper).
