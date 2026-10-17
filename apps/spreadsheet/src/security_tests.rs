//! The security review of #727 (5f3f849e), one test per finding, with the reviewer's inputs.

use serde_json::json;
use yantrik_app_runtime::control::AgentTokenScope;

use crate::session::Session;
use crate::surface;
use crate::tests::{act, Scratch};

/// The person opens a file outside the home, which no agent may read. The temp dir is outside
/// `$HOME` in every test run, so the file is hidden from agents by `home_paths`.
fn person_opens_a_secret(dir: &Scratch) -> Session {
    let secret = dir.path("secret.csv");
    std::fs::write(&secret, "topsecret,42\n").unwrap();
    let mut s = Session::new();
    act(&mut s, "open", json!({ "path": secret })).unwrap();
    s
}

#[test]
fn an_agent_reads_nothing_of_a_hidden_sheet_through_any_action() {
    let dir = Scratch::new("hidden");
    let mut s = person_opens_a_secret(&dir);
    let copy = dir.path("x.csv");
    {
        let _agent = AgentTokenScope::enter(Some("tok-sheets-review".into()));
        for (name, args) in [
            ("select", json!({ "cell": "A1" })),
            ("set_cell", json!({ "cell": "C1", "text": "=A1&B1" })),
            ("set_cell", json!({ "cell": "C1", "text": "=VLOOKUP(\"topsecret\",A1:B1,2)" })),
            ("undo", json!({})),
            ("redo", json!({})),
            ("get_range", json!({ "range": "A1:C1" })),
            ("save", json!({ "path": "~/Documents/x.csv" })),
            ("save", json!({ "path": copy })),
            ("save_over", json!({ "path": copy })),
            ("save_over", json!({})),
        ] {
            let e = act(&mut s, name, args).unwrap_err();
            assert!(!e.contains("topsecret"), "{name}: {e}");
            assert!(e.contains("not read or saved for an agent"), "{name}: {e}");
        }
        assert!(!copy.exists(), "nothing of the hidden sheet was written");
        assert_eq!(s.doc.sheet.raw(crate::address::Cell::new(0, 2)), "", "set_cell did not run");

        let view = serde_json::to_string(&yantrik_app_runtime::control::describe_json(
            "spreadsheet",
            &surface::describe(&s),
            &[],
        ))
        .unwrap();
        assert!(!view.contains("topsecret"), "{view}");
    }
    // The person still sees and edits their own sheet.
    let got = act(&mut s, "select", json!({ "cell": "A1" })).unwrap();
    assert_eq!(got["typed"], "topsecret");
}

#[test]
fn a_hidden_sheet_quoted_by_the_status_line_is_not_read_out() {
    let dir = Scratch::new("hidden-status");
    let mut s = person_opens_a_secret(&dir);
    // The person's own formula quotes the cell's text in its error, on the status line.
    act(&mut s, "set_cell", json!({ "cell": "C1", "text": "=A1+1" })).unwrap();
    assert!(s.status().0.contains("topsecret"));
    let _agent = AgentTokenScope::enter(Some("tok-sheets-review".into()));
    let state = yantrik_app_runtime::control::describe_json("spreadsheet", &surface::describe(&s), &[]);
    let text = state.to_string();
    assert!(!text.contains("topsecret"), "{text}");
}

// ── Memory: text and work have bounds ───────────────────────────────────

use crate::address::{parse_cell, Cell};
use crate::formula::budget::{MAX_TEXT, MAX_WORK};
use crate::sheet::Sheet;

/// The reviewer's 1.3 KB file: 1,000 × `x`, then `=A1&A1`, `=A2&A2`, … for 40 rows.
#[test]
fn the_doubling_csv_opens_and_stops_at_the_cell_limit() {
    let dir = Scratch::new("doubling");
    let path = dir.path("double.csv");
    let mut text = "x".repeat(1000) + "\n";
    for r in 1..40 {
        text.push_str(&format!("=A{r}&A{r}\n"));
    }
    assert!(text.len() < 1400, "{}", text.len());
    std::fs::write(&path, text).unwrap();
    let mut s = Session::new();
    act(&mut s, "open", json!({ "path": path })).unwrap();
    let sheet = &s.doc.sheet;
    assert_eq!(sheet.value(Cell::new(5, 0)).display().len(), 32_000, "A6 still fits in a cell");
    let e = sheet.value(Cell::new(6, 0));
    assert!(e.error().unwrap().message.contains("more than the 32768"), "{e:?}");
    assert!(sheet.value(Cell::new(39, 0)).error().is_some(), "A40 shows the error passed on");
}

#[test]
fn an_agent_set_cell_chain_is_held_to_the_same_limit() {
    let mut s = Session::new();
    act(&mut s, "set_cell", json!({ "cell": "A1", "text": "x".repeat(1000) })).unwrap();
    for r in 1..40 {
        act(&mut s, "set_cell", json!({ "cell": format!("A{}", r + 1), "text": format!("=A{r}&A{r}") })).unwrap();
    }
    let got = act(&mut s, "get_range", json!({ "range": "A7" })).unwrap();
    assert_eq!(got["values"][0][0]["error"], "#VALUE!", "{got}");
}

#[test]
fn concat_upper_and_ampersand_refuse_text_longer_than_a_cell() {
    let mut s = Sheet::default();
    s.set(parse_cell("A1").unwrap(), &"y".repeat(MAX_TEXT)).unwrap();
    for f in ["=A1&\"z\"", "=CONCAT(A1,A1)", "=CONCAT(A1:A2,\"z\")"] {
        s.set(parse_cell("B1").unwrap(), f).unwrap();
        assert!(s.value(parse_cell("B1").unwrap()).error().is_some(), "{f}");
    }
    s.set(parse_cell("A1").unwrap(), &"ß".repeat(MAX_TEXT)).unwrap();
    s.set(parse_cell("B1").unwrap(), "=UPPER(A1)").unwrap();
    assert!(s.value(parse_cell("B1").unwrap()).error().is_some(), "UPPER doubles ß to SS");
}

#[test]
fn formulas_copying_a_long_text_stop_at_the_sheets_text_budget() {
    // 3,000 copies of a 32,768-character cell would be 96 MB; the sheet keeps 64 MB.
    let mut rows = vec![vec!["w".repeat(MAX_TEXT)]];
    rows.extend((0..3000).map(|_| vec!["=$A$1".to_string()]));
    let s = Sheet::from_rows(&rows).unwrap();
    assert_eq!(s.value(Cell::new(1, 0)).display().len(), MAX_TEXT);
    let last = s.value(Cell::new(3000, 0));
    assert!(last.error().unwrap().message.contains("MB of text"), "{last:?}");
}

#[test]
fn a_formula_that_reads_too_many_cells_is_refused_before_it_reads_them() {
    let mut s = Sheet::new(18_000, 26);
    let range = "A1:Z17000"; // 442,000 cells
    let reads = MAX_WORK / 442_000 + 1;
    let f = format!("=SUM({})", vec![range; reads].join(","));
    s.set(parse_cell("A18000").unwrap(), &f).unwrap();
    let v = s.value(parse_cell("A18000").unwrap());
    assert!(v.error().unwrap().message.contains("cells to work out"), "{v:?}");
}

// ── Stack: nesting has a bound ──────────────────────────────────────────

use crate::formula::parser::MAX_DEPTH;

fn nested(depth: usize) -> String {
    format!("={}1{}", "(".repeat(depth), ")".repeat(depth))
}

/// The reviewer's formula: 16,000 brackets, 32,002 characters, under the `set_cell` cap.
#[test]
fn sixteen_thousand_brackets_are_an_error_not_a_stack_overflow() {
    let f = nested(16_000);
    assert_eq!(f.len(), 32_002);
    let mut s = Session::new();
    let got = act(&mut s, "set_cell", json!({ "cell": "A1", "text": f })).unwrap();
    assert_eq!(got["value"]["error"], "#ERROR!", "{got}");
    assert!(got["problem"].as_str().unwrap().contains("nests more than 100 levels"), "{got}");
}

#[test]
fn every_kind_of_deep_nesting_is_refused_and_shallow_nesting_still_works() {
    let mut s = Sheet::default();
    let at = parse_cell("B1").unwrap();
    let deep = [
        nested(MAX_DEPTH + 1),
        format!("={}1", "-".repeat(16_000)),
        format!("={}1", "+".repeat(16_000)),
        format!("=1{}", "+1".repeat(16_000)),
        format!("=1{}", "%".repeat(16_000)),
        format!("={}1{}", "ABS(".repeat(5_000), ")".repeat(5_000)),
        format!("={}1{}", "IF(TRUE,".repeat(3_000), ")".repeat(3_000)),
    ];
    for f in &deep {
        s.set(at, f).unwrap();
        let v = s.value(at);
        assert!(v.error().is_some_and(|e| e.message.contains("levels deep")), "{}…: {v:?}", &f[..20]);
    }
    s.set(at, &nested(50)).unwrap();
    assert_eq!(s.value(at).display(), "1");
    s.set(at, &format!("=1{}", "+1".repeat(60))).unwrap();
    assert_eq!(s.value(at).display(), "61");
}

#[test]
fn a_csv_cell_longer_than_a_cell_holds_is_refused_with_its_line() {
    let dir = Scratch::new("long-cell");
    let path = dir.path("long.csv");
    std::fs::write(&path, format!("ok\n\"{}\"\n", nested(20_000))).unwrap();
    let mut s = Session::new();
    let e = act(&mut s, "open", json!({ "path": path })).unwrap_err();
    assert!(e.contains("line 2") && e.contains("32768 characters"), "{e}");
    assert!(crate::csv::parse(&"a".repeat(MAX_TEXT)).is_ok(), "exactly a cell's worth is fine");
}

#[test]
fn the_evaluator_refuses_a_tree_deeper_than_the_parser_allows() {
    use crate::formula::budget::Budget;
    use crate::formula::eval::{Eval, Grid};
    use crate::formula::{Expr, Value};
    struct Blank;
    impl Grid for Blank {
        fn value(&self, _: Cell) -> Value {
            Value::Empty
        }
        fn rows(&self) -> usize {
            1
        }
        fn cols(&self) -> usize {
            1
        }
    }
    let mut e = Expr::Number(1.0);
    for _ in 0..MAX_DEPTH + 5 {
        e = Expr::Neg(Box::new(e));
    }
    let budget = Budget::default();
    let v = Eval::new(&Blank, &budget).run(&e);
    assert!(v.error().is_some_and(|e| e.message.contains("levels deep")), "{v:?}");
}

// ── Time: recalculation is not quadratic ────────────────────────────────

/// `rows` rows of 26 `=SUM(An:An)` cells, as the reviewer's file had them.
fn sums(rows: usize) -> String {
    (1..=rows).map(|r| vec![format!("=SUM(A{r}:A{r})"); 26].join(",") + "\n").collect()
}

fn open_time(rows: usize) -> (Session, std::time::Duration) {
    let dir = Scratch::new(&format!("sums-{rows}"));
    let path = dir.path("sums.csv");
    std::fs::write(&path, sums(rows)).unwrap();
    let mut s = Session::new();
    let t = std::time::Instant::now();
    act(&mut s, "open", json!({ "path": path })).unwrap();
    let took = t.elapsed();
    assert_eq!(s.doc.sheet.value(Cell::new(rows - 1, 1)).error().map(|e| e.code()), Some("#CYCLE!"));
    (s, took)
}

/// 40,000 formula cells took 6 s in a release build before; a debug build now opens them in
/// well under that, and each edit after it costs the same, not more.
#[test]
fn forty_thousand_range_formulas_open_and_edit_in_linear_time() {
    let (mut s, took) = open_time(40_000 / 26);
    assert!(took.as_secs_f64() < 5.0, "{took:?}");
    let t = std::time::Instant::now();
    act(&mut s, "set_cell", json!({ "cell": "A1", "text": "5" })).unwrap();
    assert!(t.elapsed().as_secs_f64() < 5.0, "{:?}", t.elapsed());
    assert_eq!(s.doc.sheet.value(Cell::new(0, 1)).display(), "5");
}

/// The reviewer's 494,000 cells, about 9 MB: over 300 s before. Run with
/// `cargo test --release -- --ignored`, which prints the time.
#[test]
#[ignore]
fn the_reviewers_nine_megabyte_sheet_opens() {
    let (_, took) = open_time(19_000);
    eprintln!("494,000 formulas opened in {took:?}");
    assert!(took.as_secs_f64() < 10.0, "{took:?}");
}

#[test]
fn ranges_that_overlap_without_end_are_held_to_the_budget() {
    // Running totals over everything above: 7,000 formulas link to 24.5 million others.
    let mut rows = vec![vec!["1".to_string()]];
    rows.extend((2..=7_000).map(|r| vec![format!("=SUM(A1:A{})", r - 1)]));
    let s = Sheet::from_rows(&rows).unwrap();
    assert_eq!(s.value(Cell::new(2, 0)).display(), "2", "the first totals are worked out");
    let last = s.value(Cell::new(6_999, 0));
    assert!(last.error().unwrap().message.contains("cells to work out"), "{last:?}");
}

// ── Parsing stops at the sheet's limits ─────────────────────────────────

/// The reviewer's two 16 MB files: newlines (16 M rows, 2.1 GB before), and commas (16 M empty
/// fields, about 400 MB). Both now stop after at most one sheet's worth of fields.
#[test]
fn sixteen_megabytes_of_newlines_or_commas_stop_at_the_sheets_limits() {
    let dir = Scratch::new("huge");
    let size = crate::document::MAX_FILE_BYTES as usize;
    for (name, byte, says) in [("lines.csv", b'\n', "500000 cells"), ("commas.csv", b',', "256 columns")] {
        let path = dir.path(name);
        std::fs::write(&path, vec![byte; size]).unwrap();
        let mut s = Session::new();
        let t = std::time::Instant::now();
        let e = act(&mut s, "open", json!({ "path": path })).unwrap_err();
        assert!(e.contains(says), "{name}: {e}");
        assert!(t.elapsed().as_secs_f64() < 5.0, "{name}: {:?}", t.elapsed());
    }
}

#[test]
fn the_parser_keeps_the_limits_exactly() {
    use crate::sheet::{MAX_CELLS, MAX_SHEET_COLS};
    let wide = vec!["a"; MAX_SHEET_COLS].join(",");
    assert_eq!(crate::csv::parse(&wide).unwrap()[0].len(), MAX_SHEET_COLS);
    let e = crate::csv::parse(&format!("x\n{wide},b")).unwrap_err();
    assert!(e.contains("line 2 has more than"), "{e}");
    let full = "1,1\n".repeat(MAX_CELLS / 2);
    assert_eq!(crate::csv::parse(&full).unwrap().len(), MAX_CELLS / 2);
    assert!(crate::csv::parse(&(full + "1\n")).unwrap_err().contains("more than the 500000 cells"));
}

// ── Open reads regular files only ───────────────────────────────────────

/// Open `path` on a thread of its own, so a regression fails the test instead of hanging it.
fn open_within_seconds(path: std::path::PathBuf) -> Result<crate::document::Opened, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::document::Document::new().open(&path));
    });
    rx.recv_timeout(std::time::Duration::from_secs(5)).expect("open blocked or read without end")
}

#[test]
fn a_fifo_a_device_and_a_link_to_a_device_are_refused_at_once() {
    let dir = Scratch::new("special");
    let fifo = dir.path("pipe.csv");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let zero = dir.path("zero.csv");
    std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
    for path in [fifo, zero, "/dev/zero".into()] {
        let e = open_within_seconds(path.clone()).unwrap_err();
        assert!(e.contains("not a regular file"), "{}: {e}", path.display());
    }
    let folder = open_within_seconds(dir.path("")).unwrap_err();
    assert!(folder.contains("is a folder"), "{folder}");
}
