// live-gate: what the live instance may ask a cloud provider for (deploy/live/gate-models).
//
// The instance is public and its Mind can be talked into anything, so the gate does not forward
// what it is sent. It reads the request, refuses what is not on this list, and sends the
// provider a new request built from the fields below and nothing else. That is how the
// instance cannot pick a model billed outside the subscription, ask for an unbounded answer, or
// reach a provider-specific header or option; a per-minute rate is not a budget, so each
// provider also has a daily count of requests.

const RULES = {
    // kimi-k3 leads on the live machine: deepseek-v4.1-flash lost its way on a long build there
    // (2 Oct 2026). Flash stays allowed as the chain's fallback on the same provider.
    'ollama-cloud': { models: ['kimi-k3', 'deepseek-v4.1-flash'], daily: 1500 },
    // The subscription Pranab's own Mind calls first: the instance's fallback, and kept small.
    'nanogpt': { models: ['deepseek/deepseek-v4-pro-cheaper'], daily: 100 },
    // NVIDIA NIM: one model, the one the Mind's catalogue defaults to, and a count as small as
    // NanoGPT's: a loop on the live machine must not run down a key that is Pranab's.
    'nim': { models: ['nvidia/nemotron-3-super-120b-a12b'], daily: 100 },
};

// What the Mind's OpenAI-compatible backend sends (yantrik-ml generic_openai.rs). Anything else
// in a request is dropped, not forwarded.
const PASSED = ['messages', 'temperature', 'top_p', 'stop', 'frequency_penalty', 'think',
    'reasoning_effort', 'tools', 'tool_choice'];
const MAX_TOKENS = 8192;
// The local lane's models (AIG, Ollama-native /api/chat): the one the live Mind is set to
// (YM_LOCAL_OLLAMA_MODEL on 561).
const LOCAL_MODELS = ['qwen3.8:27b'];

function refuse(r, status, text) {
    r.headersOut['Content-Type'] = 'text/plain; charset=utf-8';
    r.return(status, text + '\n');
}

async function chat(r) {
    const provider = r.variables.live_provider;
    const rule = RULES[provider];
    if (!rule) return refuse(r, 404, 'no such provider on this gate');

    // In memory, because the location sets the body buffer to its size limit; a body that did not
    // fit would have been refused with 413 before this ran.
    let asked;
    try {
        asked = JSON.parse(r.requestText || '');
    } catch (e) {
        return refuse(r, 400, 'the request body is not JSON');
    }
    if (typeof asked !== 'object' || asked === null || Array.isArray(asked)) {
        return refuse(r, 400, 'the request body is not a JSON object');
    }
    if (!rule.models.includes(asked.model)) {
        return refuse(r, 403, 'this gate serves ' + rule.models.join(', ') + ' on ' + provider);
    }
    if (!Array.isArray(asked.messages) || asked.messages.length === 0) {
        return refuse(r, 400, 'messages must be a non-empty list');
    }
    // Functions the Mind defines, and nothing else: a provider-side tool (a web search, a code
    // runner) is one the provider bills for on top.
    if (asked.tools !== undefined && (!Array.isArray(asked.tools)
        || !asked.tools.every((t) => t && typeof t === 'object' && t.type === 'function'))) {
        return refuse(r, 400, 'tools must be a list of functions');
    }

    const day = new Date().toISOString().slice(0, 10);
    const used = ngx.shared.live_budget.incr(provider + ':' + day, 1, 0);
    if (used > rule.daily) {
        return refuse(r, 429, "today's " + provider + ' requests for the live instance are spent ('
            + rule.daily + ' a day, reset at 00:00 UTC)');
    }

    const wanted = Math.floor(Number(asked.max_tokens));
    const sent = {
        model: asked.model,
        stream: asked.stream === true,
        max_tokens: wanted > 0 ? Math.min(wanted, MAX_TOKENS) : MAX_TOKENS,
    };
    if (sent.stream) sent.stream_options = { include_usage: true };
    // An index loop: the njs engine on the gate does not take for...of.
    for (let i = 0; i < PASSED.length; i++) {
        const field = PASSED[i];
        if (Object.prototype.hasOwnProperty.call(asked, field)) sent[field] = asked[field];
    }

    const reply = await r.subrequest('/_live_upstream/' + provider,
        { method: 'POST', body: JSON.stringify(sent) });

    // A refusal of the gate's key is the gate's problem, not the instance's to read about: the
    // provider's own words stay here.
    if (reply.status === 401 || reply.status === 403) {
        return refuse(r, 502, provider + ' refused the gate (status ' + reply.status + ')');
    }
    if (reply.status === 429 || reply.status >= 500) {
        return refuse(r, reply.status, provider + ' answered ' + reply.status);
    }
    // Anything else that is not an answer (a redirect, a 4xx about the body) is the gate's 502:
    // r.return with a 3xx would read the text as a Location.
    if (reply.status < 200 || reply.status > 299) {
        return refuse(r, 502, provider + ' answered ' + reply.status);
    }
    const type = String(reply.headersOut['Content-Type'] || '');
    r.headersOut['Content-Type'] = type.startsWith('text/event-stream')
        ? 'text/event-stream' : 'application/json';
    r.return(reply.status, reply.responseText);
}

// The local lane (AIG's Ollama-native /api/chat). The Mind always streams, and AIG's stream ends
// with an empty message where the answer was a tool call (seen 17 Oct 2026: the same request
// streamed came back empty, unstreamed it called os_describe), so every agent turn on this lane
// chose no tool. The gate asks for the whole answer and hands it back as a one-line stream, which
// the Mind's reader takes like any other: it reads tool calls from whichever line carries them.
// Nothing else about the request changes; this route forwarded the body as sent before, too.
async function local(r) {
    let asked;
    try {
        asked = JSON.parse(r.requestText || '');
    } catch (e) {
        return refuse(r, 400, 'the request body is not JSON');
    }
    if (typeof asked !== 'object' || asked === null || Array.isArray(asked)) {
        return refuse(r, 400, 'the request body is not a JSON object');
    }
    if (!LOCAL_MODELS.includes(asked.model)) {
        return refuse(r, 403, 'this gate serves ' + LOCAL_MODELS.join(', ') + ' on the local lane');
    }
    // Unstreamed, a long answer holds a gate connection until it is whole, so the answer's length is
    // capped here as `chat` caps max_tokens (review of #846).
    if (typeof asked.options !== 'object' || asked.options === null || Array.isArray(asked.options)) {
        asked.options = {};
    }
    const wanted = Math.floor(Number(asked.options.num_predict));
    asked.options.num_predict = wanted > 0 ? Math.min(wanted, MAX_TOKENS) : MAX_TOKENS;
    const streamed = asked.stream !== false; // Ollama streams unless told not to
    asked.stream = false;

    const reply = await r.subrequest('/_live_upstream/aig', { method: 'POST', body: JSON.stringify(asked) });
    if (reply.status < 200 || reply.status > 299) {
        // AIG's words go to the gate's own error log, never to the instance.
        r.error('local lane: AIG answered ' + reply.status + ': ' + String(reply.responseText || '').slice(0, 200));
        return refuse(r, reply.status === 429 || reply.status >= 500 ? reply.status : 502,
            'the local model answered ' + reply.status);
    }
    let answer;
    try {
        answer = JSON.parse(reply.responseText);
    } catch (e) {
        return refuse(r, 502, 'the local model did not answer with JSON');
    }
    r.headersOut['Content-Type'] = streamed ? 'application/x-ndjson' : 'application/json';
    r.return(200, JSON.stringify(answer) + '\n');
}

export default { chat, local };
