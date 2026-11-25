---
version: 1
slug: "src-ui-rs"
primary_target: "src/ui.rs"
related_targets: ["src/look.rs"]
---

Scope: the whole Deskline window (src/ui.rs, drawn with src/look.rs). Mode: Operate.

Audience and job: the person being helped (often not technical) sends their code and answers one question; the helper connects and works. Sharer side must be dead simple and obviously safe. English and Brazilian Portuguese.

Constraints: native egui app; the maintainer forbids running or testing it on the dev machine, so there are no captures; visual checks are by hand.

## Direction contract

THESIS: The window is a calm page from luannzin.com: one column, one size of Inter, hierarchy by weight and ink tier, hairlines and rows instead of cards. Refuses the category default: an accent-coloured sidebar app with icon tiles and a 9-digit ID in a big blue box.

OWN-WORLD: Ground #161616, tile #1f1f1f, well #1c1c1c (fields only), hairline #242424; ink #f5f5f5 / soft #b2b2b2 / muted #888888. Inter 15px regular and medium. Radii 6 controls, 10 menus, 14 rows and prompts. Nothing boxed at rest but fields and prompts; keyboard focus always shows in ink. The only filled button on a screen is paper ink with ground text. The 32px pixel mark, always at a whole multiple of its pixels. Colour only means: red bar = someone is in this computer, green = you have control, the dusk (#3a2a6b #7a3361 #e0703f #ffd180, 2px dots on a 3px Bayer grid) = the link between computers.

STORY: The helped person reads their code aloud straight off the page, copies it with the one ink button, gets one plain question, and from then on a red bar names who is in and stops them in one click: in the window, and while the window is not in front, in a small bar above every other window. The helper pastes, watches the dusk line being drawn while waiting, then works with the link's health always visible as dusk density.

FIRST VIEWPORT: Top bar on ground: mark 32px, "Deskline" tab, session tabs, Settings at the right; current tab ink with a 1px ink underline. A 560px column, scrolled by the whole window: "Your code" (medium) over one soft line, the code as bare type at 24px Geist Mono: nine digits on one line (the long code, when they can't be published, in three even lines of 5, 5, 3 groups); "Copy code" (ink button) beside the unattended-access state and a quiet Change. 40px below, "Connect to a computer": field and the ink Connect on one line. Then Recent rows, each one click to connect, and nothing after them.

FORM: Pinned by the user's brief (luannzin.com system plus the Deskline pixel/dusk identity); no concept roll. Seed key: none (pinned). Signature move: the link drawn as dusk dither, its density and warmth the connection's quality, sweeping while connecting, drifting sparse while reconnecting.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
