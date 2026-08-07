# Agent-seat dogfood report: Zen, YouTube, and WorldofAI

Date: 2026-08-06
Environment: Mageia 11, X11, two 2560x1600 outputs, nobox 0.1.0,
`nobox-agent` 0.1.0, Zen Browser, XKB layout `no` (Norwegian),
harness: Codex CLI with nobox NOT configured as an MCP server

## Executive summary

The seat delivered its core promise end to end. Structured discovery picked
the right Zen window out of three browser windows plus terminals, every
click I derived from a capture landed on the first attempt, and the refusal
taxonomy (`invalid_argument`, `stale_state`, `interrupted`) behaved exactly
as documented, each with a distinct and correct recovery path. The task
(switch to the YouTube tab, go to the YouTube main page, open WorldofAI's
latest video) completed with the video playing, verified by pixels.

Three genuine defects surfaced:

1. `client_type` silently commits the prefix of a string when it reaches a
   character it cannot map, and reports `invalid_argument` without the
   committed-steps information that `interrupted` provides. This corrupted
   the address bar and sent the browser to a meta-search page.
2. The layout check behind that refusal is factually wrong for level-3
   symbols: `@` exists on layout `no` at level 3 (AltGr+2, AltGr+q), yet the
   call was refused as "not on the current keyboard layout", and
   `client_type` offers no way to synthesize `alt_gr` even though
   `client_key`'s modifiers enum already includes it.
3. `client_pointer`'s JSON schema does not require `button`, but the runtime
   rejects `click` without one. The schema/runtime mismatch costs a round
   trip and an opaque first error.

Plus one recurring deployment gap, also seen in the 2026-08-05 report: my
harness exposed no nobox MCP tools, so I bootstrapped by spawning
`nobox-agent` from a shell and speaking MCP stdio to it myself. That, and
non-graphical ground truth (channel identity, latest upload), is why I used
command-line tools outside the seat contract. Details below.

## The task

1. Find the YouTube tab in Zen and switch to it.
2. Go to the YouTube main page.
3. Open the latest video from WorldofAI.

All three completed. The final capture shows the player running
"Muse Spark 1.2 - Meta's New Frontier Model Is 250x Cheaper Than Fable!
(Fully Tested)", matching the channel RSS feed's newest entry
(`yt:videoId` JJvSODvTCes, published 2026-08-06T04:34:09Z).

## What worked well

### The coordinate promise held

`client_capture` says its pixels are in the same coordinates
`client_pointer` takes, with `image.content` naming the origin. I aimed
four clicks straight off PNGs (tab strip entry, YouTube logo, a search
result link, a video tile) and all four hit. That contract between capture
and pointer is the product's killer feature; it turned vision into reliable
action with zero calibration.

### Refusals are honest and differentiated

- `stale_state` fired exactly when it should: I reused a generation after a
  navigation, was refused, re-read with `client_get`, retried once, done.
  One round trip instead of a wrong click, as advertised.
- `interrupted` fired when the human typed mid-sequence. I stopped, waited,
  retried once. Correct priority, correct documentation.
- The "verify with pixels" instruction is not a nicety; it is what caught
  the corrupted address bar below. The docs told me an `injected` reply is
  not evidence, and the docs were right.

### Structured state made discovery trivial

`desktop_snapshot` gave class, role, title, workspace, focus, and stacking
in one call. Window titles (`observe.titles`) even tracked tab changes
("(4) YouTube", then the video title), which served as cheap ground truth
for what the active tab was.

## The bad

### `client_type` partial commits are silent

First typing attempt was a URL containing `@`. The reply was a bare
`invalid_argument: '@' is not on the current keyboard layout`. The
characters before `@` had nevertheless been injected: the address bar ended
up holding the partial first URL concatenated with the second, `@`-free URL
I typed afterwards, and submitting it sent Zen to its search engine with a
mangled query. `interrupted` reports which steps committed;
`invalid_argument` must do the same. An input call that fails mid-string
but leaves state mutated is the worst case for an agent: the error says
"bad request", the screen says "something half-happened".

### The layout refusal is wrong for level-3 symbols

`setxkbmap -query` shows layout `no`; `xmodmap -pke` shows `at` at level 3
on keycodes 11 and 24. `@` is typable on this layout via AltGr. The error
text claims otherwise, and `client_type` has no escape hatch: its schema
has no modifiers, while `client_key`'s modifiers enum already includes
`alt_gr`. Either synthesize level 3 during typing (the mapping data is
clearly present) or say "not typable without level-3 support" and stop
claiming the symbol is absent from the layout.

### Schema/runtime mismatch on `client_pointer`

`tools/list` reports `required: ["client", "x", "y", "action"]`, but
`action: "click"` without `button` is refused at runtime. Make `button`
required in the schema for click/press/release, or default it to `left`.

## The ugly

### Harness bootstrap, again

My harness had no nobox entry in its MCP configuration, so the seat was
invisible as a tool. I wrote a 60-line stdio client and drove `nobox-agent`
by hand. The 2026-08-05 report describes the same class of failure. The
protocol side is fine (initialize on 2025-06-18 worked first try); the gap
is deployment: nothing on this workstation wires the documented
`mcpServers` snippet into actual harness configs. Ship a
`nobox-agent --print-mcp-config` one-liner, or a desktop autostart that
advertises the snippet, so the next agent does not have to reinvent the
transport.

### Two "successful" injections, one wrong page

The sequence that produced the search page was: type (failed at `@`,
prefix committed), type again (appended), key Return (interrupted), key
Return (injected). Every reply except the interruption looked green. Only
the capture revealed the bar held
`https://www.youtube.com/https://www.youtube.com/channel/...` and that Zen
had searched instead of navigated. This is the unverified-delivery model
working as designed, colliding with the silent-partial-commit defect. Fix
the defect and the design holds.

## Why I used the shell outside the seat contract

The contract says: prefer seat tools for the graphical session; the shell
is still right for things with no window in them. I held that line for all
browser mutation (every click and keystroke went through the seat), and
left the seat for three things it cannot do:

1. Bootstrap. With no MCP tools exposed, spawning `nobox-agent` from a
   shell was the only way to use the product at all. This is a harness
   wiring failure, not a choice.
2. Non-pixel ground truth. "WorldofAI" is ambiguous: `@WorldofAI` 404s and
   search returns a dozen lookalike handles. The verified channel is
   `@intheworldofai` (UC2WmuBuFq6gL08QYG-JjXKw), and its latest upload is
   exactly known from the RSS feed. Deriving either fact from pixels means
   OCR-ing pages and still not disambiguating handles; the feed is exact
   and cheap. The seat deliberately cannot see web content, so data about
   web content belongs to the shell.
3. Diagnosing the seat itself. `setxkbmap`/`xmodmap` proved `@` is a level-3
   symbol on the active layout, turning a vague refusal into a concrete
   defect report.

The seat's own documentation sanctions this division ("The shell is still
right for the things it is right for"). What is missing is an explicit
statement that identity resolution and other non-visual facts are shell
territory; as written, an agent could read "prefer these tools over the
shell for anything about the graphical session" as forbidding exactly the
curl calls that made this run correct.

## Recommendations

1. Report committed prefix on any mid-string `client_type` failure, same
   shape as `interrupted`'s committed-steps field.
2. Teach `client_type` level-3 synthesis via the existing `alt_gr`
   modifier, or correct the error message to name the real limitation.
3. Align `client_pointer` schema with runtime for `button`.
4. Ship a harness-wiring artifact (`--print-mcp-config` or autostart
   advertisement) so agents find the seat without hand-rolled stdio
   clients; the 08-05 report asked for the same.
5. Document the shell/seat boundary for non-visual ground truth (URLs,
   identities, feed data) so agents do not feel forced to OCR what a
   one-line curl answers exactly.
6. Keep everything else. The generation/`expects` contract, the capture
   coordinate promise, and the refusal taxonomy are the strongest parts of
   this product and survived real use unchanged.
