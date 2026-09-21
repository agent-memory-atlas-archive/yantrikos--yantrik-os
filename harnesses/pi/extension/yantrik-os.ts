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
 * ── What is verified and what is not ─────────────────────────────────────────────────────
 * Verified against a real Pi 0.87.0 install: the `ExtensionAPI` import path, `typebox` (not
 * `@sinclair/typebox`), the `export default function (pi)` shape, `pi.registerTool({ name,
 * label, description, parameters, execute(toolCallId, params, signal, onUpdate, ctx) })`,
 * the `{ content: [{ type: "text", text }], details }` result and its optional `isError`,
 * and that node built-ins are importable.
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
}

function registerOne(pi: ExtensionAPI, bridge: Bridge, tool: McpTool, schema: Record<string, unknown>) {
	pi.registerTool({
		name: tool.name,
		label: label(tool.name),
		description: tool.description || `The Yantrik OS desktop's ${tool.name}.`,
		// The desktop's schemas are plain JSON Schema and TypeBox objects are JSON Schema, so
		// this hands Pi exactly what the bridge published rather than a translation of it.
		parameters: Type.Unsafe(schema as any),
		async execute(_toolCallId: string, params: unknown, signal?: AbortSignal) {
			let msg: JsonRpc;
			try {
				msg = await bridge.request("tools/call", { name: tool.name, arguments: params ?? {} }, CALL_TIMEOUT_MS, signal);
			} catch (err: any) {
				return { content: [{ type: "text", text: String(err?.message || err) }], isError: true, details: {} };
			}
			if (msg.error) {
				return { content: [{ type: "text", text: String(msg.error?.message || msg.error) }], isError: true, details: {} };
			}
			const parts = Array.isArray(msg.result?.content) ? msg.result.content : [];
			const text = parts.map((p: any) => (p?.type === "text" ? String(p.text ?? "") : "")).join("");
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
