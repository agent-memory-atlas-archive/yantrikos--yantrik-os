"""Every JSON-RPC method a service answers is in docs/app-control.md, and nothing else is.

A service's own methods sit beside its gated `app.act` and meet no ceiling, no mode and no grant
(#161). Until a caller can be told apart from a person's own button, the guide lists them, so what
is ungated is at least written down. A list kept by hand goes stale the day somebody adds a
method, so this reads the services' dispatch and the guide's table and fails when they disagree.

The dispatch is read, not run: a match arm whose pattern is a dotted string literal, or a constant
that one of the service's `yantrik_ipc_contracts` modules (or its own source) defines as one.

    python3 -m unittest discover -s tests/service-methods -v
"""

import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SERVICES = ROOT / "services"
CONTRACTS = ROOT / "crates" / "yantrik-ipc-contracts" / "src"
GUIDE = ROOT / "docs" / "app-control.md"

BEGIN = "<!-- service-methods:begin -->"
END = "<!-- service-methods:end -->"

# The surface's own two, which the rule in the guide already covers wherever they are answered.
SURFACE = {"app.describe", "app.act"}

_PATTERN = r'(?:"[a-z0-9_]+\.[a-z0-9_.]+"|(?:[a-z_]+::)?[A-Z][A-Z0-9_]*)'
ARM = re.compile(r"^\s*(%s(?:\s*\|\s*%s)*)\s*=>" % (_PATTERN, _PATTERN), re.M)
CONST = re.compile(r'const\s+([A-Z][A-Z0-9_]*)\s*:\s*&str\s*=\s*"([a-z0-9_]+\.[a-z0-9_.]+)"')


def dispatched(service: pathlib.Path) -> set:
    """The method names one service's source matches on."""
    src = "\n".join(p.read_text() for p in sorted((service / "src").rglob("*.rs")))
    consts = {}
    for module in set(re.findall(r"yantrik_ipc_contracts::(\w+)", src)):
        contract = CONTRACTS / f"{module}.rs"
        if contract.exists():
            consts.update(CONST.findall(contract.read_text()))
    consts.update(CONST.findall(src))

    names = set()
    for arm in ARM.findall(src):
        for alt in re.split(r"\s*\|\s*", arm):
            if alt.startswith('"'):
                names.add(alt.strip('"'))
            else:
                value = consts.get(alt.split("::")[-1])
                if value:
                    names.add(value)
    return names - SURFACE


def services() -> dict:
    return {
        s.name: dispatched(s)
        for s in sorted(SERVICES.iterdir())
        if (s / "src").is_dir()
    }


def guide_rows() -> list:
    """The guide's table, as (service, [methods], kind, gated, callers, changes) rows."""
    text = GUIDE.read_text()
    start, end = text.index(BEGIN) + len(BEGIN), text.index(END)
    rows = []
    for line in text[start:end].strip().splitlines()[2:]:  # past the header and its rule
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if len(cells) != 6:
            raise AssertionError(f"a row of the table does not have six cells: {line}")
        service, methods, kind, gated, callers, changes = cells
        rows.append((service, re.findall(r"`([a-z0-9_]+\.[a-z0-9_.]+)`", methods), kind, gated,
                     callers, changes))
    return rows


class TheGuideListsEveryServiceMethod(unittest.TestCase):
    def test_the_scan_still_sees_the_dispatch(self):
        # A parser that has rotted to finding nothing would agree with an empty table.
        found = services()
        self.assertIn("sysmon.kill_process", found["system-monitor-service"])
        self.assertIn("network.wifi_connect", found["network-service"])  # through a constant
        self.assertIn("notifications.add", found["notifications-service"])  # through a glob import

    def test_every_method_is_listed_under_its_service_and_nothing_else_is(self):
        listed = {}
        for service, methods, *_ in guide_rows():
            for method in methods:
                self.assertNotIn(method, listed.get(service, set()), f"{method} is listed twice")
                listed.setdefault(service, set()).add(method)

        for service, methods in services().items():
            with self.subTest(service=service):
                documented = listed.pop(service, set())
                self.assertEqual(
                    sorted(methods - documented), [],
                    f"{service} answers these, and docs/app-control.md's table of a service's own "
                    "methods does not list them. Add a row: what it changes, and the app.act "
                    "action that does the same thing under the gate, if there is one.",
                )
                self.assertEqual(
                    sorted(documented - methods), [],
                    f"docs/app-control.md lists these under {service}, which no longer answers "
                    "them. Take them out of the table.",
                )
        self.assertEqual(listed, {}, "the table names services this repository does not have")

    def test_a_row_that_changes_something_says_what_and_how_else(self):
        for service, methods, kind, gated, callers, changes in guide_rows():
            with self.subTest(service=service, methods=methods):
                self.assertIn(kind, ("reads", "changes"))
                self.assertTrue(callers, "say who calls it, or that nothing does")
                if kind == "changes":
                    self.assertTrue(gated, "name the gated way in, or `—` for none")
                    self.assertTrue(changes, "say what it changes")
                else:
                    self.assertFalse(gated or changes, "a read has no gate and changes nothing")


if __name__ == "__main__":
    unittest.main()
