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
#[test]
fn amber_is_not_used_for_data() {
    for file in ["bond.slint", "personality.slint", "system_dashboard.slint", "system_monitor.slint", "components/model_tier_badge.slint"] {
        let src = read(&format!("{UI}{file}"));
        assert!(!src.contains("Theme.amber;") && !src.contains("? Theme.amber :"), "{file} paints data amber");
    }
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
