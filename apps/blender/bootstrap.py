#!/usr/bin/env python3
"""How the OS starts Blender: `blender --python .../bootstrap.py`.

The launcher's whole job is to get this file in front of Blender's own Python and let the
addon do the rest. In a window, `start()` returns once the socket is bound and Blender
carries on booting into its GUI with the surface pumping between event-loop turns. Under
`blender -b` there is no GUI to return to, so `start()` becomes the main loop and this
process stays up serving the socket until it is stopped — which is exactly what a test or
a headless render wants.

The addon (`yantrik_blender`) and the surface SDK it is built on (`yantrik_surface`) live
at `addon/yantrik_blender` and `../../sdk/python/yantrik_surface` in the source tree, and
side by side under `share/blender` in a release. Only a directory that holds the addon
itself is put on the path — an older layout's leftovers beside it must not be imported in its
place — and the addon finds its SDK relative to itself.
"""

import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
for _candidate in (
    os.path.join(_HERE, "addon"),  # the source tree: apps/blender/addon/yantrik_blender
    _HERE,                         # a release: /opt/yantrik/share/blender/yantrik_blender
):
    if os.path.isdir(os.path.join(_candidate, "yantrik_blender")):
        if _candidate not in sys.path:
            sys.path.insert(0, _candidate)
        break

from yantrik_blender import start  # noqa: E402

start()
