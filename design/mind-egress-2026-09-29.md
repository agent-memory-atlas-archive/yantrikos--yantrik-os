# Where a mind may connect: egress for the mind account

29 September 2026. Status: **step 1 built** (`crates/yantrik-egress`: the proxy, audit and enforce, the control socket);
**step 3 built 5 October 2026** (the kernel holds the mind account to the proxy, in audit and enforce
alike: section 1); the rest for review by Pranab and yantrik-mind-72.
Prompted by NVIDIA OpenShell 0.1.0 (28 Sep 2026): its default-deny egress is the one thing it does
that we don't.

## The gap

`yantrik-mind` cannot read the person's files: `ProtectHome`, `InaccessiblePaths`, its own
account. It reaches the desktop only at the mind door, where every act is graded. But its unit
allows `AF_INET`/`AF_INET6` to anywhere. So a mind, or a plugin or MCP server it was talked into
loading, can send whatever it holds to any host:

- its memory (`mind.db`);
- what the desktop showed it;
- what a person typed to it;

and it can fetch and run anything. The door governs what it can **do** on the desktop. Nothing
governs where it can **send**.

## What we build

### 1. Default deny, by account

nftables, in the same shape as the DevTools guard (`yantrik-update`, `table inet yantrik_devtools`),
matched on the socket's owner. Until this, the proxy was advice: the mind's unit points
`HTTPS_PROXY` at it, and anything running as `yantrik-mind` that ignored the variable (a plugin, a
tool it spawned, a library that connects by itself) reached anywhere directly, past the policy,
enforce and Private mode. Now the kernel enforces it, in audit and in enforce alike (audit is the
proxy's policy, not a hole in the kernel's). As built (`/etc/yantrik/mind-egress.nft`):

```
table inet yantrik_mind_egress {
  counter refused {}  counter dns {}  counter direct {}
  set resolvers4 { type ipv4_addr; … }            set resolvers6 { type ipv6_addr; … }
  set direct4 { type ipv4_addr . inet_service; … } set direct6 { type ipv6_addr . inet_service; … }
  chain output {
    type filter hook output priority filter; policy accept;
    meta skuid <yantrik-mind> jump mind
  }
  chain mind {
    oifname "lo" accept                                    # the proxy, the memory server, a local model
    meta l4proto { tcp, udp } th dport 53 ip daddr @resolvers4 counter name "dns" accept
    meta l4proto { tcp, udp } th dport 53 ip6 daddr @resolvers6 counter name "dns" accept
    ip daddr . tcp dport @direct4 counter name "direct" accept
    ip6 daddr . tcp dport @direct6 counter name "direct" accept
    limit rate 6/minute burst 12 packets log prefix "yantrik-mind-egress refused: " level info
    meta l4proto tcp counter name "refused" reject with tcp reset
    counter name "refused" reject with icmpx admin-prohibited
  }
}
```

- **A positive match and a jump.** A packet with no socket (a kernel reply, IPv6 neighbour
  discovery) has no owner; `meta skuid != N return` would let such a packet fall into the
  refusals for every account on the machine. Only packets the mind account owns enter `mind`.
- **Resolvers** are the ones `/etc/resolv.conf` names when the table is made (the first three,
  loopback left out because loopback is let through whole, a zone dropped). systemd-resolved's
  127.0.0.53 is loopback.
- **The direct set** is the only way past the proxy, and it comes only from the person's policy
  (section 3), never from anything the mind account can write. An entry is a rule with
  `lan: true` whose host is a **literal address on the local network** (10/8, 172.16/12,
  192.168/16, 100.64/10, fc00::/7), one entry per port, TCP only. A host name never makes an entry:
  it keeps going through the proxy, which decides on the address it resolves to. A public address
  never does either: the internet is reached through the proxy, where it is counted. Private mode
  empties the set, so Private mode still cuts everything but loopback and DNS.
- **Who loads it.** Not the proxy, which stays unprivileged (no `CAP_NET_ADMIN`). `yantrik-update
  mind-egress apply`, as root: it asks the proxy's own code for the set (`yantrik-egress direct`,
  run as `yantrik-egress` with `setpriv`, so the policy is parsed by the code that enforces it and
  never as root, and root never opens a file in a directory another account owns), checks every
  line again (literal LAN addresses, ports 1–65535, at most 64 entries), writes the file whole
  (beside it, renamed over it) and loads it with `nft -f`, one transaction. Three units, written by
  the updater: `yantrik-mind-egress.service` at boot (after nftables, before the network, the
  proxy and the mind; `PartOf=nftables.service`, so a reload of the ruleset puts it back),
  `yantrik-mind-egress.path`, which runs `yantrik-mind-egress-refresh.service` when `policy.yaml`,
  the `private` marker or `/etc/resolv.conf` changes, and every `yantrik-update reconcile`/`apply`.
- **Fail closed.** A policy that does not read, an answer that does not check, or more than 64
  entries loads the table with an **empty** direct set, never the previous one; the file's comment
  and the journal say why. A table that does not load is replaced by one with nothing direct and
  no DNS (loopback only).

Why `skuid` rather than `IPAddressDeny=` in the unit: it covers every process of the account,
including one a harness starts outside the unit's cgroup, and it filters by port as well as
address. It is also written by the updater as root and put back on every update, like the DevTools
guard. Nothing the mind can change.

**What it does not close:**
- **DNS.** The mind may send queries to the system's resolvers, and a query is a message: a name
  under a domain someone controls carries data out to whoever serves it (a DNS tunnel), slowly.
  Closing it means refusing DNS to the account outright, which the proxy makes possible (it
  resolves for the mind); the `dns` counter shows whether the Mind resolves anything at all
  before that is decided.
- **Loopback services.** Everything on loopback is let through, and so is this machine's own LAN
  address (it is routed over `lo`). A service there that forwards (an open proxy, a model server
  that pulls from the internet, anything bound to 0.0.0.0) is a way out the table cannot see. The
  DevTools and VNC ports are already closed to the account by the DevTools guard.
- **Direct entries are not counted by the proxy.** What the person allowed by address is reached
  without it; the `direct` counter counts packets, not destinations.
- **A flush.** `nft flush ruleset` by root removes the table until nftables is reloaded (which
  puts it back) or the next update or boot.

### 2. One way out: `yantrik-egress`

A small Rust forward proxy on `127.0.0.1:<proxy>`, running as its own account (`yantrik-egress`),
which the table lets out. The mind's unit sets `HTTPS_PROXY`, `HTTP_PROXY` and `ALL_PROXY` to it.

- **HTTP `CONNECT host:port` only.** No TLS interception: the proxy sees the host name and the
  port, never the traffic. This is a deliberate difference from OpenShell, which terminates TLS to
  inspect requests. Holding every mind's TLS keys would make this machine the thing that reads
  everything; host-level control is enough to stop exfiltration to an unknown host. Plain
  `http://` gets forwarded only for rules that say so (a LAN Ollama).
- **Who is asking.** The proxy asks the kernel through `sock_diag`, the netlink call `ss -p` uses,
  which uid owns the connecting socket. Only `yantrik-mind` is served. Version 2 pins the program:
  the socket's inode leads to a pid and `/proc/<pid>/exe`, and a rule can be limited to a binary
  and its digest, as OpenShell pins on first use.
- **DNS.** The mind need resolve nothing itself; the proxy resolves the name in each `CONNECT`,
  and only once the name may be reached. With Private mode on, or in enforce without a rule, the
  name is refused unresolved: a lookup is itself a message to whoever serves the name. (The kernel
  table still lets the account ask the system's resolvers directly; see its residuals in
  section 1.) Programs that honour proxy variables need no change: curl, Python
  requests/httpx, Node's undici with a proxy agent, and Go all send `CONNECT` by name. A program
  that ignores them fails, which is default deny working.
- **Never a destination.** Loopback, link-local (the cloud metadata address), unspecified,
  multicast, IPv4 hidden in IPv6 forms that lead to those, and every address this machine has.
  A service bound to 0.0.0.0 answers on the machine's LAN address too, and a name that resolves
  to either must not be a way past the loopback guards.
- **The local network.** In enforce, only a rule with `lan: true` reaches it. Audit lets it
  through, marked, as it lets everything through: the mind reaches the LAN today anyway.
- **What it cannot see.** Because it opens no TLS, it cannot see a request whose TLS names a
  different site from the `CONNECT` (domain fronting) on a shared CDN. Host-level policy stops a
  new destination; it does not make an allowed CDN a single site.
- **Plain http** is forwarded one request per connection: strict CRLF, no folded headers, `Host`
  replaced with the decided authority, only a `Content-Length` body (chunked is refused), and
  nothing after it.
- **Private mode.** The shell tells the proxy "private"; every `CONNECT` is then refused. This
  sits beside the freeze and the closed door.

### 3. The policy is the person's

- **Where it lives.** `/var/lib/yantrik-egress/policy.yaml`, owned by the proxy. It is changed only
  through the proxy's control socket, `/run/yantrik-egress/control`. That socket admits the desktop
  owner's uid and root (`SO_PEERCRED`) and nobody else. So the mind cannot widen its own rules, and
  the shell is the only writer.
- **Rules.** Each rule is `host` (exact, or `*.domain`), `ports`, `why`, optionally `http` (plain
  http, not only tunnels) and `lan` (it may resolve to the local network), and in version 2
  `program`:

  ```yaml
  - host: dashscope-intl.aliyuncs.com
    ports: [443]
    why: the Mind's model
  - host: 192.168.4.20
    ports: [11434]
    http: true
    lan: true
    why: Ollama on the home GPU box
  ```

  The second rule is also a **direct entry** (section 1): a literal LAN address with `lan: true`,
  so the kernel lets the mind account reach `192.168.4.20:11434` without the proxy, in audit and
  in enforce. That is what a LAN service needs when a program that reaches it does not honour
  proxy variables. Written as a name (`gpu.lan`), the same rule stays proxy-only. A Settings
  button that lets minds reach a LAN service by address (web search, #656) should write exactly
  such a rule, in either mode.

- **Two modes.** The whole policy starts in **audit**: everything is let through, and every
  destination is counted. After a week, Settings → Minds → *Where the Mind connects* shows what it
  reached, and how often, with Allow / Block beside each. Moving to **enforce** is a switch the
  person turns. (A rule has no mode of its own: in an allow-list a rule that only watched would
  let through exactly what one in force does.)

### 4. Asking, instead of silently failing

When the mind connects somewhere no rule allows, the proxy refuses at once with `403` and a body
saying why. It also records a proposal: host, port, count, first and last time, and the program in
version 2. The shell shows the proposal as an ordinary approval card:

> The Mind wants to reach `api.example.com` (port 443) · Allow once · Always · No

A phone can answer the card like any other, since cards reach channels. Only the person's answer
writes a rule. This is OpenShell's Policy Advisor, done through the approvals we already have.
Repeated attempts to one host make one card, not a stream.

## What this is not

- **Not a sandbox for the person's own processes.** Agents' terminals run as the person, and
  harness user units (Hermes, Pi) run as the person too. Those need Landlock and seccomp at launch,
  which is a separate design.
- **Not content filtering.** An allowed host gets whatever the mind sends it. Choosing which hosts
  to allow is the control. That is why each rule has a `why`, and why the audit week comes first.
- **Not inference routing.** Which model a mind uses stays the mind's choice. This only decides
  whether it may reach it.

## Questions for yantrik-mind-72

1. **Hosts.** Which hosts does the Mind reach today? Its model provider(s), web search and fetch,
   plugin registries, MCP servers over HTTP, package installs by the coder. The audit week will
   find them, but a starting list means enforce breaks nothing on day one.
2. **Proxy variables.** Does everything in the Mind honour `HTTPS_PROXY`? Any websockets, gRPC or
   raw TCP (IMAP?) that would need `CONNECT` too, or a SOCKS5 front on the same proxy?
3. **127.0.0.1:7440** (the memory server): is it still the only loopback port the Mind needs?

## Order of work

1. **The proxy.** `yantrik-egress`, audit only: `CONNECT`, the `sock_diag` uid check, counting,
   the control socket. The unit gets the proxy variables. No nft yet, so nothing can break.
2. **Settings.** *Where the Mind connects*: the counts, and Allow / Block.
3. **The nft table** in `yantrik-update`: default deny with the proxy as the only way out, beside
   loopback, DNS to the resolvers and the person's direct set. Built 5 October 2026, and on in both
   modes rather than tied to enforce: in audit the proxy lets everything through and counts it,
   which is only true if everything goes through it.
4. **Cards** for proposals.
5. **Version 2:** rules per program, pinned by digest.

A test for each step, and the release gate on VM 520 checks both sides:
- a connect from the mind account to `1.1.1.1:443`, and to UDP port 53 anywhere but the
  resolvers, is refused; one to a direct entry is let out; another account is untouched;
- a `CONNECT` to an allowed host succeeds;
- a `CONNECT` to an unlisted host returns 403 and makes one card.
