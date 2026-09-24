# service-methods

Holds `docs/app-control.md`'s table of a service's own methods — the JSON-RPC methods each
service answers beside `app.describe` and `app.act`, which meet no ceiling, no mode and no grant
(#161) — to what the services in `services/` actually dispatch.

It reads the source rather than running it: a match arm whose pattern is a dotted string, or a
constant that the service's `yantrik_ipc_contracts` modules (or its own source) define as one.
A method added to a service without a row fails, and so does a row for a method nothing answers.

```sh
python3 -m unittest discover -s tests/service-methods -v
```
