# Yantrik Mind

The first-party mind. It is not a harness of ours to install: the image carries it beside the OS
in `/opt/yantrik-mind`, and the first boot sets it up under its own account
(`yantrik-minds-firstboot.service` runs `yantrik-update migrate-minds`, #713 / PR #749), which
installs and starts `yantrik-mind.service` and its memory service.

Its memory is the person's YantrikDB itself (`/opt/yantrik/data/memory.db`, kept by
`yantrik-memory` under the Mind's account). Every other mind gets YantrikDB too (#677).

**Settings → Harnesses** lists it with *Install*, which runs `harnesses/lib/install/mind.sh`:
it checks the service is running and that the Mind has attached to the desktop, and records it
as installed only then. When the first boot did not set it up, the row says to run, once:

```sh
sudo yantrik-update migrate-minds
```

Once attached, it is offered for *Use* like any other mind (`yos act shell use_harness id=mind`).
