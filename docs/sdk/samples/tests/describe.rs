//! "The same template in Rust publishes the same describe, key for key" (docs/sdk/describe.md):
//! the Rust template's `describe` against the one the page shows, which `test_guide.py`
//! regenerates from the Python template. Two SDKs, one dispatch — this is where that is checked
//! for the thing an author copies.

use std::path::Path;

use yantrik_surface::serde_json::{self, Value};

/// The JSON block the guide marks `<!-- output: python-template-describe -->`.
fn shown_in_the_guide() -> Value {
    let page = Path::new(env!("CARGO_MANIFEST_DIR")).join("../describe.md");
    let text = std::fs::read_to_string(&page).expect("docs/sdk/describe.md");
    let after = text
        .split("<!-- output: python-template-describe -->")
        .nth(1)
        .expect("describe.md shows the template's describe");
    let body = after.split("```json").nth(1).and_then(|b| b.split("```").next()).expect("a json block");
    serde_json::from_str(body).expect("the block is JSON")
}

#[test]
fn the_rust_template_publishes_the_describe_the_guide_shows() {
    assert_eq!(my_surface::surface().describe_json(), shown_in_the_guide());
}
