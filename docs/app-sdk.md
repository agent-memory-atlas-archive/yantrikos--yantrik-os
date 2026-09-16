# Writing a Yantrik app

An app is a Rust binary with a Slint window. Everything about how it *looks like part of this
OS* — the top edge, the buttons in it, the type, the spacing — comes from the SDK, and not from
anything you write.

That division is deliberate, and it is worth a paragraph on why.

## Why the frame is not yours

This OS shipped sixteen apps, each drawing its own header. Their heights came out 28, 32, 36,
44, 48, 56 and 64 pixels. Download Manager had no app header at all: a row of statistics stood
where every other window says its name. Nobody chose any of that. It is just what happens when
sixteen files each get to decide.

There *were* two shared components, and each was instantiated by exactly zero apps:

| Component  | Why nobody used it |
|------------|--------------------|
| `YToolbar` | Spent Slint's `@children` on the slot *left* of the title, and offered nothing on the right. Every app has trailing buttons, so no app could use it. |
| `AppShell` | Spent its `@children` on nothing. The sidebar and detail panes were empty `Rectangle`s with no way to put anything inside them. |

Both failed the same way. A Slint component gets exactly **one** `@children`. A header wants
three regions — glyph and title, search, actions — so a header built out of child elements has
to drop two of them. Both dropped the one apps actually needed, so all sixteen went and wrote
their own, and "consistency" was left to sixteen authors agreeing on a number. They did not.

A half-usable shared component is worse than none, because it looks like the problem is solved.

## The fix: header content is data

`AppHeader` takes its content as **data**, not as children. You hand it a title, an icon and a
model of `AppAction`; it renders them at its own size, with its own spacing. You cannot make a
header button of a different shape any more than you can make a different mouse cursor. The one
`@children` then goes where arbitrary content genuinely belongs — the body of your app.

```slint
import { AppHeader } from "components/app_header.slint";

VerticalLayout {
    spacing: 0px;

    AppHeader {
        icon: Icons.notes;
        title: root.current-title != "" ? root.current-title : "Notes";
        subtitle: root.folder-name;          // quiet second line
        modified: root.is-modified;          // the amber unsaved dot
        status: root.export-status;          // transient, in the accent colour
        actions: [
            { id: "new",  icon: Icons.plus, label: "New",  emphasis: 2 },
            { id: "save", icon: Icons.save, label: "Save", active: root.is-modified },
            { id: "del",  icon: Icons.trash, disabled: root.selected-index < 0 },
        ];
        action(id) => {
            if (id == "new") { root.new-note(); }
            else if (id == "save") { root.save-note(); }
            else if (id == "del") { root.delete-note(); }
        }
    }

    // ... your app ...
}
```

### `AppAction`

| Field      | Default | Meaning |
|------------|---------|---------|
| `id`       | `""`    | Handed back through `action(id)`. Switch on it. |
| `label`    | `""`    | Empty ⇒ icon-only button. |
| `icon`     | `""`    | A path from the `Icons` global. Empty ⇒ text-only. |
| `emphasis` | `0`     | 0 quiet, 1 raised, 2 primary, 3 danger. |
| `active`   | `false` | A pressed toggle — meeting mode on, sidebar showing. Drawn as primary. |
| `disabled` | `false` | Greyed and unclickable. |
| `loading`  | `false` | Spinner; the click is swallowed. |

Every field defaults to the common case, `emphasis` included. That is not politeness — a wrong
default is how the drift started. In a toolbar the ordinary button is the quiet one, so the
quiet one is what you get for saying nothing. (Note this is deliberately *not* `YButton`'s
numbering, which puts primary at 0. `AppHeader` maps between them in one place.)

### The other slots

| Property           | For |
|--------------------|-----|
| `leading-actions`  | Controls that change **what you are looking at** — view modes, tabs. They sit beside the title. Verbs go on the right. |
| `show-search`      | The standard 240px search field. Bind `search-text` and handle `search-edited` / `search-accepted`. |
| `show-back`        | A leading back arrow with a `back()` callback. It is the frame's job because it must never move. |
| `show-divider`     | The hairline under the bar. On by default. |

### Two rules that are not style preferences

**Disable, do not hide.** An action that needs a selection should be `disabled` when there
isn't one, not removed from the model. A toolbar that reflows as you click around it is the
jumpiest thing a window can do — Notes used to lose seven buttons the moment you deselected a
note.

**Put state in `subtitle`, not in a bar of its own.** Downloads had a 56px row of statistics and
Network had a 56px connection panel, each in place of a header. Both are one line of text under
the app's name now. Same information, at the size the information deserves.

## What the frame does *not* cover

A **ribbon** (Document Editor, Spreadsheet, Presentation) and a **view-control strip** (Image
Viewer's zoom and rotate) are real, distinct patterns and they stay as their own rows *below*
the header. The rule is about what the top edge is, not about forbidding a second row.

Anything genuinely app-specific — a tab strip, a status line, a formula bar — is yours. Draw it
in the body.

## The Rust half

```rust
use yantrik_app_runtime::prelude::*;

slint::include_modules!();

fn main() {
    init_tracing("notes");
    let app = NotesApp::new().unwrap();
    // wire callbacks...
    app.run().unwrap();
}
```

`yantrik-app-runtime` gives you the instance guard, theme plumbing, the companion client and the
control surface (`app.describe` / `app.act`). An app crate depends on it plus
`yantrik-ui-kit` and `yantrik-design-tokens`; `build.rs` puts all three on the Slint include
path. Copy an existing app's `Cargo.toml` and `build.rs` — they are four lines each and
identical across all sixteen.

## Where things live

```
crates/yantrik-ui-kit/slint/         the components. ONE implementation, edit here.
crates/yantrik-ui-slint/ui/components/   three-line re-export shims so the shell can say
                                         "components/x.slint". Do not put code here.
crates/yantrik-ui-slint/ui/<app>.slint   the app's screen. Your markup.
apps/<app>/ui/app.slint                  a thin Window around that screen.
apps/<app>/src/main.rs                   wiring.
```

The shims exist because the shell's own screens import by relative path and the apps import from
the kit's include path. Three components used to be full copies in both places, free to drift
one edit at a time; they are shims now.

## The guard

`cargo test -p yantrik-ui-kit` fails if a shipped app stops using `AppHeader`, if `AppHeader`
stops taking its height from `Theme.h-app-header`, or if `YToolbar` or `AppShell` come back.

Adding an app means adding a line to `APP_SCREENS` in `crates/yantrik-ui-kit/src/lib.rs`. That
is the point: the list of apps whose headers must match is written down, so the next one cannot
quietly be the seventeenth exception.
