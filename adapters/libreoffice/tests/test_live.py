"""The adapter against a real LibreOffice, end to end, headless.

Skipped unless this machine has LibreOffice (`soffice` on PATH) and a Python that can
`import uno` (python3-uno on Debian). Where both are there, this is the whole path a mind takes:
the adapter started with `--headless` (a LibreOffice of its own, no window, a private profile),
a generated .odt and .ods opened through `yos act`, their text and cells read and written, both
saved and read back from disk, a PDF exported and checked for text, and the adapter stopped with
the LibreOffice it started.

    python3 -m unittest discover -s adapters/libreoffice/tests -p test_live.py -v
"""

import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
import unittest
import zipfile
import zlib

import support
from yantrik_surface import call_once

NS = ('xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
      'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" '
      'xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" office:version="1.2"')


def odf(path, mimetype, body):
    """A minimal OpenDocument package: the mimetype first and stored, a manifest, the content."""
    manifest = ('<?xml version="1.0" encoding="UTF-8"?>\n<manifest:manifest xmlns:manifest='
                '"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2">'
                '<manifest:file-entry manifest:full-path="/" manifest:media-type="%s"/>'
                '<manifest:file-entry manifest:full-path="content.xml" '
                'manifest:media-type="text/xml"/></manifest:manifest>' % mimetype)
    content = ('<?xml version="1.0" encoding="UTF-8"?>\n<office:document-content %s>'
               '<office:body>%s</office:body></office:document-content>' % (NS, body))
    with zipfile.ZipFile(path, "w") as z:
        z.writestr(zipfile.ZipInfo("mimetype"), mimetype, compress_type=zipfile.ZIP_STORED)
        z.writestr("META-INF/manifest.xml", manifest, compress_type=zipfile.ZIP_DEFLATED)
        z.writestr("content.xml", content, compress_type=zipfile.ZIP_DEFLATED)


def content_of(path):
    with zipfile.ZipFile(path) as z:
        return z.read("content.xml").decode("utf-8")


def uno_importable():
    done = subprocess.run([sys.executable, "-c", "import uno"], capture_output=True)
    return done.returncode == 0


def pdf_text_operators(data):
    """How many text-showing operators (Tj, TJ) the PDF's content streams hold, inflated."""
    count = 0
    for stream in re.findall(rb"stream\r?\n(.*?)\r?\nendstream", data, re.S):
        try:
            stream = zlib.decompressobj().decompress(stream)
        except zlib.error:
            pass  # not deflated: read as it is
        count += len(re.findall(rb"\bT[jJ]\b", stream))
    return count


@unittest.skipUnless(shutil.which("soffice"), "no LibreOffice (soffice) on this machine")
@unittest.skipUnless(uno_importable(), "this Python cannot import uno (python3-uno)")
@unittest.skipUnless(os.path.isfile(support.YOS), "deploy/yantrik-os/yos is not in this tree")
class TestAgainstARealLibreOffice(unittest.TestCase):
    def setUp(self):
        self.machine = support.Machine(ceiling="sensitive", mode="auto")
        self.addCleanup(self.machine.cleanup)
        self.files = os.path.join(self.machine.tmp, "files")
        os.makedirs(self.files)
        self.adapter = subprocess.Popen(
            [sys.executable, os.path.join(support.BIN, "yantrik-libreoffice-adapter"), "--headless"],
            env=self.machine.env(), stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.stop)
        deadline = time.monotonic() + 90
        while True:
            self.assertIsNone(self.adapter.poll(), "the adapter exited before LibreOffice answered")
            self.assertLess(time.monotonic(), deadline, "LibreOffice never answered on its pipe")
            if os.path.exists(self.machine.socket()):
                described = call_once(self.machine.socket(), "app.describe", {})["result"]
                if described["state"]["connected"]:
                    break
            time.sleep(0.5)

    def stop(self):
        if self.adapter.poll() is None:
            self.adapter.send_signal(signal.SIGTERM)
            try:
                self.adapter.communicate(timeout=30)
            except subprocess.TimeoutExpired:
                self.adapter.kill()
                self.adapter.communicate()

    def yos(self, *args, ok=True):
        done = subprocess.run([sys.executable, support.YOS, *args], env=self.machine.env(),
                              capture_output=True, text=True, timeout=180)
        if ok:
            self.assertEqual(done.returncode, 0, done.stderr)
        return done

    def act(self, action, *args):
        """`yos act libreoffice <action> <args> --full`, and the `result` it printed."""
        out = self.yos("act", "libreoffice", action, *args, "--full").stdout
        return json.loads(out)["result"]

    def test_writer_open_read_write_save_and_export(self):
        report = os.path.join(self.files, "report.odt")
        odf(report, "application/vnd.oasis.opendocument.text",
            "<office:text><text:p>Quarterly report</text:p>"
            "<text:p>Sales rose in the north.</text:p></office:text>")
        opened = self.act("open", "path=%s" % report)
        self.assertEqual((opened["document"], opened["kind"]), ("report.odt", "writer"))
        text = self.act("read_text", "document=report.odt")["text"]
        self.assertIn("Quarterly report", text)
        self.assertIn("Sales rose in the north.", text)

        self.act("write_text", "document=report.odt", "text=\nWritten by the adapter.")
        self.act("save", "document=report.odt")
        self.assertIn("Written by the adapter.", content_of(report), "saved to the .odt on disk")

        pdf = os.path.join(self.files, "report.pdf")
        exported = self.act("export_pdf", "document=report.odt", "path=%s" % pdf)
        self.assertEqual(exported["path"], pdf)
        with open(pdf, "rb") as f:
            data = f.read()
        self.assertTrue(data.startswith(b"%PDF-"), data[:20])
        self.assertGreater(pdf_text_operators(data), 0, "the PDF draws no text")
        if shutil.which("pdftotext"):
            words = subprocess.run(["pdftotext", pdf, "-"], capture_output=True, text=True).stdout
            self.assertIn("Written by the adapter.", words)
            self.assertIn("Quarterly report", words)

    def test_calc_open_read_write_save_and_export(self):
        budget = os.path.join(self.files, "budget.ods")
        cell = '<table:table-cell office:value-type="float" office:value="%s"><text:p>%s</text:p></table:table-cell>'
        text = '<table:table-cell office:value-type="string"><text:p>%s</text:p></table:table-cell>'
        odf(budget, "application/vnd.oasis.opendocument.spreadsheet",
            '<office:spreadsheet><table:table table:name="Costs">'
            '<table:table-row>%s%s</table:table-row>'
            '<table:table-row>%s%s</table:table-row>'
            '<table:table-row>%s%s</table:table-row>'
            '</table:table></office:spreadsheet>'
            % (text % "Item", text % "Amount", text % "Rent", cell % (1200, 1200),
               text % "Food", cell % (350.5, 350.5)))
        self.act("open", "path=%s" % budget)
        read = self.act("read_cells", "document=budget.ods")
        self.assertEqual(read["sheet"], "Costs")
        self.assertEqual(read["rows"], [["Item", "Amount"], ["Rent", 1200], ["Food", 350.5]])

        self.act("write_cells", "document=budget.ods",
                 'cells={"A4": "Total", "B4": "=SUM(B2:B3)", "C1": "Checked"}')
        read = self.act("read_cells", "document=budget.ods", "range=A4:B4")
        self.assertEqual(read["rows"], [["Total", 1550.5]])
        self.assertEqual(read["formulas"], {"B4": "=SUM(B2:B3)"})
        self.act("save", "document=budget.ods")
        saved = content_of(budget)
        self.assertIn("Checked", saved)
        self.assertIn("of:=SUM([.B2:.B3])", saved)

        pdf = os.path.join(self.files, "budget.pdf")
        exported = self.act("export_pdf", "document=budget.ods", "path=%s" % pdf)
        self.assertGreater(exported["bytes"], 0)
        with open(pdf, "rb") as f:
            data = f.read()
        self.assertTrue(data.startswith(b"%PDF-"))
        self.assertGreater(pdf_text_operators(data), 0, "the PDF draws no text")

    def test_yos_check_finds_nothing_wrong(self):
        self.act("open", "path=%s" % self._blank_writer())
        done = self.yos("check", "libreoffice", "--json", ok=False)
        report = json.loads(done.stdout)
        failed = [r for r in report["surfaces"][0]["checks"] if r["status"] == "fail"]
        self.assertEqual((done.returncode, failed), (0, []))

    def _blank_writer(self):
        path = os.path.join(self.files, "blank.odt")
        odf(path, "application/vnd.oasis.opendocument.text",
            "<office:text><text:p/></office:text>")
        return path


if __name__ == "__main__":
    unittest.main()
