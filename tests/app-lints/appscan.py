#!/usr/bin/env python3
"""appscan.py -- finding the apps and their two halves.

An app in this repo is a directory under `apps/` with a window in
`ui/app.slint` and its Rust under `src/`. Both lints need the same two lists,
so they are built once here.

`desktop-files` is a directory of `.desktop` entries, not an app; it has
neither half and is skipped by the same rule that skips anything else
half-built.
"""

import os

__all__ = ["repo_root", "App", "discover_apps", "find_app"]


def repo_root(start=None):
    """The repository root, found by walking up from this file to a `.git`."""
    here = os.path.abspath(start or os.path.dirname(os.path.abspath(__file__)))
    while True:
        if os.path.isdir(os.path.join(here, ".git")):
            return here
        parent = os.path.dirname(here)
        if parent == here:
            # Not in a checkout: fall back to two levels up from tests/app-lints.
            return os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
        here = parent


class App:
    """One app: its name, its window file, and every .rs file under its src."""

    def __init__(self, name, path, slint_path, rust_paths):
        self.name = name
        self.path = path
        self.slint_path = slint_path
        self.rust_paths = rust_paths

    def __repr__(self):
        return "App(%r, slint=%s, rs=%d)" % (
            self.name,
            "yes" if self.slint_path else "no",
            len(self.rust_paths),
        )

    def read_slint(self):
        if not self.slint_path:
            return ""
        return _read(self.slint_path)

    def read_rust(self):
        """[(path, text)] for every .rs file under src/, in sorted order."""
        return [(p, _read(p)) for p in self.rust_paths]


def _read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def _rust_files(src_dir):
    found = []
    for dirpath, dirnames, filenames in os.walk(src_dir):
        dirnames[:] = sorted(d for d in dirnames if d not in ("target", ".git"))
        for name in sorted(filenames):
            if name.endswith(".rs"):
                found.append(os.path.join(dirpath, name))
    return found


def discover_apps(root=None):
    """Every app under apps/ that has at least a window or some Rust, name-sorted."""
    root = root or repo_root()
    apps_dir = os.path.join(root, "apps")
    apps = []
    if not os.path.isdir(apps_dir):
        return apps
    for name in sorted(os.listdir(apps_dir)):
        path = os.path.join(apps_dir, name)
        if not os.path.isdir(path):
            continue
        slint = os.path.join(path, "ui", "app.slint")
        slint = slint if os.path.isfile(slint) else None
        src = os.path.join(path, "src")
        rust = _rust_files(src) if os.path.isdir(src) else []
        if slint is None and not rust:
            continue
        apps.append(App(name, path, slint, rust))
    return apps


def find_app(name, root=None):
    for app in discover_apps(root):
        if app.name == name:
            return app
    return None
