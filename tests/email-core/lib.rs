//! Email's rules, tested without a mailbox, a socket or a desktop.
//!
//! The faults these cover were each invisible from one side alone. The app read a dead service as
//! an unconfigured machine, so an audit of this OS concluded it had no mail account when what it
//! had was a service nothing ever started. `open_message which=1` answered `nothing in this
//! folder matches ""`, because a JSON number read through `as_str()` is the empty string. Closing
//! the composer discarded what was in it. And a password typed into the setup form had to be kept
//! out of every sentence this app can produce, which is the kind of rule that only holds if
//! something checks it.
//!
//! Three modules are included directly, all of them free of Slint, sockets and the network:

/// The app's side: which of three states it is in, which message a caller means, the draft, and
/// what the setup form makes of what was typed.
#[path = "../../apps/email/src/state.rs"]
pub mod state;

/// The service's side: where accounts live, how they are picked, and how they are written.
#[path = "../../services/email-service/src/accounts.rs"]
pub mod accounts;

/// The service's side: turning a mail server's refusal into a sentence that names it.
#[path = "../../services/email-service/src/connect.rs"]
pub mod connect;

#[cfg(test)]
mod tests {
    use super::accounts::{self, Account};
    use super::connect::{self, Attempt};
    use super::state::{self, Draft, MailState, MessageRow, Triage};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use yantrik_ipc_contracts::email::{
        without_secret, AccountSettings, AccountsResult, EmailAccountSummary,
    };

    static ID: AtomicUsize = AtomicUsize::new(0);

    /// The password used everywhere below. Nothing this app produces may contain it, and the
    /// last block of tests is nothing but looking for it.
    const SENTINEL: &str = "hunter2-SENTINEL-do-not-print";

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "email-test-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        /// The accounts file, under a directory the store has to create for itself: the real one
        /// is `~/.config/yantrik/email.json` and `~/.config/yantrik` may not exist.
        fn config(&self) -> PathBuf {
            self.0.join("config/yantrik/email.json")
        }

        fn draft(&self) -> PathBuf {
            self.0.join("share/yantrik/email/draft.json")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn settings() -> AccountSettings {
        AccountSettings {
            email: "someone@example.com".into(),
            display_name: "Someone".into(),
            provider: "advanced".into(),
            imap_server: "imap.example.com".into(),
            imap_port: 993,
            smtp_server: "smtp.example.com".into(),
            smtp_port: 587,
            password: SENTINEL.into(),
        }
    }

    fn summary(email: &str) -> EmailAccountSummary {
        EmailAccountSummary {
            id: accounts::id_for(email),
            email: email.into(),
            display_name: String::new(),
            provider: "gmail".into(),
            imap_server: "imap.gmail.com".into(),
            imap_port: 993,
            smtp_server: "smtp.gmail.com".into(),
            smtp_port: 587,
            uses_oauth: false,
        }
    }

    fn rows() -> Vec<MessageRow> {
        vec![
            MessageRow::new("Launch review", "Priya Raman", "priya@lumen.dev"),
            MessageRow::new("Invoice R0093 for September", "Hetzner", "billing@hetzner.com"),
            MessageRow::new("Re: Launcher grid", "Ananya Sen", "ananya@lumen.dev"),
        ]
    }

    // ── The three states ─────────────────────────────────────────────
    //
    // The whole point of this file. A dead service and an unconfigured machine used to be one
    // picture, and an audit read that picture as the machine having no account.

    #[test]
    fn a_service_that_could_not_be_reached_is_not_an_absent_account() {
        let s = state::decide(Err("could not start the email service: Binary not found".into()));
        assert_eq!(s.service_word(), "unreachable");
        // Not Some(false). With nothing answering, the app has not been told either way.
        assert_eq!(s.has_account(), None);
        assert!(s.notice().contains("Binary not found"));
        assert!(!s.summary().to_lowercase().contains("no account"));
    }

    #[test]
    fn a_service_that_answered_with_no_accounts_says_so() {
        let s = state::decide(Ok(AccountsResult {
            accounts: Vec::new(),
            config_path: "/home/p/.config/yantrik/email.json".into(),
            secrets_are_plaintext: true,
        }));
        assert_eq!(s.service_word(), "up");
        assert_eq!(s.has_account(), Some(false));
        // Nothing is wrong, so nothing is said.
        assert_eq!(s.notice(), "");
        // And the summary names the file, so a person is told where an account would go.
        assert!(s.summary().contains("/home/p/.config/yantrik/email.json"));
    }

    #[test]
    fn a_service_holding_an_account_is_ready() {
        let s = state::decide(Ok(AccountsResult {
            accounts: vec![summary("someone@example.com")],
            config_path: "/tmp/email.json".into(),
            secrets_are_plaintext: true,
        }));
        assert_eq!(s.service_word(), "up");
        assert_eq!(s.has_account(), Some(true));
        assert_eq!(s.account_name(), "someone@example.com");
        assert_eq!(s.account_id(), "someone-example-com");
        assert_eq!(s.notice(), "");
    }

    #[test]
    fn the_three_states_are_three_different_sentences() {
        let down = state::decide(Err("no socket".into())).summary();
        let empty = state::decide(Ok(AccountsResult {
            accounts: Vec::new(),
            config_path: "/tmp/e.json".into(),
            secrets_are_plaintext: true,
        }))
        .summary();
        let ready = state::decide(Ok(AccountsResult {
            accounts: vec![summary("a@b.com")],
            config_path: "/tmp/e.json".into(),
            secrets_are_plaintext: true,
        }))
        .summary();
        assert_ne!(down, empty);
        assert_ne!(empty, ready);
        assert_ne!(down, ready);
    }

    #[test]
    fn an_unreachable_service_reports_no_account_store_and_no_account() {
        let s = MailState::Unreachable { reason: "refused".into() };
        assert_eq!(s.config_path(), "");
        assert_eq!(s.account_id(), "");
        assert_eq!(s.account_name(), "");
        assert!(!s.secrets_are_plaintext());
    }

    // ── Which message a caller means ─────────────────────────────────

    #[test]
    fn a_row_number_opens_that_row() {
        assert_eq!(state::resolve_which(&rows(), "1"), Ok(0));
        assert_eq!(state::resolve_which(&rows(), "3"), Ok(2));
        // Counting from one, the way `describe.messages` publishes it.
        assert_eq!(state::resolve_which(&rows(), "2"), Ok(1));
    }

    #[test]
    fn a_number_with_spaces_round_it_is_still_a_number() {
        assert_eq!(state::resolve_which(&rows(), "  2  "), Ok(1));
    }

    #[test]
    fn row_numbers_count_from_one() {
        for below in ["0", "-1"] {
            let e = state::resolve_which(&rows(), below).unwrap_err();
            assert!(e.contains("count from 1"), "{e}");
            assert!(!e.contains("\"\""), "{e}");
        }
    }

    #[test]
    fn a_number_is_a_row_number_and_is_not_then_tried_as_text() {
        // "9" is a substring of "Invoice R0093 for September". A caller asking for the ninth
        // message of a folder holding three must not be handed the second one.
        assert!(state::resolve_which(&rows(), "9").is_err());
        assert!(state::resolve_which(&rows(), "0093").is_err());
    }

    #[test]
    fn an_exact_subject_beats_a_partial_one() {
        let rows = vec![
            MessageRow::new("Launch review notes", "A", "a@x.com"),
            MessageRow::new("Launch", "B", "b@x.com"),
        ];
        assert_eq!(state::resolve_which(&rows, "Launch"), Ok(1));
    }

    #[test]
    fn a_partial_subject_a_sender_and_an_address_all_resolve() {
        assert_eq!(state::resolve_which(&rows(), "invoice"), Ok(1));
        assert_eq!(state::resolve_which(&rows(), "Ananya Sen"), Ok(2));
        assert_eq!(state::resolve_which(&rows(), "priya@lumen.dev"), Ok(0));
    }

    #[test]
    fn a_number_past_the_end_says_how_many_there_are() {
        let e = state::resolve_which(&rows(), "9").unwrap_err();
        assert!(e.contains("no message 9"), "{e}");
        assert!(e.contains('3'), "{e}");
        assert!(!e.contains("\"\""), "{e}");
    }

    #[test]
    fn asking_for_row_one_of_an_empty_folder_is_refused_without_a_quoted_nothing() {
        // The exact call the probe makes on a machine with no mail. It used to answer
        // `nothing in this folder matches ""`, which reports a search for nothing.
        let e = state::resolve_which(&[], "1").unwrap_err();
        assert!(e.contains("no message 1"), "{e}");
        assert!(e.contains("none"), "{e}");
        assert!(!e.contains("\"\""), "{e}");
    }

    #[test]
    fn text_that_matches_nothing_names_what_was_asked_for() {
        let e = state::resolve_which(&rows(), "zebra").unwrap_err();
        assert!(e.contains("zebra"), "{e}");
        assert!(!e.contains("\"\""), "{e}");
    }

    #[test]
    fn an_empty_which_is_refused_without_quoting_it() {
        for empty in ["", "   "] {
            let e = state::resolve_which(&rows(), empty).unwrap_err();
            assert!(e.contains("row number"), "{e}");
            assert!(!e.contains("\"\""), "{e}");
            assert!(!e.contains("\u{201c}\u{201d}"), "{e}");
        }
    }

    // ── The triage tabs ──────────────────────────────────────────────

    #[test]
    fn the_triage_tabs_are_three_and_filter_what_is_in_hand() {
        assert_eq!(Triage::from_index(0), Some(Triage::All));
        assert_eq!(Triage::from_index(1), Some(Triage::Unread));
        assert_eq!(Triage::from_index(2), Some(Triage::Flagged));
        // There is no fourth. Priority was a tab over a quality no message carries.
        assert_eq!(Triage::from_index(3), None);
        assert_eq!(Triage::from_index(-1), None);
    }

    #[test]
    fn each_tab_keeps_what_it_says_it_keeps() {
        // (is_read, is_flagged)
        assert!(Triage::All.keeps(true, false));
        assert!(Triage::All.keeps(false, true));
        assert!(Triage::Unread.keeps(false, false));
        assert!(!Triage::Unread.keeps(true, true));
        assert!(Triage::Flagged.keeps(true, true));
        assert!(!Triage::Flagged.keeps(false, false));
    }

    #[test]
    fn a_tab_index_survives_the_round_trip() {
        for t in [Triage::All, Triage::Unread, Triage::Flagged] {
            assert_eq!(Triage::from_index(t.index()), Some(t));
        }
    }

    // ── The draft ────────────────────────────────────────────────────

    #[test]
    fn a_draft_survives_being_written_and_read_back() {
        let f = Fixture::new();
        let draft = Draft {
            to: "priya@lumen.dev".into(),
            cc: "ananya@lumen.dev".into(),
            bcc: String::new(),
            subject: "Re: launch".into(),
            body: "Half a sentence and then the".into(),
        };
        state::save_draft(&f.draft(), &draft).unwrap();
        assert_eq!(state::load_draft(&f.draft()).unwrap(), Some(draft));
    }

    #[test]
    fn the_draft_directory_is_made_if_it_is_not_there() {
        let f = Fixture::new();
        assert!(!f.draft().parent().unwrap().exists());
        state::save_draft(&f.draft(), &Draft { body: "x".into(), ..Draft::default() }).unwrap();
        assert!(f.draft().exists());
    }

    #[test]
    fn no_draft_file_is_no_draft_and_not_an_error() {
        let f = Fixture::new();
        assert_eq!(state::load_draft(&f.draft()).unwrap(), None);
    }

    #[test]
    fn a_draft_of_nothing_is_not_a_draft() {
        let f = Fixture::new();
        let blank = Draft { to: "  ".into(), ..Draft::default() };
        assert!(blank.is_empty());
        state::save_draft(&f.draft(), &blank).unwrap();
        assert_eq!(state::load_draft(&f.draft()).unwrap(), None);
    }

    #[test]
    fn clearing_a_draft_that_is_not_there_is_not_an_error() {
        let f = Fixture::new();
        state::clear_draft(&f.draft()).unwrap();
        state::save_draft(&f.draft(), &Draft { body: "x".into(), ..Draft::default() }).unwrap();
        state::clear_draft(&f.draft()).unwrap();
        assert_eq!(state::load_draft(&f.draft()).unwrap(), None);
    }

    #[test]
    fn a_draft_file_that_is_not_a_draft_is_an_error_rather_than_a_shrug() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.draft().parent().unwrap()).unwrap();
        std::fs::write(f.draft(), "{ this is not json").unwrap();
        let e = state::load_draft(&f.draft()).unwrap_err();
        assert!(e.contains("not a draft"), "{e}");
    }

    // ── The setup form ───────────────────────────────────────────────

    #[test]
    fn a_provider_fills_in_its_own_servers() {
        let s = state::account_settings_from_form(
            "someone@gmail.com",
            SENTINEL,
            "Someone",
            "gmail",
            "",
            "993",
            "",
            "587",
        )
        .unwrap();
        assert_eq!(s.imap_server, "imap.gmail.com");
        assert_eq!(s.smtp_server, "smtp.gmail.com");
        assert_eq!(s.imap_port, 993);
        assert_eq!(s.smtp_port, 587);
    }

    #[test]
    fn every_named_provider_has_servers() {
        for provider in ["gmail", "outlook", "yahoo", "icloud"] {
            assert!(state::servers_for(provider).is_some(), "{provider}");
        }
        assert!(state::servers_for("advanced").is_none());
        assert!(state::servers_for("").is_none());
    }

    #[test]
    fn advanced_takes_what_was_typed() {
        let s = state::account_settings_from_form(
            "someone@example.com",
            SENTINEL,
            "",
            "advanced",
            " mail.example.com ",
            "1993",
            "smtp.example.com",
            "2587",
        )
        .unwrap();
        assert_eq!(s.imap_server, "mail.example.com");
        assert_eq!(s.imap_port, 1993);
        assert_eq!(s.smtp_port, 2587);
    }

    #[test]
    fn advanced_with_no_servers_is_refused_in_words() {
        let e = state::account_settings_from_form(
            "someone@example.com",
            SENTINEL,
            "",
            "advanced",
            "",
            "993",
            "",
            "587",
        )
        .unwrap_err();
        assert!(e.contains("Advanced"), "{e}");
    }

    #[test]
    fn an_address_that_is_not_one_is_refused() {
        for bad in ["", "someone", "someone@", "@example.com", "someone@localhost"] {
            assert!(
                state::account_settings_from_form(
                    bad, SENTINEL, "", "gmail", "", "993", "", "587"
                )
                .is_err(),
                "{bad} was accepted"
            );
        }
    }

    #[test]
    fn a_form_with_no_password_is_refused_rather_than_saved() {
        let e =
            state::account_settings_from_form("a@b.com", "", "", "gmail", "", "993", "", "587")
                .unwrap_err();
        assert!(e.to_lowercase().contains("password"), "{e}");
    }

    #[test]
    fn a_port_that_is_not_a_number_names_the_field() {
        let e = state::account_settings_from_form(
            "a@b.com", SENTINEL, "", "advanced", "i.b.com", "nine", "s.b.com", "587",
        )
        .unwrap_err();
        assert!(e.contains("IMAP"), "{e}");
        let e = state::account_settings_from_form(
            "a@b.com", SENTINEL, "", "advanced", "i.b.com", "993", "s.b.com", "0",
        )
        .unwrap_err();
        assert!(e.contains("SMTP"), "{e}");
    }

    #[test]
    fn both_halves_of_a_connection_test_are_reported() {
        let one_sided = state::test_summary(true, "IMAP ok", false, "SMTP refused");
        assert!(one_sided.contains("SMTP refused"), "{one_sided}");
        assert!(one_sided.to_lowercase().contains("not sent") || one_sided.contains("not sent"));
        let other = state::test_summary(false, "IMAP refused", true, "SMTP ok");
        assert!(other.contains("IMAP refused"), "{other}");
        let both = state::test_summary(true, "IMAP ok", true, "SMTP ok");
        assert!(both.starts_with("Signed in"), "{both}");
    }

    #[test]
    fn the_storage_note_says_plainly_what_happens_to_the_password() {
        let note = state::password_storage_note("/home/p/.config/yantrik/email.json", true);
        assert!(note.contains("clear text"), "{note}");
        assert!(note.contains("0600"), "{note}");
        assert!(note.contains("/home/p/.config/yantrik/email.json"), "{note}");
        // And stops saying it the day it stops being true.
        let other = state::password_storage_note("/tmp/e.json", false);
        assert!(!other.contains("clear text"), "{other}");
    }

    // ── Where accounts live ──────────────────────────────────────────

    #[test]
    fn an_account_survives_being_written_and_read_back() {
        let f = Fixture::new();
        let mut all = accounts::load(&f.config()).unwrap();
        assert!(all.is_empty());
        let id = accounts::upsert(&mut all, &settings());
        accounts::save(&f.config(), &all).unwrap();

        let back = accounts::load(&f.config()).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, id);
        assert_eq!(back[0].email, "someone@example.com");
        assert_eq!(back[0].imap_server, "imap.example.com");
        assert_eq!(back[0].password, SENTINEL);
    }

    #[test]
    fn saving_the_same_address_twice_edits_one_account() {
        let f = Fixture::new();
        let mut all = Vec::new();
        accounts::upsert(&mut all, &settings());
        let changed = AccountSettings { imap_server: "imap2.example.com".into(), ..settings() };
        accounts::upsert(&mut all, &changed);
        accounts::save(&f.config(), &all).unwrap();

        let back = accounts::load(&f.config()).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].imap_server, "imap2.example.com");
    }

    #[test]
    fn a_missing_accounts_file_is_no_accounts_and_not_an_error() {
        let f = Fixture::new();
        assert_eq!(accounts::load(&f.config()).unwrap().len(), 0);
    }

    #[test]
    fn an_accounts_file_that_is_broken_is_an_error_rather_than_no_account() {
        // This is the distinction the whole app turns on: a config with a typo in it must not
        // read as a machine nobody has configured.
        let f = Fixture::new();
        std::fs::create_dir_all(f.config().parent().unwrap()).unwrap();
        std::fs::write(f.config(), "{ not a list }").unwrap();
        let e = accounts::load(&f.config()).unwrap_err();
        assert!(e.contains("not a list of accounts"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn the_accounts_file_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let mut all = Vec::new();
        accounts::upsert(&mut all, &settings());
        accounts::save(&f.config(), &all).unwrap();

        let mode = std::fs::metadata(f.config()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the file holding a password was {mode:o}");
        let dir = std::fs::metadata(f.config().parent().unwrap()).unwrap().permissions().mode()
            & 0o777;
        assert_eq!(dir, 0o700, "the directory holding it was {dir:o}");
    }

    #[test]
    fn an_account_is_picked_by_id_by_address_or_by_being_the_only_one() {
        let mut all = Vec::new();
        accounts::upsert(&mut all, &settings());
        accounts::upsert(
            &mut all,
            &AccountSettings { email: "other@example.com".into(), ..settings() },
        );

        assert_eq!(accounts::pick(&all, Some("other-example-com")).unwrap().email, "other@example.com");
        assert_eq!(accounts::pick(&all, Some("OTHER@example.com")).unwrap().email, "other@example.com");
        // The bug: the app sent nothing, the service substituted "default", found no account
        // with that id, and answered "Unknown account: default" — which the app drew as having
        // no account at all.
        assert_eq!(accounts::pick(&all, Some("default")).unwrap().email, "someone@example.com");
        assert_eq!(accounts::pick(&all, None).unwrap().email, "someone@example.com");
        assert!(accounts::pick(&[], Some("default")).is_none());
    }

    #[test]
    fn an_id_is_derived_from_the_address_so_it_is_stable() {
        assert_eq!(accounts::id_for("Someone@Example.COM"), "someone-example-com");
        assert_eq!(accounts::id_for(""), "account");
        assert_eq!(accounts::id_for("@@@"), "account");
    }

    #[test]
    fn settings_that_cannot_work_are_refused_before_a_server_is_dialled() {
        accounts::refuse_bad_settings(&settings()).unwrap();
        for bad in [
            AccountSettings { email: "nope".into(), ..settings() },
            AccountSettings { password: String::new(), ..settings() },
            AccountSettings { imap_server: "  ".into(), ..settings() },
            AccountSettings { smtp_server: String::new(), ..settings() },
            AccountSettings { imap_port: 0, ..settings() },
        ] {
            assert!(accounts::refuse_bad_settings(&bad).is_err());
        }
    }

    #[test]
    fn a_summary_carries_no_secret_at_all() {
        let mut all = Vec::new();
        accounts::upsert(&mut all, &settings());
        let s = all[0].summary();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains(SENTINEL), "{json}");
        assert!(!json.contains("password"), "{json}");
        assert_eq!(accounts::summaries(&all).len(), 1);
    }

    // ── Naming a connection failure ──────────────────────────────────

    fn imap() -> Attempt<'static> {
        Attempt::new("IMAP", "imap.example.com", 993)
    }

    #[test]
    fn a_name_that_does_not_resolve_is_said_as_that() {
        let said = connect::name_failure(
            &imap(),
            "failed to lookup address information: Name or service not known",
            SENTINEL,
        );
        assert!(said.contains("no such host"), "{said}");
        assert!(said.contains("imap.example.com"), "{said}");
    }

    #[test]
    fn a_closed_port_is_said_as_that_and_names_the_port() {
        let said = connect::name_failure(&imap(), "Connection refused (os error 111)", SENTINEL);
        assert!(said.contains("refused"), "{said}");
        assert!(said.contains("993"), "{said}");
    }

    #[test]
    fn a_silent_server_is_a_timeout_not_a_generic_failure() {
        let said =
            connect::name_failure(&imap(), "connection timed out (os error 110)", SENTINEL);
        assert!(said.contains("timed out"), "{said}");
    }

    #[test]
    fn a_certificate_problem_is_said_as_tls() {
        let said = connect::name_failure(
            &imap(),
            "TLS handshake failed: certificate verify failed: self signed certificate",
            SENTINEL,
        );
        assert!(said.contains("TLS"), "{said}");
        assert!(said.contains("self signed"), "{said}");
    }

    #[test]
    fn a_refused_password_is_said_as_a_refused_sign_in() {
        // Gmail's actual reply to an account password where an app password is needed.
        let said = connect::name_failure(
            &imap(),
            "[AUTHENTICATIONFAILED] Invalid credentials (Failure)",
            SENTINEL,
        );
        assert!(said.contains("rejected"), "{said}");
        assert!(said.contains("AUTHENTICATIONFAILED"), "{said}");
    }

    #[test]
    fn a_smtp_refusal_names_smtp_and_its_port() {
        let said = connect::name_failure(
            &Attempt::new("SMTP", "smtp.example.com", 587),
            "535 5.7.8 Authentication credentials invalid",
            SENTINEL,
        );
        assert!(said.starts_with("SMTP smtp.example.com:587"), "{said}");
    }

    #[test]
    fn an_unfamiliar_reply_is_passed_through_in_the_servers_own_words() {
        let said = connect::name_failure(&imap(), "SERVERBUG: try again later", SENTINEL);
        assert!(said.contains("SERVERBUG"), "{said}");
    }

    #[test]
    fn a_paragraph_of_a_reply_becomes_one_line() {
        let said = connect::name_failure(
            &imap(),
            "Web login required.\nSee https://support.example.com/mail/answer/78754\n",
            SENTINEL,
        );
        assert!(!said.contains('\n'), "{said}");
        assert!(said.contains("Web login required"), "{said}");
    }

    #[test]
    fn a_very_long_reply_is_cut_rather_than_filling_the_strip() {
        let said = connect::name_failure(&imap(), &"x".repeat(1000), SENTINEL);
        assert!(said.chars().count() < 250, "{} chars", said.chars().count());
    }

    #[test]
    fn a_success_is_as_specific_as_a_failure() {
        let said = connect::name_success(&imap());
        assert!(said.contains("imap.example.com:993"), "{said}");
        assert!(said.contains("signed in"), "{said}");
    }

    // ── The password appears in nothing ──────────────────────────────
    //
    // The rule that only holds if something checks it. A mail server is free to quote back the
    // line it was sent, and one echo would put a credential in a notice, in `app.describe`, and
    // in the transcript a mind is reading.

    #[test]
    fn a_server_that_echoes_the_password_does_not_get_it_onto_the_screen() {
        let echoed = format!("BAD Invalid command: LOGIN someone@example.com {SENTINEL}");
        let said = connect::name_failure(&imap(), &echoed, SENTINEL);
        assert!(!said.contains(SENTINEL), "{said}");
        assert!(said.contains("<redacted>"), "{said}");
    }

    #[test]
    fn redaction_of_an_empty_secret_does_not_shred_the_message() {
        // `replace("", …)` splices the marker between every character.
        assert_eq!(without_secret("hello", ""), "hello");
    }

    #[test]
    fn the_debug_of_settings_is_not_a_way_to_print_a_password() {
        let printed = format!("{:?}", settings());
        assert!(!printed.contains(SENTINEL), "{printed}");
        assert!(printed.contains("<redacted>"), "{printed}");
        // And the address is still there, so the redaction has not made it useless.
        assert!(printed.contains("someone@example.com"), "{printed}");
    }

    #[test]
    fn the_debug_of_a_stored_account_is_not_either() {
        let mut all = Vec::new();
        accounts::upsert(&mut all, &settings());
        let printed = format!("{:?}", all[0]);
        assert!(!printed.contains(SENTINEL), "{printed}");
    }

    #[test]
    fn an_oauth_token_is_redacted_the_same_way() {
        let account = Account {
            oauth_token: Some("ya29.SENTINEL-TOKEN".into()),
            use_oauth: true,
            ..Account::default()
        };
        let printed = format!("{account:?}");
        assert!(!printed.contains("ya29"), "{printed}");
    }

    #[test]
    fn nothing_the_setup_form_can_say_contains_the_password() {
        // Every refusal the form produces, built with the sentinel in the password field.
        let mut said: Vec<String> = Vec::new();
        for (email, password, provider, imap, imap_port, smtp, smtp_port) in [
            ("", SENTINEL, "gmail", "", "993", "", "587"),
            ("nope", SENTINEL, "gmail", "", "993", "", "587"),
            ("a@b", SENTINEL, "gmail", "", "993", "", "587"),
            ("a@b.com", "", "gmail", "", "993", "", "587"),
            ("a@b.com", SENTINEL, "advanced", "", "993", "", "587"),
            ("a@b.com", SENTINEL, "advanced", "i.b.com", "nine", "s.b.com", "587"),
            ("a@b.com", SENTINEL, "advanced", "i.b.com", "993", "s.b.com", ""),
        ] {
            match state::account_settings_from_form(
                email, password, "Name", provider, imap, imap_port, smtp, smtp_port,
            ) {
                Ok(settings) => said.push(format!("{settings:?}")),
                Err(e) => said.push(e),
            }
        }

        // Every state the app can be in, every sentence it can produce about one.
        for s in [
            MailState::Unreachable { reason: format!("connect: LOGIN x {SENTINEL}") },
            state::decide(Ok(AccountsResult {
                accounts: Vec::new(),
                config_path: "/tmp/e.json".into(),
                secrets_are_plaintext: true,
            })),
            state::decide(Ok(AccountsResult {
                accounts: vec![summary("a@b.com")],
                config_path: "/tmp/e.json".into(),
                secrets_are_plaintext: true,
            })),
        ] {
            said.push(s.summary());
            said.push(s.notice());
            said.push(s.account_name());
            said.push(s.config_path().to_string());
        }

        // And the two sentences the setup screen carries.
        said.push(state::test_summary(
            false,
            &connect::name_failure(&imap(), &format!("BAD LOGIN {SENTINEL}"), SENTINEL),
            false,
            &connect::name_failure(
                &Attempt::new("SMTP", "smtp.b.com", 587),
                &format!("535 rejected {SENTINEL}"),
                SENTINEL,
            ),
        ));
        said.push(state::password_storage_note("/tmp/e.json", true));

        // The one place the sentinel is allowed: the `Unreachable` reason above is a string this
        // test wrote itself, standing in for a transport error. It is redacted at the point a
        // server's words enter, not afterwards — so what is checked here is that nothing the
        // app *formats* adds one.
        for sentence in &said {
            if sentence.contains("connect: LOGIN") {
                continue;
            }
            assert!(!sentence.contains(SENTINEL), "a password reached: {sentence}");
        }
    }

    #[test]
    fn a_saved_account_is_json_and_nothing_else_reads_as_a_summary() {
        // The file on disk does hold the password — that is the decision recorded in
        // `design/email-2026-09-20.md` and in `accounts.rs`. What must never happen is that file
        // being confused with what the service answers with.
        let f = Fixture::new();
        let mut all = Vec::new();
        accounts::upsert(&mut all, &settings());
        accounts::save(&f.config(), &all).unwrap();

        let on_disk = std::fs::read_to_string(f.config()).unwrap();
        assert!(on_disk.contains(SENTINEL), "the service could not sign in with this account");

        let answered = serde_json::to_string(&AccountsResult {
            accounts: accounts::summaries(&all),
            config_path: f.config().display().to_string(),
            secrets_are_plaintext: true,
        })
        .unwrap();
        assert!(!answered.contains(SENTINEL), "{answered}");
    }

    // ── The wire, as both ends build it ──────────────────────────────
    //
    // The payload tests. The calendar's two ends each spelled their own parameter names and
    // disagreed, so every listing failed while both files looked right on their own page.

    #[test]
    fn the_settings_the_form_builds_parse_as_the_service_parses_them() {
        let built = state::account_settings_from_form(
            "someone@gmail.com",
            SENTINEL,
            "Someone",
            "gmail",
            "",
            "993",
            "",
            "587",
        )
        .unwrap();
        let sent = serde_json::to_value(&built).unwrap();
        let received: AccountSettings = serde_json::from_value(sent).unwrap();
        accounts::refuse_bad_settings(&received).unwrap();
        assert_eq!(received.imap_server, "imap.gmail.com");
        assert_eq!(received.password, SENTINEL);
    }

    #[test]
    fn the_accounts_answer_parses_into_the_state_the_app_decides_from() {
        let mut all = Vec::new();
        accounts::upsert(&mut all, &settings());
        let answered = serde_json::to_value(AccountsResult {
            accounts: accounts::summaries(&all),
            config_path: "/tmp/e.json".into(),
            secrets_are_plaintext: true,
        })
        .unwrap();
        let parsed: AccountsResult = serde_json::from_value(answered).unwrap();
        assert_eq!(state::decide(Ok(parsed)).has_account(), Some(true));
    }

    #[test]
    fn an_account_file_written_by_hand_in_the_old_shape_still_loads() {
        // The file predates this pass and deployments have one. It had no `display_name` and no
        // `provider`, and its id could be anything.
        let f = Fixture::new();
        std::fs::create_dir_all(f.config().parent().unwrap()).unwrap();
        std::fs::write(
            f.config(),
            r#"[{"id":"work","email":"a@b.com","password":"x",
                "imap_server":"imap.b.com","smtp_server":"smtp.b.com"}]"#,
        )
        .unwrap();
        let all = accounts::load(&f.config()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "work");
        assert_eq!(all[0].imap_port, 993);
        assert_eq!(all[0].smtp_port, 587);
        // And the app finds it, although its id is not the address slug.
        assert_eq!(accounts::pick(&all, Some("default")).unwrap().email, "a@b.com");
    }
}
