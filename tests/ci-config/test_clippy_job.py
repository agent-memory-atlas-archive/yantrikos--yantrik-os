"""The clippy job in .github/workflows/ci.yml, held by what it promises: it runs the linter over
the whole workspace and it reports without failing, so warnings show up on every pull request and
no pull request is ever red because of it.

Stdlib only, like every other test in this repository: the workflow file is read line by line
rather than parsed as YAML, because no test here imports PyYAML (the CI runners have it, and the
cloud-init check uses it inline, but nothing checked in does).

    python3 -m unittest discover -s tests/ci-config -v      # pytest collects it too
"""
import os
import unittest

CI_YML = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                      "..", "..", ".github", "workflows", "ci.yml")


def job_lines(text, name):
    """The lines of one job, from its key to the next key at the same indent."""
    lines = text.splitlines()
    starts = [i for i, line in enumerate(lines) if line.rstrip() == "  %s:" % name]
    if not starts:
        return None
    body = []
    for line in lines[starts[-1] + 1:]:
        stripped = line.strip()
        if stripped and not line.startswith("   ") and not stripped.startswith("#"):
            break
        body.append(line)
    return body


class ClippyJob(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(CI_YML) as f:
            cls.text = f.read()

    def test_ci_has_a_clippy_job(self):
        self.assertIsNotNone(job_lines(self.text, "clippy"),
                             ".github/workflows/ci.yml has no clippy job")

    def test_it_lints_the_whole_workspace(self):
        body = job_lines(self.text, "clippy") or []
        self.assertTrue(any("cargo clippy --workspace --locked" in line for line in body),
                        "the clippy job runs no `cargo clippy --workspace --locked`")

    def test_it_reports_without_failing(self):
        # The tree has never been linted: a clippy job that fails would fail every pull request
        # on the day it lands, and the first person to be annoyed by it turns it off instead of
        # reading it. continue-on-error is the whole reason this job is allowed to exist.
        body = job_lines(self.text, "clippy") or []
        self.assertTrue(any(line.strip() == "continue-on-error: true" for line in body),
                        "the clippy job is missing continue-on-error: true")


if __name__ == "__main__":
    unittest.main()
