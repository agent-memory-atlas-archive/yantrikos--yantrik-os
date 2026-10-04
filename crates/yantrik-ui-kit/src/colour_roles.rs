//! The colour roles, held where a sweep of VM 520 (4 Oct 2026) found them broken.
//!
//! Teal is the minds' colour and nothing else's; amber is "needs you" and nothing else; a
//! primary button is the accent in every app (design/minds-surfaces-spec-2026-10-02.md,
//! "Colour roles"). These read the source, so they run without building Slint.

use std::path::{Path, PathBuf};

fn read(rel: &str) -> String {
    let root: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

const UI: &str = "crates/yantrik-ui-slint/ui/";

/// Screens that are not a mind's, and so must not draw the minds' teal. Containers' "Run New",
/// About's version pill and section bars, the Skills chips, the System and System Monitor bars,
/// the setup and login screens, and the shared chart, toast and ribbon all did.
#[test]
fn teal_stays_off_screens_that_are_not_a_minds() {
    for file in [
        "container_manager.slint", "about.slint", "skill_store.slint", "settings.slint",
        "system_dashboard.slint", "system_monitor.slint", "recipes.slint", "permission_dashboard.slint",
        "installer.slint", "onboarding.slint", "login.slint", "terminal.slint",
        "components/onboard_kit.slint", "components/app_ribbon.slint", "components/window_frame.slint",
        "components/model_tier_badge.slint",
    ] {
        let src = read(&format!("{UI}{file}"));
        for teal in ["Theme.cyan", "Theme.tint-cyan"] {
            assert!(!src.contains(teal), "{file} draws `{teal}`, the minds' colour");
        }
    }
    for kit in ["line_chart.slint", "toast_banner.slint"] {
        assert!(!read(&format!("crates/yantrik-ui-kit/slint/{kit}")).contains("Theme.cyan"), "{kit} defaults to the minds' teal");
    }
}

/// A primary that wears the app's identity colour is teal in Containers and red in Permissions
/// ("Scanning..." looked destructive). These screens use the kit's YButton now.
#[test]
fn a_primary_button_is_the_accent_not_the_apps_colour() {
    for file in ["container_manager.slint", "permission_dashboard.slint", "network_manager.slint"] {
        let src = read(&format!("{UI}{file}"));
        assert!(!src.contains("primary ? AppIdentity.accent"), "{file} fills its primary with the app's colour");
        assert!(src.contains("import { YButton }"), "{file} uses the kit's button");
    }
    let perm = read(&format!("{UI}permission_dashboard.slint"));
    let scan = perm.find("\"Scanning...\" : \"Scan\"").expect("the Scan button");
    assert!(perm[scan..scan + 120].contains("variant: 0;"), "Scan is the accent primary, never the destructive kind");
}

/// Amber means a person's answer is pending. A bond score, a busy CPU and a model tier are not.
/// A disk at the limit the status row checks is — it needs its owner — so amber may appear on
/// these screens only behind that limit (`needs-you`), never as the colour of a reading.
#[test]
fn amber_is_not_used_for_data() {
    for file in ["bond.slint", "personality.slint", "system_dashboard.slint", "system_monitor.slint", "components/model_tier_badge.slint"] {
        let src = read(&format!("{UI}{file}"));
        for line in src.lines().filter(|l| l.contains("Theme.amber")) {
            assert!(line.contains("needs-you ? Theme.amber"), "{file} paints data amber: {}", line.trim());
        }
    }
}

/// System Monitor drew a disk at 95% in red, the colour of an action that destroys something.
/// A full disk is amber, at the same limit the status row checks, and never red.
#[test]
fn a_full_disk_is_amber_never_red() {
    let src = read(&format!("{UI}system_monitor.slint"));
    let start = src.find("for disk in root.disks").expect("the disk card's rows");
    let rows = &src[start..start + src[start..].find("Rectangle { height: 1px").expect("the row's rule")];
    assert!(rows.contains("disk.needs-you ? Theme.amber"), "the disk bar is amber at the limit");
    for red in ["color-emotional", "color-danger"] {
        assert!(!rows.contains(red), "a disk row draws `{red}`");
    }
    let row = read(&format!("{UI}components/machine_status_row.slint"));
    let glyph = row.find("if root.needs-you : Icon").expect("the warning glyph, only when a limit is reached");
    assert_eq!(row.matches("Theme.amber").count(), 1, "amber is the glyph's and nothing else's");
    assert!(row.find("Theme.amber").unwrap() > glyph);
}

/// A finished recipe step is not drawn as a success (the success token's dark value was a teal):
/// done recedes to neutral, the accent marks the current step, amber the person's turn.
#[test]
fn a_done_recipe_step_is_neutral() {
    let src = read(&format!("{UI}recipes.slint"));
    assert!(!src.contains("color-success"), "recipes.slint draws done in the success colour");
}

/// The hue of `#rrggbb` (alpha after it ignored), in degrees, or None for a grey, which has no
/// hue to collide with anything.
fn hue(hex: &str) -> Option<f32> {
    let h = hex.trim_start_matches('#');
    let channel = |at: usize| u8::from_str_radix(h.get(at..at + 2)?, 16).ok().map(|v| v as f32 / 255.0);
    let (r, g, b) = (channel(0)?, channel(2)?, channel(4)?);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let delta = max - min;
    if delta < 0.04 {
        return None;
    }
    let h = if max == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    Some(h)
}

/// Every `#rrggbb[aa]` literal in `text`.
fn hexes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices('#') {
        let digits: String = text[at + 1..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        if digits.len() == 6 || digits.len() == 8 {
            out.push(format!("#{digits}"));
        }
    }
    out
}

/// The two hue families that already mean something: amber is "needs you", teal is a mind.
fn in_a_reserved_family(hex: &str) -> Option<&'static str> {
    match hue(hex)? {
        h if (30.0..=50.0).contains(&h) => Some("amber (needs you)"),
        h if (160.0..=190.0).contains(&h) => Some("teal (a mind)"),
        _ => None,
    }
}

/// The accent is every primary and every ON, so it may not wear a colour that means something
/// else: picking the amber preset made every button say "needs you". No accent preset (dark or
/// light, any of its shades) and no theme's accent may sit in the amber (30-50 deg) or teal
/// (160-190 deg) family.
#[test]
fn no_accent_is_amber_or_teal() {
    let theme = read("crates/yantrik-design-tokens/slint/theme.slint");
    let start = theme.find("export global AccentPreset").expect("the presets");
    let presets = &theme[start..start + theme[start..].find("\n}\n").expect("the presets end")];
    let colours = hexes(presets);
    assert!(colours.len() >= 18, "read the presets' colours, found {}", colours.len());
    for c in &colours {
        assert!(in_a_reserved_family(c).is_none(), "an accent preset is {c}, in the {} family", in_a_reserved_family(c).unwrap());
    }
    // Settings offers the soft blue, violet and pink, and nothing the presets no longer have.
    for gone in ["swatch-1", "swatch-3"] {
        assert!(!presets.contains(gone), "{gone} was amber or green; it is not a preset any more");
    }
    let settings = read(&format!("{UI}settings.slint"));
    let list = &settings[settings.find("for accent in [").unwrap()..];
    let list = &list[..list.find("] : AccentChoice").unwrap()];
    for gone in ["\"amber\"", "\"green\""] {
        assert!(!list.contains(gone), "Settings offers the {gone} accent");
    }

    // A theme file's accent, and the fallback a theme file gets for an accent it leaves out.
    let themes: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../yantrik-design-tokens/themes");
    let mut seen = 0;
    for entry in std::fs::read_dir(&themes).expect("the themes") {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let accent = text
            .lines()
            .find(|l| l.trim_start().starts_with("accent = \"#"))
            .unwrap_or_else(|| panic!("{} has no palette accent", path.display()));
        for c in hexes(accent) {
            assert!(in_a_reserved_family(&c).is_none(), "{}'s accent {c} is in the {} family", path.display(), in_a_reserved_family(&c).unwrap());
        }
        seen += 1;
    }
    assert!(seen >= 2, "Lake and Nightfall at least");
    let fallback = theme.lines().find(|l| l.contains("accent-override:")).expect("the override's default");
    for c in hexes(fallback) {
        assert!(in_a_reserved_family(&c).is_none(), "the accent override's default {c} is in a reserved family");
    }
}

/// The hue check itself, on the colours whose meaning it guards.
#[test]
fn the_reserved_families_are_where_the_roles_are() {
    assert_eq!(in_a_reserved_family("#d4a574"), Some("amber (needs you)"), "needs-you amber");
    assert_eq!(in_a_reserved_family("#f2b73e"), Some("amber (needs you)"), "the retired amber preset");
    assert_eq!(in_a_reserved_family("#38d8cd"), Some("teal (a mind)"), "the minds' teal");
    assert_eq!(in_a_reserved_family("#69c8bc"), Some("teal (a mind)"), "the dock's mind teal");
    assert_eq!(in_a_reserved_family("#8fb4e3"), None, "the soft blue");
    assert_eq!(in_a_reserved_family("#b98aef"), None, "violet");
    assert_eq!(in_a_reserved_family("#f27aa0"), None, "pink");
    assert_eq!(hue("#808080"), None, "a grey has no hue");
}

/// Success is green, not teal: the dark success colour was #26a69a, the minds' hue, so a done
/// read as a mind being present.
#[test]
fn success_is_green_not_teal() {
    let theme = read("crates/yantrik-design-tokens/slint/theme.slint");
    for token in ["color-success:", "color-success-dim:"] {
        let line = theme.lines().find(|l| l.contains(token)).unwrap_or_else(|| panic!("{token}"));
        let colours = hexes(line);
        assert_eq!(colours.len(), 2, "{token} has a dark and a light value");
        for c in colours {
            let h = hue(&c).unwrap_or_else(|| panic!("{c} is grey"));
            assert!((90.0..=150.0).contains(&h), "{token} {c} is not a green (hue {h:.0})");
        }
    }
}

/// Downloads painted red on everything a file that is gone touches: the row's dot, a "File gone"
/// pill, a full bar under the row and the banner that says the list was restored. None of it is
/// destructive. A settled row is a dim glyph and the neutral outline pill; the banner is the kit's
/// neutral notice.
#[test]
fn a_download_that_is_gone_is_not_red() {
    let src = read(&format!("{UI}download_manager.slint"));
    for status in ["missing", "failed", "completed"] {
        assert!(!src.contains(&format!("dl.status == \"{status}\" ? Theme.color")), "a {status} row is painted a hue");
    }
    assert!(src.contains("Icons.file-missing"), "a gone file is a shape, not a red dot");
    assert!(src.contains("YStatusPill {"), "a settled row wears the neutral outline pill");
    let bar = src.find("if root.error-text != \"\" : YNoticeBar").expect("the banner is the kit's notice");
    assert!(src[bar..bar + 200].contains("tone: 0;"), "and the neutral kind of it");
    let engine = read("apps/download-manager/src/engine.rs");
    assert!(!engine.contains("file(s) no longer"), "a count is said in words");
}

/// "Installed" was a teal-green pill, and the row centred its content so the name drifted.
#[test]
fn a_package_row_is_a_left_aligned_grid_with_a_neutral_installed_pill() {
    let src = read(&format!("{UI}package_manager.slint"));
    let row = &src[src.find("for pkg[idx] in root.packages").expect("the list")..];
    let row = &row[..row.find("row-ta := TouchArea").expect("the row's touch area")];
    let grid = &row[row.find("HorizontalLayout {").expect("the row's grid")..];
    let alignment = &grid[grid.find("alignment:").expect("the grid's alignment")..];
    assert!(alignment.starts_with("alignment: start;"), "the row centres its content");
    assert!(!row.contains("color-success"), "a package row draws the success teal");
    assert!(row.contains("YStatusPill {") && row.contains("text: \"Installed\";"));
    for width in ["width: 16px;", "width: 72px;", "width: 96px;"] {
        assert!(row.contains(width), "the grid lost its `{width}` column");
    }
}

/// The risk tiles were filled with hues that contradicted their words. Amber is "needs you", and
/// only high risk with files in it is that.
#[test]
fn a_risk_tile_is_neutral_and_only_high_risk_is_amber() {
    let src = read(&format!("{UI}permission_dashboard.slint"));
    assert!(!src.contains("badge-bg"), "a risk tile is filled with a hue");
    let card = &src[src.find("component RiskSummaryCard").expect("the tile")..];
    let card = &card[..card.find("\n}\n").expect("the tile ends")];
    assert!(card.contains("background: Theme.tile-off;") && card.contains("tint: Theme.amber;"));
    assert!(src.contains("needs-you: root.perm-high-risk-count > 0;"));
    assert!(src.contains("text: \"Scan path\";"), "the scan path field says what it is");
}

/// WCAG 2 contrast of two `#rrggbb` colours, from 1:1 to 21:1.
fn contrast(a: &str, b: &str) -> f64 {
    let luminance = |hex: &str| {
        let h = hex.trim_start_matches('#');
        let channel = |at: usize| {
            let v = u8::from_str_radix(&h[at..at + 2], 16).unwrap_or_else(|_| panic!("{hex}")) as f64 / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
    };
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The value of `out property <color> {name}:` in `scope`, up to its `;`.
fn token<'a>(scope: &'a str, name: &str) -> &'a str {
    let key = format!("out property <color> {name}:");
    let start = scope.find(&key).unwrap_or_else(|| panic!("no `{name}` token")) + key.len();
    &scope[start..start + scope[start..].find(';').expect("the token ends")]
}

/// Every theme file: its name, whether it is dark, and its palette's `bg_deep` and `accent`.
fn theme_files() -> Vec<(String, bool, String, String)> {
    let dir: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../yantrik-design-tokens/themes");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the themes") {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let dark = text.lines().any(|l| l.trim() == "dark = true");
        let palette = &text[text.find("[palette]").unwrap_or_else(|| panic!("{name} has no palette"))..];
        let colour = |key: &str| {
            let line = palette.lines().find(|l| l.trim_start().starts_with(&format!("{key} = \"#")));
            hexes(line.unwrap_or_else(|| panic!("{name} has no palette {key}")))[0].clone()
        };
        let (bg_deep, accent) = (colour("bg_deep"), colour("accent"));
        out.push((name, dark, bg_deep, accent));
    }
    out
}

/// Text and glyphs on an accent fill are `Theme.text-on-accent`: the theme's deepest ground in dark
/// mode, white in light. White on the dark accents was 2.1:1 (soft blue) and 2.6:1 (violet). This
/// reads the rule and every colour it meets from the source: each dark preset, at rest and at its
/// hover step, under the stock ground and every theme file's; each light preset under white; and
/// each theme file's own accent under its own ink. 4.5:1 is the floor for 14px text.
#[test]
fn the_ink_on_an_accent_fill_reads_on_every_accent() {
    let tokens = read("crates/yantrik-design-tokens/slint/theme.slint");
    let start = tokens.find("export global AccentPreset").expect("the presets");
    let presets = &tokens[start..start + tokens[start..].find("\n}\n").expect("the presets end")];
    let theme = &tokens[tokens.find("export global Theme {").expect("the Theme global")..];
    let rule = token(theme, "text-on-accent").trim();
    assert_eq!(rule, "ThemeMode.dark ? Theme.bg-deep : #ffffff", "the ink follows the ground in dark mode and is white in light");

    let mut dark_inks = vec![("theme.slint".to_string(), hexes(token(theme, "bg-deep"))[0].clone())];
    let themes = theme_files();
    assert!(themes.len() >= 2, "Lake and Nightfall at least");
    dark_inks.extend(themes.iter().filter(|t| t.1).map(|t| (t.0.clone(), t.2.clone())));

    // A dark preset is its swatch, and its hover step is accent-light's dark half. The light
    // presets are the hexes written into `accent` itself.
    let rest: Vec<String> = presets.lines().filter(|l| l.contains("property <color> swatch-")).flat_map(hexes).collect();
    let hover: Vec<String> = hexes(token(presets, "accent-light")).into_iter().step_by(2).collect();
    let light = hexes(token(presets, "accent"));
    assert_eq!((rest.len(), hover.len(), light.len()), (3, 3, 3), "three presets, read in each form");

    for (whose, ink) in &dark_inks {
        for accent in rest.iter().chain(&hover) {
            let ratio = contrast(ink, accent);
            assert!(ratio >= 4.5, "{whose}'s ink {ink} on the dark accent {accent} is {ratio:.2}:1");
        }
    }
    for accent in &light {
        let ratio = contrast("#ffffff", accent);
        assert!(ratio >= 4.5, "white on the light accent {accent} is {ratio:.2}:1");
    }
    for (name, dark, bg_deep, accent) in &themes {
        let ink = if *dark { bg_deep.as_str() } else { "#ffffff" };
        let ratio = contrast(ink, accent);
        assert!(ratio >= 4.5, "{name}: its ink {ink} on its accent {accent} is {ratio:.2}:1");
    }
}

/// The contrast sum itself, on pairs with published ratios.
#[test]
fn contrast_is_wcag() {
    assert!((contrast("#000000", "#ffffff") - 21.0).abs() < 0.01);
    assert!((contrast("#777777", "#ffffff") - 4.48).abs() < 0.01);
    assert!((contrast("#ffffff", "#8fb4e3") - 2.14).abs() < 0.01, "white on the soft blue, the sign-off's case");
}

/// Every .slint file under `dir`, build output skipped.
fn slint_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() && path.file_name().is_some_and(|n| n != "target") {
            slint_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "slint") {
            out.push(path);
        }
    }
}

/// Nothing sits on an accent fill in white, near-white text-primary, or bg-deep (pale in light
/// mode, so a hand-written "dark ink" turned pale there): the ink is Theme.text-on-accent. A fill
/// is an element whose own background names the accent (or the app's identity accent, which is
/// as light); a colour, tint, glyph fill or knob anywhere inside it is checked. A value that also
/// names text-on-accent chooses between the fill's ink and another state's, and passes.
#[test]
fn nothing_on_an_accent_fill_is_white_or_text_primary() {
    use crate::slint_source::{blocks, names, own_text, strip, values};
    const FORBIDDEN: [&str; 5] = ["white", "#fff", "Theme.text-primary", "Theme.bg-deep", "Theme.lock-ground"];
    const ACCENTS: [&str; 3] = ["Theme.accent", "AccentPreset.accent", "AppIdentity.accent"];
    let root: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut files = Vec::new();
    for dir in ["crates/yantrik-ui-slint/ui", "crates/yantrik-ui-kit/slint", "crates/yantrik-lock/ui", "apps", "tests/ui-preview"] {
        slint_files(&root.join(dir), &mut files);
    }
    assert!(files.len() > 100, "read the shell, the kit and the apps, found {}", files.len());
    let mut fills = 0;
    for path in &files {
        let s = strip(&std::fs::read_to_string(path).unwrap());
        let all = blocks(&s);
        for (i, block) in all.iter().enumerate() {
            let own = own_text(&s, *block, &all);
            if !values(&own, "background").iter().any(|v| ACCENTS.iter().any(|t| names(v, t))) {
                continue;
            }
            fills += 1;
            let inner = &s[block.open + 1..block.close];
            let mut inks: Vec<String> = ["color", "tint", "fill"].iter().flat_map(|p| values(inner, p)).collect();
            for inside in all[i + 1..].iter().take_while(|b| b.open < block.close) {
                inks.extend(values(&own_text(&s, *inside, &all), "background"));
            }
            for ink in inks.iter().filter(|v| !v.contains("text-on-accent")) {
                if let Some(bad) = FORBIDDEN.iter().find(|f| ink.contains(*f)) {
                    let line = s[..block.open].matches('\n').count() + 1;
                    panic!("{}:{line} puts `{ink}` ({bad}) on an accent fill; the ink there is Theme.text-on-accent", path.display());
                }
            }
        }
    }
    assert!(fills > 50, "found the accent fills, {fills} of them");
}
