# Harnesses

One YAML file here declares one mind the shell can hand a question to. The files in this
directory are installed to `/etc/yantrik/harnesses/`; a person's own go in
`~/.config/yantrik/harnesses/`, and an id in both wins from theirs — so pointing the shipped
`mind` entry at your own machine does not mean editing a file the next image overwrites.

Adding a harness is this file and nothing else. There is no build step, because almost every
agent runtime worth plugging in already serves `POST /v1/chat/completions` — `yantrik-mind` does,
so do hermes-agent, Ollama, vLLM and llama.cpp — and one adapter covers all of them.

    id: mind                                 # what you type to select it
    name: Yantrik Mind                       # what the picker shows
    kind: openai-http                        # openai-http | stdio
    endpoint: http://192.168.4.66:8080/v1
    model: qwen2.5
    api_key_env: YANTRIK_MIND_KEY            # the NAME of the variable, never the key

A CLI agent instead of an endpoint:

    id: openclaw
    name: OpenClaw
    kind: stdio
    command: [/usr/local/bin/openclaw, --json]

It is handed `{"text":...,"context":...}` on stdin and answers with one JSON object per line —
`{"delta":"..."}` repeatedly, then `{"done":true}`, or `{"error":"..."}`. A script that just
prints prose works too; its output is taken as the answer.

Check your work without opening the shell:

    harnessctl list            # what is declared, and whether it answers
    harnessctl ask mind "hi"   # put a real turn to it

A file that will not parse is listed by `harnessctl list` under "not usable" with the reason,
rather than quietly not appearing — a harness that is merely absent is the hardest kind to debug.
