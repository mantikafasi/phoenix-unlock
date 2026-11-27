<div align="center">

# Sunbird

**A cross-platform client modification and plugin framework for Discord Phoenix.**  
*Think Vencord, built natively for Discord's Rust client across mobile and desktop.*

[![Website](https://img.shields.io/badge/website-sunbird.tools-5865F2?style=flat-square&logo=googlechrome&logoColor=white)](https://sunbird.tools/)
[![Discord](https://img.shields.io/discord/917308687423533086?style=flat-square&logo=discord&logoColor=white&label=Discord&color=5865F2)](https://discord.gg/jEmw3bC2m7)
[![License: EUPL-1.2](https://img.shields.io/badge/license-EUPL--1.2-2ea44f?style=flat-square)](LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Android%20%7C%20iOS%20%7C%20Windows%20%7C%20Linux%20%7C%20macOS-313244?style=flat-square)](#install)

[Install](#install) · [Features](#features) · [Plugins](#bundled-plugins) · [Writing Plugins](#writing-a-plugin) · [Building](#building-from-source)

</div>

---

Sunbird extends **Discord Phoenix** (Discord's native Rust client) with a sandboxed TypeScript plugin ecosystem, native in-app settings, custom slash commands, message tools, and theming across **Android, iOS, Windows, Linux, and macOS**.

No root, no Xposed, and on desktop Discord's own installation stays untouched.

## Features

| | |
|---|---|
| **Native Settings UI** | Manage plugins, toggle features, tweak per-plugin settings, edit code, and install plugins from the catalog, a URL, or a local file right inside **Discord -> Settings -> Sunbird**. |
| **26 Built-in Plugins** | Ships with community favorites including FakeNitro, Fix Embeds, Message Logger, Translate, ReviewDB, Platform Indicators, Clear URLs, DeArrow, and more. |
| **Sandboxed & Transparent** | Plugins run in an isolated QuickJS environment with frozen globals, memory/CPU limits, and five explicit permissions (`gateway`, `discord`, `http`, `messages`, `hooks`) shown before you install or update. |
| **Stay Up to Date** | One-tap plugin updates across all platforms, self-updating desktop launchers with optional autostart, and in-app verified Discord updates on Android. |
| **Built for Developers** | Full TypeScript definitions ([`phoenix.d.ts`](crates/runtime/phoenix.d.ts)), an in-app JS console, and desktop DevTools (`Ctrl+Shift+I` / `Cmd+Shift+I`). |

## Install

Visit **[sunbird.tools](https://sunbird.tools/)** or use the steps for your platform below:

| Platform | Support | How to install |
|---|---|---|
| **Windows** (x86-64) | Full | Download and run `SunbirdSetup.exe` from [sunbird.tools](https://sunbird.tools/). Includes a self-updating launcher, onboarding, and optional autostart. |
| **macOS** (Apple silicon) | Full | Run the installer command below in Terminal. |
| **Linux** (x86-64) | Full | Run the installer command below in your terminal. |
| **Android** (9+, arm64 & armv7) | Full | Open [sunbird.tools](https://sunbird.tools/) in your browser to build a ready-to-install APK (`com.discord` or side-by-side `com.discord.phoenix`) with your chosen plugins. |
| **iOS** | Full | Open [sunbird.tools](https://sunbird.tools/) in your browser to prepare an IPA for sideloading (AltStore, Sideloadly, SideStore, etc.). |

**Linux (x86-64):**
```sh
curl -fsSL https://assets.sunbird.tools/plugins/sunbird-linux.sh | sh
```

**macOS (Apple silicon):**
```sh
curl -fsSL https://assets.sunbird.tools/plugins/sunbird-macos.sh | sh
```

Once installed, open **Discord -> Settings -> Sunbird** to enable plugins, customize settings, browse updates, or open the built-in console.

## Bundled Plugins

Sunbird ships with **26 curated plugins** built from [`plugins/`](plugins/) and updated automatically via [assets.sunbird.tools](https://assets.sunbird.tools/):

- **Messaging & Media**: FakeNitro, Fix Embeds, Translate, Clear URLs, DeArrow, Message Logger, Text Replace, Silent Typing, Silent Message Toggle, Spoiler Mode, No Reply Mention, View Raw
- **UI & Profiles**: ReviewDB, Global Badges, Platform Indicators, Permissions Viewer, Show Timeout Duration, No Blocked Messages, No Profile Themes, Hide Composer Buttons, Collapse Channels, Desktop Themes
- **Quality of Life**: TapTap (double-tap to reply/edit), Message Latency, No Haptics, No Pending Count

<details>
<summary>View all 26 bundled plugins</summary>

| Plugin | What it does |
|---|---|
| [`clear-urls`](plugins/clear-urls) | Removes tracking parameters from links you send and receive |
| [`collapse-channels`](plugins/collapse-channels) | Collapses the channel list to the server rail from a header button or shortcut |
| [`dearrow`](plugins/dearrow) | Replaces YouTube titles and thumbnails in embeds using DeArrow |
| [`desktop-themes`](plugins/desktop-themes) | Live `.theme.css` themes on Desktop Phoenix |
| [`fakenitro`](plugins/fakenitro) | Sends custom and animated emojis as links, and renders fake emojis, stickers, and GIFs natively |
| [`fix-embeds`](plugins/fix-embeds) | Rewrites X/Twitter, Instagram, TikTok, and custom links for working embeds |
| [`global-badges`](plugins/global-badges) | Shows user badges from other Discord client mods on profiles |
| [`hide-composer-buttons`](plugins/hide-composer-buttons) | Hides extra buttons in the message bar (Gift, Voice Message, etc.) |
| [`message-latency`](plugins/message-latency) | Flags messages with high send latency |
| [`message-logger`](plugins/message-logger) | Keeps deleted messages and edit history |
| [`no-blocked-messages`](plugins/no-blocked-messages) | Hides messages from blocked (and optionally ignored) users completely |
| [`no-haptics`](plugins/no-haptics) | Disables haptic feedback and vibration across the app |
| [`no-pending-count`](plugins/no-pending-count) | Hides incoming friend request and message request badge counts on desktop |
| [`no-profile-themes`](plugins/no-profile-themes) | Strips profile themes, effects, avatar decorations, and name styles |
| [`no-reply-mention`](plugins/no-reply-mention) | Stops replies from pinging the person you reply to |
| [`permissions-viewer`](plugins/permissions-viewer) | Shows a user's roles and permissions on a server and in a channel |
| [`platform-indicators`](plugins/platform-indicators) | Shows which devices people are online from (desktop, mobile, web, or console) |
| [`reviewdb`](plugins/reviewdb) | Adds a ReviewDB tab to user profiles |
| [`show-timeout-duration`](plugins/show-timeout-duration) | Shows remaining timeout duration beside usernames |
| [`silent-message-toggle`](plugins/silent-message-toggle) | Sends messages without notifying anyone, always or with a prefix |
| [`silent-typing`](plugins/silent-typing) | Prevents Discord from sending typing indicators |
| [`spoiler-mode`](plugins/spoiler-mode) | Adds a toggle to automatically spoiler everything you send |
| [`taptap`](plugins/taptap) | Double-tap a message to reply, or your own message to edit |
| [`text-replace`](plugins/text-replace) | Rewrites outgoing messages using find-and-replace rules |
| [`translate`](plugins/translate) | Translates messages from the message menu or in selected channels via Kagi or Google |
| [`view-raw`](plugins/view-raw) | Opens a message's raw text and JSON in a viewer |

</details>

## Writing a Plugin

Plugins are TypeScript scripts that receive a global `phoenix` API object. See [`crates/runtime/phoenix.d.ts`](crates/runtime/phoenix.d.ts) for the complete API reference and [`plugins/example`](plugins/example/src/index.ts) for a working template.

```ts
// @description Logs incoming messages and adds a /shrug command
// @version 1.0.0
// @author you
// @permissions gateway

phoenix.on(phoenix.gateway.messageCreate, (message) => {
  phoenix.log.info(`<${message.author.username}> ${message.content}`);
});

phoenix.commands.register({
  name: "shrug",
  description: "Sends your text with ¯\\_(ツ)_/¯",
  execute: (args) => `${args} ¯\\_(ツ)_/¯`.trim(),
});
```

Available APIs include `gateway` (event listeners and hooks), `settings` (native in-app settings pages), `storage` (per-plugin JSON persistence), `ui` (buttons, decorations, message actions, profile tabs, dialogs, toasts), `commands` (slash commands), `messages` (local messages and send interceptors), `stores` (read-only cache queries), `fetch`, `discord` (session API and request/response hooks), and `themes`.

Plugins declare only the capabilities they need via `// @permissions` (or `permissions` in `manifest.ts`): `gateway`, `discord`, `http`, `messages`, and `hooks`. Updates that ask for new permissions are flagged before applying.

## Building from Source

<details>
<summary>Repository layout</summary>

| Path | Description |
|---|---|
| `crates/loader` | Native platform entry library for Android, iOS, Windows, Linux, and macOS |
| `crates/runtime` | Shared QuickJS plugin runtime, `phoenix` API, sandbox, and permission enforcement |
| `crates/runtime-mobile` | Android and iOS runtime: native Settings pages, UI extensions, and in-app updates |
| `crates/runtime-desktop` | Desktop runtime: native GPUI Settings, modals, notices, theming, and system tray |
| `crates/inject` | Windows desktop launcher and self-updater (`SunbirdSetup.exe`) |
| `crates/hook`, `crates/sigs`, `crates/sigscan` | Cross-platform integration primitives, build-independent locators, and the `sigscan` CLI |
| `crates/ui-mobile`, `crates/wire` | Mobile UI-op and bridge frame codecs |
| `crates/ui-desktop` | Desktop host utilities (clipboard, URLs, notifications, console) |
| `crates/patcher`, `crates/patcher-web` | Mobile package builder CLI and WebAssembly build for [sunbird.tools](https://sunbird.tools/) |
| `crates/stand-in*` | Standalone test harness for verifying gateway hooks without Discord |
| `plugins/` | Bundled TypeScript plugins |
| `scripts/` | Desktop installers and dev build scripts |
| `web/` | Plugin bundle tooling (`web/tools/bundle-plugins.ts`) and static assets |

</details>

<details>
<summary>Build instructions by platform</summary>

- **Android (Linux with Nix)**:
  ```sh
  nix develop
  nu scripts/phoenix.nu base.apk config.arm64_v8a.apk config.xxhdpi.apk config.en.apk
  ```
  Swap `config.arm64_v8a.apk` for `config.armeabi_v7a.apk` on 32-bit devices.
- **Android (Windows)**: `.\scripts\dev.ps1` (requires `cargo-ndk`, Android SDK/NDK, and a JDK).
- **Windows desktop**: `.\scripts\dev.ps1 -Desktop`, or `cargo build --release -p phoenix-loader -p phoenix-inject`.
- **Linux desktop**: `cargo build --release -p phoenix-loader`.
- **macOS desktop**: `cargo build --release -p phoenix-loader`.
- **iOS**: built on macOS with Xcode for `aarch64-apple-ios` (see [`.github/workflows/ios.yml`](.github/workflows/ios.yml)).
- **Plugins & site assets**: `node web/tools/bundle-plugins.ts` and `nu scripts/site.nu web`.

The website lives in [sunbird-tools/site](https://github.com/sunbird-tools/site). If you're contributing with a coding agent, point it at [AGENTS.md](AGENTS.md) first.

</details>

## Contributors

<a href="https://github.com/sunbird-tools/sunbird/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=sunbird-tools/sunbird" alt="Contributors" />
</a>

---

<sub>Licensed under <a href="LICENSE">EUPL-1.2</a>. Not affiliated with Discord.</sub>
