#!/usr/bin/env python3
"""How the OS starts Blender: `blender --python .../bootstrap.py`.

The launcher's whole job is to get this file in front of Blender's own Python and let the
addon do the rest. In a window, `start()` returns once the socket is bound and Blender
carries on booting into its GUI with the surface pumping between event-loop turns. Under
`blender -b` there is no GUI to return to, so `start()` becomes the main loop and this
process stays up serving the socket until it is stopped — which is exactly what a test or
a headless render wants.

The addon lives beside this file (`addon/yantrik_surface`) in the source tree and under
`share/blender/yantrik_surface` in a release, so both candidate paths are tried.
"""

import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
for _candidate in (
    os.path.join(_HERE, "addon"),  # the source tree: apps/blender/addon/yantrik_surface
    _HERE,                         # a release: /opt/yantrik/share/blender/yantrik_surface
):
    if os.path.isdir(os.path.join(_candidate, "yantrik_surface")) and _candidate not in sys.path:
        sys.path.insert(0, _candidate)

from yantrik_surface import start  # noqa: E402

start()
