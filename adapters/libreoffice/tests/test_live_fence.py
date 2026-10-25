"""The agents' own LibreOffice, fenced, against a real soffice (#942).

Skipped unless this machine has LibreOffice, a Python that can `import uno`, and Landlock. A
spreadsheet whose formulas point at a protected file is opened in the agents' LibreOffice with
no file rule and no scan at all, so the fence is the only thing deciding, and what LibreOffice
computes, saves and exports must not hold the protected file's sentinel.

    python3 -m unittest discover -s adapters/libreoffice/tests -p test_live_fence.py -v
"""

import os
import shutil
import tempfile
import unittest

import support
from test_fence import landlock_missing
from test_live import odf, uno_importable
from yantrik_libreoffice.agent_office import AgentOffice

SENTINEL = "SENTINEL-live-fence-5be0"


def formula_cell(formula):
    return ('<table:table-cell table:formula="of:%s" office:value-type="string">'
            '<text:p></text:p></table:table-cell>' % formula.replace('"', "&quot;"))


@unittest.skipUnless(shutil.which("soffice"), "no LibreOffice (soffice) on this machine")
@unittest.skipUnless(uno_importable(), "this Python cannot import uno (python3-uno)")
@unittest.skipIf(landlock_missing(), "no Landlock on this kernel: %s" % landlock_missing())
class TestTheFencedLibreOffice(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="yantrik-live-fence-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.home = os.path.join(self.tmp, "home")
        secret = os.path.join(self.home, ".local", "share", "yantrik", "snippets.csv")
        os.makedirs(os.path.dirname(secret))
        with open(secret, "w", encoding="utf-8") as f:
            f.write(SENTINEL + "\n")
        self.documents = os.path.join(self.home, "Documents")
        os.makedirs(self.documents)
        self.crafted = os.path.join(self.documents, "crafted.ods")
        url = "file://" + secret
        row = (formula_cell("INDIRECT(\"'%s'#$snippets.A1\")" % url)
               + formula_cell("['%s'#$snippets.A1]" % url)
               + formula_cell("WEBSERVICE(\"%s\")" % url))
        odf(self.crafted, "application/vnd.oasis.opendocument.spreadsheet",
            "<office:spreadsheet><table:table table:name=\"S\"><table:table-row>%s"
            "</table:table-row></table:table></office:spreadsheet>" % row)
        self.agents = AgentOffice(home=self.home, env=dict(os.environ, HOME=self.home))
        self.addCleanup(self.agents.stop)

    def test_nothing_libreoffice_computes_saves_or_exports_holds_the_protected_file(self):
        # Where the fence cannot run (Debian's soffice cannot read /var/spool/libreoffice under it),
        # the agents' office never starts. That is the sandbox #953 builds, not a failure of this test.
        try:
            office = self.agents.office(self.crafted)
        except Exception as e:
            self.skipTest("the fenced LibreOffice cannot start on this machine (#953): %s" % e)
        self.assertGreaterEqual(self.agents.fenced_with, 1)
        office.open(self.crafted)
        rows = office.read_cells("A1:C1")
        self.assertNotIn(SENTINEL, str(rows))
        csv = os.path.join(self.documents, "out.csv")
        office.save_as(csv)
        pdf = os.path.join(self.documents, "out.pdf")
        _, work = office.export_pdf_plan(pdf)
        work()
        for path in (csv, pdf):
            with open(path, "rb") as f:
                self.assertNotIn(SENTINEL.encode(), f.read(), path)


if __name__ == "__main__":
    unittest.main()
