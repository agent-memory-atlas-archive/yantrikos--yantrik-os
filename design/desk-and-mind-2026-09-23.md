# The desk and the mind — what the next shell takes from the concept mockups

*23 September 2026. Pranab shared two concept mockups of Yantrik OS and asked to take what is good:
"the file browser, the brightness and sharpness of the UX … the context parts, status … long running
recipes with current status and how it is flowing through different stages/states … reusable agent
arsenal, like agent council … an agent catalog will help to distribute or hand over work."*

The mockups themselves are not in the repo: they carry personal details and other companies' icons.
What follows is what we take from them, in words.

## The idea, in one line

**The OS holds the desk; you bring the mind.** The desktop stays a real desktop. The mind — whichever
one is answering, and every agent working — is always visible at the edge: who it is, what it may do,
what it is doing, and what it did.

## What we take

### 1. Bright and sharp

The mockups read as crisp: near-white primary text, clearly separated surfaces, one-pixel borders
that catch the light, generous radius, and every app with its own colour. Ours reads muddy by
comparison: low-contrast greys on grey.

- A token pass in `theme.slint`: raise text contrast (primary near-white, secondary clearly lighter
  than today), lift card surfaces off the background, a light hairline border on cards, one radius
  scale. Dark stays the default; light gets the same treatment.
- **Colour per app**, used for its icon tile and nowhere else loud: Files blue, Calendar red, Notes
  amber, Terminal green, Mail blue, Browser teal, Studio violet. The taskbar and the launcher use it.
- **No blur.** Slint has no backdrop blur and the VM renders in software. Panels that want glass get
  a pre-blurred copy of the wallpaper behind them, made once when the wallpaper changes.
- Rule kept from before: a card ships only when live data backs it. Nothing on this desk is a
  placeholder number.

### 2. Files

The mockup's file browser, and ours made to match it: a places sidebar (Home, Documents, Downloads,
Pictures, Music, Videos, Projects, Trash), a breadcrumb, large folder tiles with the item count and
when it last changed, a recent-files row under the grid, and grid/list toggles. Folder tiles in the
app's blue. Real counts, real times.

### 3. The mind panel

A right-hand panel, collapsible to a slim strip (so it costs nothing over a working app), expanded
on the home screen:

- **Now:** the answering mind and its model; the mode and the machine's ceiling.
- **Context:** the active project and workspace (`describe shell` already has `active_project`), the
  memory store and how many minds share it.
- **Working:** agents that are running or waiting for the person, from the Agents store; the
  recipes in flight, each with its current stage.
- **Recent actions:** the last acts, with their time and outcome — read from the mind audit today,
  from the ledger (#148) when it lands. Never invented.
- A strip state that pulses when an agent needs the person.

This replaces the machine rail's companion section rather than adding a second one.

### 4. Recipes, flowing

The companion already has a recipe engine (`crates/yantrik-companion/src/recipe.rs`,
`recipe_executor.rs`): typed steps — Tool, Think, JumpIf, WaitFor, Notify, AskUser, ThinkCited,
Validate, Render, Branch, and the data steps — a status per recipe (pending, running, waiting, done,
failed) and per step, and a tick-driven executor. Nothing shows it.

A **Recipes** screen: every recipe, running first; each drawn as its stages left to right, the
current one lit, done ones ticked, a failed one red with its error, a waiting one saying what it
waits for (a person's answer draws the question and its choices right there). Live while it runs.
The mind panel shows the one-line version.

### 5. The agent catalog, and handing work off

A **catalog** of reusable agents, each a role rather than a mind:

| field | meaning |
|---|---|
| `id`, `name`, `purpose` | what it is for, in one line — "reviews a change for bugs, reads only" |
| `mind` | which attached mind runs it by default (a fallback list) |
| `brief` | its standing instructions |
| `reach` | what it may touch: surfaces and a grade ceiling **narrower** than the machine's (a reviewer is `safe`, a coder may ask for `sensitive`) |
| `returns` | the shape of what it hands back |
| `budget` | turns and minutes before it stops and reports |

Shipped roles to begin with: **Researcher**, **Planner**, **Coder**, **Reviewer**, **Red team**,
**Writer**, **Chair** (synthesises), **Scribe** (summarises). Defaults live in the image; the person's
own go in `~/.config/yantrik/agents/`, editable from the Agents view.

**Handing off** is starting a catalog agent on a task: `shell.hand_off {agent, task, context?, wait?}`.
The person does it from the Agents view (New agent → from the catalog); a mind does it through the
bridge. It is gated like `new_agent` in the Agents design: sensitive, depth one, no inherited grants,
and the catalog agent's `reach` caps it further.

### 6. Formations — the arsenal

A **formation** is a recipe whose steps are catalog agents. One new recipe step does it:
`Agent { role, prompt, store_as }` — send a turn to a catalog agent and keep its answer. Then:

- **Council** — three roles answer the question independently; the Chair reads all three, names
  where they agree and where they do not, and gives a verdict. (What Claude does by hand with GPT-6
  red-teams, as a button.)
- **Red team** — the author proposes, the Red team attacks, the author revises; two rounds.
- **Build** — Planner → Coder → Reviewer, the Reviewer's findings looping back once.
- **Writers' room** — a showrunner's beats, cast roles write the lines. The Cluster episode
  "Allow Once" was made this way by hand on 23 September; this makes it a template.

Each formation runs visibly: a row per agent in the Agents view, and the whole flow in the Recipes
screen.

## Order

| # | Piece | Needs |
|---|---|---|
| 1 | Bright and sharp token pass; app colours; Files redesign | — |
| 2 | Mind panel | Agents store (#162), mode file (#152); ledger (#148) later |
| 3 | Agents glue: events and commands into the store as reported/verified cards, approvals in the agent's pane, `describe shell` → `agents`, `new_agent / send_to_agent / stop_agent`, "open in Agents" from the Lens, notifications | #158, #159, #162, #163 |
| 4 | Recipes screen, read side, live | the companion's RecipeStore |
| 5 | Catalog, `hand_off`, the `Agent` recipe step, the four formations | 3 and 4 |
