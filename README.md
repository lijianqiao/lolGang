# lolPing-rust

[中文](README_zh.md) | **English**

> A Rust rewrite of [lolPing](https://github.com/darrenprx/lolPing) (by Danrui Meng, MIT license): pings, emotes, teams, the team panel, votes and typing work the way lolPing does, plus hold-to-talk voice, a chat box with history, 6 text effects, and a control panel styled after the League of Legends client.
>
> **This is not an official version of lolPing**: lolPing-rust is an independent project, not affiliated with the author of lolPing, and the two cannot talk to each other. Once installed, it is called **lolPing** in the Start menu and on its windows. It was called lolGang before 3.3.0.

[![Latest release](https://img.shields.io/github/v/release/lijianqiao/lolPing-rust?label=release)](https://github.com/lijianqiao/lolPing-rust/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/lijianqiao/lolPing-rust/total?label=downloads)](https://github.com/lijianqiao/lolPing-rust/releases)
[![CI](https://github.com/lijianqiao/lolPing-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/lijianqiao/lolPing-rust/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/lijianqiao/lolPing-rust?label=license)](LICENSE)

A small League of Legends toy that lives on your desktop: hold Alt and drag to send a League ping anywhere on screen; hold Ctrl and drag for an emote. Team up with friends and your pings, emotes and messages appear at the same spot on their screens, and holding V lets them hear you.

No server needed: on the same LAN it connects directly, otherwise messages go through public relays, encrypted. Built with Tauri 2 (Rust core + React control panel).

## What it looks like

Everything below is what the app actually draws on screen (captured from its transparent window; the background was added afterwards). The control panel screenshots show the Chinese interface; it is available in English too.

**Pings**: Danger, On My Way, Assist Me, Enemy Missing and more, 12 in total, with sounds.

![Four ping animations](.github/readme/pings.gif)

**Emotes**: 44 League of Legends emotes. You can also import your own images; teammates who don't have one get it automatically.

![Three emote animations](.github/readme/emotes.gif)

**Typing**: press Alt + T, type a line, and it pops up on teammates' screens, with 6 text effects such as gold, neon, bounce and shake, or as a scrolling bullet comment.

![Four text effects](.github/readme/text.gif)

**Control panel**: choose which ping or emote goes in each slot of the wheels, and change every key.

<p>
  <img src=".github/readme/panel-pings.png" alt="Control panel: ping wheel" width="49%">
  <img src=".github/readme/panel-emotes.png" alt="Control panel: emote wheel" width="49%">
</p>

## Download

Download from [Releases](https://github.com/lijianqiao/lolPing-rust/releases/latest). The same files are on the [Gitee mirror](https://gitee.com/lijianqiao/lolPing-rust/releases), which is faster in mainland China (since 3.3.0):

| System | File | Status |
|---|---|---|
| Windows 10, 11 | `lolping_<version>_x64-setup.exe` | Supported |
| macOS 13 or later (Apple silicon) | `lolping_<version>_aarch64.dmg` | Experimental, never tested on a real Mac. Allow lolPing to use Accessibility and the microphone in System Settings → Privacy & Security |
| 3.2.0 and earlier | `lolgang_<version>_…` | Called lolGang back then; existing installs update to lolPing automatically and keep their data |

The first time you run it:

- Windows may show "Windows protected your PC": the installer isn't signed with a paid code-signing certificate. Click "More info" → "Run anyway".
- The macOS build isn't notarized by Apple. If it says the developer can't be verified, click "Open Anyway" in System Settings → Privacy & Security.

Once installed, it checks for new versions by itself; click "Update and restart" on the About page to upgrade. The automatic check asks GitHub first and Gitee if GitHub can't be reached; on the About page, "Check for updates" asks GitHub and "Check via Gitee" asks Gitee (since 3.3.1).

**If the original lolPing is installed too**: both apps have the same name, so their Start menu and desktop shortcuts are both called lolPing and are the same file; whichever was installed last wins.
The programs themselves, their data and their start-with-Windows entries don't interfere, and both can run at the same time. To tell them apart, check the publisher in "Installed apps": this project's is lijianqiao.

## How to use it

| What | Default key |
|---|---|
| Ping wheel: hold and drag, release to send the ping you point at | Alt + left-drag |
| Emote wheel | Ctrl + left-drag |
| Hold to talk, release to send to teammates | V |
| Open or close the chat box | Alt + T |
| Hold to show the team panel (teammates, who's online, votes) | Tab |
| Pause or resume all interception | Ctrl + Alt + P |

Change any key on the Shortcuts page of the control panel. Only these gestures are intercepted; other clicks and keys go through to the program underneath as usual. The chat box, talk key and team panel are only intercepted while you're in a team.

- **Teams**: create a team on the Team page and send the team code to your friends. Up to 16 people.
- **Chat box**: the top shows this team's history (text, voice, pings, emotes), the bottom is the input box; voice messages can be replayed. You can also type /ff to surrender, /noff to vote against surrendering and /ready for a ready check. History is stored only on your own computer: leaving the team clears yours, disbanding the team clears everyone's.
- **Voice**: a quick tap of V types the letter as usual; hold it to talk, up to 30 seconds at a time. The Voice page lets you pick a microphone, set the volume and test it.
- **Emotes**: 44 League of Legends emotes, and you can import your own images (animated and with sound both work); teammates who don't have one get it automatically.
- **Votes**: surrender needs 70% yes, a ready check needs everyone, a kick needs more than half.
- **Language**: the interface is available in Chinese and English and follows the system display language by default (Chinese on a Chinese system, English otherwise). Change it on the Features page (since 3.3.2). Teammates using different languages can play together.

Closing the control panel only hides it in the tray; to quit, right-click the tray icon and choose "Quit".

## Network and privacy

- LAN: broadcasts on UDP port 47480. The first time it runs, Windows Firewall will ask; tick "Private networks" and allow it, or teammates on the same LAN won't find you.
- Internet: when not on the same LAN, messages go through public Nostr relays. Everything is encrypted with the team key, so relays only see ciphertext. You can turn off "Allow internet" on the Team page to use the LAN only.
- It doesn't connect to any other server and collects no data. Your identity key is kept in the system's credential store (Windows Credential Manager, macOS Keychain).
- If the app quits unexpectedly, it writes one line to `%APPDATA%\com.lijia.lolgang\crash.log` on your computer (`~/Library/Application Support/com.lijia.lolgang/crash.log` on macOS): the time, the version and where in the code it failed. No keystrokes or chat content, only the last 20 entries are kept, and it is never sent anywhere. If you report "the app just disappeared", please paste it into the issue.

## Development

```bash
pnpm install
pnpm tauri dev                 # run in development
cd src-tauri && cargo test     # Rust tests
pnpm release                   # build a Windows installer locally (no update signature, for trying it yourself)
```

## Star the project

If you like it, give the project a star ⭐ on [GitHub](https://github.com/lijianqiao/lolPing-rust) or [Gitee](https://gitee.com/lijianqiao/lolPing-rust) so more people find it. The app's About page has these two buttons too.

## License and credits

The code is under the [MIT license](LICENSE). The ping wheel, ping animations, emotes, team panel, votes, gesture detection and the macOS input listener are ported from [lolPing](https://github.com/darrenprx/lolPing) (by Danrui Meng, MIT license); ported files say so at the top. Other third-party notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

The ping icons, sounds and emotes come from League of Legends and are owned by Riot Games; they are not covered by the MIT license. lolPing-rust (app name lolPing) isn't endorsed by Riot Games and doesn't reflect the views or opinions of Riot Games.
