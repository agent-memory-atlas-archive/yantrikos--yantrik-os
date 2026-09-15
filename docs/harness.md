# Attaching a harness

Yantrik OS does not run your agent. It does not hold your endpoint, your model name or your API
key, and it has nowhere to put them. `yantrik-mind` has its own setup, hermes-agent has its own,
OpenClaw has its own — that work is done, and doing it again in this OS would mean doing it worse
and keeping it in sync forever.

What the OS owns is **which mind the person is talking to**. This is the interface for becoming
one of the candidates.

## The whole thing

Six methods on the `harness` socket, all spoken by the harness:

```text
harness.attach   {id, name, detail?, tools?, memory?}  → {session}
harness.poll     {session}                             → {turn_id, text, context} | {}
harness.chunk    {session, turn_id, delta}             → {}
harness.complete {session, turn_id}                    → {}
harness.fail     {session, turn_id, error}             → {}
harness.detach   {session}                             → {}
```

Attach, then loop: ask for a turn, stream the answer back in pieces, say you are done.
`crates/yantrik-harness/examples/echo_harness.rs` is a working one end to end, and the only part
a real harness replaces is the function that produces the answer.

## Why the harness dials in

The OS never connects to you. That is deliberate, and three useful things follow:

- **Anything with a JSON-RPC client can be a harness.** No callback URL, no inbound port, no
  reachability requirement. A harness in a container or behind NAT works like one running beside
  the shell.
- **The OS stores nothing about you.** There is no config file to install and no field anywhere
  for an endpoint or a credential. The protocol has no place to put one, and there is a test that
  fails if a field is ever added.
- **Being attached is what makes you exist.** You appear in the picker when you attach and are
  gone when you stop polling. Nothing to deregister, and nothing can be listed that is not
  actually there.

## Driving the desktop

Separate, and it already exists. An attached harness reads and steers the OS through the control
surface every app publishes — `app.describe` and `app.act`, or the `yos` command — which is
graded `safe`/`standard`/`sensitive`/`dangerous` and enforced against your ceiling. See
[app-control.md](app-control.md).

Attaching is about the conversation. Driving is about the desktop. Keeping them apart means a
harness can do either without the other: a mind that only talks never needs permissions, and a
script that only acts never needs to attach.

## Rules worth knowing

- **`id` is what a person types to select you**, so it cannot be empty or contain spaces.
- **You cannot attach over a built-in.** The companion is compiled into the shell; a client
  taking its id could leave a machine with no working mind and no way to say so.
- **Re-attaching under the same id replaces the old you.** That is what a harness that crashed
  and came back should get, and any turn the old one owed is failed rather than left hanging.
- **Stop polling and you are dropped** after 90 seconds, with anything you owed failed so nobody
  is left waiting on an answer that is not coming.
- **Nothing waiting is an ordinary reply**, not an error. You will poll far more often than a
  person types.

## What is not here yet

The shell does not serve this socket yet, and the picker and Settings screen are not built. The
protocol, the host state machine and the reference client are, and the host is tested without a
socket in the loop — `cargo test -p yantrik-harness`.
