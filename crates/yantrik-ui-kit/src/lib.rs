// Yantrik UI Kit — reusable Slint components.
// Consuming crates access .slint files via DEP_YANTRIK_UI_KIT_SLINT_PATH env var.
//
// Components: AppHeader, YButton, YIconButton, YInput, YDialog, YTabs,
// YSidebar, YListItem, YContextMenu, YEmptyState, LineChart, RadarChart,
// ToastBanner, MessageBubble, Icon.
//
// AppHeader is the one every app is required to use; see docs/app-sdk.md. YToolbar and
// AppShell used to sit here and were instantiated by nothing, because each spent Slint's
// single `@children` on a region no app needed. They are gone.

#[cfg(test)]
mod app_header_is_mandatory {
    use std::path::{Path, PathBuf};

    /// Every app we ship, and the screen component that draws it.
    ///
    /// An app binary is a thin `Window` around one of these, so this is where an app's top edge
    /// is decided. Adding an app means adding a line here; that is the point.
    const APP_SCREENS: &[(&str, &str)] = &[
        ("calendar", "calendar.slint"),
        ("container-manager", "container_manager.slint"),
        ("document-editor", "document_editor.slint"),
        ("download-manager", "download_manager.slint"),
        ("email", "email.slint"),
        ("image-viewer", "image_viewer.slint"),
        ("music-player", "music_player.slint"),
        ("network-manager", "network_manager.slint"),
        ("notes", "notes_editor.slint"),
        ("presentation", "presentation.slint"),
        ("snippet-manager", "snippet_manager.slint"),
        ("spreadsheet", "spreadsheet.slint"),
        ("system-monitor", "system_monitor.slint"),
        ("terminal", "terminal.slint"),
        ("text-editor", "text_editor.slint"),
        ("weather", "weather.slint"),
    ];

    fn ui_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../yantrik-ui-slint/ui")
            .canonicalize()
            .expect("the shell's ui directory sits beside the kit")
    }

    /// The check that makes the header consistent by construction rather than by agreement.
    ///
    /// Sixteen apps once drew their own top bar and the heights came out 28, 32, 36, 44, 48, 56
    /// and 64 — nobody chose that, it is just what happens when sixteen files each decide. A
    /// screen that instantiates AppHeader cannot choose: the height, the padding, the glyph tile
    /// and the button size all belong to the component, and the app supplies only data.
    ///
    /// So this asserts the one thing that cannot be re-derived from the markup: that the app
    /// went through the frame at all.
    #[test]
    fn every_app_we_ship_draws_its_header_with_the_shared_component() {
        let dir = ui_dir();
        let mut missing = Vec::new();

        for (app, screen) in APP_SCREENS {
            let path = dir.join(screen);
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{app}: cannot read {}: {e}", path.display()));

            if !src.contains("AppHeader {") {
                missing.push(format!("{app} ({screen}) draws its own header"));
            }
        }

        assert!(
            missing.is_empty(),
            "these apps do not use the shared header:\n  {}\n\n\
             Use AppHeader from the UI kit. Header content is data — `title`, `icon`, \
             `subtitle` and an `actions` model of AppAction — so that every app's top edge is \
             identical without anyone having to remember a number. See docs/app-sdk.md.",
            missing.join("\n  ")
        );
    }

    /// The header's height is a token, not a literal an app can retype.
    ///
    /// `h-app-header` exists so there is exactly one answer to "how tall is an app's top edge".
    /// If AppHeader ever hardcodes a number instead, the token stops meaning anything and the
    /// drift can start again from one file.
    #[test]
    fn the_header_takes_its_height_from_the_token() {
        let src = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("slint/app_header.slint"),
        )
        .expect("app_header.slint is part of this crate");

        assert!(
            src.contains("height: Theme.h-app-header;"),
            "AppHeader must take its height from Theme.h-app-header, so the one number that \
             decides how tall an app's top edge is lives in the design tokens"
        );
    }

    /// The components that got us here are not allowed to come back.
    ///
    /// YToolbar and AppShell were each instantiated by nothing for the same reason: a Slint
    /// component has one `@children`, both spent it on a region no app needed, and so every app
    /// went and drew its own header instead. A half-usable shared component is worse than none —
    /// it looks like the problem is solved.
    #[test]
    fn the_shells_that_nobody_could_use_are_still_gone() {
        let kit = Path::new(env!("CARGO_MANIFEST_DIR")).join("slint");
        for dead in ["y_toolbar.slint", "app_shell.slint"] {
            assert!(
                !kit.join(dead).exists(),
                "{dead} is back. If a shared component cannot express what every app needs \
                 (for a header: trailing actions), apps will bypass it and hand-roll their own. \
                 Pass the content as data instead of as children — see AppHeader."
            );
        }
    }
}
