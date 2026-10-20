//! The stack's model: where each window sits, how long ago it was used, and what the caption
//! says. No Wayland and no Slint in here, so every rule is a test.
//!
//! labwc owns the order and the selection (the switcher session, `zlabwc_window_switcher_
//! renderer_v1`); this only draws what it is told. A card's depth is how far it is behind the
//! selected window in labwc's own order, so Tab, which moves labwc's selection, moves the stack.

/// One window of a session, as labwc sent it. `app_id` and `title` come from the window's own
/// client: untrusted text, cleaned by [`clean`] before anything draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub id: u32,
    pub app_id: String,
    pub title: String,
    pub workspace: String,
    pub current_workspace: bool,
    pub minimized: bool,
    pub focused_at_start: bool,
    /// Milliseconds since the window last had focus; 0 when labwc never saw it focused.
    pub idle_ms: u32,
}

/// How many cards are drawn: the selected one and four behind it. The rest are counted.
pub const VISIBLE: usize = 5;

/// Where a card sits and how faded it is, in the 1280×800 frame the design was drawn in; the
/// caller scales it to the output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// 1.0 for the selected card, fading with depth.
    pub alpha: f32,
    pub depth: usize,
}

/// The card at depth `d`: each step back is 15.5% smaller, up and to the right, and fainter.
pub fn place(d: usize) -> Place {
    let k = 0.845f32.powi(d as i32);
    Place {
        x: 150.0 + 96.0 * d as f32,
        y: 214.0 - 37.0 * d as f32,
        w: 640.0 * k,
        h: 400.0 * k,
        alpha: [1.0, 0.8, 0.6, 0.42, 0.28][d.min(4)],
        depth: d,
    }
}

/// How far `index` is behind `selected` in an `n`-window list.
pub fn depth(index: usize, selected: usize, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    (index + n - selected % n) % n
}

/// Unicode's format characters (general category Cf): bidi overrides and isolates, zero-width
/// spaces and joiners, the soft hyphen, word joiners, the BOM, interlinear annotation marks and
/// tag characters. None draws anything a person can see, and each can make one title look like
/// another or turn text around, so a title loses them all.
pub fn is_format(c: char) -> bool {
    matches!(c,
        '\u{00ad}' | '\u{0600}'..='\u{0605}' | '\u{061c}' | '\u{06dd}' | '\u{070f}' | '\u{0890}'..='\u{0891}'
        | '\u{08e2}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{110bd}' | '\u{110cd}'
        | '\u{13430}'..='\u{1343f}' | '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0001}'
        | '\u{e0020}'..='\u{e007f}')
}

/// A title or app id fit to draw: control and format characters (bidi, zero-width) dropped,
/// whitespace runs made one space, and at most `max` characters, ending in "…" when cut.
pub fn clean(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in s.chars() {
        if c.is_control() || is_format(c) {
            if c.is_whitespace() || c == '\n' || c == '\t' {
                space = !out.is_empty();
            }
            continue;
        }
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    if out.chars().count() > max {
        let cut: String = out.chars().take(max.saturating_sub(1)).collect();
        return format!("{}…", cut.trim_end());
    }
    out
}

/// What the window is called on its card: its title, or its app id when it has none.
pub fn name(w: &Window) -> String {
    let title = clean(&w.title, 120);
    if title.is_empty() {
        let app = clean(&w.app_id, 60);
        if app.is_empty() {
            "Untitled window".into()
        } else {
            app
        }
    } else {
        title
    }
}

/// The tile's letter: the first letter or digit of the name, upper case.
pub fn letter(w: &Window) -> String {
    name(w).chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "·".into())
}

/// A stable colour slot (0..8) for a window's tile, from its app id or, for the shell's own apps
/// (which send no app id), its title's first word, so a window keeps its colour as it ages.
pub fn tile_slot(w: &Window) -> usize {
    let key = if w.app_id.trim().is_empty() {
        name(w).split_whitespace().next().unwrap_or("").to_lowercase()
    } else {
        w.app_id.to_lowercase()
    };
    let mut h: u32 = 2166136261;
    for b in key.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    (h % 8) as usize
}

/// How long ago, said plainly: "now", "4 min ago", "1 h ago", "2 h 15 min ago". `None` when
/// labwc never saw the window focused.
pub fn ago(idle_ms: u32) -> Option<String> {
    if idle_ms == 0 {
        return None;
    }
    let min = idle_ms / 60_000;
    Some(match min {
        0 => "now".into(),
        1..=59 => format!("{min} min ago"),
        _ if min.is_multiple_of(60) => format!("{} h ago", min / 60),
        _ => format!("{} h {} min ago", min / 60, min % 60),
    })
}

/// The time line runs from now (top) to two hours ago (bottom) on a log scale, so the last few
/// minutes, where most switching happens, get most of the room. Older than two hours sits in the
/// "earlier" band at the very bottom. Returns 0.0 (now) to 1.0 (earlier).
pub fn timeline_pos(idle_ms: u32) -> f32 {
    let min = idle_ms as f32 / 60_000.0;
    if min > 120.0 {
        return 1.0;
    }
    0.92 * (1.0 + min).log10() / 121f32.log10()
}

/// Nudge positions apart so no two labels on the time line are closer than `gap`: each keeps its
/// order and moves down only as far as it must. Returned in the input's order.
pub fn spread(ys: Vec<f32>, gap: f32) -> Vec<f32> {
    let mut order: Vec<usize> = (0..ys.len()).collect();
    order.sort_by(|&a, &b| ys[a].partial_cmp(&ys[b]).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = ys.clone();
    let mut last: Option<f32> = None;
    for i in order {
        let y = match last {
            Some(prev) if ys[i] < prev + gap => prev + gap,
            _ => ys[i],
        };
        out[i] = y;
        last = Some(y);
    }
    out
}

/// The time line's marks: label and position.
pub fn ticks() -> Vec<(&'static str, f32)> {
    vec![("now", 0.0), ("1 min", timeline_pos(60_000)), ("10 min", timeline_pos(600_000)), ("1 h", timeline_pos(3_600_000)), ("earlier", 1.0)]
}

/// The window type-to-jump goes to: the first, from the selected one onwards in labwc's order,
/// whose name or app id has a word starting with `typed` (case ignored). None when nothing
/// matches or nothing was typed.
pub fn jump(windows: &[Window], selected: usize, typed: &str) -> Option<usize> {
    let typed = typed.trim().to_lowercase();
    if typed.is_empty() || windows.is_empty() {
        return None;
    }
    let n = windows.len();
    (0..n).map(|i| (selected + i) % n).find(|&i| {
        let w = &windows[i];
        let hay = format!("{} {}", name(w), clean(&w.app_id, 60)).to_lowercase();
        hay.split(|c: char| !c.is_alphanumeric()).any(|word| word.starts_with(&typed)) || hay.starts_with(&typed)
    })
}

/// The caption's second line: where the window is and when it was last used.
pub fn whereabouts(w: &Window) -> String {
    let mut parts = Vec::new();
    let app = clean(&w.app_id, 40);
    if !app.is_empty() && !name(w).to_lowercase().contains(&app.to_lowercase()) {
        parts.push(app);
    }
    let ws = clean(&w.workspace, 40);
    parts.push(if w.current_workspace { "this desktop".to_string() } else if ws.is_empty() { "another desktop".into() } else { ws });
    if w.minimized {
        parts.push("minimized".into());
    }
    if let Some(a) = ago(w.idle_ms) {
        parts.push(if a == "now" { "in use".into() } else { format!("used {a}") });
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(id: u32, title: &str, idle_ms: u32) -> Window {
        Window {
            id,
            app_id: String::new(),
            title: title.into(),
            workspace: "Workspace 1".into(),
            current_workspace: true,
            minimized: false,
            focused_at_start: false,
            idle_ms,
        }
    }

    #[test]
    fn the_selected_card_is_in_front_and_the_rest_step_back_in_labwcs_order() {
        assert_eq!(depth(2, 2, 5), 0);
        assert_eq!(depth(3, 2, 5), 1);
        assert_eq!(depth(1, 2, 5), 4, "the one before the selection is the deepest");
        let (front, back) = (place(0), place(4));
        assert!(back.w < front.w && back.x > front.x && back.y < front.y && back.alpha < front.alpha);
        assert_eq!(front.w, 640.0);
    }

    #[test]
    fn a_title_cannot_draw_controls_or_turn_text_around() {
        assert_eq!(clean("Bank\u{202e}moc.live\u{1b}[2J\n  x", 80), "Bankmoc.live[2J x");
        assert_eq!(clean("  spaced \t out  ", 80), "spaced out");
        // Look-alikes: zero-width space, joiner, word joiner, soft hyphen, BOM, a tag character.
        assert_eq!(clean("Pay\u{200b}Pal\u{200d}\u{2060}\u{00ad}\u{feff}\u{e0041}", 80), "PayPal");
        assert_eq!(clean(&"a".repeat(200), 10).chars().count(), 10);
        assert!(clean(&"a".repeat(200), 10).ends_with('…'));
    }

    #[test]
    fn a_window_with_no_title_is_named_by_its_app_or_says_so() {
        let mut w = win(1, "  ", 0);
        assert_eq!(name(&w), "Untitled window");
        w.app_id = "foot".into();
        assert_eq!(name(&w), "foot");
        assert_eq!(letter(&w), "F");
    }

    #[test]
    fn age_is_said_plainly_and_unknown_is_not_made_up() {
        assert_eq!(ago(0), None);
        assert_eq!(ago(5_000).as_deref(), Some("now"));
        assert_eq!(ago(4 * 60_000).as_deref(), Some("4 min ago"));
        assert_eq!(ago(60 * 60_000).as_deref(), Some("1 h ago"));
        assert_eq!(ago(135 * 60_000).as_deref(), Some("2 h 15 min ago"));
    }

    #[test]
    fn the_time_line_gives_recent_minutes_most_of_the_room() {
        let (one, ten, hour) = (timeline_pos(60_000), timeline_pos(600_000), timeline_pos(3_600_000));
        assert!(timeline_pos(0) == 0.0 && one > 0.0 && ten > one && hour > ten && hour < 1.0);
        assert!(one > 0.1, "the first minute is not squeezed to nothing: {one}");
        assert_eq!(timeline_pos(10 * 3_600_000), 1.0, "older than two hours is 'earlier'");
    }

    #[test]
    fn typing_jumps_to_the_next_window_with_a_word_that_starts_so() {
        let ws = vec![win(1, "Weather", 1), win(2, "Containers", 1), win(3, "plan.md · Tumble", 1)];
        assert_eq!(jump(&ws, 0, "tum"), Some(2));
        assert_eq!(jump(&ws, 0, "CON"), Some(1));
        assert_eq!(jump(&ws, 2, "w"), Some(0), "wraps round from the selection");
        assert_eq!(jump(&ws, 0, "zzz"), None);
        assert_eq!(jump(&ws, 0, ""), None);
    }

    #[test]
    fn the_caption_says_where_and_when() {
        let mut w = win(1, "Weather", 4 * 60_000);
        assert_eq!(whereabouts(&w), "this desktop · used 4 min ago");
        w.current_workspace = false;
        w.workspace = "Workspace 2".into();
        w.minimized = true;
        assert_eq!(whereabouts(&w), "Workspace 2 · minimized · used 4 min ago");
    }

    #[test]
    fn labels_at_the_same_moment_do_not_sit_on_each_other() {
        let out = spread(vec![120.0, 120.0, 300.0, 125.0], 18.0);
        assert_eq!(out[0], 120.0);
        assert_eq!(out[1], 138.0);
        assert_eq!(out[3], 156.0, "the third at 'now' goes below the second");
        assert_eq!(out[2], 300.0, "one far away is left alone");
    }

    #[test]
    fn a_window_keeps_its_tile_colour() {
        let a = win(1, "Weather · London", 1);
        let b = win(2, "Weather · Paris", 1);
        assert_eq!(tile_slot(&a), tile_slot(&b), "same app, same colour");
        assert!(tile_slot(&a) < 8);
    }
}
