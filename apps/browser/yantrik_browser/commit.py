"""Controls whose label reads as a commitment: pressing one spends money, sends something, or
removes something, and what it did cannot be taken back.

The same words as `yos web` and yantrik-mind's browser driver, so every path draws the same line.
Broad on purpose: a false positive costs the person one card; a false negative costs money or a
sent message.
"""

import re

COMMIT_WORDS = (
    "buy", "purchase", "order", "checkout", "check out", "pay", "payment", "subscribe",
    "place order", "send", "submit", "post", "publish", "confirm", "book now", "reserve",
    "delete", "remove", "cancel subscription", "deactivate", "close account", "transfer",
    "withdraw", "sign contract", "agree and", "accept and", "apply now", "donate", "tweet",
    "reply", "share", "unsubscribe", "empty trash", "sign up", "create account", "register",
)

# Words that make a label harmless whatever else it says: "Order history", "Shopping cart".
HARMLESS = ("history", "status", "details", "track", "summary", "help", "learn more", "sort",
            "view", "filter")

_WORD = {w: re.compile(r"(?<![a-z])" + re.escape(w) + r"(?![a-z])") for w in COMMIT_WORDS}


def reads_as_commitment(label):
    """The commitment word a control's label carries, or None."""
    text = " ".join(str(label or "").lower().split())
    if not text:
        return None
    for word, pattern in _WORD.items():
        if pattern.search(text):
            if any(h in text for h in HARMLESS) and word in ("order", "post", "share", "reply"):
                continue
            return word
    return None


def same_label(a, b):
    """Whether two labels are the same words, whatever the spacing and case."""
    fold = lambda s: " ".join(str(s or "").lower().split()).strip(" .…")
    return fold(a) == fold(b)
