# Yantrik OS — findings from inside the machine

Written 2026-09-17, around 21:40 CDT, by Claude Code running as a process on the machine (not attached as a mind).
Build: `/opt/yantrik/BUILD` → `v0.1.0-179-g6fc8b13`, nightly, installed 2026-09-17T18:52Z. Uptime 7h41m.
I stuck to reading. The only thing I "did" to the desktop was read-only `yos describe` / MCP describe calls.
The scripts I used are in `~/inside-report/scratch/`. None of them write outside that folder.

Left out because they are already filed: apps listed that cannot open, and a second copy of a single-instance app recorded as a failed launch.

## Order, and why #1 is first

#1 comes first because it affects every user all the time without them doing anything, and they notice it physically: a hot, loud, slow machine, or a drained battery on a laptop. Next come the things that make the machine's main promise, a mind that knows you and can act for you, quietly untrue: #2 the built-in mind has no brain, #3 its memory is noise, #4 approvals die silently. After those come things a Mac/Windows user would hit in the first hour (#5 to #7), then correctness of what the OS tells its minds (#8 to #10), then hygiene.

---

## 1. The Terminal eats one and a half CPU cores while it sits doing nothing

- **Seen:** `yos describe terminal` → `Terminal — in /home/yantrik, nothing run yet`, `"last_command": null`. At the same moment:
  ```
  top:  20704 yantrik ... S 165.3  2.0 289:10.33 yantrik-terminal
  ps:   20704  118%  04:04:22 yantrik-termina
  system-monitor: "busiest yantrik-termina (118%)"
  load average: 2.63 2.67 2.57 on 4 cores
  ```
  That is 289 CPU-minutes in 4 hours for a window showing only `$ `. labwc is also at 27% constantly, which fits a window redrawing non-stop. The shell's own hourly snapshots record "CPU: avg 53%" on an idle desktop.
- **Who it hurts:** anyone who opens Terminal once and leaves it open (it is pinned in the dock). On a laptop: fans and battery. Everywhere: every other app gets slower.
- **Verified.** I measured it twice with top and once with the OS's own system-monitor. I did not profile *why*. My guess is a busy redraw or PTY-poll loop.
- **Do:** profile the idle loop (`perf top -p`). Make the terminal block on PTY input and redraw only on damage. Add a CI check that fails if any app uses more than 2% CPU when idle.
- **Size:** a day.

## 2. The desktop's built-in assistant has had no brain all day, and nobody was told

- **Seen:** in `/opt/yantrik/logs/yantrik-os.log`:
  ```
  Using API LLM backend base_url=http://localhost:8341/v1 model="qwen3.5-4b"
  Onboarding: no LLM runtime reachable tried=3 ... recommend="cloud"
  WARN LLM offline: API request failed: io: Connection refused
  Response served by offline responder          (the morning brief)
  ```
  Port 8341 is not listening; the only listeners are 22, 7440, 8077 and 8090. Counts for today: "LLM offline" 447 + 202, "Suppressing EXECUTE urges — LLM offline" 446 + 201. `yos describe shell` shows `"companion_online": false`. So the morning brief, the proactive features ("resource_guardian", "error_companion", …) and the urges never ran. Onboarding worked out that there was no runtime, recommended cloud, and then kept pointing at localhost anyway.
- **Also in this loop:** the urge prompt says `Surface one interesting memory connection for .`, with an empty name, even though `config.yaml` has `user_name: "Pranab"`. The cooldown logs `elapsed_secs=1789671372`, which is counting from the Unix epoch. Both lines repeat every minute, which is most of the log's volume.
- **Who it hurts:** the owner, every day. The "proactive mind" features are silently dead. The chat panel still works only because Hermes is the chosen harness.
- **Verified.**
- **Do:** when no LLM is reachable, send the companion's work to the attached harness (Hermes or Mind, which *do* have models). Failing that, put one visible "Assistant offline — set up a model" item in the shell instead of 450 log lines. Fix the empty name, and the epoch-based cooldown.
- **Size:** a day.

## 3. "169 memories" is 136 copies of "Connected to network" — nothing about the person, and not the memory the minds share

- **Seen:** `yos describe shell` → `"memories": 169`. That comes from `/opt/yantrik/data/memory.db` (log: `Companion initialized db_path="/opt/yantrik/data/memory.db"`). Read-only query:
  ```
  source=system 170/170
  136  Connected to network 'Wired connection 1'
    2  App opened: yantrik-termina      (names cut to 15 chars)
   10  system/snapshot   4 system/general
  ```
  Meanwhile the "one shared memory" the minds use is a *different* store: `127.0.0.1:7440/health → {"served_by":"yantrik-mind","server":"yantrik-memory"}` backed by `~/.local/share/yantrik-mind/mind.db`. Yantrik Mind has logged `rehearse: nothing stored yet` every 15 minutes for 7 hours, although Hermes wrote to it at 11:53 and 11:58 (`yantrikdb_remember … stored: true`).
- **Who it hurts:** anyone who opens the Memory screen expecting to see what the machine knows about them. They get a counter padded by a DHCP/link event firing about every 3 minutes. And the minds can't see the shell's store, or the other way round.
- **Verified** for the contents and for there being two stores. **Suspected** for why Mind reports "nothing stored" despite Hermes' writes: it may count only its own records, or a different namespace.
- **Do:** only record a network memory when the state *changes*, and store system telemetry apart from personal memory so it never counts as "memories". Decide on one memory store (the 7440 server) and point the shell's companion at it.
- **Size:** a day for de-duplicating and separating. More to merge the two stores.

## 4. (Absence) When a mind needs permission, the desktop has no way to ask — you have to type `/approve` into chat in time

- **Seen:** `~/.hermes/logs/gateway.log`:
  ```
  [yantrik] turn 15 waits for the person: ⚠️ **Dangerous command requires approval:** ```…
  User approved 1 dangerous command(s) via /approve (session)
  [yantrik] turn 18 waits for the person: ⚠️ **Confirm /new** … Choose: • **Approve Once** …
  ```
  and in `state.db`, 4 tool results today:
  `BLOCKED: Command timed out without user response. The user has NOT consented…`
  The "choices" are markdown bullets in the chat stream. The shell's conversation shows the result literally as user `/approve` → assistant `✨ Session reset! Starting fresh.`, which reads like approving caused a reset. No notification is raised: the `notifications` surface reports `count: 0`.
- **Who it hurts:** anyone who gives a mind a task and walks away, or has the chat panel closed. The work dies on a timeout they never saw. Mac and Windows both have a system-level consent dialog, and this machine's whole premise is a mind that acts.
- **Verified** (log and transcript). The claim that no notification is raised is based on the notifications surface being empty now, not on watching one arrive.
- **Do:** add a harness-level `request_approval` message that the shell shows as a real prompt with Approve/Deny buttons plus a notification, and have Hermes' adapter use it instead of chat text.
- **Size:** more than a day (protocol + shell UI + adapter).

## 5. Notifications from other apps skip the desktop's notification centre

- **Seen:** `yantrik-os.log` line 27:
  `WARN D-Bus notification daemon failed to start — notifications from other apps will not appear … error=name already taken on the bus`.
  `busctl --user status org.freedesktop.Notifications` → `PID=754 … mako`. `mako-notifier 1.10.0-1` is installed and wins the race at login.
- **Who it hurts:** anyone using Chromium, or any non-Yantrik app, that sends notifications. Those appear as mako popups in a different style, never reach Yantrik's centre or Do Not Disturb, and are invisible to the minds (`notifications` describe: `count: 0`).
- **Verified** that mako owns the name and the shell lost it. **Not verified** end-to-end with a real Chromium notification.
- **Do:** stop starting mako in the session (labwc autostart or the D-Bus activation file), or make the shell take over the name with replacement.
- **Size:** an hour.

## 6. The browser can't open a Save or Upload dialog properly

- **Seen:** Chromium launched from the dock logs:
  `Failed to call method: org.freedesktop.DBus.Properties.Get: /org/freedesktop/portal/desktop: No such interface "org.freedesktop.portal.FileChooser"`.
  `/usr/share/xdg-desktop-portal/portals/` contains only `wlr.portal`, which covers screenshot/screencast but not file chooser. No gtk or gnome portal is installed.
- **Who it hurts:** anyone who tries to attach a file to an email in the browser, upload a photo, or "Save as…". It is the first thing a Windows or Mac user does.
- **Verified** that the interface is missing. **Suspected** what exactly the user sees (a fallback GTK dialog or nothing); I did not click through it.
- **Do:** ship `xdg-desktop-portal-gtk`, or a Yantrik FileChooser backend that opens the Files screen, plus a `portals.conf` routing FileChooser to it.
- **Size:** an hour for gtk, more than a day for a native one.

## 7. The shell tells its minds that open apps aren't running, and that it's on Wi-Fi when it's on a cable

- **Seen:** `yos describe shell --full`:
  ```
  Yantrik — files screen, 6 windows open, calendar, email, notes and perception not running
  "windows": [ Browser, Calendar, Downloads, Email, Notes, Terminal ]
  "services": notes/calendar/email → "status": "stopped", "note": "on demand"
  "wifi": true
  ```
  But `yos describe notes` answers ("Desktop smoke test", 145 words), `ps` shows yantrik-notes up for 7h14m, and `network` says `online via ethernet … "ssid": null`. Hermes' log shows it believing the headline: `notes is not running, so it cannot be described…`.
- **Who it hurts:** every mind that reads the shell's one-line summary (the first thing an agent sees) and then re-opens apps or reasons from wrong state. People also see a Wi-Fi icon on a wired machine.
- **Verified.**
- **Do:** build the "not running" list from live sockets and windows, not from the service manager's on-demand record. Set `wifi` from the network service's `type`.
- **Size:** an hour or two.

## 8. System Monitor reports the wrong CPU numbers and chops process names

- **Seen:** system-monitor, at the same moment as `top -b`:
  ```
  system-monitor: yantrik-ui 92.7%   top: yantrik-ui 5.0%
  system-monitor: name "yantrik-termina", "yantrik-calenda", "yantrik-downloa"
  ```
  92.7% is yantrik-ui's *lifetime average* (`ps pcpu` = cputime/elapsed), not its current use. Names are the kernel's 15-character `comm`. The same cut names end up in memories (#3).
- **Who it hurts:** anyone hunting what is slowing the machine, and any mind using `kill_process` (dangerous) on that list. It would blame the shell for load it isn't causing.
- **Verified.**
- **Do:** sample `/proc/<pid>/stat` deltas over an interval, and use `/proc/<pid>/cmdline` or exe basename for names.
- **Size:** an hour.

## 9. The "what has the OS noticed" tool always fails

- **Seen:** MCP `os_perception(count=10)` → `yos: no socket for 'perception' (try: yos ls)`. The log shows `Service registered service="perception"` but never "Starting service"; the shell lists it as `"note": "on demand", "status": "stopped"`. `perception-service` and `perception-journal` are in `/opt/yantrik/bin`. Hermes hit the same error at 17:39.
- **Who it hurts:** every mind. The tool is advertised to them as "the machine's own account of what has been happening", and it has never answered on this boot.
- **Verified.**
- **Do:** start the service on the first `perception` request (the demand never triggers a start), or start it at boot. Until it works, hide the tool.
- **Size:** an hour to a day.

## 10. (Absence) Most of the installed apps, and the browser, can't be driven by the mind

- **Seen:** `/opt/yantrik/share/applications` offers 16 Yantrik apps and the dock pins `browser`. `yos ls` gives control surfaces for only 5 of them: calendar, download-manager, email, notes, terminal. None exists for Editor, Images, Music, ySheets, yDoc, yPresent, Snippets, Containers, Network, System Monitor (the app), Weather (the app), or Chromium. The shell does have `editor_*` actions, but those drive its own built-in editor, not `yantrik-text-editor`. So "open this spreadsheet and fill column B" or "open that page" is impossible, although the premise is that the mind can drive the desktop.
- **Who it hurts:** anyone asking a mind to do real document or web work. That is the headline use case.
- **Verified** (listing against sockets). The claim that the missing apps can't be driven another way is **suspected**: the a11y service might reach them, and I did not test it.
- **Do:** give the document apps and the browser (via CDP, which the `web_*` MCP tools suggest exists somewhere) a minimum `app.describe` / `app.act` surface: open, read, write, save. Until then, mark in the launcher which apps a mind can operate.
- **Size:** more than a day (per app, about a day each).

## 11. The mind's web login page is open to anyone on the machine, even though it's configured off

- **Seen:** the unit sets `Environment=YM_WEB=off`, and `~/.config/yantrik-mind.env` does not override it. `yantrik-mind` still logs:
  ```
  [web-ui] first-time registration is OPEN — code in ~/.local/share/yantrik-mind/web-pairing.code
  [web-ui] browser surface on http://127.0.0.1:8090
  ```
  Port 8090 is listening.
- **Who it hurts:** the owner, if any other local process or user registers first. It is loopback only, so the risk is local, but the switch that says off does nothing.
- **Verified** that it is listening. I did not try to register.
- **Do:** honour `YM_WEB=off`, and close first-time registration by default.
- **Size:** an hour.

## 12. The machine can't agree on its own version number

- **Seen:** `/opt/yantrik/.version` → `0.3.0`. `/opt/yantrik/BUILD` → `version=v0.1.0-179-g6fc8b13`. The terminal banner says `Yantrik Terminal v0.1.0`.
- **Who it hurts:** anyone filing a bug or reading an About screen, and the updater's own comparisons if they read the wrong file.
- **Verified** (files). Which file the updater reads is **not checked**.
- **Do:** generate one version at build time and stamp it into `.version`, `BUILD` and every binary's `--version`.
- **Size:** an hour.

---

### Not included, noted in passing
- Hermes' Discord adapter logs "No bot token configured" every few minutes (74 lines in errors.log). That is Hermes config, not the OS.
- Hermes overflowed its 65K context at 17:57, and one `write_file` call had its content replaced with `{}` by argument sanitising. That is Hermes-side and probably known.
- labwc warns that `titlebar.height is no longer supported`, and `XCURSOR_THEME`/`XCURSOR_SIZE` are unset, so the cursor may be the fallback size. Cosmetic, not verified on screen.
- The Yantrik Mind harness advertises `"tools": false` in the mind picker. That may be by design; I didn't check.
