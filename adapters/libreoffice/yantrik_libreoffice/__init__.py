"""LibreOffice on the Yantrik desktop: a surface for a program that cannot host one itself.

An adapter process — started by the shell beside LibreOffice (`X-Yantrik-Adapter`), by hand, or
headless — that binds `app-libreoffice.sock` and drives LibreOffice over UNO: open documents,
read and write Writer text and Calc cells, save, save as, export PDF, close. Built only on the
public surface SDK (`yantrik_surface`).

    office.py    LibreOffice over UNO; the only file that touches `uno`
    surface.py   the actions, their grades and descriptions, and what describe shows
    adapter.py   the program: when it starts, what it serves, when it stops
"""

from .office import NotReachable, Office, OfficeError
from .surface import APP_ID, LibreOfficeSurface, build

__all__ = ["APP_ID", "LibreOfficeSurface", "NotReachable", "Office", "OfficeError", "build"]
