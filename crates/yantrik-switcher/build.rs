fn main() {
    // The shared theme (colours, Barlow), so Alt+Tab is drawn in the shell's own look.
    let tokens = std::env::var("DEP_YANTRIK_DESIGN_TOKENS_SLINT_PATH")
        .expect("yantrik-design-tokens provides DEP_YANTRIK_DESIGN_TOKENS_SLINT_PATH");
    let config = slint_build::CompilerConfiguration::new().with_include_paths(vec![tokens.into()]);
    slint_build::compile_with_config("ui/switcher.slint", config).unwrap();
    println!("cargo:rerun-if-changed=../../packaging/labwc/labwc-window-switcher-renderer-unstable-v1.xml");
}
