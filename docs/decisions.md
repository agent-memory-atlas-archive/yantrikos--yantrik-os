# Decisions: one verdict from every decision model

Yantrik uses small decision models — System One models such as TypeSafe's Jev, Kev, Laya and
Jeff, or the chat model standing in for one — for quick judgements. Examples:

- which tool a request needs;
- whether a browser press would buy, send or delete something;
- how urgent a notice is.

Which model answers is the person's choice, made in **Settings → AI & Intelligence → Decision
model**. Off is a choice too.

Every model answers in the same **verdict**. A caller written against it does not know which
model answered, which is what lets the model be switched without anything else changing.

## Questions

| type | asks | answer |
|---|---|---|
| `noul` | does this condition hold? | the probability of yes |
| `choice` | which of these named options? | the pick, a probability per option, and how concentrated they are |
| `score` | where on this scale (levels, lowest first)? | the expected level (fractional) and a probability per level |

The questions asked together all see the same state, and each is answered independently.

## The verdict (wire form)

```json
{
  "answers": {
    "commit":  {"type": "noul", "yes": 0.93},
    "tool":    {"type": "choice", "choice": "get_weather",
                "probabilities": {"get_weather": 0.91, "none": 0.09}, "confidence": 0.91},
    "urgency": {"type": "score", "value": 1.6, "distribution": [0.1, 0.2, 0.7]},
    "other":   {"type": "abstain", "reason": "no decision model is set"}
  },
  "by": {
    "adapter": "systemone",
    "provider": "kev",
    "model": "kev-latest",
    "locality": "home",
    "calibrated": true
  },
  "latency_ms": 41
}
```

- **`abstain`** means no decision. The model is off or unreachable, or it could not tell. A caller
  treats it exactly as it would treat having no decision model. It is not an error, and a caller
  must never read it as a "no".
- **`adapter`** says how the model was reached:
  - `systemone`: a `/v1/systemone` server;
  - `chat_model`: the configured chat model answering as JSON;
  - `off`;
  - `custom`: anything else.
- **`provider`** is `jev`, `kev`, `laya`, `jeff`, `systemone` (another such server), the chat
  backend's name, or `off`.
- **`locality`** is where the model ran, which is where the state went:
  - `this_machine`;
  - `home` (a private or `.local` address);
  - `cloud`;
  - `nowhere` (off).

  It is read from the endpoint's address. An address that cannot be told apart counts as
  `cloud`.
- **`calibrated`** is true for a System One model, whose probabilities are trained to mean what
  they say. It is false for a chat model's numbers and for an unknown judge. Thresholds tuned on
  a calibrated model are coarser guides for an uncalibrated one.
- **A score is a position on a scale, never a probability.**

## Adapters

In Rust these live in `crates/yantrik-ml/src/judge/`: the `Judge` trait, `Judge::decide` giving
a `Verdict`, and `Verdict::to_json`.

| adapter | what it handles |
|---|---|
| `SystemOneJudge` + `Dialect` | Jev needs a key (only the name of the variable holding it is configured). Laya's confidence is recomputed from its probabilities. Jeff's 529 "busy" is waited out. |
| `ChatJudge` | The chat model writes JSON for the same questions. The state is marked as data, not instructions. Anything unusable becomes an abstention. |
| `OffJudge` | Abstains on every question. |

Adding a model means adding a dialect (or an adapter) that answers in this verdict. No caller
changes.

## Rules for callers

- **An abstention is "no judge".** Decide as you would without one.
- **In a safety path a judge may only add friction, never remove it.** The browser's commitment
  check is the word list OR the judge: a judge can add a card and cannot take one away. No judge
  authorises, declassifies or consents.
- **Respect `locality`.** A caller holding private data asks only a model whose locality its
  labels allow. In incognito the companion asks no model at all.

## Configuration

This is the companion's `judge:` section, written by Settings:

```yaml
judge:
  provider: kev            # off | jev | kev | laya | jeff | systemone | chat_model
  endpoint: "http://192.168.4.20:8009"
  model: kev-latest
  api_key_env: ""          # the NAME of the variable holding a key, e.g. JEV_API_KEY
  timeout_ms: 2000
  route_tools: true        # use it to choose which tool a request needs
  browser_commitments: true  # use it to spot purchases, sends and deletes in the browser
```

An older section without `provider` is read as a System One server when `endpoint` is set, and
as off otherwise.
