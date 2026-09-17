"""Yantrik OS desktop platform for Hermes Agent. See adapter.py."""

try:
    from .adapter import register
except ImportError:  # imported outside Hermes, e.g. to test desktop.py on its own
    register = None

__all__ = ["register"]
