"""Each finding of #942's review at b5bf0784, as a probe that failed before its fix, and the
agents' own LibreOffice that now serves every agent call (agent_office.py).

The scan is the second wall: an agent's LibreOffice runs inside the kernel's fence
(test_fence.py), and these hold even where that fence is not the one deciding."""

import os
import re
from unittest import mock

import support
from test_agent_files import SENTINEL, AgentCase
from yantrik_libreoffice import instance
from yantrik_libreoffice.agent_office import HELD, OUTSIDE, lock_file

STORE = "INDIRECT(\"'file:///h/.local/share/yantrik/snippets/s.csv'#$s.A1\")"


def read_bytes(path):
    with open(path, "rb") as f:
        return f.read()


class TestFindings(AgentCase):
    def crafted(self, cells, name="book.ods"):
        """A spreadsheet an agent left in ~/Documents, opened by an agent."""
        path = os.path.join(self.home, "Documents", name)
        support.fake_uno.write_doc(path, "calc", sheets=[{"name": "Sheet1", "cells": cells}])
        self.agent("open", path=path)
        return path

    # ── H1: Calc reports one cell of an array formula as `{=…}` ──────────────

    def test_h1_an_array_formula_that_reads_a_file_refuses_the_read(self):
        self.crafted({"A1": "=1"})
        sheet = self.agent_soffice.documents[-1].sheets[0]
        sheet.cells["B1"] = {"formula": "{=%s}" % STORE, "value": SENTINEL}
        computed = []
        with mock.patch.object(support.fake_uno._Range, "getDataArray",
                               lambda me: computed.append(1) or ((SENTINEL,),)):
            said = self.agent_refused("read_cells", range="B1")
        self.assertIn("INDIRECT", said)
        self.assertNotIn(SENTINEL, said)
        self.assertEqual(computed, [], "nothing was computed before the refusal")

    def test_h1_an_array_formula_of_the_documents_own_cells_is_read_and_shown(self):
        self.crafted({"A1": 2, "A2": 3})
        sheet = self.agent_soffice.documents[-1].sheets[0]
        sheet.cells["B1"] = {"formula": "{=A1:A2*2}", "value": 4.0}
        read = self.agent("read_cells", range="A1:B1")["result"]
        self.assertEqual(read["formulas"], {"B1": "{=A1:A2*2}"})

    # ── H2: save, save_as and export_pdf write what formulas compute ─────────

    def test_h2_no_agent_store_writes_a_document_whose_formula_reads_a_file(self):
        path = self.crafted({"A1": "=%s" % STORE})
        before = read_bytes(path)
        documents = os.path.join(self.home, "Documents")
        for action, args in (("save_as", {"path": os.path.join(documents, "out.csv")}),
                             ("save_as", {"path": os.path.join(documents, "out.xlsx")}),
                             ("export_pdf", {"path": os.path.join(documents, "out.pdf")}),
                             ("save", {})):
            said = self.agent_refused(action, **args)
            self.assertIn("INDIRECT", said, action)
            if "path" in args:
                self.assertFalse(os.path.exists(args["path"]), action)
        self.assertEqual(read_bytes(path), before, "the file was not saved over")
        self.assertEqual(self.agent_soffice.stores, [], "nothing was written")

    def test_h2_a_link_that_appears_as_a_file_is_written_takes_the_file_back(self):
        path = self.crafted({"A1": "=A2"})
        before = read_bytes(path)
        doc = self.agent_soffice.documents[-1]
        documents = os.path.join(self.home, "Documents")
        for action, args, method in (
                ("save_as", {"path": os.path.join(documents, "copy.csv")}, "storeAsURL"),
                ("export_pdf", {"path": os.path.join(documents, "copy.pdf")}, "storeToURL"),
                ("save", {}, "store")):
            real = getattr(type(doc), method)

            def linking(me, *a, real=real):
                real(me, *a)
                me.external_links.append("file:///h/.ssh/id_ed25519")
            with mock.patch.object(type(doc), method, linking):
                said = self.agent_refused(action, **args)
            doc.external_links.clear()
            self.assertIn("taken back", said, action)
            if "path" in args:
                self.assertFalse(os.path.exists(args["path"]), action)
            doc.url = self.agent_soffice_url(path)
        self.assertEqual(read_bytes(path), before, "the old contents were put back")

    def agent_soffice_url(self, path):
        return support.fake_uno.FakeUno.systemPathToFileUrl(path)

    def test_h2_the_person_still_saves_their_own_document_as_they_like(self):
        path = os.path.join(self.home, "Documents", "theirs.ods")
        support.fake_uno.write_doc(path, "calc", sheets=[{"name": "S", "cells": {"A1": "=%s" % STORE}}])
        self.act("open", path=path)
        made = os.path.join(self.home, "Documents", "theirs.csv")
        self.assertEqual(self.result("save_as", path=made)["path"], made)

    # ── L1: what `Content/Update/Link` means to each app ─────────────────────

    def test_l1_the_profile_says_never_in_each_apps_own_numbers(self):
        """Calc.xcs: 0 always, 1 never, 2 on request. Writer.xcs: 0 always, 1 on request,
        2 never. The profile had 2 for both, which for Calc is on request."""
        def value(app):
            m = re.search(r'Office\.%s/Content/Update">.*?<value>(\d)</value>' % app,
                          instance.NO_LINK_UPDATES)
            return int(m.group(1))
        self.assertEqual((value("Calc"), value("Writer")), (1, 2))

    # ── the agents' own LibreOffice ──────────────────────────────────────────

    def test_a_file_the_person_has_open_is_refused_to_an_agent(self):
        theirs = os.path.join(self.home, "Documents", "shared.odt")
        support.fake_uno.write_doc(theirs, "writer", text=SENTINEL)
        self.act("open", path=theirs)
        said = self.agent_refused("open", path=theirs)
        self.assertEqual(said, HELD % theirs)
        self.assertEqual(self.agent_soffice.documents, [])

    def test_a_file_locked_by_another_libreoffice_is_refused_to_an_agent(self):
        path = os.path.join(self.home, "Documents", "locked.odt")
        support.fake_uno.write_doc(path, "writer", text="x")
        with open(lock_file(path), "w"):
            pass
        self.assertEqual(self.agent_refused("open", path=path), HELD % path)

    def test_a_file_the_fence_does_not_reach_is_refused_before_anything_starts(self):
        path = os.path.join(self.home, ".cache", "x.odt")
        os.makedirs(os.path.dirname(path))
        support.fake_uno.write_doc(path, "writer", text="x")
        self.assertEqual(self.agent_refused("open", path=path), OUTSIDE % path)
        self.assertEqual(self.agents.starts, 0)

    def test_an_agent_opening_its_own_document_again_is_not_refused_by_its_lock(self):
        path = os.path.join(self.home, "Documents", "mine.odt")
        support.fake_uno.write_doc(path, "writer", text="mine")
        self.agent("open", path=path)
        with open(lock_file(path), "w"):
            pass
        self.assertTrue(self.agent("open", path=path)["result"]["already_open"])

    def test_a_folder_made_after_the_start_restarts_an_idle_agent_office(self):
        self.crafted({"A1": 1})
        self.agent("close")
        later = os.path.join(self.home, "Later", "l.odt")
        os.makedirs(os.path.dirname(later))
        support.fake_uno.write_doc(later, "writer", text="later")
        self.assertEqual(self.agent("open", path=later)["result"]["path"], later)
        self.assertEqual(self.agents.starts, 2)

    def test_describe_lists_the_agents_documents_apart_from_the_persons(self):
        self.crafted({"A1": 1})
        _, state, _ = self.view()
        self.assertEqual([d["name"] for d in state["agent_documents"]], ["book.ods"])
        self.assertEqual(state["documents"], [])

