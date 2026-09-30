use super::*;

struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("handoff-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".config/yantrik")).unwrap();
        Home(dir)
    }
    fn deepseek(&self) -> PathBuf {
        self.0.join(".config/yantrik/deepseek.json")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const KEY: &str = "nvapi-SECRET-4f3e2d1c";

fn nim() -> ProviderStoreEntry {
    ProviderStoreEntry {
        id: "p-nim".into(),
        name: "NVIDIA NIM".into(),
        provider_type: "nvidia-nim".into(),
        base_url: "https://integrate.api.nvidia.com/v1".into(),
        api_key: Some(KEY.into()),
        auth_type: "bearer".into(),
        is_primary: true,
        is_fallback: false,
        model: "nvidia/nemotron-3-super-120b-a12b".into(),
    }
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn deepseek_is_given_the_address_model_and_key_and_keeps_the_rest_of_its_file() {
    let home = Home::new("merge");
    std::fs::write(home.deepseek(), r#"{"base_url":"https://api.deepseek.com/v1","api_key_env":"DEEPSEEK_API_KEY","max_steps":12,"decider":{"kind":"jev"}}"#).unwrap();
    let plan = adapter_for("deepseek").unwrap().plan(&home.0, &nim()).unwrap();
    apply(&home.0, &plan).unwrap();

    let written = json(&home.deepseek());
    assert_eq!(written["base_url"], "https://integrate.api.nvidia.com/v1");
    assert_eq!(written["model"], "nvidia/nemotron-3-super-120b-a12b");
    assert_eq!(written["api_key"], KEY, "the key is in the file the harness reads");
    assert!(written.get("api_key_env").is_none(), "a variable naming a key it no longer uses is dropped");
    assert_eq!(written["max_steps"], 12, "the person's other settings stay");
    assert_eq!(written["decider"]["kind"], "jev");
}

#[test]
fn the_key_is_in_the_600_file_and_nowhere_the_person_or_a_log_can_read_it() {
    let home = Home::new("secret");
    let plan = adapter_for("deepseek").unwrap().plan(&home.0, &nim()).unwrap();
    let card = plan.card(&home.0);
    assert!(!card.contains(KEY) && card.contains("~/.config/yantrik/deepseek.json"), "{card}");
    assert!(!format!("{plan:?}").contains(KEY), "not even in Debug");
    let m = apply(&home.0, &plan).unwrap();
    let marker_text = std::fs::read_to_string(marker_path(&home.0, "deepseek")).unwrap();
    assert!(!marker_text.contains(KEY), "{marker_text}");
    assert_eq!(m.provider_name, "NVIDIA NIM");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [home.deepseek(), marker_path(&home.0, "deepseek")] {
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} is {mode:o}", p.display());
        }
    }
}

#[test]
fn revert_puts_back_the_persons_own_file_even_after_two_assignments() {
    let home = Home::new("revert");
    let original = r#"{"base_url":"https://api.deepseek.com/v1","api_key_env":"DEEPSEEK_API_KEY"}"#;
    std::fs::write(home.deepseek(), original).unwrap();
    let plan = adapter_for("deepseek").unwrap().plan(&home.0, &nim()).unwrap();
    apply(&home.0, &plan).unwrap();
    // A second assignment must not keep the first assignment as "the original".
    let mut other = nim();
    other.name = "OpenRouter".into();
    other.base_url = "https://openrouter.ai/api/v1".into();
    other.model = "x/y".into();
    let plan = adapter_for("deepseek").unwrap().plan(&home.0, &other).unwrap();
    apply(&home.0, &plan).unwrap();
    assert!(row_line(&home.0, "deepseek").contains("OpenRouter · x/y"));

    revert(&home.0, "deepseek").unwrap();
    assert_eq!(std::fs::read_to_string(home.deepseek()).unwrap(), original);
    assert!(marker(&home.0, "deepseek").is_none());
    assert_eq!(row_line(&home.0, "deepseek"), "Provider: its own settings");
}

#[test]
fn a_harness_with_no_file_of_its_own_has_the_written_one_removed_on_revert() {
    let home = Home::new("fresh");
    let plan = adapter_for("deepseek").unwrap().plan(&home.0, &nim()).unwrap();
    apply(&home.0, &plan).unwrap();
    assert!(home.deepseek().exists());
    revert(&home.0, "deepseek").unwrap();
    assert!(!home.deepseek().exists(), "there was nothing before, so there is nothing after");
}

#[test]
fn a_provider_with_no_model_chosen_is_refused_with_what_to_do() {
    let home = Home::new("nomodel");
    let mut p = nim();
    p.model.clear();
    let err = adapter_for("deepseek").unwrap().plan(&home.0, &p).unwrap_err();
    assert!(err.contains("pick one"), "{err}");
    assert!(!home.deepseek().exists(), "nothing written");
}

#[test]
fn a_file_that_is_not_json_is_left_alone() {
    let home = Home::new("garbage");
    std::fs::write(home.deepseek(), "not json at all").unwrap();
    assert!(adapter_for("deepseek").unwrap().plan(&home.0, &nim()).is_err());
    assert_eq!(std::fs::read_to_string(home.deepseek()).unwrap(), "not json at all");
}

#[test]
fn only_harnesses_with_an_adapter_offer_it() {
    assert!(adapter_for("deepseek").is_some());
    assert!(adapter_for("hermes").is_none(), "not yet");
    let home = Home::new("line");
    assert_eq!(row_line(&home.0, "hermes"), "");
}
