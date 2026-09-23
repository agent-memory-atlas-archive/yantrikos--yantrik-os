//! The checks every call's arguments meet before its handler runs: present when required, known
//! to the action, and of the type the action publishes.
//!
//! Checked here rather than in every handler: a missing or mistyped argument is the most common
//! way a model gets a call wrong, and the error should name the argument and say what was wanted,
//! not panic in the app or — worse, and more usual — read `as_str()` on a number, find nothing,
//! and act on the empty string.
//!
//! # The sentences
//!
//! Each refusal is one sentence a caller can correct from, and a caller's code can match on:
//!
//! ```text
//! `open_note` needs argument `title`
//! `open_note` has no argument `colour`; it takes: title
//! `new_note` takes no arguments, but `title` was given
//! `kill_process` argument `pid` must be an integer, and a number with a fraction arrived
//! `notify` argument `urgency` must be one of `low`, `normal`, `critical`, and another string arrived
//! `open_files` argument `paths` must be an array of strings, and `paths[2]` is a number
//! `open_note` takes its arguments as an object of named values, and an array arrived
//! ```
//!
//! A refusal names the kind of value that arrived and never the value itself: a number a caller
//! sends may be a PIN, a year of birth or a dose, and a refusal is shown, logged and handed back
//! to a model. The declaration — the type, and an enum's values — is what a caller corrects from.
//!
//! # The type rules
//!
//! * `string` — a JSON string. A number is not text here, even one that spells an id: the
//!   declaration is what a caller reads before it calls, and a dispatch that quietly accepted
//!   something else would make the declaration a guess.
//! * `number` — any JSON number.
//! * `integer` — a JSON number written without a fraction or exponent: `3`, not `3.0` or `3e0`.
//!   Stricter than JSON Schema (which counts `3.0` as an integer) on purpose: a handler reading
//!   the argument with `as_u64` finds nothing in `3.0`, so accepting it would hand the handler a
//!   value it cannot read. Python's `json` draws the same line (`3` is an `int`, `3.0` a `float`).
//! * `boolean` — `true` or `false`.
//! * `object` — a JSON object.
//! * `array` — a JSON array whose every item is of the declared item type.
//! * `enum` — a `string` whose value is one of the declared values.
//! * `null` for an OPTIONAL argument is the same as leaving it out, and is passed to the handler
//!   as it came (or replaced by the declared default). For a required argument it is a value of
//!   the wrong type.
//! * A declaration this dispatch does not understand — a type off [`PARAM_TYPES`] — refuses every
//!   call, the way an off-ladder grade does: a typo must fail closed, not become "anything goes".

use std::borrow::Cow;

use serde_json::Value;
use yantrik_ipc_contracts::control_surface::{Action, Param, PARAM_TYPES};

/// Check `args` against what `spec` declares: an object of named values, every required argument
/// present, nothing undeclared, and each argument of its declared type — in that order, and the
/// first failure is the answer.
pub fn check_arguments(spec: &Action, args: &Value) -> Result<(), String> {
    let name = spec.name.as_str();

    // Named arguments are an object. `null` (and `args` left out, which arrives as `{}`) is
    // "nothing given"; anything else cannot hold a named argument at all.
    if !(args.is_object() || args.is_null()) {
        return Err(format!(
            "`{name}` takes its arguments as an object of named values, and {} arrived",
            arrived("object", args)
        ));
    }

    for p in spec.params.iter().filter(|p| p.required) {
        if args.get(&p.name).is_none() {
            return Err(format!("`{name}` needs argument `{}`", p.name));
        }
    }

    // The mirror of the check above, and the omission that actually bit: an argument the action
    // does not declare used to be dropped in silence. `new_note title='Handover'` answered
    // accepted:true and wrote a note called "Untitled" — the caller was told its instruction had
    // landed when nothing had read it. Refusing names the mistake and costs one retry; accepting
    // it hides the mistake and costs the whole task.
    if let Some(given) = args.as_object() {
        for key in given.keys() {
            if spec.params.iter().any(|p| &p.name == key) {
                continue;
            }
            let known: Vec<&str> = spec.params.iter().map(|p| p.name.as_str()).collect();
            return Err(if known.is_empty() {
                format!("`{name}` takes no arguments, but `{key}` was given")
            } else {
                format!("`{name}` has no argument `{key}`; it takes: {}", known.join(", "))
            });
        }
    }

    // In declared order, so the answer to a call with two mistakes is always the same one.
    for p in &spec.params {
        let Some(value) = args.get(&p.name) else { continue };
        if value.is_null() && !p.required {
            continue;
        }
        check_value(name, p, value)?;
    }
    Ok(())
}

/// One argument against its declaration.
pub fn check_value(action: &str, p: &Param, value: &Value) -> Result<(), String> {
    if !PARAM_TYPES.contains(&p.kind) {
        return Err(undefined(action, p, p.kind));
    }
    if !is(p.kind, value) {
        return Err(format!(
            "`{action}` argument `{}` must be {}, and {} arrived",
            p.name,
            wanted(p),
            arrived(p.kind, value)
        ));
    }
    if !p.values.is_empty() {
        let given = value.as_str().unwrap_or_default();
        if !p.values.iter().any(|v| v == given) {
            return Err(format!(
                "`{action}` argument `{}` must be one of {}, and another string arrived",
                p.name,
                listed(&p.values),
            ));
        }
    }
    if let (Some(item), Some(list)) = (p.items, value.as_array()) {
        if !PARAM_TYPES.contains(&item) {
            return Err(undefined(action, p, &format!("an array of {item}")));
        }
        for (i, v) in list.iter().enumerate() {
            if !is(item, v) {
                return Err(format!(
                    "`{action}` argument `{}` must be {}, and `{}[{i}]` is {}",
                    p.name,
                    wanted(p),
                    p.name,
                    arrived(item, v)
                ));
            }
        }
    }
    Ok(())
}

/// `args` as the handler receives them: what the caller sent, with every declared default filled
/// in for an argument it left out (or sent as `null`). Borrowed untouched when the action
/// declares no defaults, which is every action written before defaults existed.
pub fn with_defaults<'a>(spec: &Action, args: &'a Value) -> Cow<'a, Value> {
    if spec.params.iter().all(|p| p.default.is_none()) {
        return Cow::Borrowed(args);
    }
    let mut filled = if args.is_object() { args.clone() } else { Value::Object(Default::default()) };
    for p in &spec.params {
        let Some(default) = &p.default else { continue };
        if filled.get(&p.name).map_or(true, Value::is_null) {
            filled[&p.name] = default.clone();
        }
    }
    Cow::Owned(filled)
}

/// What is wrong with the way `spec` declares itself, as sentences for its author: a type this
/// dispatch does not know, an enum on something that is not a string, a default of the wrong
/// type, a name declared twice, a grade off the ladder. Empty for a sound declaration.
///
/// Nothing refuses on this — a call to a badly declared action is refused by the dispatch on its
/// own terms, closed — but an author's test can assert it is empty, and a surface logs it once
/// when the action is added.
pub fn declaration_problems(spec: &Action) -> Vec<String> {
    let mut problems = Vec::new();
    let name = spec.name.as_str();
    if yantrik_ipc_transport::gate::grade(spec.permission).is_none() {
        problems.push(format!(
            "`{name}` is graded `{}`, which is not a level this OS defines ({}); every call to it \
             will be refused",
            spec.permission,
            yantrik_ipc_transport::gate::LADDER.join(" < ")
        ));
    }
    for (i, p) in spec.params.iter().enumerate() {
        if spec.params[..i].iter().any(|q| q.name == p.name) {
            problems.push(format!("`{name}` declares argument `{}` twice", p.name));
        }
        if !PARAM_TYPES.contains(&p.kind) {
            problems.push(undefined(name, p, p.kind));
            continue;
        }
        if !p.values.is_empty() && p.kind != "string" {
            problems.push(format!(
                "`{name}` argument `{}` lists values but is declared `{}`; only a string can be one of a list",
                p.name, p.kind
            ));
        }
        match (p.kind, p.items) {
            ("array", Some(item)) if !PARAM_TYPES.contains(&item) => {
                problems.push(undefined(name, p, &format!("an array of {item}")));
            }
            ("array", _) | (_, None) => {}
            (kind, Some(_)) => problems.push(format!(
                "`{name}` argument `{}` declares an item type but is declared `{kind}`, not an array",
                p.name
            )),
        }
        if let Some(default) = &p.default {
            if let Err(why) = check_value(name, p, default) {
                problems.push(format!("the default is wrong: {why}"));
            }
        }
    }
    problems
}

fn undefined(action: &str, p: &Param, declared: &str) -> String {
    format!(
        "`{action}` argument `{}` is declared as {}, which is not a type this OS defines ({}), \
         so it was not run.",
        p.name,
        if declared.starts_with("an array of ") { declared.to_string() } else { format!("`{declared}`") },
        PARAM_TYPES.join(", ")
    )
}

/// Whether `value` is of JSON Schema type `kind`, with `integer` read as the JSON token was
/// written (see the module's type rules).
fn is(kind: &str, value: &Value) -> bool {
    match kind {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        _ => false,
    }
}

/// What the declaration asks for, as a reader would say it.
fn wanted(p: &Param) -> String {
    match (p.kind, p.items) {
        ("array", Some(item)) => format!("an array of {}", plural(item)),
        (kind, _) => singular(kind).to_string(),
    }
}

fn singular(kind: &str) -> &'static str {
    match kind {
        "string" => "a string",
        "number" => "a number",
        "integer" => "an integer",
        "boolean" => "a boolean",
        "object" => "an object",
        "array" => "an array",
        _ => "a value of an undefined type",
    }
}

fn plural(kind: &str) -> &'static str {
    match kind {
        "string" => "strings",
        "number" => "numbers",
        "integer" => "integers",
        "boolean" => "booleans",
        "object" => "objects",
        "array" => "arrays",
        _ => "values of an undefined type",
    }
}

/// What arrived, as a reader would say it: its kind, never its value (see the module docs). A
/// number where an integer was wanted says what was wrong with it, which is all the correction
/// needs.
fn arrived(wanted: &str, value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) if wanted == "integer" => "a number with a fraction",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn listed(values: &[String]) -> String {
    values.iter().map(|v| format!("`{v}`")).collect::<Vec<_>>().join(", ")
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one(p: Param) -> Action {
        Action::new("act", "An action with one argument").arg(p)
    }

    fn refusal(p: Param, value: Value) -> String {
        check_arguments(&one(p), &json!({ "x": value })).expect_err("refused")
    }

    fn accepts(p: Param, value: Value) -> bool {
        check_arguments(&one(p), &json!({ "x": value })).is_ok()
    }

    #[test]
    fn a_string_is_a_string_and_nothing_else_is() {
        assert!(accepts(Param::text("x"), json!("hello")));
        assert!(accepts(Param::text("x"), json!("")));
        assert_eq!(
            refusal(Param::text("x"), json!(67)),
            "`act` argument `x` must be a string, and a number arrived"
        );
        assert_eq!(
            refusal(Param::text("x"), json!({"id": "evt-3"})),
            "`act` argument `x` must be a string, and an object arrived"
        );
        assert_eq!(refusal(Param::text("x"), json!(true)), "`act` argument `x` must be a string, and a boolean arrived");
    }

    #[test]
    fn a_number_is_any_number() {
        assert!(accepts(Param::number("x"), json!(3)));
        assert!(accepts(Param::number("x"), json!(-3.25)));
        assert!(accepts(Param::number("x"), json!(1e3)));
        assert_eq!(
            refusal(Param::number("x"), json!("1024")),
            "`act` argument `x` must be a number, and a string arrived"
        );
    }

    /// A number is never quoted back: a caller's number may be a PIN or a year of birth, and a
    /// refusal is logged, shown and handed to a model.
    #[test]
    fn a_refusal_names_the_kind_that_arrived_and_never_the_value() {
        let err = refusal(Param::text("x"), json!(4921));
        assert!(!err.contains("4921"), "{err}");
        let err = refusal(Param::integer("x"), json!(19.84));
        assert!(!err.contains("19.84") && err.ends_with("a number with a fraction arrived"), "{err}");
        let err = refusal(Param::array("x", "string"), json!(["a", 4921]));
        assert!(!err.contains("4921"), "{err}");
    }

    #[test]
    fn an_integer_is_a_number_written_whole() {
        assert!(accepts(Param::integer("x"), json!(42)));
        assert!(accepts(Param::integer("x"), json!(-1)));
        assert!(accepts(Param::integer("x"), json!(u64::MAX)));
        assert_eq!(
            refusal(Param::integer("x"), json!(3.5)),
            "`act` argument `x` must be an integer, and a number with a fraction arrived"
        );
        // Whole in value but not as written: a handler's `as_u64` finds nothing in it.
        let written_as_float: Value = serde_json::from_str("3.0").unwrap();
        assert!(!accepts(Param::integer("x"), written_as_float.clone()));
        assert!(written_as_float.as_u64().is_none(), "the reason for the rule");
        assert!(!accepts(Param::integer("x"), json!("42")));
    }

    #[test]
    fn a_flag_is_true_or_false() {
        assert!(accepts(Param::flag("x"), json!(false)));
        // `"true"` read with `as_bool().unwrap_or(false)` used to be quietly false.
        assert_eq!(
            refusal(Param::flag("x"), json!("true")),
            "`act` argument `x` must be a boolean, and a string arrived"
        );
        assert!(!accepts(Param::flag("x"), json!(1)));
    }

    #[test]
    fn an_enum_is_one_of_its_values_and_says_which_they_are() {
        let urgency = || Param::one_of("x", &["low", "normal", "critical"]);
        assert!(accepts(urgency(), json!("critical")));
        assert_eq!(
            refusal(urgency(), json!("urgent")),
            "`act` argument `x` must be one of `low`, `normal`, `critical`, and another string arrived"
        );
        // Case is part of the value: `Low` is not `low`.
        assert!(!accepts(urgency(), json!("Low")));
        // Not a string at all is the type's refusal, not the list's.
        assert_eq!(refusal(urgency(), json!(2)), "`act` argument `x` must be a string, and a number arrived");
        // What the caller sent is never quoted back: it may be anything, including a secret.
        assert!(!refusal(urgency(), json!("hunter2")).contains("hunter2"));
    }

    #[test]
    fn an_array_is_checked_item_by_item() {
        let paths = || Param::array("x", "string");
        assert!(accepts(paths(), json!([])));
        assert!(accepts(paths(), json!(["/a", "/b"])));
        assert_eq!(
            refusal(paths(), json!(["/a", "/b", 3])),
            "`act` argument `x` must be an array of strings, and `x[2]` is a number"
        );
        assert_eq!(
            refusal(paths(), json!("/a")),
            "`act` argument `x` must be an array of strings, and a string arrived"
        );
        assert!(accepts(Param::array("x", "integer"), json!([1, 2, 3])));
        assert!(!accepts(Param::array("x", "integer"), json!([1, 2.5])));
        assert!(accepts(Param::array("x", "object"), json!([{"a": 1}])));
    }

    #[test]
    fn an_object_is_an_object() {
        assert!(accepts(Param::object("x"), json!({})));
        assert!(accepts(Param::object("x"), json!({"id": "evt-3", "confirm": true})));
        // The shell's `args_json`, sent as the string a raw caller used to send.
        assert_eq!(
            refusal(Param::object("x"), json!(r#"{"id":"evt-3"}"#)),
            "`act` argument `x` must be an object, and a string arrived"
        );
        assert!(!accepts(Param::object("x"), json!([1])));
    }

    #[test]
    fn null_is_leaving_an_optional_argument_out_and_a_wrong_value_for_a_required_one() {
        assert!(accepts(Param::integer("x").optional(), Value::Null));
        assert!(accepts(Param::one_of("x", &["a"]).optional(), Value::Null));
        assert_eq!(refusal(Param::integer("x"), Value::Null), "`act` argument `x` must be an integer, and null arrived");
    }

    #[test]
    fn the_first_mistake_in_declared_order_is_the_answer() {
        let spec = Action::new("move", "Move").arg(Param::integer("from")).arg(Param::integer("to"));
        let err = check_arguments(&spec, &json!({ "to": "b", "from": "a" })).unwrap_err();
        assert!(err.starts_with("`move` argument `from`"), "{err}");
    }

    #[test]
    fn presence_and_names_are_checked_before_types() {
        // A missing argument and a mistyped one: the missing one is named, as it always was.
        let spec = Action::new("open", "Open").arg(Param::text("title")).arg(Param::integer("line").optional());
        assert_eq!(check_arguments(&spec, &json!({ "line": "x" })).unwrap_err(), "`open` needs argument `title`");
        // An undeclared one and a mistyped one: the undeclared one is named.
        let err = check_arguments(&spec, &json!({ "title": 1, "colour": "red" })).unwrap_err();
        assert_eq!(err, "`open` has no argument `colour`; it takes: title, line");
    }

    #[test]
    fn arguments_that_are_not_an_object_are_refused_as_that() {
        let spec = Action::new("open", "Open").arg(Param::text("title").optional());
        assert_eq!(
            check_arguments(&spec, &json!(["a"])).unwrap_err(),
            "`open` takes its arguments as an object of named values, and an array arrived"
        );
        assert!(check_arguments(&spec, &Value::Null).is_ok(), "null args are no args");
    }

    #[test]
    fn a_type_this_os_does_not_define_refuses_every_call() {
        let mut p = Param::text("x");
        p.kind = "strnig";
        let err = refusal(p, json!("hello"));
        assert!(err.contains("`strnig`") && err.contains("not a type this OS defines"), "{err}");
        let err = refusal(Param::array("x", "strnig"), json!(["a"]));
        assert!(err.contains("an array of strnig") && err.contains("not a type this OS defines"), "{err}");
    }

    #[test]
    fn a_default_is_what_the_handler_gets_for_an_argument_left_out() {
        let spec = Action::new("export", "Export")
            .arg(Param::one_of("format", &["pdf", "png"]).default("pdf"))
            .arg(Param::integer("dpi").default(150))
            .arg(Param::text("path").optional());
        assert_eq!(*with_defaults(&spec, &json!({})), json!({"format": "pdf", "dpi": 150}));
        assert_eq!(
            *with_defaults(&spec, &json!({"format": "png", "dpi": null, "path": "/tmp/x"})),
            json!({"format": "png", "dpi": 150, "path": "/tmp/x"})
        );
        assert_eq!(*with_defaults(&spec, &Value::Null), json!({"format": "pdf", "dpi": 150}));

        // An action with no defaults hands over exactly what arrived, null and all.
        let plain = Action::new("open", "Open").arg(Param::text("path").optional());
        let args = json!({"path": null});
        assert!(matches!(with_defaults(&plain, &args), Cow::Borrowed(_)));
    }

    #[test]
    fn a_declaration_that_cannot_be_checked_is_named_for_its_author() {
        assert!(declaration_problems(&Action::new("ok", "Fine").arg(Param::integer("n").default(3))).is_empty());

        let spec = Action::new("bad", "Everything wrong")
            .risk("catastrophic")
            .arg(Param::integer("n").default("three"))
            .arg(Param::text("n"))
            .arg(Param::array("tags", "strnig"));
        let problems = declaration_problems(&spec);
        assert!(problems.iter().any(|p| p.contains("`catastrophic`")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("the default is wrong") && p.contains("must be an integer")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("declares argument `n` twice")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("an array of strnig")), "{problems:?}");

        let mut enum_on_a_number = Param::integer("n");
        enum_on_a_number.values = vec!["1".into()];
        assert!(!declaration_problems(&Action::new("x", "x").arg(enum_on_a_number)).is_empty());
    }
}
