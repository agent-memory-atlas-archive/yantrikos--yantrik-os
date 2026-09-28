"""What the browser surface does, one method per action, each answering with what happened.

Every act answers with its consequence, not just "done": whether the page navigated, what it is
now, what text appeared (the "Invalid password" a click produced), whether a dialog or a new tab
opened, and — when the page is a new one — what is on it. A caller that has to read the page
again after every step to learn whether the step worked spends a round trip per step and still
misses the error that flashed and went; this is the one reading that answers it.

Before pressing anything it checks what a person would see: that the element is still there,
that nothing covers it, that it is not disabled, and that its label does not read as a
commitment — which only `commit` presses, and `commit` asks the person.
"""

import threading
import time
import urllib.parse

from . import cdp, commit, hands

NAVIGATE_LIMIT = 15.0     # seconds a navigation is given to settle
ACT_LIMIT = 4.0           # seconds a click or a key is given
TEXT_LIMIT = 20000


class Refused(Exception):
    """An action declining, with the sentence the caller reads."""


def short(target_id):
    return target_id[:8]


def host_of(url):
    try:
        return (urllib.parse.urlsplit(url).hostname or "").lower()
    except ValueError:
        return ""


def fold_site(site):
    s = str(site or "").strip().lower()
    if "://" in s:
        s = host_of(s)
    s = s.split("/")[0]
    return s[4:] if s.startswith("www.") else s


class Driver:
    def __init__(self, browser=None):
        self.browser = browser or cdp.Browser()
        self.lock = threading.RLock()

    # ── plumbing ──

    def _tab(self, tab=None):
        try:
            return self.browser.tab(tab)
        except cdp.BrowserClosed:
            self.browser.close()
            raise Refused("the browser is not open, so there is no page to read or act on. Open "
                          "it: act shell open_app name=browser — then read.") from None
        except cdp.CdpError as e:
            raise Refused(str(e)) from None

    def _run(self, tab, fn, *args):
        try:
            return self.browser.run(tab, fn, *args)
        except cdp.BrowserClosed:
            raise Refused("the browser closed while this was being done") from None
        except cdp.CdpError as e:
            raise Refused(str(e)) from None

    def _call(self, method, params=None, tab=None):
        try:
            return self.browser.call(method, params, session=tab.session if tab else None)
        except cdp.BrowserClosed:
            raise Refused("the browser closed while this was being done") from None
        except cdp.CdpError as e:
            raise Refused(str(e)) from None

    def _target(self, tab, ref):
        t = self._run(tab, "target", ref)
        if not t or t.get("gone"):
            raise Refused("%s is no longer on the page — it changed since it was read. Read it again "
                          "and use the new ref." % ref)
        return t

    # ── what an act answers with ──

    def _act(self, tab, doing, work, limit=ACT_LIMIT, watch=True):
        """Run `work()` on `tab` and answer with what it did to the page."""
        before_nav = tab.navigations
        before_url = tab.url
        before_tabs = {t.id for t in self.browser.pages()}
        if watch:
            self._run(tab, "watch")
        work()
        took, why = hands.settle(self.browser, tab, limit, since_navigations=before_nav)
        return self._consequence(tab, doing, before_nav, before_url, before_tabs, took, why, watch)

    def _consequence(self, tab, doing, before_nav, before_url, before_tabs, took, why, watched):
        out = {"did": doing, "settled_in": took}
        if why == "limit":
            out["still_loading"] = True
        new = [t for t in self.browser.pages() if t.id not in before_tabs]
        if new:
            out["new_tabs"] = [{"tab": short(t.id), "url": t.url, "title": t.title} for t in new]
            # A link that opened a tab: the work continues there, as it does for a person.
            opened_here = [t for t in new if t.opened_by == tab.id] or new
            self.browser.active = self.browser.attach(opened_here[-1])
            hands.settle(self.browser, self.browser.active, NAVIGATE_LIMIT / 2)
            out["now_in_tab"] = short(self.browser.active.id)
            tab = self.browser.active
        if tab.dialog:
            out["dialog"] = dict(tab.dialog)
            out["url"] = tab.url
            return out
        navigated = tab.navigations > before_nav or tab.url != before_url
        if not navigated and watched and not new:
            try:
                changes = self.browser.run(tab, "changes")
            except (cdp.CdpError, cdp.BrowserClosed):
                changes = None
            if changes and changes.get("appeared"):
                out["appeared"] = changes["appeared"]
        page = self._run(tab, "snapshot", False)
        out["url"] = page.get("url")
        out["title"] = page.get("title")
        if navigated or new:
            out["navigated"] = True
            out["page"] = page
        elif page.get("modal"):
            out["modal"] = page["modal"]
        return out

    # ── reading ──

    def status(self):
        """The view: whether the browser is open, its tabs, and the one actions go to."""
        with self.lock:
            try:
                self.browser.connect()
            except (cdp.BrowserClosed, cdp.CdpError):
                self.browser.close()
                return {"open": False}
            pages = self.browser.pages()
            active = self.browser.active if self.browser.active in pages else (pages[0] if pages else None)
            return {
                "open": True,
                "tabs": [{"tab": short(t.id), "title": t.title[:100], "url": t.url[:200],
                          "active": t is active} for t in pages],
                "dialog": active.dialog if active else None,
            }

    def read(self, tab=None, all=False):
        with self.lock:
            t = self._tab(tab)
            page = self._run(t, "snapshot", bool(all))
            page["tab"] = short(t.id)
            if t.dialog:
                page["dialog"] = dict(t.dialog)
            return page

    def find(self, query, tab=None):
        if not str(query or "").strip():
            raise Refused("find needs words to look for")
        with self.lock:
            t = self._tab(tab)
            found = self._run(t, "find", query, 40)
            return {"query": query, "found": found, "url": t.url, "tab": short(t.id)}

    def text(self, tab=None, limit=TEXT_LIMIT):
        with self.lock:
            t = self._tab(tab)
            got = self._run(t, "text", max(200, min(int(limit or TEXT_LIMIT), 200000)))
            got["url"] = t.url
            got["title"] = t.title
            return got

    def tabs(self):
        return self.status()

    def wait(self, text=None, seconds=5.0, tab=None):
        seconds = max(0.5, min(float(seconds or 5), 30.0))
        with self.lock:
            t = self._tab(tab)
            deadline = time.monotonic() + seconds
            while True:
                if text:
                    found = self._run(t, "find", text, 1)
                    if found:
                        return {"appeared": True, "element": found[0], "url": t.url}
                    body = self._run(t, "text", 200000)
                    if text.lower() in (body.get("text") or "").lower():
                        return {"appeared": True, "url": t.url}
                else:
                    took, why = hands.settle(self.browser, t, seconds)
                    return {"settled_in": took, "still_loading": why == "limit", "url": t.url}
                if time.monotonic() >= deadline:
                    return {"appeared": False, "waited": seconds, "url": t.url}
                time.sleep(0.25)

    # ── going places ──

    def go(self, url, tab=None, new_tab=False):
        url = str(url or "").strip()
        if not url:
            raise Refused("go needs a url")
        if "://" not in url and not url.startswith(("about:", "chrome:")):
            url = "https://" + url
        scheme = urllib.parse.urlsplit(url).scheme.lower()
        if scheme not in ("http", "https", "about"):
            raise Refused("go opens web pages (http, https); `%s:` is not one" % scheme)
        with self.lock:
            if new_tab:
                self._tab(None)
                before = {t.id for t in self.browser.pages()}
                t = self.browser.new_tab(url)
                took, why = hands.settle(self.browser, t, NAVIGATE_LIMIT)
                return self._consequence(t, "opened %s in a new tab" % url, -1, "", before, took, why, False)
            t = self._tab(tab)
            return self._act(t, "went to %s" % url,
                             lambda: self._call("Page.navigate", {"url": url}, t),
                             limit=NAVIGATE_LIMIT, watch=False)

    def history(self, step, tab=None):
        with self.lock:
            t = self._tab(tab)
            h = self._call("Page.getNavigationHistory", tab=t)
            index = h.get("currentIndex", 0) + step
            entries = h.get("entries", [])
            if not 0 <= index < len(entries):
                raise Refused("there is no page %s this one in this tab's history"
                              % ("before" if step < 0 else "after"))
            return self._act(t, "went %s to %s" % ("back" if step < 0 else "forward", entries[index].get("url")),
                             lambda: self._call("Page.navigateToHistoryEntry", {"entryId": entries[index]["id"]}, t),
                             limit=NAVIGATE_LIMIT, watch=False)

    def reload(self, tab=None):
        with self.lock:
            t = self._tab(tab)
            return self._act(t, "reloaded %s" % t.url, lambda: self._call("Page.reload", {}, t),
                             limit=NAVIGATE_LIMIT, watch=False)

    # ── hands ──

    def _pressable(self, t, ref, allow_commitment=False):
        target = self._target(t, ref)
        if target.get("invisible"):
            raise Refused("%s (%s \"%s\") has no size on the page: it cannot be pressed. Read the "
                          "page again; it may be inside something closed." % (ref, target["role"], target["name"]))
        if target.get("disabled"):
            raise Refused("%s (%s \"%s\") is disabled: the page will not let it be pressed yet — "
                          "usually a field it needs is empty or invalid." % (ref, target["role"], target["name"]))
        cover = target.get("covered_by")
        if cover:
            raise Refused("%s (\"%s\") is covered by %s \"%s\" (%s): pressing there would press that "
                          "instead. Deal with it first — often a cookie or sign-in dialog — then try again."
                          % (ref, target["name"], cover.get("role"), cover.get("name"), cover.get("ref")))
        word = commit.reads_as_commitment(target["name"])
        if word and not allow_commitment:
            raise Refused("%s (%s \"%s\") reads as a commitment (\"%s\"): pressing it may spend money, "
                          "send something or remove something, and that cannot be taken back. It is "
                          "pressed with commit ref=%s label=\"%s\" site=%s, which asks the person first."
                          % (ref, target["role"], target["name"], word, ref, target["name"], fold_site(t.url)))
        return target

    def click(self, ref, tab=None):
        with self.lock:
            t = self._tab(tab)
            target = self._pressable(t, ref)
            return self._act(t, "clicked %s \"%s\" (%s)" % (target["role"], target["name"], ref),
                             lambda: hands.click(self.browser, t, target["x"], target["y"]))

    def commit(self, ref, label, site, tab=None):
        with self.lock:
            t = self._tab(tab)
            here = fold_site(t.url)
            if fold_site(site) != here:
                raise Refused("this tab is on %s, not %s: nothing was pressed. The page changed since "
                              "the person was asked." % (here, fold_site(site)))
            if ref == "dialog":
                if not t.dialog:
                    raise Refused("no dialog is open on %s: nothing was accepted" % here)
                if not commit.same_label(t.dialog.get("message", "")[:len(label) + 20][:len(str(label))], label) \
                        and not commit.same_label(t.dialog.get("message"), label):
                    raise Refused("the dialog now says \"%s\", not \"%s\": nothing was accepted"
                                  % (t.dialog.get("message"), label))
                self._call("Page.handleJavaScriptDialog", {"accept": True}, t)
                return {"did": "accepted the dialog \"%s\"" % t.dialog.get("message"), "url": t.url}
            target = self._pressable(t, ref, allow_commitment=True)
            if not commit.same_label(target["name"], label):
                raise Refused("%s now reads \"%s\", not \"%s\": nothing was pressed. The page changed "
                              "since the person was asked." % (ref, target["name"], label))
            return self._act(t, "pressed %s \"%s\" (%s) on %s" % (target["role"], target["name"], ref, here),
                             lambda: hands.click(self.browser, t, target["x"], target["y"]))

    def _enter_is_safe(self, t, ref):
        names = self._run(t, "submits", ref) or []
        for name in names:
            word = commit.reads_as_commitment(name)
            if word:
                raise Refused("Enter here would submit a form whose button reads \"%s\" (\"%s\"), and "
                              "that cannot be taken back. Press that button with commit instead, which "
                              "asks the person first; nothing was typed." % (name, word))

    def type(self, ref, text, clear=True, enter=False, tab=None):
        with self.lock:
            t = self._tab(tab)
            target = self._target(t, ref)
            if target.get("covered_by"):
                cover = target["covered_by"]
                raise Refused("%s (\"%s\") is covered by %s \"%s\" (%s): deal with that first."
                              % (ref, target["name"], cover.get("role"), cover.get("name"), cover.get("ref")))
            if enter:
                self._enter_is_safe(t, ref)
            focused = self._run(t, "focus", ref, bool(clear))
            if not focused or focused.get("gone"):
                raise Refused("%s is no longer on the page; read it again" % ref)
            if not focused.get("focused"):
                raise Refused("%s (%s \"%s\") would not take the focus, so nothing was typed. It may "
                              "not be a field; read the page again." % (ref, focused.get("role"), focused.get("name")))

            def work():
                if clear:
                    hands.press(self.browser, t, "delete")
                hands.insert_text(self.browser, t, str(text))
                if enter:
                    hands.press(self.browser, t, "enter")
            shown = "(%d characters)" % len(str(text)) if focused.get("password") else "\"%s\"" % str(text)[:80]
            return self._act(t, "typed %s into %s \"%s\" (%s)%s" % (
                shown, focused.get("role"), focused.get("name"), ref, " and pressed Enter" if enter else ""),
                work, limit=NAVIGATE_LIMIT if enter else ACT_LIMIT)

    def press(self, key, ref=None, tab=None):
        k = hands.key_named(key)
        if k is None:
            raise Refused("`%s` is not a key this knows. Keys: %s, a letter or digit, with ctrl+, "
                          "shift+, alt+ or meta+ in front" % (key, ", ".join(sorted(hands.KEYS))))
        with self.lock:
            t = self._tab(tab)
            if ref:
                focused = self._run(t, "focus", ref, False)
                if not focused or focused.get("gone"):
                    raise Refused("%s is no longer on the page; read it again" % ref)
            if k[0] == "Enter":
                self._enter_is_safe(t, ref)
            return self._act(t, "pressed %s%s" % (key, (" in %s" % ref) if ref else ""),
                             lambda: hands.press(self.browser, t, key),
                             limit=NAVIGATE_LIMIT if k[0] == "Enter" else ACT_LIMIT)

    def select(self, ref, option, tab=None):
        with self.lock:
            t = self._tab(tab)
            got = {}

            def work():
                got.update(self._run(t, "pick", ref, str(option)) or {})
            out = self._act(t, "chose \"%s\" in %s" % (option, ref), work)
            if got.get("gone"):
                raise Refused("%s is no longer on the page; read it again" % ref)
            if got.get("not_select"):
                raise Refused("%s is a %s, not a list to choose from: click it to open it, then click "
                              "the option" % (ref, got.get("role") or "control"))
            if got.get("no_option"):
                raise Refused("%s has no option \"%s\". It has: %s" % (ref, option, ", ".join(got.get("options", []))))
            out["did"] = "chose \"%s\" in %s" % (got.get("chosen"), ref)
            return out

    def scroll(self, direction="down", ref=None, tab=None):
        with self.lock:
            t = self._tab(tab)
            if ref:
                moved = self._run(t, "scrollBy", 0, ref)
                if moved.get("gone"):
                    raise Refused("%s is no longer on the page; read it again" % ref)
            else:
                d = str(direction or "down").lower()
                page = self._run(t, "scrollBy", 0, None)
                h = page["viewport"][1]
                dy = {"down": h * 0.85, "up": -h * 0.85, "top": -10 ** 7, "bottom": 10 ** 7}.get(d)
                if dy is None:
                    raise Refused("scroll goes up, down, top or bottom — or to a ref")
                self._run(t, "scrollBy", int(dy), None)
            hands.settle(self.browser, t, 1.5)
            page = self._run(t, "snapshot", False)
            page["tab"] = short(t.id)
            return page

    def dialog(self, accept=True, text=None, tab=None):
        with self.lock:
            t = self._tab(tab)
            if not t.dialog:
                raise Refused("no dialog is open in this tab")
            message = t.dialog.get("message", "")
            word = commit.reads_as_commitment(message)
            if accept and word and t.dialog.get("type") == "confirm":
                raise Refused("the dialog asks \"%s\", which reads as a commitment (\"%s\"). It is accepted "
                              "with commit ref=dialog label=\"%s\" site=%s, which asks the person first."
                              % (message[:120], word, message[:120], fold_site(t.url)))
            params = {"accept": bool(accept)}
            if text is not None and t.dialog.get("type") == "prompt":
                params["promptText"] = str(text)
            self._call("Page.handleJavaScriptDialog", params, t)
            took, why = hands.settle(self.browser, t, ACT_LIMIT)
            return {"did": "%s the %s \"%s\"" % ("accepted" if accept else "dismissed",
                                                 t.dialog and t.dialog.get("type") or "dialog", message[:120]),
                    "url": t.url, "settled_in": took}

    def switch(self, tab):
        with self.lock:
            t = self._tab(tab)
            self._call("Target.activateTarget", {"targetId": t.id})
            self.browser.active = t
            return {"did": "switched to tab %s" % short(t.id), "url": t.url, "title": t.title}

    def close_tab(self, tab):
        with self.lock:
            t = self._tab(tab)
            if len(self.browser.pages()) <= 1:
                raise Refused("that is the browser's last tab; closing it would close the browser")
            self._call("Target.closeTarget", {"targetId": t.id})
            if self.browser.active is t:
                self.browser.active = None
            return {"did": "closed tab %s (%s)" % (short(t.id), t.url)}
