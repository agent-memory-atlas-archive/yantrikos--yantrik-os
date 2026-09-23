/**
 * The Yantrik OS desktop, as Pi tools.
 *
 * Pi has no MCP client, so this extension is the client: it registers every tool
 * `yos-mcp` publishes and proxies each call straight through to it. Nothing here decides
 * anything. Modes, permission grades, approval cards and the taint rule all live in the
 * bridge, which is the only place they can be kept correct — an extension that re-checked
 * any of them would be a second policy to keep in sync with the first, and the first is the
 * one the desktop actually enforces.
 *
 * Install: this file is passed to Pi with `-e /path/to/yantrik-os.ts` by the harness
 * (`yantrik_pi.py`). Copying it to ~/.pi/agent/extensions/yantrik-os.ts also works and loads
 * it for every Pi run, which is usually not what you want on a desktop.
 *
 * Dependency-free on purpose: only `typebox` and node built-ins, both of which Pi resolves
 * for an extension. No package.json, no install step, nothing to keep up to date.
 *
 * When this Pi runs as one of the person's agents (its harness puts the agent's token in
 * `YANTRIK_AGENT_TOKEN`, which the bridge inherits), the bridge also offers the agent's own
 * terminal — `run_command`, `command_status`, `command_input`, `command_kill` — and, if Pi's
 * built-in tools are off, this file registers a `bash` of its own on top of `run_command`, so Pi
 * keeps the tool it was trained on and the command lands in the agent's pane on the desktop.
 *
 * ── What is verified and what is not ─────────────────────────────────────────────────────
 * Verified against a real Pi 0.87.0 install: the `ExtensionAPI` import path, `typebox` (not
 * `@sinclair/typebox`), the `export default function (pi)` shape, `pi.registerTool({ name,
 * label, description, parameters, execute(toolCallId, params, signal, onUpdate, ctx) })`,
 * the `{ content: [{ type: "text", text }], details }` result and its optional `isError`,
 * and that node built-ins are importable.
 *
 * Read from the same install (dist/core/tools/bash.js, dist/cli/args.js, pi-agent-core's
 * agent-loop.js): `bash`'s schema `{command, timeout?}` and its "Command exited with code N" /
 * "Command timed out after N seconds" endings; `--no-builtin-tools` / `-nbt`; and that every
 * `onUpdate(...)` becomes a `tool_execution_update` event. `harnesses/tests/test_pi_extension.py`
 * runs this file under node against a fake bridge; it has not been loaded into a real Pi with
 * the `bash` below.
 *
 * NOT verified offline:
 *  - Whether Pi awaits an async default export. This file therefore does its `tools/list`
 *    SYNCHRONOUSLY with `spawnSync`, so registration is complete before the factory returns
 *    whether or not Pi would have waited. It costs one extra short-lived `yos-mcp` process
 *    at startup and removes the question entirely.
 *  - Whether `Type.Unsafe(jsonSchema)` is accepted verbatim as a tool's `parameters`. It is
 *    a TypeBox escape hatch for "this JSON Schema, as-is", and the desktop's schemas are
 *    plain JSON Schema, so it should be. If a Pi version rejects it, the fix is to pass the
 *    raw schema object instead — the shapes are the same thing.
 */

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { spawn, spawnSync, type ChildProcessWithoutNullStreams } from "node:child_process";

/** Where the bridge lives. Overridable for a checkout that is not installed. */
const BRIDGE = process.env.YOS_MCP_BIN || "/opt/yantrik/bin/yos-mcp";

/**
 * How long one tool call may take.
 *
 * An `os_act` above this session's ceiling asks the person and waits for them inside the
 * bridge — a little over 270 seconds in the worst case. A client that gives up sooner cuts
 * the person off mid-decision and reports a timeout for a machine that was working fine.
 */
const CALL_TIMEOUT_MS = 300_000;

/**
 * The bridge's command tools — offered when this Pi runs as one of the person's agents, with
 * `YANTRIK_AGENT_TOKEN` in its environment — also wait for the command itself: `wait_seconds`,
 * 120 by default, at most 600. They are given that on top of CALL_TIMEOUT_MS, or the client
 * would cut off the command it asked to wait for.
 */
// `hand_off` waits for a catalog role's answer the same way, and not at all unless told to.
const WAITING_TOOLS: Record<string, number> = { run_command: 120, command_status: 120, hand_off: 0 };
const WAIT_MOST_S = 600;

/** How long one call to `name` may take, in milliseconds. */
export function callTimeoutMs(name: string, params: any): number {
	const fallback = WAITING_TOOLS[name];
	if (fallback === undefined) return CALL_TIMEOUT_MS;
	let wait = Number(params?.wait_seconds ?? fallback);
	if (!Number.isFinite(wait)) wait = fallback;
	return CALL_TIMEOUT_MS + Math.min(Math.max(wait, 0), WAIT_MOST_S) * 1000;
}

/**
 * How often a call still waiting on the desktop tells Pi it is alive.
 *
 * The harness gives up on a turn when Pi has said nothing for 420 seconds
 * (`yantrik_pi.py`, DEFAULT_SILENCE_TIMEOUT), and a tool call that waits — for a person's card,
 * for a command's `wait_seconds` — produces no events of its own. An empty update is one: Pi
 * reports it as `tool_execution_update`, which counts as life and adds nothing to the card.
 */
const HEARTBEAT_MS = 30_000;

/** The `initialize`/`tools/list` probe at startup — short, because nothing is waiting on a person. */
const LIST_TIMEOUT_MS = 20_000;

const PROTOCOL_VERSION = "2024-11-05";

/**
 * The name on the approval card.
 *
 * Self-declared and treated as such by the bridge, which prints it as "says the caller" and
 * verifies the calling process separately. It is still worth sending: it is the name the
 * person recognises, and without it the card reads "an unnamed caller".
 */
const CLIENT = { name: "Pi", version: "1.0" };

type JsonRpc = { jsonrpc: "2.0"; id?: number; method?: string; params?: unknown; result?: any; error?: any };
type McpTool = { name: string; description?: string; inputSchema?: Record<string, unknown> };

/**
 * A long-lived `yos-mcp` child, one JSON-RPC message per line.
 *
 * Lazy, so a Pi session that never touches the desktop never starts one, and restarted if it
 * dies — losing the bridge should cost one tool call, not the desktop.
 */
class Bridge {
	private child: ChildProcessWithoutNullStreams | null = null;
	private buffer = "";
	private nextId = 1;
	private pending = new Map<number, { resolve: (m: JsonRpc) => void; reject: (e: Error) => void }>();

	private start(): ChildProcessWithoutNullStreams {
		const child = spawn(BRIDGE, [], { stdio: ["pipe", "pipe", "ignore"] });
		child.stdout.setEncoding("utf8");
		child.stdout.on("data", (piece: string) => this.absorb(piece));
		const die = (why: string) => {
			if (this.child === child) this.child = null;
			this.buffer = "";
			for (const waiter of this.pending.values()) waiter.reject(new Error(why));
			this.pending.clear();
		};
		child.on("exit", (code) => die(`the desktop bridge exited (${code}) before it answered`));
		child.on("error", (err) => die(`the desktop bridge could not be run: ${err.message}`));
		this.child = child;
		// Fire-and-forget: the handshake is a request like any other and its reply lands in
		// `absorb` with everything else.
		this.write({ jsonrpc: "2.0", id: 0, method: "initialize", params: { protocolVersion: PROTOCOL_VERSION, capabilities: {}, clientInfo: CLIENT } });
		this.write({ jsonrpc: "2.0", method: "notifications/initialized", params: {} });
		return child;
	}

	private absorb(piece: string): void {
		this.buffer += piece;
		let cut: number;
		while ((cut = this.buffer.indexOf("\n")) >= 0) {
			const line = this.buffer.slice(0, cut).trim();
			this.buffer = this.buffer.slice(cut + 1);
			if (!line) continue;
			let msg: JsonRpc;
			try {
				msg = JSON.parse(line);
			} catch {
				continue; // a stray line on stdout is not a reason to lose the bridge
			}
			if (typeof msg.id !== "number") continue;
			const waiter = this.pending.get(msg.id);
			if (!waiter) continue;
			this.pending.delete(msg.id);
			waiter.resolve(msg);
		}
	}

	private write(msg: JsonRpc): void {
		this.child?.stdin.write(JSON.stringify(msg) + "\n");
	}

	async request(method: string, params: unknown, timeoutMs: number, signal?: AbortSignal): Promise<JsonRpc> {
		if (!this.child || this.child.exitCode !== null) this.start();
		const id = ++this.nextId;
		return await new Promise<JsonRpc>((resolve, reject) => {
			const timer = setTimeout(() => {
				this.pending.delete(id);
				reject(new Error(`${method} did not answer within ${Math.round(timeoutMs / 1000)}s. The desktop may be busy rather than broken; nothing was rolled back.`));
			}, timeoutMs);
			const settle = (fn: (v: any) => void) => (v: any) => {
				clearTimeout(timer);
				signal?.removeEventListener("abort", onAbort);
				fn(v);
			};
			const onAbort = () => {
				this.pending.delete(id);
				settle(reject)(new Error("stopped before the desktop answered"));
			};
			signal?.addEventListener("abort", onAbort, { once: true });
			this.pending.set(id, { resolve: settle(resolve), reject: settle(reject) });
			try {
				this.write({ jsonrpc: "2.0", id, method, params });
			} catch (err) {
				this.pending.delete(id);
				settle(reject)(err);
			}
		});
	}
}

/**
 * `tools/list` in one synchronous shot, so registration needs no await.
 *
 * Writes both lines and closes stdin; the bridge answers each and exits at EOF. Returns an
 * empty list rather than throwing, so a machine with no bridge still starts Pi.
 */
function listToolsSync(): McpTool[] {
	const input =
		JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: PROTOCOL_VERSION, capabilities: {}, clientInfo: CLIENT } }) +
		"\n" +
		JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} }) +
		"\n";
	let out: string;
	try {
		const probe = spawnSync(BRIDGE, [], { input, encoding: "utf8", timeout: LIST_TIMEOUT_MS, maxBuffer: 8 * 1024 * 1024 });
		if (probe.error || probe.status !== 0) return [];
		out = probe.stdout || "";
	} catch {
		return [];
	}
	for (const line of out.split("\n")) {
		if (!line.trim()) continue;
		try {
			const msg: JsonRpc = JSON.parse(line);
			const tools = msg?.result?.tools;
			if (Array.isArray(tools)) return tools.filter((t: McpTool) => t && typeof t.name === "string");
		} catch {
			/* keep looking */
		}
	}
	return [];
}

/**
 * The names and shapes of the desktop's tools, if the bridge could not be asked.
 *
 * Mirrored from `deploy/yantrik-os/yos-mcp`'s TOOLS. Deliberately thin: the real descriptions
 * are long and carefully worded and they belong in one place, so this is only enough for the
 * tools to exist and explain themselves when the bridge comes back. The bridge is the
 * authority; this is a placeholder for a machine where it is missing.
 */
const FALLBACK: McpTool[] = [
	{ name: "os_apps", description: "What is on this desktop: the apps that are open and the ones that can be opened." },
	{ name: "os_describe", description: "The state of one app or service, and the actions it offers.", inputSchema: { type: "object", properties: { app: { type: "string" }, actions: { type: "string" } }, required: ["app"] } },
	{ name: "os_act", description: "Do one published action on one app.", inputSchema: { type: "object", properties: { app: { type: "string" }, action: { type: "string" }, args: { type: "object", additionalProperties: true } }, required: ["app", "action"] } },
	{ name: "os_perception", description: "What this machine has noticed recently.", inputSchema: { type: "object", properties: { count: { type: "integer" } } } },
	{ name: "web_read", description: "What is on the current web page." },
	{ name: "web_text", description: "What the current page says, as prose.", inputSchema: { type: "object", properties: { limit: { type: "integer" } } } },
	{ name: "web_find", description: "Locate something anywhere on the page.", inputSchema: { type: "object", properties: { words: { type: "string" } }, required: ["words"] } },
	{ name: "web_go", description: "Navigate the browser to a URL.", inputSchema: { type: "object", properties: { url: { type: "string" } }, required: ["url"] } },
	{ name: "web_click", description: "Click one referenced control on the page.", inputSchema: { type: "object", properties: { ref: { type: "integer" } }, required: ["ref"] } },
	{ name: "web_type", description: "Type into one referenced field on the page.", inputSchema: { type: "object", properties: { ref: { type: "integer" }, text: { type: "string" } }, required: ["ref", "text"] } },
	{ name: "web_listen", description: "What the page says out loud, transcribed.", inputSchema: { type: "object", properties: { seconds: { type: "integer" } } } },
];

/** `os_act` → `os act`, for the label Pi shows beside a running tool. */
function label(name: string): string {
	return name.replace(/_/g, " ");
}

/**
 * Whether Pi was started with its own tools off. The harness passes `--no-builtin-tools` unless
 * the person turned them on (`builtin_tools` in pi.json). Read from Pi's own command line
 * because an extension cannot ask Pi for its tools while it is being loaded.
 */
function builtinToolsOff(argv: string[] = process.argv): boolean {
	return argv.includes("--no-builtin-tools") || argv.includes("-nbt");
}

export default function (pi: ExtensionAPI) {
	const bridge = new Bridge();
	const discovered = listToolsSync();
	const tools = discovered.length ? discovered : FALLBACK;

	for (const tool of tools) {
		const schema = tool.inputSchema && typeof tool.inputSchema === "object" ? tool.inputSchema : { type: "object", properties: {} };
		// One tool with a schema this Pi version dislikes must not cost the other ten. The
		// desktop publishes plain JSON Schema and Pi has not been seen to refuse any of it, but
		// this file is loaded into somebody else's agent and an extension that throws at load
		// takes the whole agent with it.
		try {
			registerOne(pi, bridge, tool, schema);
		} catch (err: any) {
			console.error(`yantrik-os: could not register ${tool.name}: ${err?.message || err}`);
		}
	}

	// Pi's own `bash`, run in this agent's terminal on the desktop — only when the bridge offers
	// one (this Pi is one of the person's agents) and Pi's built-in `bash` is off, so it never
	// shadows the real one.
	if (builtinToolsOff() && discovered.some((t) => t.name === "run_command")) {
		try {
			registerBash(pi, bridge);
		} catch (err: any) {
			console.error(`yantrik-os: could not register bash: ${err?.message || err}`);
		}
	}
}

/** One call to the bridge, with Pi told it is alive while it waits. */
async function callBridge(
	bridge: Bridge,
	name: string,
	params: unknown,
	signal?: AbortSignal,
	onUpdate?: (update: any) => void,
): Promise<JsonRpc> {
	const beat = onUpdate ? setInterval(() => onUpdate({ content: [], details: {} }), HEARTBEAT_MS) : undefined;
	try {
		return await bridge.request("tools/call", { name, arguments: params ?? {} }, callTimeoutMs(name, params), signal);
	} finally {
		if (beat) clearInterval(beat);
	}
}

function textOf(msg: JsonRpc): string {
	const parts = Array.isArray(msg.result?.content) ? msg.result.content : [];
	return parts.map((p: any) => (p?.type === "text" ? String(p.text ?? "") : "")).join("");
}

function registerOne(pi: ExtensionAPI, bridge: Bridge, tool: McpTool, schema: Record<string, unknown>) {
	pi.registerTool({
		name: tool.name,
		label: label(tool.name),
		description: tool.description || `The Yantrik OS desktop's ${tool.name}.`,
		// The desktop's schemas are plain JSON Schema and TypeBox objects are JSON Schema, so
		// this hands Pi exactly what the bridge published rather than a translation of it.
		parameters: Type.Unsafe(schema as any),
		async execute(_toolCallId: string, params: unknown, signal?: AbortSignal, onUpdate?: (update: any) => void) {
			let msg: JsonRpc;
			try {
				msg = await callBridge(bridge, tool.name, params, signal, onUpdate);
			} catch (err: any) {
				return { content: [{ type: "text", text: String(err?.message || err) }], isError: true, details: {} };
			}
			if (msg.error) {
				return { content: [{ type: "text", text: String(msg.error?.message || msg.error) }], isError: true, details: {} };
			}
			const text = textOf(msg);
			// isError comes straight from the bridge and is not second-guessed here. A REFUSED
			// answer arrives UNFLAGGED on purpose: the desktop ran, it was healthy, and it said
			// no. Flagging it would teach a model to retry a refusal, and clients that count
			// errors have already switched the whole desktop off over three of them.
			return {
				content: [{ type: "text", text: text || "(the tool returned nothing)" }],
				isError: Boolean(msg.result?.isError),
				details: {},
			};
		},
	});
}

// ── Pi's `bash`, in the agent's own terminal ────────────────────────────────────────────
//
// Pi's models are trained on a `bash` tool: `{command, timeout?}` (timeout in seconds, none by
// default), answering with the output, and failing with "Command exited with code N" or
// "Command timed out after N seconds" (pi 0.87, dist/core/tools/bash.js). This is that tool,
// with the command run by the desktop's `run_command` instead of a child of Pi — so it lands in
// the agent's own terminal in its pane, where the person can watch it, answer a prompt in it,
// and stop it, and it is graded and asked about like any other act.
//
// The one place it cannot be the same: a command that stops to wait for input. Pi's own bash
// has no terminal and would hang; here the person can answer it in the card, so the call returns
// with the command still running and says how to follow it up.

/** How long each wait for a command is, when no timeout bounds it. The shell's default. */
const BASH_SLICE_S = 120;

type Command = {
	job?: string;
	running?: boolean;
	waiting_for_input?: boolean;
	exit_code?: number;
	signal?: number;
	signal_name?: string;
	tail?: string;
	tail_clipped?: boolean;
};

/** The shell's own account of a command, which the bridge carries beside its sentence. */
function commandOf(msg: JsonRpc): Command | undefined {
	const meta = msg.result?._meta?.["yantrik/command"];
	return meta && typeof meta === "object" ? (meta as Command) : undefined;
}

function outputOf(command: Command): string {
	const tail = String(command.tail ?? "").replace(/\n+$/, "");
	const text = tail || "(no output)";
	return command.tail_clipped ? `${text}\n\n[Showing the last lines; the whole output is in your pane on the desktop]` : text;
}

function withStatus(text: string, status: string): string {
	return `${text ? `${text}\n\n` : ""}${status}`;
}

function registerBash(pi: ExtensionAPI, bridge: Bridge) {
	pi.registerTool({
		name: "bash",
		label: "bash",
		description:
			"Execute a bash command in your own terminal on the Yantrik OS desktop, shown in your pane there. " +
			"Returns its output (the end of it, as the terminal shows it). The working directory carries from one command to the next; " +
			"exported variables and other shell state do not. Optionally provide a timeout in seconds. " +
			"It runs as the person, so in `ask` mode they allow each command first.",
		// Pi's own schema, word for word, so the model meets the tool it was trained on.
		parameters: Type.Object({
			command: Type.String({ description: "Shell command to execute" }),
			timeout: Type.Optional(Type.Number({ description: "Timeout in seconds (optional, no default timeout)" })),
		}),
		async execute(_toolCallId: string, params: any, signal?: AbortSignal, onUpdate?: (update: any) => void) {
			const fail = (text: string) => ({ content: [{ type: "text", text }], isError: true, details: {} });
			const command = String(params?.command ?? "");
			const timeout = params?.timeout;
			if (timeout !== undefined && !(typeof timeout === "number" && Number.isFinite(timeout) && timeout > 0)) {
				return fail("Invalid timeout: must be a finite number of seconds");
			}
			const deadline = timeout === undefined ? Infinity : Date.now() + timeout * 1000;
			// Each wait is at most a slice, and never past the timeout. The first is the timeout
			// itself when that is shorter, so the command's own card says what was asked for.
			const slice = () => Math.max(0, Math.min(WAIT_MOST_S, BASH_SLICE_S, (deadline - Date.now()) / 1000));
			const first = Math.min(WAIT_MOST_S, BASH_SLICE_S, timeout ?? Infinity);

			let job: string | undefined;
			const stop = async () => {
				if (!job) return undefined;
				try {
					return commandOf(await callBridge(bridge, "command_kill", { job }));
				} catch {
					return undefined;
				}
			};

			let msg: JsonRpc;
			try {
				msg = await callBridge(bridge, "run_command", { command, wait_seconds: first }, signal, onUpdate);
				for (;;) {
					if (msg.error) return fail(String(msg.error?.message || msg.error));
					const now = commandOf(msg);
					// A refusal, a card the person said no to, a desktop that could not be reached:
					// the bridge's own words, flagged exactly as it flagged them.
					if (!now) return { content: [{ type: "text", text: textOf(msg) || "(the tool returned nothing)" }], isError: Boolean(msg.result?.isError), details: {} };
					job = now.job ?? job;
					if (!now.running) {
						const text = outputOf(now);
						if (now.exit_code === 0) return { content: [{ type: "text", text }], details: {} };
						if (typeof now.exit_code === "number") return fail(withStatus(text, `Command exited with code ${now.exit_code}`));
						return fail(withStatus(text, `Command terminated by ${now.signal_name || "a signal"}`));
					}
					if (now.waiting_for_input) {
						return {
							content: [{
								type: "text",
								text: withStatus(outputOf(now),
									`[Still running in your terminal on the desktop, and it looks like it is waiting for input (job ${job}). ` +
									`The person can answer it in its card; command_input sends it text; command_status waits for it; command_kill stops it.]`),
							}],
							details: {},
						};
					}
					if (Date.now() >= deadline) {
						const last = (await stop()) ?? now;
						return fail(withStatus(String(last.tail ?? "").replace(/\n+$/, ""), `Command timed out after ${timeout} seconds`));
					}
					// Still going: show what it has printed so far, and wait again.
					onUpdate?.({ content: [{ type: "text", text: String(now.tail ?? "") }], details: {} });
					msg = await callBridge(bridge, "command_status", { job, wait_seconds: slice() }, signal, onUpdate);
				}
			} catch (err: any) {
				if (signal?.aborted) {
					const last = await stop();
					return fail(withStatus(String(last?.tail ?? "").replace(/\n+$/, ""), "Command aborted"));
				}
				return fail(String(err?.message || err));
			}
		},
	});
}
