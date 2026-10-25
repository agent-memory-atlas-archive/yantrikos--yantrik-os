"""LibreOffice is closed to agents until its sandbox exists (#942, #953): every action an agent
(or the Mind) can call is refused with one sentence, and no LibreOffice is contacted for it. The
person's own use through the desktop is unaffected."""

import os
import types
import unittest
from unittest import mock

import support
from yantrik_libreoffice import surface
from test_agent_files import AgentCase

ACTIONS = {
    "open": {"path": "{doc}"},
    "close": {},
    "read_text": {},
    "read_cells": {},
    "write_text": {"text": "x"},
    "write_cells": {"cells": {"A1": 1}},
    "save": {},
    "save_as": {"path": "{home}/Documents/copy.odt"},
    "export_pdf": {"path": "{home}/Documents/copy.pdf"},
}


class TestClosedToAgents(AgentCase):
    def setUp(self):
        super().setUp()
        mock.patch.stopall()   # AgentCase turns the switch on for the fence tests; here it is off
        self.assertFalse(surface.AGENTS_OPEN)
        self.doc_path = self.doc(self.home, "Documents", "plan.odt", text="plan")

    def args(self, action):
        return {k: (v.format(doc=self.doc_path, home=self.home) if isinstance(v, str) else v)
                for k, v in ACTIONS[action].items()}

    def test_the_table_covers_every_action_the_surface_offers(self):
        offered = {a["name"] for a in self.surface.describe_json()["actions"]}
        self.assertEqual(offered, set(ACTIONS))

    def test_every_action_refuses_an_agent_with_one_sentence_and_touches_no_libreoffice(self):
        for action in ACTIONS:
            said = self.agent_refused(action, **self.args(action))
            self.assertEqual(said, surface.CLOSED_TO_AGENTS, action)
        self.assertIn("#953", surface.CLOSED_TO_AGENTS)
        self.assertEqual(self.agent_soffice.connections, 0, "the agents' UNO was never reached")
        self.assertEqual(self.agents.starts, 0, "the fenced LibreOffice was never started")
        self.assertEqual(self.soffice.connections, 0, "the person's UNO was never reached")
        self.assertEqual(self.soffice.documents + self.agent_soffice.documents, [])

    def test_the_minds_account_is_refused_with_no_token_at_all(self):
        """A peer on the Mind's uid is an agent whether or not it carries a token (L3)."""
        peer = types.SimpleNamespace(uid=4242, pid=77)
        with mock.patch("yantrik_surface.files.caller", return_value=peer),                 mock.patch("yantrik_surface.mind_door.mind_uid", return_value=4242):
            for action in ACTIONS:
                with self.assertRaises(support.RpcError) as caught:
                    self.surface.act({"action": action, "args": self.args(action)})
                self.assertEqual(caught.exception.message, surface.CLOSED_TO_AGENTS, action)
        self.assertEqual(self.agents.starts + self.agent_soffice.connections + self.soffice.connections, 0)

    def test_the_real_agent_office_is_never_started_or_fenced(self):
        with mock.patch("yantrik_libreoffice.agent_office.start_headless") as start, \
                mock.patch("yantrik_libreoffice.fence.command") as fence:
            surface_ = surface.build(self.office, settings_path=self.machine.settings,
                                     mode_path=self.machine.mode_file, stat=self.stat)
            for action in ACTIONS:
                with self.assertRaises(support.RpcError):
                    surface_.act({"action": action, "args": self.args(action),
                                  "agent_token": "tok-lo"})
            start.assert_not_called()
            fence.assert_not_called()

    def test_the_person_still_works_in_their_own_libreoffice(self):
        opened = self.result("open", path=self.doc_path)
        self.assertEqual(opened["document"], "plan.odt")
        self.assertEqual(self.result("read_text")["text"], "plan")
        self.assertEqual(self.agent_soffice.connections, 0)


if __name__ == "__main__":
    unittest.main()
