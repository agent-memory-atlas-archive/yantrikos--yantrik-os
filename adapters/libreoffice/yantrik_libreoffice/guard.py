"""Whether a document reads from outside itself, asked before anything is evaluated (#942).

Calc evaluates a formula lazily and registers an external link only when it does, so a check of
`ExternalDocLinks` made before a read can pass and the read itself then fetch the file. So an
agent's read first looks at what is written down, which evaluates nothing: every formula in the
document, and every named expression, must pass the same allowlist an agent's own formula does.
The links are asked again after the read, as a second wall."""

from . import formulas

# Cells whose formulas are looked at before an agent reads. A larger document is refused, not
# skimmed: a check that stops early is no check.
MAX_SCANNED = 250000


def _names_in(owner):
    try:
        ranges = owner.NamedRanges
        return [(name, ranges.getByName(name).getContent()) for name in ranges.getElementNames()]
    except Exception:  # noqa: BLE001 - a document or sheet without named expressions
        return []


def _text_section_links(doc):
    out = []
    try:
        sections = doc.getTextSections()
        for name in sections.getElementNames():
            url = getattr(sections.getByName(name).FileLink, "FileURL", "")
            if url:
                out.append(url)
    except Exception:  # noqa: BLE001 - not a text document
        pass
    return out


def links(doc):
    """Every other file or source `doc` is linked to, empty when none."""
    names = formulas.linked(doc)
    for attribute in ("SheetLinks", "AreaLinks"):
        try:
            names += list(getattr(doc, attribute).getElementNames())
        except Exception:  # noqa: BLE001 - a document of another kind has no such links
            continue
    return names + _text_section_links(doc)


def outside_sentence(doc, sheets, used):
    """A sentence when any formula or named expression of a Calc document names anything outside
    it, else None. `sheets` are the document's sheets; `used(sheet)` is a sheet's used area."""
    for owner in [doc] + list(sheets):
        for name, content in _names_in(owner):
            said = formulas.refusal("=" + content)
            if said:
                return "the named expression `%s` in this document is not one an agent reads " \
                       "through (%s)" % (name, said)
    scanned = 0
    for sheet in sheets:
        left, top, right, bottom = used(sheet)
        scanned += (right - left + 1) * (bottom - top + 1)
        if scanned > MAX_SCANNED:
            return "this document is too large to check for formulas that read outside it"
        rows = sheet.getCellRangeByPosition(left, top, right, bottom).getFormulaArray()
        for row in rows:
            for formula in row:
                if isinstance(formula, str) and formula.startswith("="):
                    said = formulas.refusal(formula)
                    if said:
                        return ("a formula in this document names something outside it, so an "
                                "agent does not read its values (%s)" % said)
    return None


def problem(doc, scan=False):
    """What an agent's read or write must be refused for, as (kind, detail) for `Office._admit`,
    or None. With `scan` the formulas are looked at too (a Calc document)."""
    names = links(doc)
    if names:
        return "links", names
    if scan:
        said = outside_sentence(doc, sheets_of(doc), used_area)
        if said:
            return "outside", said
    return None


def sheets_of(doc):
    sheets = doc.getSheets()
    return [sheets.getByIndex(i) for i in range(sheets.getCount())]


def used_area(sheet):
    """A sheet's used cells as (left, top, right, bottom)."""
    cursor = sheet.createCursor()
    cursor.gotoStartOfUsedArea(False)
    cursor.gotoEndOfUsedArea(True)
    a = cursor.getRangeAddress()
    return a.StartColumn, a.StartRow, a.EndColumn, a.EndRow
