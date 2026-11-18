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
  ink-soft: "#b2b2b2"
  ink-muted: "#818181"
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
  code-display:
    fontFamily: "Geist Mono"
    fontSize: "20px"
    fontWeight: 400
  mono:
    fontFamily: "Geist Mono"
    fontSize: "14px"
    fontWeight: 400
rounded:
  control: "6px"
  menu: "10px"
  row: "14px"
spacing:
  hair: "4px"
  item: "8px"
  pair: "12px"
  line: "16px"
  gap: "24px"
  block: "40px"
  close: "56px"
components:
  button-primary:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.ground}"
    typography: "{typography.title}"
    rounded: "{rounded.control}"
    padding: "6px 12px"
    height: "32px"
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
  code-well:
    backgroundColor: "{colors.well}"
    textColor: "{colors.ink}"
    typography: "{typography.code-display}"
    rounded: "{rounded.row}"
    padding: "16px 20px"
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
  dusk-strip:
    height: "24px"
  mark:
    size: "32px"
---

# Design System: Deskline

<!-- Recorded from source, not from a render. The app may not be run on the maintainer's development machine, so every value here is read from src/look.rs, src/ui.rs and tools/logo.py; line heights and exact egui widget metrics not set in code are egui 0.3x defaults and are not recorded. Visual verification is by the maintainer, by hand. -->

## Overview

**Creative North Star: "The Quiet Column, Lit by the Line"**

The window is a calm page from luannzin.com (the maintainer's own system, `Documents/luannzin.com/DESIGN.md`, which this one follows): one centred column on a near-black neutral ground, one size of Inter, hierarchy by weight and ink tier, hairlines and rows instead of cards and boxes. The interface recedes; a row lights up as a tile only under the pointer.

Colour is reserved for meaning. Red is a bar that says someone is in this computer, with the way to stop them one click away. Green says you have control. The dusk (violet, plum, ember, gold in a 4x4 Bayer ordered dither) is the one piece of art, taken from the Deskline mark and luannzin.com's footer strip, and it does a job: it is the link between the two computers, drawn denser and warmer the better it is, sweeping while connecting and drifting sparse while reconnecting.

It refuses the remote-desktop category default: an accent-coloured sidebar app with icon tiles and a numeric ID in a big blue box. The code is quiet type in a well; the one filled button is paper ink.

Every string exists in English and Brazilian Portuguese (`src/i18n.rs`); Portuguese runs longer, so lines that pair a field with its button lay out right to left and let the field take whatever width the words leave.

**Key Characteristics:**
- A 560px column, centred, left-aligned inside, scrolling as a whole (Home, Settings); a 360px centred stack for the connecting screen.
- One text size (15px Inter); hierarchy is Inter Medium against Regular and three ink tiers. Code is Geist Mono.
- Neutral greys for all chrome; colour only for sharing (red), control (green) and the link (dusk).
- Hairlines (1px) divide; rows light up as 14px-cornered tiles on hover; nothing is boxed at rest except the code well and the prompts.
- Pixel art drawn by code: the 32px mark at whole-pixel multiples, dither dots snapped to device pixels.

## Colors

A strictly neutral near-black palette with three meanings drawn in colour on top of it.

### Primary
- **Paper Ink** (`ink`): names, titles, the current tab and its 1px underline, the text cursor, file and contact names, and the fill of the one ink button (with Soft Black Ground text on it).

### Secondary
- **Sharing Red** (`sharing-red`): the bar at the top of the window, one per person, while someone sees or controls this computer; also the small always-on-top bar window shown while Deskline is not in front, and the text of the white Stop sharing button on it. It means only that.
- **Control Green** (`control-green`): the words "You have control" in the session toolbar. It means only that.

### Tertiary
- **The Dusk** (`dusk-violet`, `dusk-plum`, `dusk-ember`, `dusk-gold`), sky to horizon: the link meter, the connecting sweep, the strip that closes Home, the screen of the mark. Always as 2px dots on a 3px Bayer grid, never as a fill behind text.
- **Night** (`night`): the ground of the README banner, drawn by `tools/logo.py`. Art only; it never appears in the window.

### Neutral
- **Soft Black Ground** (`ground`): every panel, the top bar, the session toolbar, the side panels.
- **Tile Grey** (`tile`): a row under the pointer, the prompt cards, floating windows and menus.
- **Well Grey** (`well`): the recess behind the code and inside text fields.
- **Hairline** (`hairline`): 1px dividers between Settings sections, the line under the session toolbar, the edge of side panels, prompts, windows, and resting buttons.
- **Raised / Hover / Pressed** (`raised`, `hover`, `pressed`): the three states of a plain button and a disabled ink button's fill (raised); hover also fills file progress bars; pressed is the selection fill.
- **Soft Ink** (`ink-soft`): explanatory lines, tabs not current, quiet buttons, notes.
- **Muted Ink** (`ink-muted`): supporting state ("Unattended access is on"), the link's kind and round trip, key fingerprints, file sizes, "You" and "Them" over chat lines, the version line, the close mark at rest.
- **On Red** (`on-red`): text on the sharing bar and the Stop sharing button's fill; secondary buttons on the bar are this white at 28/255 alpha over the red.

### Named Rules
**The Colour Means Something Rule.** Red is someone in this computer, green is you having control, the dusk is the link. Nothing else is coloured, and none of the three is ever used to decorate.

**The No Accent Rule.** Chrome has no hue. Selection, hover, links and focus all stay on the neutral axis (hyperlinks are ink; selection is the pressed grey with an ink stroke).

**The Dusk Is the Link Rule.** The dusk palette appears only as Bayer-dithered dots, and in the window only where it stands for the connection between two computers (or closes the Home page as luannzin.com's strip does). Its density and warmth carry information; it is never a background.

## Typography

**Body Font:** Inter Regular, embedded (`assets/fonts`, OFL), with egui's built-in fonts as glyph fallback.
**Title Font:** Inter Medium, embedded, the same size.
**Mono Font:** Geist Mono Regular, embedded (OFL), for the code and anything measured.

**Character:** One neutral grotesque at one size, the page reading like a note rather than an app; Geist Mono only where characters must be read exactly.

### Hierarchy
- **Title** (Inter Medium 500, 15px): section heads ("Your code", "Connect to a computer", "Recent", Settings sections), the current tab, prompt questions, the peer's name in the toolbar, the sharing bar's sentence.
- **Body** (Inter 400, 15px): everything else, including buttons, fields, rows, egui's Small style, and the status plate over a frozen picture. Weight and ink tier set it apart, never size.
- **Code Display** (Geist Mono 400, 20px): this computer's code in its well only, big enough to read over the phone, split into three even lines (13 groups as 5, 5, 3) with two spaces between groups.
- **Mono** (Geist Mono 400, 14px): key fingerprints in Recent and Always allowed, the peer code on the connecting screen.

### Named Rules
**The One Size Rule.** All Inter text is 15px. A new head gets Inter Medium and full ink, not a bigger size. Geist Mono is the only family with other sizes: 20px for the code in the well, 14px for fingerprints.

## Layout

Home and Settings share one column: 560px wide (or the window less 16px), centred, its contents left-aligned, the whole page scrolling. Above it the top bar spans the window. The connecting screen is a centred 360px stack, starting at 22% of the window height (never less than 40px). A session is the remote picture full-bleed on black under the session toolbar, with Files (320px) or Chat (300px) docked on the right when open.

Rhythm as built: 40px above the first head and between Home's groups ("Your code", "Connect to a computer", "Recent"); 4px from a head to its field or rows; 8px from the explanatory line to the code well, 12px from the well to its buttons; 56px from the last group to the dusk strip, 24px below it. Settings divides its three sections with a hairline, 24px above and 16px below. Prompts set 12px under the question, 16px before the options, 20px before the buttons. egui item spacing is 8px both ways.

Rows keep their text in line with the column; their hover tile reaches 12px past it on both sides, so nothing shifts when a row lights.

### Named Rules
**The One Column Rule.** Pages are one column; groups are separated by space and hairlines, not by panels or cards.

## Elevation & Depth

Mostly flat and tonal: depth is Tile Grey and Well Grey against the ground, edged by a 1px Hairline. Shadows exist only where egui floats something over the page: floating windows (black at 140/255) and popups and combo menus (black at 120/255), egui's default shadow geometry. Pinned panels, rows, the code well and the prompt cards carry none (the prompt card is a modal and sits over egui's dimmed backdrop). The one plate drawn over the remote picture (the reconnecting status) is black at 200/255 with 10px corners, so it reads over anything.

### Named Rules
**The Hairline Edge Rule.** A container at rest is a tonal fill with a 1px Hairline, or nothing at all. Shadows belong to things that float, never to things on the page.

## Shapes

Small, soft corners in three steps: 6px for controls (buttons, fields, checkboxes, selectable labels), 10px for menus and the status plate, 14px for rows' hover tiles, the code well, the prompt cards and floating windows. No widget expands on hover.

The mark is 32x32 pixel art (a monitor whose screen shows the dusk in Bayer dither, a pointer on it), always drawn at the whole multiple of its pixels nearest the asked size in device pixels, so no row or column doubles. The dither dots are 2px on a 3px grid, both rounded to whole device pixels so they stay crisp at 125% and 150% scaling. The close mark is two 1.5px strokes in a 16px square, never a glyph.

## Components

### Buttons
- **Ink (primary):** Paper Ink fill, Soft Black Ground text in Inter Medium, no stroke, 6px corners, 12px by 6px padding, 32px tall. It is the action that completes its group: Copy code (reads "Copied" in place for 2 seconds), Connect, Accept, Allow, Set password, Log in, Send, Ask for control, Restart to update. Disabled, it drops to Raised fill with Muted Ink text.
- **Plain:** Raised fill, Paper Ink text, 1px Hairline at rest; Hover fill with a Pressed edge under the pointer, Pressed fill while held; 30px tall. For secondary choices: Decline, Deny, Save, Full screen, Install for all users.
- **Quiet:** Soft Ink text with no frame until the pointer is on it, then the hover fill. For Change, Cancel, Disconnect, Turn off, Remove, Download, Up.

### Inputs / Fields
- **Style:** Well Grey fill, Hairline edge, 6px corners, 32px tall, text left and vertically centred, hint in Muted Ink. A field paired with its button sits on one line, the button laid out first from the right.
- **Focus:** egui's text cursor in ink at 1.5px; selection in the pressed grey with an ink stroke.
- **Checkboxes and choices:** egui checkboxes and selectable labels on the same three button states; a chosen selectable label takes the selection fill.

### Navigation
- **Top bar:** on the ground, 16px by 10px padding: the 32px mark (clicking it returns Home), the "Deskline" tab, one tab per session (its name, an unread count after two spaces, then the drawn close mark), Settings at the far end. The current tab is Inter Medium in ink with a 1px ink underline 6px below; the others Soft Ink, with a pointing hand on hover.
- **Session toolbar:** on the ground, 16px by 8px, a hairline under it, wrapping when narrow: the peer's name (title), the link meter, the link's kind and round trip in Muted Ink, the control state, the session tools as selectable labels and combo boxes, and Disconnect as a quiet button at the end.

### Code Well
This computer's code in Well Grey, 14px corners, 20px by 16px padding, full column width: Geist Mono 20px in ink, three even lines. Under it the ink Copy code, the unattended-access state in Muted Ink and a quiet Change.

### Rows
Recent contacts, Always allowed, and file listings. Text in line with the column, 8px above and below; under the pointer a Tile Grey tile with 14px corners reaches 12px past the text each side. Names in ink, fingerprints and sizes in Muted Ink, the row's action as a quiet button at the end.

### Prompts
The Accept and Control questions as egui modals: Tile Grey, 1px Hairline, 14px corners, 24px padding, 380px and 360px wide. The question as a title, the requester, one soft line of advice, the options as checkboxes, then the ink button (Accept, Allow) beside a plain one (Decline, Deny).

### Sharing Bar
A Sharing Red panel at the top of the window, one per person, 16px by 8px padding: "{name} sees your screen" or "sees and controls your screen" in Inter Medium white; at the end a white Stop sharing button with red text, then Take back control or Give control and Chat as white-on-red buttons (white at 28/255). While the window is not in front, the same sentence and Stop sharing in a 560 by 48px undecorated always-on-top window centred 12px from the top of the screen, so it is never behind what the helper opened.

### Link Meter (signature)
The link between the two computers, drawn: a 48 by 15px strip of dusk dots. It reaches as far along as the link is good and grows denser with it (quality from the link's kind, LAN 1.0, Direct 0.9, relay 0.6, less the round trip over 600ms, kept between 0.15 and 1), warming from violet to gold along its length. With no link it shows sparse violet dots drifting, repainted every 120ms, so it reads as searching. Its tooltip is the kind and round trip.

### Waiting Sweep
While connecting (280px wide) and while a folder listing loads (full panel width): a 24px band where a front sweeps left to right every 2.4 seconds and the dusk dots settle in behind it in Bayer order, so the line between the computers is visibly being drawn.

### Dusk Strip
luannzin.com's footer strip closing Home: 24px tall, full column width, four equal blocks of violet, plum, ember and gold at 25, 50, 75 and 100% Bayer density.

### Side Panels
Files and Chat, docked right on the ground with a Hairline edge and 16px padding. Chat: "You" or "Them" in Muted Ink over each line in ink, 8px apart, the field and the ink Send pinned at the bottom. Files: Send files and Send a folder as plain buttons, transfers as progress bars in the hover grey, a hairline, then the folder's name as a title over its rows.

### Identity Assets
`tools/logo.py` (Python and PIL, pixels placed one by one, scaled with nearest neighbour) draws `assets/icon.png` (512), `icon.ico` (16 to 256), `tray.png` and `tray_alert.png` (32) and `banner.png` (1600x420: a field of 3px dots on a 4px grid dithered from night down to the dusk glow, the mark, and the name in a 5x7 pixel face drawn in the script). Nothing is generated by an image model; change the art by changing the script.

## Do's and Don'ts

### Do:
- **Do** keep new pages in the 560px column, separated by 40px and hairlines, with an Inter Medium 15px head 4px above its content.
- **Do** use the three ink tiers: ink for names and what you act on, soft for explanations, muted for supporting state.
- **Do** give each group exactly one ink button, its completing action; everything else is a plain or quiet button.
- **Do** say state in words, and use red, green or the dusk only for the three meanings they carry.
- **Do** draw the dusk as 2px dots on a 3px Bayer grid snapped to device pixels, and the mark at whole multiples of its pixels.
- **Do** draw small marks (close) as strokes in code rather than glyphs.
- **Do** write every string in English and Brazilian Portuguese and let fields, not buttons, absorb the length difference.

### Don't:
- **Don't** add an accent colour, a coloured selection or a coloured link.
- **Don't** raise Inter's size for hierarchy; use weight and ink tier.
- **Don't** box groups in cards or tiles at rest; a tile appears only under the pointer, and the code well and prompts are the only resting containers.
- **Don't** put shadows on anything that sits on the page.
- **Don't** use the dusk as a fill, a gradient or a decoration away from the link, the strip and the mark.
- **Don't** hide the sharing bar or move Stop sharing more than one click away while someone is in.
