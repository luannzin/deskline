# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

Native desktop app (Rust, egui) with one design language on Windows, macOS and Linux: pointer
and keyboard, resizable window, tray. Recorded as `web` because no per-OS design language is
used; it is not a website.

## Users

Mainly people helping family and friends. The helper is comfortable with computers; the person
being helped often is not. The helper connects to their screen, sees it, and with permission
drives the mouse and keyboard to fix things.

The two roles see different things and the sharing side matters most: it must be dead simple
and obviously safe. A large, easy-to-read code to send, a plain Accept / Decline, and at all
times an unmissable sign of who is connected and how to stop them.

## Product Purpose

Remote desktop with no server for the user to run, good enough to swap AnyDesk out without
anyone having to relearn: paste a code, accept, see and control the other screen, plus the
day-to-day parts (unattended access to your own computers, sign-in screen on Windows, files,
clipboard, sound, chat, several monitors, an address book). Success: the helped person never
needs instructions beyond "open Deskline and send me your code".

## Positioning

Each computer's code is its public key: connections are end-to-end encrypted QUIC dialed by
key (iroh), with no account, no company server in the middle and no ID handed out by one. To
read over the phone, Home shows nine digits the computer picks itself each run; a key made
from them signs a record of the real code onto iroh's public pkarr relay while the window is
open (src/short.rs). Free and open source. One file to run on Windows.

## Operating Context

- The helper and the helped person usually talk on the phone or a chat app while connecting.
- The short code (9 digits) is read out or pasted; the long code (52 characters) shows when
  the short one can't be published (offline) and in Settings for unattended access; Recent
  keeps a name per computer.
- The window lives in the tray while waiting; a request brings it forward.
- Windows can be installed as a service for the sign-in screen and unattended access.

## Capabilities and Constraints

- Platforms: Windows 10/11, macOS 11+, Linux (GNOME, KDE, wlroots, X11).
- Session features: view, control (asked for or granted at accept), Ctrl+Alt+Del, privacy mode
  (Windows), monitor picker, picture quality (Balanced / Sharp text / Slow connection), sound,
  actual size, full screen, files (browse, download, send, drag and drop), chat, several
  sessions in tabs, several viewers per screen.
- Host features: Accept prompt with "Let them control" and "Always allow them", red bar while
  sharing (Chat, Take back control / Give control, Stop sharing), unattended-access password,
  trusted list, session log, relay server setting, start at sign-in, install as service.
- Interface languages: English and Brazilian Portuguese, picked from the system language.
- Rendering: egui immediate mode with its built-in fonts unless a font is embedded on purpose.
- The app may not be run or tested on the maintainer's development machine by the agent; only
  compiled. Visual verification is done by the maintainer by hand.

## Brand Commitments

- Name: Deskline.
- Identity: 32x32 pixel-art mark (a monitor whose screen shows a dusk sky in a 4x4 Bayer
  ordered dither, with a pointer) and the dusk palette #3a2a6b, #7a3361, #e0703f, #ffd180 on a
  near-black night (#110d24), drawn by `tools/logo.py`.
- Family resemblance, binding: luannzin.com (Documents/luannzin.com: DESIGN.md, the dither
  strip, Bayer-dithered dot fields, neutral ink tiers, no accent colour in the interface),
  LanPet (pixel-art icon) and PartyLine (pixel type, ordered dither).
- Voice: Luan's: plain, concrete, first-person when it fits, no marketing tone
  (luannzin.com/IDENTITY.md).

## Evidence on Hand

- Releases on GitHub (v0.8.0 latest) with Windows exe, Linux deb and portable binary, macOS
  dmg and zip.
- Screenshots in `docs/` are of v0.8.0's release exe on Windows (2026-11-20): two copies on one
  machine (`--home` folders, made-up names), the viewer on one monitor showing the other.
- No users, testimonials or usage numbers exist yet. Do not invent them.

## Product Principles

1. The person being helped never has to think: one code to send, one decision to make.
2. Who can see and control this computer is always visible and always stoppable in one click.
3. Nothing reaches a computer without a click, a password or an "always allow" its owner gave.
4. Swapping from AnyDesk costs nothing to relearn: same jobs, findable in the same places.
5. Colour means something; decoration is art from the identity, never chrome.
