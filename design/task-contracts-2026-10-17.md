# Task contracts: the centre of Yantrik OS

17 Oct 2026. This came out of a three-round discussion between Claude and GPT-6 Astra that Pranab asked for, and Pranab adopted it as the direction the same day. The transcripts are in the PR description.

## The vision

Yantrik is a personal operating system that carries work forward without taking ownership away from you. You ask for an outcome, and it keeps four things together: what you meant, what it may do, what it has produced, and how you will know it worked. It works quietly within the permission you gave it, stops when it needs more, and tells a promising draft apart from a verified result. Its memory keeps useful context without keeping everything forever. Its home screen shows what matters now: work underway, results ready, and decisions that genuinely need you. You can inspect the evidence, change direction or stop the machine. The organising idea is simple: every undertaking has a purpose, a boundary, and an honest account of what happened.

## Why

In one week the machine showed the same failure in five shapes, and every one of them collapsed four different events into one: **said, attempted, changed, achieved**.

- A mind said FINISHED over code it had never written.
- A mind claimed it needed approval while an approval rule was in force.
- Three different models made the same unasked offers because our own prompt told them to.
- Apps shipped with buttons that only logged.
- The live stream showed exertion (grep) instead of consequences (the game).

So the problem isn't model honesty. The architecture lets an utterance stand in for an outcome.

## The primitive: a task contract

A task contract is a revisioned, executable agreement about an intended outcome. It holds the authority envelope for reaching that outcome, and the conditions under which a result may be accepted. It isn't a prompt, and it isn't a promise of success. It is the object through which work gains, and loses, permission and status.

| Field | Contents | Who may write it |
|---|---|---|
| `id`, `revision`, `parent_revision` | identity and an immutable revision chain | the contracts module |
| `intent` | brief, intended outcome, beneficiary, material assumptions | proposed by the requester's agent; activated by the requester or a standing policy |
| `owner` | the accountable requester and the assigned executor | the requester, or an authorised scheduler |
| `grant` | purpose, allowed actions, resources, recipients, limits, expiry | an authorised grant issuer, never the executor by proposing it |
| `acceptance` | required checks, evidence required, the authorised acceptor | the requester, or a preauthorised template |
| `state`, `reason` | proposed / ready / running / blocked / delivered / accepted / failed / cancelled | the contracts module, from authorised events |
| `artifacts` | versioned candidates and branch references, each labelled isolated or proposal | the executor submits references |
| `evidence` | check results, input versions, receipt references, known gaps | attributed runners and evaluators; the executor's own reports stay labelled as its own |
| `decision` | accept or reject, the actor, the revision and evidence it applied to | the named acceptor, validated by the module |
| `retention` | capture scope and deletion deadlines | the person's policy; the executor can't broaden it |

Rules:
- **Delivered is not accepted.** "FINISHED" from a mind produces `delivered`, never `accepted`.
- A change to the intent, grant or acceptance criteria creates a revision. No approval carries across a material change.
- Execution uses the contract's grant intersected with the global ceilings, and an emergency freeze always wins.
- When the work reaches an irreversible step that wasn't declared in advance, it stops hard and asks.
- A later-found defect attaches to the historical decision; it never rewrites it.

What it absorbs:
- **Director missions** become contracts. PLAN.md stays as a checklist.
- **Approval cards** become views onto grant requests, showing the permission delta rather than "allow shell for this session".
- **Notices** stay as presentation, linked to a contract where there is one.
- **Control surfaces** gain effect declarations.
- **The decision log** stays the receipt store, now with contract references.
- Not everything is a contract: only delegated outcomes are.

## Effect semantics

Effect is a dimension of its own, separate from the risk grade (safe / standard / sensitive / dangerous):

- **observe**: reads, without changing external state or disclosing anything beyond the approved boundary.
- **stage**: writes only inside an enforced, isolated candidate space; publishing is a separate action.
- **compensatable**: changes live state and has a named corrective action, without promising full reversal.
- **irreversible**: sending, publishing, disclosing, paying, or anything else that can't reliably be recalled.
- **unknown**: effects not characterised; it cannot run unattended.

Each declaration names the resources it touches, and whether its enforcement is **verified** or merely **asserted**. A browser GET is not automatically observe. A shell command is not safe because its text looks harmless.

Rollout, our amendment: declare effects first on the actions that live missions actually use. *unknown* blocks unattended runs only on paths that have become contracts. Turned on everywhere at once, it would stop the machine.

## Plan: three one-week builds, live machine first

1. **One live mission becomes legible.** Contract persistence, revisions, runner events, and contract cards on the stream; the showcase work on `feat/live-showcase` is the display. One mission template, *repair an app's save→restart→restore*, runs as a contract. Acceptance is manual at first.
   - Worked if: Pranab can follow the mission without opening logs, the contract survives a backend restart, and a deliberately false FINISHED stays `delivered`.
2. **One honest boundary.** A contract-aware dispatcher. The mission may read the selected project, edit its isolated checkout, run sandboxed tests, and request application to the live project. Unrestricted shell and network are off on that path. Grants are bound to a revision and checked at dispatch. Cards show the permission delta.
   - Worked if: the mission runs unattended in isolation, and editing another project, reaching an unapproved secret, or applying after revocation is refused by the boundary itself and explained on the stream.
   - This retires #823's session-wide `agent_run` allowance, a labelled stopgap until then.
3. **Acceptance that survives an adversarial executor.** A fixed save→restart→restore harness outside the executor's writable space. Artifacts are pinned to their check results, and a changed artifact invalidates its old check.
   - Worked if: a real repair is accepted, and fake receipts, modified tests, stale evidence and a deliberately broken implementation can't reach `accepted`.

## Stop doing

- Treating FINISHED, ticked checklists or screenshots as proof.
- Granting broad `shell.agent_run` through session approvals (labelled as legacy until week 2).
- Building the stream from grep.
- Conflating the risk grade with the effect type.
- Calling live browser work a sandbox.
- Using hash-change as a general acceptance gate. It rejects a correct "already working" and rewards meaningless edits. The Director's guard from #838/#839 is an interim heuristic.
- Letting authority expand automatically from a track record. A record may recommend a narrow grant, never mint one.
- Capturing everything forever.
- Wrapping every notice or click in a contract.

## How we would know this is wrong

Run Pranab's next 20 ordinary, bounded missions, half through contracts and half through the old mission interface. Use the same executor, tools and safety boundaries, and plant some harmless false completions, scope changes and stale evidence. Measure:
- steering time;
- mistaken completions;
- permission-boundary errors;
- whether Pranab can explain what happened.

**If contracts add more than 20% steering time without reducing mistaken completions or boundary errors, and he keeps leaving the contract view to understand the work, contracts are not the centre.** Grants and receipts stay as infrastructure, and the product centres on artifacts and conversation instead.

## Open, not settled by this document

- **One assistant over a federation of minds.** Memory keeps disagreements and stays quiet, raising them only when a decision depends on them. What the person sees of that is not designed yet.
- **Where the contracts module lives** once it outgrows the live machine. An OS-owned service like notifications is the likely answer: a mind must not be able to write its own acceptance.

## An ordinary Tuesday

Pranab says, "The notes app lost yesterday's shopping list. Fix that before lunch." A small card appears: **Make notes survive restart**, with "Working on a private copy. Your notes stay untouched." He makes coffee. Yantrik reproduces the bug, repairs it, and tests a saved list through a restart. The card changes: **Ready to apply. This will restart Notes.** He taps Apply. Yantrik backs up the data, installs the tested version and checks persistence again. The card settles into **Fixed · save-and-restore check passed**, with the backup underneath. No victory speech. His shopping list is there.
