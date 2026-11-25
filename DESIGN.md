---
name: Deskline
description: Remote desktop as a calm page from luannzin.com, with the link between two computers drawn in dusk.
colors:
  ground: "#161616"
  tile: "#1f1f1f"
  well: "#1c1c1c"
  hairline: "#242424"
  raised: "#262626"
  hover: "#323232"
  pressed: "#404040"
  ink: "#f5f5f5"
  ink-hover: "#dedede"
  ink-pressed: "#c8c8c8"
  ink-soft: "#b2b2b2"
  ink-muted: "#888888"
  sharing-red: "#9b2c2c"
  on-red: "#ffffff"
  control-green: "#5ac878"
  dusk-violet: "#3a2a6b"
  dusk-plum: "#7a3361"
  dusk-ember: "#e0703f"
  dusk-gold: "#ffd180"
  night: "#110d24"
typography:
  title:
    fontFamily: "Inter Medium, Inter"
    fontSize: "15px"
    fontWeight: 500
  body:
    fontFamily: "Inter"
    fontSize: "15px"
    fontWeight: 400
  prose:
    fontFamily: "Inter"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: "22px"
  code-display:
    fontFamily: "Geist Mono"
    fontSize: "24px"
    fontWeight: 400
  mono:
    fontFamily: "Geist Mono"
    fontSize: "14px"
    fontWeight: 400
rounded:
  control: "6px"
  focus: "8px"
  menu: "10px"
  row: "14px"
spacing:
  hair: "4px"
  item: "8px"
  pair: "12px"
  line: "16px"
  gap: "24px"
  block: "40px"
components:
  button-primary:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.ground}"
    typography: "{typography.title}"
    rounded: "{rounded.control}"
    padding: "6px 12px"
    height: "32px"
  button-primary-hover:
    backgroundColor: "{colors.ink-hover}"
  button-primary-active:
    backgroundColor: "{colors.ink-pressed}"
  button-primary-disabled:
    backgroundColor: "{colors.raised}"
    textColor: "{colors.ink-muted}"
  button:
    backgroundColor: "{colors.raised}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "6px 12px"
    height: "30px"
  button-hover:
    backgroundColor: "{colors.hover}"
  button-active:
    backgroundColor: "{colors.pressed}"
  button-quiet:
    textColor: "{colors.ink-soft}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "6px 12px"
  button-quiet-hover:
    backgroundColor: "{colors.hover}"
  focus-ring:
    textColor: "{colors.ink}"
    rounded: "{rounded.focus}"
    width: "2px"
  text-field:
    backgroundColor: "{colors.well}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    height: "32px"
  tab:
    textColor: "{colors.ink-soft}"
    typography: "{typography.body}"
  tab-current:
    textColor: "{colors.ink}"
    typography: "{typography.title}"
  top-bar:
    backgroundColor: "{colors.ground}"
    padding: "10px 16px"
  session-toolbar:
    backgroundColor: "{colors.ground}"
    padding: "8px 16px"
  row:
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.row}"
    padding: "8px 12px"
  row-hover:
    backgroundColor: "{colors.tile}"
  code:
    textColor: "{colors.ink}"
    typography: "{typography.code-display}"
  prompt:
    backgroundColor: "{colors.tile}"
    textColor: "{colors.ink}"
    rounded: "{rounded.row}"
    padding: "24px"
  menu:
    backgroundColor: "{colors.tile}"
    textColor: "{colors.ink}"
    rounded: "{rounded.menu}"
  side-panel:
    backgroundColor: "{colors.ground}"
    padding: "16px"
    width: "320px"
  sharing-bar:
    backgroundColor: "{colors.sharing-red}"
    textColor: "{colors.on-red}"
    typography: "{typography.title}"
    padding: "8px 16px"
  sharing-bar-stop:
    backgroundColor: "{colors.on-red}"
    textColor: "{colors.sharing-red}"
    rounded: "{rounded.control}"
  sharing-bar-window:
    backgroundColor: "{colors.sharing-red}"
    width: "560px"
    height: "48px"
  link-meter:
    width: "48px"
    height: "15px"
  mark:
    size: "32px"
---

# Design System: Deskline

<!-- Recorded from source, not from a render. The app may not be run on the maintainer's development machine, so every value here is read from src/look.rs, src/ui.rs and tools/logo.py; line heights and exact egui widget metrics not set in code are egui 0.37 defaults and are not recorded. Visual verification is by the maintainer, by hand. -->

## Overview

**Creative North Star: "The Quiet Column, Lit by the Line"**

The window is a calm page from luannzin.com (the maintainer's own system, `Documents/luannzin.com/DESIGN.md`, which this one follows): one centred column on a near-black neutral ground, one size of Inter, hierarchy by weight and ink tier, hairlines and rows instead of cards and boxes. The interface recedes; a row lights up as a tile only under the pointer.

Colour is reserved for meaning. Red is a bar that says someone is in this computer, with the way to stop them one click away. Green says you have control. The dusk (violet, plum, ember, gold in a 4x4 Bayer ordered dither) is the one piece of art, taken from the Deskline mark, and it does a job: it is the link between the two computers, drawn denser and warmer the better it is, sweeping while connecting and drifting sparse while reconnecting. It appears nowhere else in the window.

It refuses the remote-desktop category default: an accent-coloured sidebar app with icon tiles and a numeric ID in a big blue box. The code is type, set large on the page with nothing around it; the one filled button is paper ink.

Every string exists in English and Brazilian Portuguese (`src/i18n.rs`); Portuguese runs longer, so lines that pair a field with its button lay out right to left and let the field take whatever width the words leave, and lines of words and buttons wrap rather than run out of the window.

**Key Characteristics:**
- A 560px column, centred, left-aligned inside, at least 16px from the window's edges; the whole window scrolls it (Home, Settings), each page keeping its own place. A 360px centred stack, also scrolling, for the connecting screen.
- One text size (15px Inter); hierarchy is Inter Medium against Regular and three ink tiers. The code is Geist Mono 24px.
- Neutral greys for all chrome; colour only for sharing (red), control (green) and the link (dusk).
- Nothing is boxed at rest except text fields and the prompts. Hairlines (1px) divide; rows light up as 14px-cornered tiles on hover.
- Keyboard focus is always a visible ink mark: a 2px ring on custom controls, an ink edge on egui's own.
- Pixel art drawn by code: the 32px mark at whole-pixel multiples, dither dots snapped to device pixels.

## Colors

A strictly neutral near-black palette with three meanings drawn in colour on top of it.

### Primary
- **Paper Ink** (`ink`): names, titles, the current tab and its 1px underline, the code, the text cursor, file and contact names, the focus ring, and the fill of the one ink button (with Soft Black Ground text on it). Under the pointer the ink button drops to `ink-hover`, pressed or focused to `ink-pressed`.

### Secondary
- **Sharing Red** (`sharing-red`): the bar at the top of the window, one per person, while someone sees or controls this computer; also the small always-on-top bar window shown while Deskline is not in front, and the text of the white Stop sharing button on it. It means only that.
- **Control Green** (`control-green`): the words "You have control" in the session toolbar. It means only that.

### Tertiary
- **The Dusk** (`dusk-violet`, `dusk-plum`, `dusk-ember`, `dusk-gold`), sky to horizon: the link meter, the connecting sweep, the folder-listing sweep, the screen of the mark. Always as 2px dots on a 3px Bayer grid, never as a fill behind text.
- **Night** (`night`): the ground of the README banner, drawn by `tools/logo.py`. Art only; it never appears in the window.

### Neutral
- **Soft Black Ground** (`ground`): every panel, the top bar, the session toolbar, the side panels.
- **Tile Grey** (`tile`): a row under the pointer or with focus, the prompt cards, floating windows and menus.
- **Well Grey** (`well`): the recess inside text fields.
- **Hairline** (`hairline`): 1px dividers between Settings sections, the line under the session toolbar, the edge of side panels, prompts, windows, fields and resting plain buttons.
- **Raised / Hover / Pressed** (`raised`, `hover`, `pressed`): the three states of a plain button and a disabled ink button's fill (raised); hover also fills file progress bars; pressed is the selection fill.
- **Soft Ink** (`ink-soft`): explanatory lines, field labels, tabs not current, quiet buttons, notes.
- **Muted Ink** (`ink-muted`): supporting state ("Unattended access is on"), the link's kind and round trip, key fingerprints, file sizes, "You" and "Them" over chat lines, the version line, field hints, the close mark at rest. Measured 5.10:1 on the ground, 4.65:1 on the tile, 4.81:1 in the well.
- **On Red** (`on-red`): text on the sharing bar and the Stop sharing button's fill; the bar's other buttons are this white at 28/255 alpha over the red (52 under the pointer, 76 pressed).

### Named Rules
**The Colour Means Something Rule.** Red is someone in this computer, green is you having control, the dusk is the link. Nothing else is coloured, and none of the three is ever used to decorate.

**The No Accent Rule.** Chrome has no hue. Selection, hover, links and focus all stay on the neutral axis (hyperlinks are ink; selection is the pressed grey with an ink stroke; focus is ink).

**The Dusk Is the Link Rule.** The dusk palette appears only as Bayer-dithered dots, and in the window only where it stands for the connection between two computers. Its density and warmth carry information; it is never a background or a page ornament.

## Typography

**Body Font:** Inter Regular, embedded (`assets/fonts`, OFL), with egui's built-in fonts as glyph fallback.
**Title Font:** Inter Medium, embedded, the same size.
**Mono Font:** Geist Mono Regular, embedded (OFL), for the code and anything measured.

**Character:** One neutral grotesque at one size, the page reading like a note rather than an app; Geist Mono only where characters must be read exactly.

### Hierarchy
- **Title** (Inter Medium 500, 15px): section heads ("Your code", "Connect to a computer", "Recent", Settings sections), the current tab, prompt questions, the peer's name in the toolbar, the sharing bar's sentence.
- **Body** (Inter 400, 15px): everything else, including buttons, fields, rows, egui's Small style, and the status plate over a frozen picture. Weight and ink tier set it apart, never size.
- **Prose** (Inter 400, 15px, 22px line height): explanations that can run past one line (under "Your code", Unattended access, Relay server, the installed note, the prompts' advice), so a paragraph reads as one.
- **Code Display** (Geist Mono 400, 24px): this computer's code on Home, big enough to read over the phone: the nine digits on one line, or, when they couldn't be published, the long code split into three even lines (13 groups as 5, 5, 3), with two spaces between groups. In a window too narrow for the lines at 24px it steps down, whole pixels at a time, until they fit; a line never wraps.
- **Mono** (Geist Mono 400, 14px): key fingerprints in Recent and Always allowed, the peer code on the connecting screen, the link's round trip in milliseconds (so the toolbar doesn't shift as the digits change).

### Named Rules
**The One Size Rule.** All Inter text is 15px. A new head gets Inter Medium and full ink, not a bigger size. Geist Mono is the only family with other sizes: 24px for the code, 14px for fingerprints and measurements.

## Layout

Home and Settings share one column: 560px wide (or the window less 32px), centred, its contents left-aligned. The scroll area fills the window under the top bar, so the wheel works anywhere on the page and the bar sits at the window's edge; Home, Settings and each connecting screen keep their own scroll position. The connecting screen is a centred 360px stack, starting at 22% of the window height (never less than 40px), and scrolls when its password form doesn't fit. A session is the remote picture full-bleed on black under the session toolbar, with Files (320px) or Chat (300px) docked on the right when open.

Rhythm as built: 40px above the first head and between Home's groups ("Your code", "Connect to a computer", "Recent"), and 40px after the last; 4px from a head to its field or rows; 16px from the explanation to the code and from the code to its buttons (8px of space plus egui's 8px item spacing). Settings divides its three sections with a hairline, 24px above and 16px below; its password fields each carry a soft label above them. Prompts set 12px under the question, 16px before the options, 20px before the buttons. egui item spacing is 8px both ways.

Rows keep their text in line with the column; their hover tile reaches 12px past it on both sides, so nothing shifts when a row lights.

Lines that mix words and buttons (Copy code with the unattended-access state, the update notice, Install for all users, the password buttons) wrap when the window or the language runs out of room. Prompts narrow with the window (never under 160px) instead of being cut off.

### Named Rules
**The One Column Rule.** Pages are one column; groups are separated by space and hairlines, not by panels or cards.

## Elevation & Depth

Mostly flat and tonal: depth is Tile Grey and Well Grey against the ground, edged by a 1px Hairline. Shadows exist only where egui floats something over the page: floating windows (black at 140/255) and popups and combo menus (black at 120/255), egui's default shadow geometry. Pinned panels, rows and the prompt cards carry none (the prompt card is a modal and sits over egui's dimmed backdrop). The plate drawn over the remote picture (reconnecting, a captured mouse's way out, files being dropped or pasted; one at a time, in that order) is black at 200/255 with 10px corners, so it reads over anything.

### Named Rules
**The Hairline Edge Rule.** A container at rest is a tonal fill with a 1px Hairline, or nothing at all. Shadows belong to things that float, never to things on the page.

## Shapes

Small, soft corners in four steps: 6px for controls (buttons, fields, checkboxes, selectable labels), 8px for the focus ring around a 6px button (concentric, 2px out), 10px for menus and the status plate, 14px for rows' hover tiles, the prompt cards and floating windows. No widget expands on hover.

The mark is 32x32 pixel art (a monitor whose screen shows the dusk in Bayer dither, a pointer on it), always drawn at the whole multiple of its pixels nearest the asked size in device pixels, so no row or column doubles. The dither dots are 2px on a 3px grid, both rounded to whole device pixels so they stay crisp at 125% and 150% scaling. The close mark is two 1.5px strokes across a 7px square in a 24px target, never a glyph.

## Components

### Buttons
- **Ink (primary):** Paper Ink fill, Soft Black Ground text in Inter Medium, no stroke, 6px corners, 12px by 6px padding, 32px tall; `ink-hover` under the pointer, `ink-pressed` pressed or focused. It is the action that completes its group: Copy code (reads "Copied" in place for 2 seconds), Connect, Accept, Allow, Set password, Log in, Send, Ask for control, Restart to update. Disabled, it drops to Raised fill with Muted Ink text.
- **Plain:** Raised fill, Paper Ink text, 1px Hairline at rest; Hover fill with a Pressed edge under the pointer, Pressed fill with an ink edge while held or focused; 30px tall. For secondary choices: Decline, Deny, Save, Install for all users, Send files.
- **Quiet:** Soft Ink text with no frame until the pointer is on it, then the hover fill. For Change, Cancel, Disconnect, Turn off, Remove, Rename, Forget, Undo, Download, Up.
- **Session tools:** in the session toolbar every tool (Ctrl+Alt+Del, the screen and picture menus, Sound, Actual size, Full screen, Files, Chat) is ink words with no frame until the pointer is on it; a tool that is on takes the selection fill. Only Ask for control is filled.

### Focus
Keyboard focus always shows in ink. Ink buttons and the red bar's buttons get a 2px ring 2px outside them (ink, or white on the red); a focused row, tab or close mark gets a 2px ink ring; egui's own buttons, checkboxes and menus take the pressed fill with an ink edge; a focused field takes an ink edge and the cursor. The mark in the top bar takes clicks but not focus: the Deskline tab beside it does the same job from the keyboard.

### Inputs / Fields
- **Style:** Well Grey fill, Hairline edge, 6px corners, 32px tall, text left and vertically centred. A field paired with its button sits on one line, the button laid out first from the right.
- **Labels:** every field has visible words that stay when you type: the group's head ("Connect to a computer", "Relay server") or a soft label above it (the password fields). Hints in Muted Ink are only examples, never the label.
- **Focus:** an ink edge and egui's text cursor in ink at 1.5px; selection in the pressed grey with an ink stroke.
- **Checkboxes and choices:** egui checkboxes and selectable labels on the same three button states; a chosen selectable label takes the selection fill.

### Navigation
- **Top bar:** on the ground, 16px by 10px padding: the 32px mark (clicking it returns Home), the "Deskline" tab, one tab per session (its name, an unread count after two spaces, then the drawn close mark), Settings at the far end. The current tab is Inter Medium in ink with a 1px ink underline 6px below; the others Soft Ink, with a pointing hand on hover.
- **Session toolbar:** on the ground, 16px by 8px, a hairline under it, wrapping when narrow: the peer's name (title), the link meter, the link's kind in Muted Ink and its round trip in Muted Mono, the control state, the session tools, and Disconnect as a quiet button at the end.

### Code
This computer's code set directly on the page, nothing around it: Geist Mono 24px in ink, the nine digits on one line (the long code, when shown instead, in three even lines), selectable. Under it the ink Copy code, the unattended-access state in Muted Ink and a quiet Change, wrapping when narrow.

### Rows
Recent contacts, Always allowed, and file listings. Text in line with the column, 8px above and below; under the pointer (or with focus) a Tile Grey tile with 14px corners reaches 12px past the text each side. Names in ink, fingerprints and sizes in Muted Ink, the row's actions at the end.
- **Recent:** the whole row connects on click (pointing hand); at its end Connect in ink words, then Rename and Forget as quiet buttons. Connect is the keyboard's way in, so the row itself stays out of the Tab order. Forget takes effect at once and leaves a soft line under the list, "{name} is no longer in Recent." with a quiet Undo, until you leave the page.
- **Files:** every row takes focus. A folder opens on a click or Enter; a file downloads on a double click or Enter. The name in ink (a folder's ends in "/") is cut with "…" rather than wrapped; a file's size sits at the end in Muted Mono. The quiet Download appears at the end of the row under the pointer or with focus only, and the row keeps its height either way. An empty folder says so, and a Find that matches nothing says that.

### Mac Permissions (Home)
On a Mac, under the code and its buttons, 24px down: "Two things to allow on this Mac" as a title, one prose line on why, then a line each for Screen Recording and Accessibility, laid out like a Recent row at rest (the name as a title over what it lets the other person do in Muted Ink) with the answer at the end: **Allow** while not asked this run (macOS's own question), **Open System Settings** after it (the exact pane), "Allowed" in Muted Ink once it is. Only the next thing to allow is the ink button; the other is plain. When Screen Recording was asked and still isn't allowed, a soft line says macOS shows the change after a restart, with a quiet **Restart Deskline**. The answers are read again once a second while the page shows; once both are allowed the whole group is gone.

### Report a Problem (Settings)
The last Settings section, after a hairline: the title, one prose line on where each button sends it, "What happened?" as a soft label over a four-row multiline field, then one Muted Ink line saying exactly what goes with it (Deskline's version, the system, on Windows installed or portable, on a Mac what it allows) and what never does. The ink **Open on GitHub** (a new issue with the form filled in) and a plain **Copy** (reads "Copied" for 2 seconds), both disabled while the field is empty. An error's note on Home carries a quiet **Report this**, which opens this section with the error already written and the cursor after it.

### Prompts
The Accept and Control questions as egui modals: Tile Grey, 1px Hairline, 14px corners, 24px padding, 380px and 360px wide (narrower in a small or zoomed window). The question as a title, the requester, one soft line of advice, the options as checkboxes, then the ink button (Accept, Allow) beside a plain one (Decline, Deny).

### Sharing Bar
A Sharing Red panel at the top of the window, one per person, 16px by 8px padding. The buttons sit at the end: a white Stop sharing with red text, then Take back control or Give control and Chat as white-on-red buttons. "{name} sees your screen" or "sees and controls your screen" in Inter Medium white takes the room that's left and wraps onto a second line rather than run under the buttons. While the window is not in front, the same sentence and Stop sharing in a 560 by 48px undecorated always-on-top window centred 12px from the top of the screen, so it is never behind what the helper opened; there the sentence ends in "…" if it doesn't fit, whole on hover.

### Link Meter (signature)
The link between the two computers, drawn: a 48 by 15px strip of dusk dots. It reaches as far along as the link is good and grows denser with it (quality from the link's kind, LAN 1.0, Direct 0.9, relay 0.6, less the round trip over 600ms, kept between 0.15 and 1), warming from violet to gold along its length. With no link it shows sparse violet dots drifting, repainted every 120ms, so it reads as searching. Its tooltip is the kind and round trip.

### Waiting Sweep
While connecting (280px wide) and while a folder listing loads (full panel width): a 24px band where a front sweeps left to right every 2.4 seconds and the dusk dots settle in behind it in Bayer order, so the line between the computers is visibly being drawn.

### Side Panels
Files and Chat, docked right on the ground with a Hairline edge and 16px padding. Chat: "You" or "Them" in Muted Ink over each line in ink, 8px apart, the field and the ink Send pinned at the bottom. Files (360px): at the top the way to the folder as a trail, "Their computer › / › home › luan", each earlier part a quiet button that opens it and the folder you're in as a title, with a quiet Refresh at the end; under it the starting places (Home folder, Desktop, Documents, Downloads) as quiet words; then Find, a field with its soft label beside it, and a hairline. The folder's rows fill the middle and scroll. Pinned at the bottom under a hairline: Send files (plain) and Send a folder (quiet), which say "here" when the folder on show takes them, a muted line that files can be dropped on the window, then each transfer as a line (its name in ink, cut with "…"; at the end a Muted Mono percentage and a quiet Cancel while it runs, or Done, Failed with why on hover, and a quiet Show for what came here) over a 2px line, Hairline filled in Soft Ink as far as it's got. Files dragged over the window, and a paste waiting on its files, say so on the plate over the picture.

### Identity Assets
`tools/logo.py` (Python and PIL, pixels placed one by one, scaled with nearest neighbour) draws `assets/icon.png` (512), `icon.ico` (16 to 256), `tray.png` and `tray_alert.png` (32) `banner.png` (1600x420: a field of 3px dots on a 4px grid dithered from night down to the dusk glow, the mark, and the name in a 5x7 pixel face drawn in the script) and `preview.png` (1200x630, the same field with the glow below the centred mark, the name and one line: the link preview of deskline.luannzin.com, copied into the `luannzin/lines` site). Nothing is generated by an image model; change the art by changing the script.

## Do's and Don'ts

### Do:
- **Do** keep new pages in the 560px column, inside `column()` with their own scroll id, separated by 40px and hairlines, with an Inter Medium 15px head 4px above its content.
- **Do** use the three ink tiers: ink for names and what you act on, soft for explanations and labels, muted for supporting state.
- **Do** give each group exactly one ink button, its completing action; everything else is a plain or quiet button.
- **Do** say state in words, and use red, green or the dusk only for the three meanings they carry.
- **Do** show keyboard focus on everything that takes it, in ink.
- **Do** let lines of words and buttons wrap (`horizontal_wrapped`) unless the line is a field and its button.
- **Do** put a visible label above any field whose group head doesn't already name it.
- **Do** draw the dusk as 2px dots on a 3px Bayer grid snapped to device pixels, and the mark at whole multiples of its pixels.
- **Do** draw small marks (close) as strokes in code rather than glyphs.
- **Do** write every string in English and Brazilian Portuguese and let fields, not buttons, absorb the length difference.

### Don't:
- **Don't** add an accent colour, a coloured selection or a coloured link.
- **Don't** raise Inter's size for hierarchy; use weight and ink tier.
- **Don't** box anything at rest: no cards, no wells, no tiles; a tile appears only under the pointer or with focus, and fields and prompts are the only resting containers.
- **Don't** put shadows on anything that sits on the page.
- **Don't** use the dusk as a fill, a gradient, a footer or a decoration away from the link and the mark.
- **Don't** hide the sharing bar or move Stop sharing more than one click away while someone is in.
- **Don't** make something forgettable vanish for good in one click: give it an Undo.
