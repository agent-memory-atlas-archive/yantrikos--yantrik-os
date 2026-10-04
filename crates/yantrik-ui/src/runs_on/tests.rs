use super::identity::{from_reported, from_url, ProviderRef};
use super::*;

fn mind(id: &str, name: &str, detail: Option<&str>, answering: bool) -> MindFact {
    MindFact { id: id.into(), name: name.into(), detail: detail.map(String::from), answering, builtin: false }
}

/// VM 520 on 30 Sep 2026, verbatim: every mind's own line, and the companion from config.yaml
/// with nothing saved in Settings.
fn vm_520() -> (Vec<MindFact>, CompanionFact) {
    let minds = vec![
        MindFact { id: "companion".into(), name: "Yantrik Companion".into(), detail: None, answering: false, builtin: true },
        mind("deepseek", "DeepSeek", Some("deepseek-v4.1-flash \u{b7} ollama.com"), false),
        mind("hermes", "Hermes Agent", Some("Hermes 0.14.0 \u{b7} deepseek-v4.1-flash"), false),
        mind("mind", "Yantrik Mind", Some("ollama-cloud:deepseek-v4.1-flash"), true),
        mind("openclaw", "OpenClaw", Some("ollama-cloud/kimi-k3 \u{b7} OpenClaw 2026.9.1"), false),
        mind("pi", "Pi", Some("ollamacloud/deepseek-v4.1-flash \u{b7} pi 0.87.0"), false),
    ];
    let companion = CompanionFact {
        base_url: "https://aig.mycluster.cyou/v1".into(),
        model: "qwen3.8:27b".into(),
        source: "config.yaml".into(),
        provider_name: String::new(),
    };
    (minds, companion)
}

#[test]
fn vm_520_every_mind_says_what_it_runs_on_and_where_that_is_set() {
    let (minds, companion) = vm_520();
    let rows = resolve(&minds, Some(&companion));
    let got: Vec<(String, &str, String, String)> = rows.iter().map(|r| (r.name.clone(), r.state(), r.runs_on(), r.source())).collect();
    let want = [
        ("Yantrik Mind", "Answering", "Ollama Cloud \u{b7} deepseek-v4.1-flash", "its own settings \u{b7} as it reported"),
        ("Yantrik Companion", "Built in", "Custom endpoint \u{b7} aig.mycluster.cyou \u{b7} qwen3.8:27b", "set in /opt/yantrik/config.yaml"),
        ("DeepSeek", "Attached", "Ollama Cloud \u{b7} deepseek-v4.1-flash", "its own settings \u{b7} as it reported"),
        ("Hermes Agent", "Attached", "provider not reported \u{b7} deepseek-v4.1-flash", "its own settings \u{b7} as it reported"),
        ("OpenClaw", "Attached", "Ollama Cloud \u{b7} kimi-k3", "its own settings \u{b7} as it reported"),
        ("Pi", "Attached", "Ollama Cloud \u{b7} deepseek-v4.1-flash", "its own settings \u{b7} as it reported"),
    ];
    let want: Vec<(String, &str, String, String)> = want.iter().map(|(a, b, c, d)| (a.to_string(), *b, c.to_string(), d.to_string())).collect();
    assert_eq!(got, want);
    assert_eq!(
        summary(&rows),
        "Ollama Cloud runs 4 minds. Custom endpoint \u{b7} aig.mycluster.cyou runs 1 mind. 1 mind does not say what it runs on. No mind here runs on a signed-in account."
    );
    // The thing that started this: nothing here may name Claude, whose sign-in no mind uses.
    for r in &rows {
        assert!(!r.runs_on().contains("Claude") && !r.source().contains("Claude"), "{r:?}");
    }
}

#[test]
fn a_provider_is_never_read_from_a_model_name() {
    // "deepseek-v4.1-flash" holds the catalogue id `deepseek`; DeepSeek does not run these minds.
    assert_eq!(from_reported("deepseek-v4.1-flash"), (ProviderRef::NotReported, Some("deepseek-v4.1-flash".into())));
    assert_eq!(from_reported("qwen3.5:9b").0, ProviderRef::NotReported, "a model with a tag is not provider:model");
    assert_eq!(from_reported("qwen3.5:9b").1.as_deref(), Some("qwen3.5:9b"));
    assert_eq!(from_reported("qwen2.5 on node1").0, ProviderRef::NotReported);
    // An explicit prefix that IS a provider does name it.
    assert_eq!(from_reported("deepseek:deepseek-chat").0.label(), "DeepSeek");
}

#[test]
fn four_spellings_are_one_provider() {
    for text in [
        "ollama-cloud:deepseek-v4.1-flash",
        "deepseek-v4.1-flash \u{b7} ollama.com",
        "ollamacloud/deepseek-v4.1-flash \u{b7} pi 0.87.0",
        "ollama-cloud/kimi-k3 \u{b7} OpenClaw 2026.9.1",
        "ollama_cloud/x",
        "https://ollama.com/v1",
    ] {
        let (p, _) = from_reported(text);
        assert_eq!(p.label(), "Ollama Cloud", "{text}");
        assert_eq!(p.key().as_deref(), Some("ollama-cloud"), "{text}");
    }
}

#[test]
fn addresses_are_named_by_the_catalogue_or_by_their_host() {
    assert_eq!(from_url("https://integrate.api.nvidia.com/v1").label(), "NVIDIA NIM");
    assert_eq!(from_url("http://localhost:11434/v1").label(), "Ollama");
    assert_eq!(from_url("http://192.168.4.35:11434/v1"), ProviderRef::Local("192.168.4.35:11434".into()), "another machine's port is not \"Ollama\"");
    assert_eq!(from_url("https://aig.mycluster.cyou/v1"), ProviderRef::Custom("aig.mycluster.cyou".into()));
    assert_eq!(from_reported("192.168.4.35:11434").0, ProviderRef::Local("192.168.4.35:11434".into()));
    assert_eq!(from_url(""), ProviderRef::NotReported);
}

#[test]
fn a_companion_with_no_address_says_nothing_is_set_up_and_a_saved_one_is_named() {
    let minds = [MindFact { id: "companion".into(), name: "Yantrik Companion".into(), detail: None, answering: true, builtin: true }];
    let rows = resolve(&minds, None);
    assert_eq!((rows[0].runs_on().as_str(), rows[0].source().as_str()), ("Nothing set up", "add a provider under Providers"));
    let saved = CompanionFact { base_url: "https://integrate.api.nvidia.com/v1".into(), model: "nvidia/x".into(), source: "saved provider".into(), provider_name: "NIM work".into() };
    let rows = resolve(&minds, Some(&saved));
    assert_eq!(rows[0].runs_on(), "NVIDIA NIM \u{b7} nvidia/x");
    assert_eq!(rows[0].source(), "your saved provider \u{201c}NIM work\u{201d}");
    assert!(!summary(&rows).contains("does not say"), "the companion with an address is not silent");
}

#[test]
fn a_mind_that_says_nothing_is_not_given_a_provider() {
    let rows = resolve(&[mind("x", "X", None, false)], None);
    assert_eq!(rows[0].runs_on(), "provider not reported");
    assert_eq!(rows[0].source(), "its own settings \u{b7} as it reported");
}

#[test]
fn vm_520_the_providers_in_use_are_listed_though_none_is_saved() {
    let (minds, companion) = vm_520();
    let rows = resolve(&minds, Some(&companion));
    let in_use = in_use_not_saved(&rows, Some(&companion), &[]);
    assert_eq!(in_use.len(), 2, "{in_use:#?}");
    let ollama = &in_use[0];
    assert_eq!(ollama.label, "Ollama Cloud");
    assert_eq!(ollama.used_by, ["Yantrik Mind", "DeepSeek", "OpenClaw", "Pi"]);
    assert_eq!(ollama.models, ["deepseek-v4.1-flash", "kimi-k3"]);
    assert_eq!(ollama.where_set, "each mind keeps its own key in its own settings");
    assert_eq!(
        (ollama.preset.as_str(), ollama.base_url.as_str(), ollama.model.as_str()),
        ("ollama-cloud", "https://ollama.com/v1", "deepseek-v4.1-flash")
    );
    let aig = &in_use[1];
    assert_eq!(aig.label, "Custom endpoint \u{b7} aig.mycluster.cyou");
    assert_eq!(aig.used_by, ["Yantrik Companion"]);
    assert_eq!(aig.where_set, "set in /opt/yantrik/config.yaml \u{b7} used by the built-in companion");
    assert_eq!(
        (aig.preset.as_str(), aig.base_url.as_str(), aig.model.as_str()),
        ("custom", "https://aig.mycluster.cyou/v1", "qwen3.8:27b")
    );
    // Hermes names no provider: it is in no row rather than guessed into one.
    assert!(!in_use.iter().any(|u| u.used_by.iter().any(|n| n == "Hermes Agent")));
}

/// The line under the chat composer: where the words go, from the same facts as every other
/// surface, and nothing at all when the provider is not known.
#[test]
fn the_composer_line_says_where_words_go_or_nothing() {
    let (minds, companion) = vm_520();
    let rows = resolve(&minds, Some(&companion));
    let line = |name: &str| rows.iter().find(|r| r.name == name).unwrap().destination();
    assert_eq!(line("Yantrik Mind"), "Yantrik Mind \u{b7} deepseek-v4.1-flash \u{b7} online, via Ollama Cloud");
    assert_eq!(line("Yantrik Companion"), "Yantrik Companion \u{b7} qwen3.8:27b \u{b7} online, via aig.mycluster.cyou");
    // Hermes names a model and no provider: no line rather than a guessed one.
    assert_eq!(line("Hermes Agent"), "");

    let companion_at = |url: &str| {
        let c = CompanionFact { base_url: url.into(), model: "qwen3.5:9b".into(), source: "config.yaml".into(), provider_name: String::new() };
        resolve(&minds[..1], Some(&c))[0].destination()
    };
    assert_eq!(companion_at("http://localhost:11434/v1"), "Yantrik Companion \u{b7} qwen3.5:9b \u{b7} on this machine");
    assert_eq!(companion_at("http://127.0.0.1:8341/v1"), "Yantrik Companion \u{b7} qwen3.5:9b \u{b7} on this machine");
    assert_eq!(companion_at("http://192.168.4.35:11434/v1"), "Yantrik Companion \u{b7} qwen3.5:9b \u{b7} on this network, at 192.168.4.35:11434");
    assert_eq!(companion_at(""), "", "nothing set up, nothing said");

    // A local runtime named only in a mind's own words could be on any machine.
    let ollama = resolve(&[mind("x", "X", Some("ollama:qwen3.5:9b"), true)], None);
    assert_eq!(ollama[0].destination(), "X \u{b7} qwen3.5:9b \u{b7} via Ollama");
}

#[test]
fn a_saved_provider_is_not_listed_again_as_in_use() {
    let (minds, companion) = vm_520();
    let rows = resolve(&minds, Some(&companion));
    let in_use = in_use_not_saved(&rows, Some(&companion), &["https://ollama.com/v1".to_string()]);
    let labels: Vec<&str> = in_use.iter().map(|u| u.label.as_str()).collect();
    assert_eq!(labels, ["Custom endpoint \u{b7} aig.mycluster.cyou"]);
}
