"""unittest tests for the pure decision functions in smoke.py.

Covers the required behaviours:
  * a safe action with no required arguments is run
  * a safe action that needs an argument is not
  * standard / sensitive / dangerous actions are never run, whatever their args
  * parse_actions handles a line with no actions and one with several
  * summarize counts passes and failures correctly, including an empty set
"""
import unittest

from smoke import (
    candidate_apps,
    is_safe_to_run,
    parse_actions,
    summarize,
)


def action(grade, args, name):
    return {"grade": grade, "required_args": args, "name": name}


class ParseActionsTest(unittest.TestCase):
    def test_empty_string_and_none(self):
        self.assertEqual(parse_actions(""), [])
        self.assertEqual(parse_actions(None), [])

    def test_describe_line_with_no_actions(self):
        text = "app: notes\nsurface: up\nno actions on this surface"
        self.assertEqual(parse_actions(text), [])

    def test_single_action(self):
        text = "act: status()  [safe, settles on return]"
        acts = parse_actions(text)
        self.assertEqual(acts, [{
            "name": "status",
            "grade": "safe",
            "required_args": [],
            "settles_on_return": True,
        }])

    def test_line_with_several_actions(self):
        text = (
            "act: status()  [safe, settles on return]\n"
            "act: send(to,subject)  [standard, settles on return]\n"
            "act: open()  [safe, settles later]\n"
        )
        acts = parse_actions(text)
        self.assertEqual([a["name"] for a in acts], ["status", "send", "open"])
        self.assertEqual([a["grade"] for a in acts], ["safe", "standard", "safe"])
        self.assertEqual(acts[1]["required_args"], ["to", "subject"])
        self.assertTrue(acts[0]["settles_on_return"])
        self.assertFalse(acts[2]["settles_on_return"])

    def test_whitespace_padded_args(self):
        text = "act: send( to , subject )  [safe, settles on return]"
        acts = parse_actions(text)
        self.assertEqual(acts[0]["required_args"], ["to", "subject"])


class IsSafeToRunTest(unittest.TestCase):
    def test_safe_no_required_args_is_run(self):
        self.assertTrue(is_safe_to_run(action("safe", [], "status")))

    def test_safe_but_needs_an_argument_is_not_run(self):
        self.assertFalse(is_safe_to_run(action("safe", ["channel"], "check_update")))

    def test_standard_never_run_regardless_of_args(self):
        self.assertFalse(is_safe_to_run(action("standard", [], "refresh_apps")))
        self.assertFalse(is_safe_to_run(action("standard", ["x"], "refresh_apps")))

    def test_sensitive_never_run_regardless_of_args(self):
        self.assertFalse(is_safe_to_run(action("sensitive", [], "lock")))
        self.assertFalse(is_safe_to_run(action("sensitive", ["x"], "lock")))

    def test_dangerous_never_run_regardless_of_args(self):
        self.assertFalse(is_safe_to_run(action("dangerous", [], "files_delete")))
        self.assertFalse(is_safe_to_run(action("dangerous", ["x"], "files_delete")))

    def test_forbidden_verb_blocked_even_if_graded_safe(self):
        self.assertFalse(is_safe_to_run(action("safe", [], "send_email")))
        self.assertFalse(is_safe_to_run(action("safe", [], "set_do_not_disturb")))

    def test_grade_comparison_is_case_and_whitespace_insensitive(self):
        self.assertTrue(is_safe_to_run(action("SAFE", [], "status")))
        self.assertTrue(is_safe_to_run(action(" safe ", [], "status")))

    def test_none_and_empty_dict_are_not_run(self):
        self.assertFalse(is_safe_to_run(None))
        self.assertFalse(is_safe_to_run({}))


class SummarizeTest(unittest.TestCase):
    def test_counts_passes_failures_and_apps(self):
        recs = [
            {"app": "app-notes", "ok": True},
            {"app": "app-notes", "ok": False},
            {"app": "app-email", "ok": True},
        ]
        s = summarize(recs)
        self.assertEqual(s["apps"], 2)
        self.assertEqual(s["actions_run"], 3)
        self.assertEqual(s["passed"], 2)
        self.assertEqual(s["failed"], 1)
        self.assertEqual(s["apps_list"], ["app-email", "app-notes"])

    def test_empty_and_none_result_set(self):
        for empty in (None, []):
            self.assertEqual(summarize(empty), {
                "apps": 0, "actions_run": 0, "passed": 0, "failed": 0,
                "apps_list": [],
            })

    def test_all_pass(self):
        s = summarize([{"app": "a", "ok": True}, {"app": "a", "ok": True}])
        self.assertEqual(s["passed"], 2)
        self.assertEqual(s["failed"], 0)
        self.assertEqual(s["actions_run"], 2)

    def test_all_fail(self):
        s = summarize([{"app": "a", "ok": False}, {"app": "b", "ok": False}])
        self.assertEqual(s["passed"], 0)
        self.assertEqual(s["failed"], 2)


class CandidateAppsTest(unittest.TestCase):
    def test_selects_only_app_family(self):
        doc = {
            "apps": [{"id": "app-notes"}, {"id": "weather"}],
            "surfaces_not_up": [{"id": "app-email"}],
        }
        self.assertEqual(candidate_apps(doc), ["app-notes", "app-email"])

    def test_dedupes_across_sections(self):
        doc = {
            "apps": [{"id": "app-notes"}],
            "surfaces_not_up": [{"id": "app-notes"}],
        }
        self.assertEqual(candidate_apps(doc), ["app-notes"])

    def test_excludes_rpc_services_and_blank_ids(self):
        doc = {
            "apps": [
                {"id": "app-notes"},
                {"id": "system-monitor"},
                {"id": None},
                {"id": ""},
            ],
            "surfaces_not_up": [],
        }
        self.assertEqual(candidate_apps(doc), ["app-notes"])

    def test_empty_doc(self):
        self.assertEqual(candidate_apps({}), [])


if __name__ == "__main__":
    unittest.main()
