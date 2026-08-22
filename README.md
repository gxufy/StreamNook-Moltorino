<div align="center">

<img src="src-tauri/images/logo.png" alt="StreamNook Bluzyrino" width="220">

# StreamNook Bluzyrino

### A Windows Twitch desktop client with Bluzyrino chat built in.

Watch streams, use MultiNook and MultiChat, manage Twitch Drops, browse cosmetics, moderate chat, and use Bluzyrino directly inside StreamNook.

[Releases](https://github.com/gxufy/StreamNook-Bluzyrino/releases) ·
[Issues](https://github.com/gxufy/StreamNook-Bluzyrino/issues)

</div>

---

## Getting Started

- Open the [Releases](https://github.com/gxufy/StreamNook-Bluzyrino/releases) page.
- Download the Windows x64 installer or portable ZIP.
- Installer: run `StreamNook-Bluzyrino-...-windows-x64-setup.exe`.
- Portable: extract the ZIP and run `StreamNook.exe`.
- Launch **StreamNook Bluzyrino**.
- Sign into Twitch when prompted.
- Open a Twitch channel and start watching.

Bluzyrino is included with StreamNook Bluzyrino. You do not need to install it separately.

---

# Changing Settings

Click the **gear icon** in StreamNook to open Settings.

Settings are organized into sections for things such as:

- Profile and account
- Interface
- Player
- Chat
- Moderation
- Themes
- Integrations
- Plugins
- Notifications
- Keybindings
- Backup
- Cache
- Command Palette

Your StreamNook settings persist between launches.

## Bluzyrino Settings

Go to:

**Settings → Integrations → Chat Runtime**

This is the main place to control the Bluzyrino integration.

### Embedded Chat

**ON — default**

Bluzyrino is embedded directly inside the StreamNook chat area.

**OFF**

StreamNook uses its native chat instead.

### Open Chat Externally

**ON — default**

Allows the current Twitch channel to be opened in a separate Bluzyrino window.

**OFF**

Disables the external Bluzyrino chat action.

### Sign Into Bluzyrino

Go to:

**Settings → Integrations → Chat Runtime → Account & Settings**

This opens the full Bluzyrino application.

Use the full Bluzyrino window to:

- Sign into Twitch for Bluzyrino.
- Change Bluzyrino appearance.
- Change chat preferences.
- Configure Bluzyrino-specific settings.

Bluzyrino manages its own Twitch login and preferences separately from the StreamNook Twitch account.

Settings changed inside the full Bluzyrino application are also used by the embedded Bluzyrino chat.

---

# Features

## Bluzyrino Integration

- Embedded Bluzyrino Twitch chat.
- Standalone Bluzyrino chat window.
- Automatically follows the focused Twitch channel.
- Separate Bluzyrino Twitch login and preferences.
- Bluzyrino enabled by default.
- Embedded chat hides correctly behind StreamNook full-page overlays.
- Bundled Bluzyrino runtime.

## Twitch Playback

- Native Twitch playback.
- Source-quality playback when available.
- Per-stream quality selection.
- Theater mode.
- Picture-in-picture.
- Live-edge handling.
- VOD playback.
- Offline-channel chat.
- Automatic handling when channels go offline.

## MultiNook

Watch multiple Twitch streams at once.

<div align="center">
<img src="src-tauri/images/multinook.png" alt="MultiNook" width="900">
</div>

- Multi-stream grid.
- Drag and rearrange streams.
- Per-tile quality controls.
- Audio focus.
- Dock and undock streams.
- Multi-monitor support.

## MultiChat

Use multiple chats at the same time.

<div align="center">
<img src="src-tauri/images/multichat.png" alt="MultiChat" width="900">
</div>

- Multiple Twitch chat tabs.
- Split chat layouts.
- Separate MultiChat windows.
- Multiple monitor support.
- Background operation.
- System tray integration.

## Chat

<div align="center">
<img src="src-tauri/images/chat.png" alt="StreamNook Chat" width="900">
</div>

- Twitch chat.
- 7TV emotes.
- BetterTTV emotes.
- FrankerFaceZ emotes.
- Animated emotes.
- Zero-width emotes.
- Emoji.
- Replies and mentions.
- Twitch chat events.
- User profiles.
- Custom commands.
- Command aliases.
- Highlights.
- Custom user colors and nicknames.
- Local commands.

## Moderation

- Ban and timeout.
- Clear chat.
- Mod and unmod.
- VIP tools.
- Moderator menus.
- Moderation logs.
- User information and moderation history.

## Twitch Drops & Channel Points

<div align="center">
<img src="src-tauri/images/drops.png" alt="Drops and Channel Points" width="900">
</div>

- Twitch Drops campaign tracking.
- Drop progress.
- Drops inventory.
- Channel-point bonus claiming.
- Channel-point information.
- Background Drops tools.

## Cosmetics & Badges

<div align="center">
<img src="src-tauri/images/twitch_badges.png" alt="Twitch Badges" width="48%">
<img src="src-tauri/images/badge_details.png" alt="Badge Details" width="48%">
</div>

- Twitch badges.
- 7TV paints.
- 7TV badges.
- Supported Chatterino cosmetics.
- StreamNook badges and ranks.
- Cosmetic search and details.

<div align="center">
<img src="src-tauri/images/7tv_paints.png" alt="7TV Paints" width="48%">
<img src="src-tauri/images/7tv_badges.png" alt="7TV Badges" width="48%">
</div>

## Whispers

- Twitch whispers.
- Conversation history.
- Whisper import tools.
- Separate whisper conversations.

## Themes & Interface

- Multiple built-in themes.
- Custom theme creator.
- Compact layouts.
- Interface customization.
- Multi-monitor layouts.
- Cross-window settings synchronization.

## Power User Features

- `Ctrl+K` Command Palette.
- Desktop notifications.
- Discord Rich Presence.
- System tray support.
- Window position and size persistence.
- Optional update support.
- Custom commands and keybindings.

<div align="center">
<img src="src-tauri/images/command_palette.png" alt="Command Palette" width="700">
</div>

---

# Quick Controls

**Settings**

Click the gear icon.

**Command Palette**

Press `Ctrl+K`.

**Bluzyrino Login / Settings**

`Settings → Integrations → Chat Runtime → Account & Settings`

**Drops & Points**

Open the Drops & Points page from the title bar.

**Marketplace**

Open Marketplace from the title bar.

**Global Cosmetics**

Open Global Cosmetics from the title bar.

---

# Troubleshooting Bluzyrino

If embedded Bluzyrino chat does not appear:

1. Open **Settings → Integrations → Chat Runtime**.
2. Make sure **Embedded chat** is enabled.
3. Click **Account & Settings**.
4. Confirm the full Bluzyrino application launches.
5. Sign into Twitch inside Bluzyrino if needed.
6. Close the full Bluzyrino window.
7. Return to a Twitch channel in StreamNook.

To use StreamNook's original chat instead, disable **Embedded chat**.

---

# Built With

- Rust
- TypeScript
- React
- Tauri
- Tailwind CSS
- HLS.js
- Twitch APIs
- 7TV

---

# Credits

**StreamNook Bluzyrino** is based on the original [StreamNook](https://github.com/winters27/StreamNook) project.

Bluzyrino is integrated and distributed with StreamNook Bluzyrino with permission from its maintainer.

Additional third-party components retain their respective licenses.

See:

- `THIRD_PARTY_NOTICES.md`
- `DISTRIBUTION_NOTES.md`
- `SOURCES.md`
- `licenses/`

for redistribution, attribution, and source information.

---

# License

Repository source code remains subject to the licenses applicable to the original StreamNook and incorporated components.

Bundled third-party software retains its own respective license terms.

See the repository license and third-party notice files for details.

---

<div align="center">

## StreamNook Bluzyrino

https://github.com/gxufy/StreamNook-Bluzyrino

<sub>StreamNook Bluzyrino is not affiliated with Twitch Interactive, Inc.</sub>

</div>