# The live instance's cloud models

The live instance (VM 561) is a public machine: its screen is streamed, and its Mind can be talked
into showing anything it has. So it holds no provider key. Its only way to a model is the gate
(VM 560), which checks the instance's own key and adds the real one on the way out.

| Route on the gate (`10.99.0.1:8443`)      | Goes to                      | Rate per instance  | Key added by the gate |
|-------------------------------------------|------------------------------|--------------------|-----------------------|
| `/api/chat`, `/v1/chat/completions`, …    | AIG (`aig.mycluster.cyou`)   | 2 a second         | none needed           |
| `/ollama-cloud/v1/chat/completions`       | `ollama.com`                 | 30 a minute        | Ollama Cloud's        |
| `/nanogpt/api/v1/chat/completions`        | `nano-gpt.com`               | 10 a minute        | NanoGPT's             |

The AIG route is `gate-setup.sh`'s. The two cloud routes are this directory's. They verify the
provider's certificate, which nginx does not do by default.

NanoGPT gets the lowest rate on purpose. It is the subscription Pranab's own Mind calls first, and
a live machine stuck in a loop must not spend that week's tokens.

## Setting it up

On node2 as root, with this directory and `../guest.sh` copied there:

```sh
# 1. The routes, with the two keys on stdin. They go into the gate as root-only files and are
#    never written on node2 or printed.
grep -E '^(OLLAMA_CLOUD_KEY|NANOGPT_KEY)=' keys.env | ssh root@node2 'cd /root/live-setup/gate-models && sh setup-models.sh'

# 2. The instance's Mind, pointed at them. This restarts the Mind service, not the desktop.
ssh root@node2 'cd /root/live-setup/gate-models && sh point-mind.sh'
```

Run step 1 again to change a key. Both steps keep the file they change, as `*.before-cloud`,
beside it.

## What the Mind ends up with

1. Ollama Cloud, deepseek-v4.1-flash. It scored 8 of 9 on the desktop task battery at about 3 s a
   call.
2. NanoGPT, when Ollama Cloud refuses or fails.
3. AIG, as the survival fallback.

Private turns still go to AIG only, and fail closed when it is down. They never fall through to a
cloud.

AIG's model (bonsai2-27b) no longer leads. On this machine it could not fill a tool's named
parameters.
