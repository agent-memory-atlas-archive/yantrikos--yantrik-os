fn main() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent-dark".into())
        .with_include_paths(vec![
            root.join("crates/yantrik-design-tokens/slint"),
            root.join("crates/yantrik-ui-kit/slint"),
            root.join("crates/yantrik-ui-slint/ui"),
        ]);
    slint_build::compile_with_config("preview.slint", config).unwrap();
}
