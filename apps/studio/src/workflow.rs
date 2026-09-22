//! Turning "this picture, this size, this seed" into a ComfyUI graph.
//!
//! The graph is filled by following the sampler's own links rather than by assuming node
//! numbers, so a workflow a person exported themselves works as well as the shipped one. What
//! could not be filled is collected and reported rather than dropped: a graph that silently
//! ignores the prompt is the failure that would otherwise look like a model that does not
//! listen.
//!
//! Pure — a string in, a value out — so the templating can be tested without a server.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::backend::Request;

/// The graph that ships with the app. Read at compile time so an installed binary does not
/// depend on a file that may not have been installed beside it.
pub const BUILT_IN: &str = include_str!("../workflows/sdxl_txt2img.json");

/// The latent grid SDXL works on. A width of 1023 is not an error to ComfyUI, it is a picture
/// that comes back a different shape than was asked for, so the request is snapped first and the
/// sidecar records what was actually sent.
pub const GRID: u32 = 8;

pub fn snap_to_grid(value: u32) -> u32 {
    (((value + GRID / 2) / GRID) * GRID).max(GRID)
}

/// What came out of filling a graph in: the graph to send, the size it was told to draw, and an
/// honest account of what took and what did not.
#[derive(Clone, Debug, Default)]
pub struct Filled {
    pub graph: Value,
    /// The latent size the graph was given, if the graph has a latent to give one to.
    pub size: Option<(u32, u32)>,
    /// Short phrases naming each thing that was set. Kept for the log, not for `describe`.
    pub applied: Vec<String>,
    /// Whole sentences naming each thing that could not be set. The first of these is what a
    /// person sees, because "the picture is not what you asked for" is otherwise unexplainable.
    pub missed: Vec<String>,
}

/// The graph to fill: a person's own file if they named one and it can be read, otherwise the
/// shipped SDXL pass.
pub fn graph(override_path: &str) -> Result<Value, String> {
    let path = override_path.trim();
    if path.is_empty() {
        return api_format(BUILT_IN)
            .map_err(|e| format!("the graph Studio ships with is not usable: {e}"));
    }
    let expanded = expand_home(path);
    let text = std::fs::read_to_string(&expanded).map_err(|e| {
        format!(
            "the workflow {} named in the configuration could not be read ({e}); check the path in the studio.json config",
            expanded.display()
        )
    })?;
    api_format(&text).map_err(|e| format!("{} is not a usable ComfyUI graph: {e}", expanded.display()))
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => Path::new(&home).join(rest),
            None => Path::new(path).to_path_buf(),
        },
        None => Path::new(path).to_path_buf(),
    }
}

/// Read and check a graph before anything is filled into it.
///
/// The check exists because the most common way this fails is a person exporting the wrong thing
/// from the ComfyUI editor: the editor's own "Save" writes a document for humans, and posting it
/// to `/prompt` gets back a 400 that does not say which of the two you have. Answering that here,
/// in a sentence, saves a round trip through a server's log.
pub fn api_format(text: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("it is not valid JSON ({e})"))?;
    // Somebody who saved the whole request body rather than the graph gets the graph out of it.
    let root = match value.get("prompt") {
        Some(inner) if inner.is_object() => inner.clone(),
        _ => value,
    };
    let Some(nodes) = root.as_object() else {
        return Err("its top level is not a map of nodes".to_string());
    };
    if nodes.contains_key("nodes") || nodes.contains_key("links") {
        return Err(
            "it looks like a workflow saved from the ComfyUI editor, not the API format Studio sends. In the editor, turn on Dev mode in its settings and use 'Save (API Format)'."
                .to_string(),
        );
    }
    let mut saw_a_node = false;
    for (id, node) in nodes {
        // Keys starting with an underscore are notes for a person reading the file, and are
        // stripped before the graph is sent; the shipped one carries its own explanation there.
        if id.starts_with('_') {
            continue;
        }
        let Some(node) = node.as_object() else {
            return Err(format!("node {id} is not an object"));
        };
        match node.get("class_type").and_then(|c| c.as_str()) {
            Some(class) if !class.is_empty() => saw_a_node = true,
            _ => return Err(format!("node {id} names no class_type, so ComfyUI cannot run it")),
        }
        if !node.contains_key("inputs") {
            return Err(format!("node {id} ({}) has no inputs", node["class_type"]));
        }
    }
    if !saw_a_node {
        return Err("it holds no nodes".to_string());
    }
    Ok(root)
}

/// Fill a graph with one request. Returns the graph to post and what happened while filling it.
pub fn fill(
    template: Value,
    request: &Request,
    model: &str,
    filename_prefix: &str,
) -> Result<Filled, String> {
    let Some(mut nodes) = template.as_object().cloned() else {
        return Err("the graph is not a map of nodes".to_string());
    };
    let mut filled = Filled::default();

    // The explanation in the shipped file is for a person reading the repository. It is stripped
    // here rather than left in, because ComfyUI would try to run it as a node.
    nodes.retain(|id, _| !id.starts_with('_'));

    // Every node id this needs is taken as an owned string up front. Holding a reference into
    // `nodes` while also writing to it is the mistake this avoids, and it costs one clone of an
    // id per graph.
    let sampler = first_node(&nodes, |class| class.starts_with("KSampler"));
    let advanced = sampler
        .as_ref()
        .is_some_and(|id| class_of(&nodes[id]).starts_with("KSamplerAdvanced"));

    // ── the sampler: seed, steps, guidance ──
    if let Some(id) = &sampler {
        if advanced {
            set(&mut nodes, id, "noise_seed", json!(request.seed), &mut filled, "the seed");
            set(&mut nodes, id, "steps", json!(request.steps), &mut filled, "the step count");
            // An advanced sampler is a *range* of steps. Leaving the graph's own start would make
            // it resume halfway through a pass nobody started, and the seed would mean nothing.
            set(&mut nodes, id, "start_at_step", json!(0), &mut filled, "the first step");
            set(&mut nodes, id, "end_at_step", json!(request.steps), &mut filled, "the last step");
            if has_key(&nodes, id, "cfg") {
                set(&mut nodes, id, "cfg", json!(request.cfg), &mut filled, "the guidance");
            } else {
                filled.missed.push(format!(
                    "the graph's sampler is a KSamplerAdvanced, which takes its guidance elsewhere, so cfg {} was not applied",
                    request.cfg
                ));
            }
        } else {
            set(&mut nodes, id, "seed", json!(request.seed), &mut filled, "the seed");
            set(&mut nodes, id, "steps", json!(request.steps), &mut filled, "the step count");
            set(&mut nodes, id, "cfg", json!(request.cfg), &mut filled, "the guidance");
            set(&mut nodes, id, "denoise", json!(1.0), &mut filled, "a full denoise");
        }
    } else {
        filled.missed.push(
            "the graph has no KSampler, so the seed, the step count and the guidance were not applied"
                .to_string(),
        );
    }

    // ── the two prompts, found by following the sampler's own links ──
    for (input, text, what) in [
        ("positive", &request.prompt, "the prompt"),
        ("negative", &request.negative, "the negative prompt"),
    ] {
        match linked_node(&nodes, sampler.as_deref(), input) {
            Some(id) if has_key(&nodes, &id, "text") => {
                set(&mut nodes, &id, "text", json!(text), &mut filled, what);
            }
            Some(id) => filled.missed.push(format!(
                "node {id} feeds the sampler's {input} input but takes no text, so {what} was not applied"
            )),
            None if input == "positive" => match first_node(&nodes, |class| class == "CLIPTextEncode") {
                Some(id) => {
                    set(&mut nodes, &id, "text", json!(text), &mut filled, what);
                    filled.missed.push(
                        "nothing is wired to the sampler's positive input, so the prompt went to the first text encoder in the graph instead".to_string(),
                    );
                }
                None => filled
                    .missed
                    .push("the graph has no text encoder, so the prompt was not applied".to_string()),
            },
            None => filled.missed.push(format!(
                "nothing is wired to the sampler's {input} input, so {what} was not applied"
            )),
        }
    }

    // ── the size, snapped to the latent grid ──
    let width = snap_to_grid(request.width);
    let height = snap_to_grid(request.height);
    let latent = linked_node(&nodes, sampler.as_deref(), "latent_image").or_else(|| {
        first_node(&nodes, |class| {
            class == "EmptyLatentImage"
                || class == "EmptySD3LatentImage"
                || class == "EmptyHunyuanLatent"
        })
    });
    if let Some(id) = &latent {
        set(&mut nodes, id, "width", json!(width), &mut filled, "the width");
        set(&mut nodes, id, "height", json!(height), &mut filled, "the height");
        set(&mut nodes, id, "batch_size", json!(1), &mut filled, "one image per pass");
        filled.size = Some((width, height));
    } else {
        filled.missed.push(format!(
            "the graph has no empty latent image, so {width}x{height} was not applied and the server drew at its own size"
        ));
    }

    // ── the checkpoint ──
    let loader = linked_node(&nodes, sampler.as_deref(), "model")
        .or_else(|| first_node(&nodes, |class| class.starts_with("CheckpointLoader")));
    match loader {
        Some(id) if !model.trim().is_empty() => {
            // `ckpt_name` is the usual spelling, but a LoRA-stacked graph may load its model some
            // other way. Setting a key the node does not have makes ComfyUI refuse the whole
            // graph, which is worse than saying this one part did not take.
            if has_key(&nodes, &id, "ckpt_name") {
                set(&mut nodes, &id, "ckpt_name", json!(model), &mut filled, &format!("the checkpoint {model}"));
            } else {
                filled.missed.push(format!(
                    "node {id} loads a model but has no ckpt_name, so {model} was not applied and the graph's own checkpoint was used"
                ));
            }
        }
        // No model named means "whatever the graph says", which is a reasonable thing to want
        // from a graph a person exported themselves.
        Some(_) => {}
        None if !model.trim().is_empty() => {
            filled.missed.push(format!("the graph loads no checkpoint, so {model} was not applied"))
        }
        None => {}
    }

    // ── where the server writes its own copy ──
    // The bytes are fetched back and saved under Studio's folder whatever this says; naming the
    // date here means a copy left on the server can be matched to the one in the gallery.
    if let Some(id) = first_node(&nodes, |class| class == "SaveImage") {
        set(&mut nodes, &id, "filename_prefix", json!(filename_prefix), &mut filled, "the server-side filename");
    }

    filled.graph = Value::Object(nodes);
    Ok(filled)
}

fn class_of(node: &Value) -> String {
    node.get("class_type").and_then(|c| c.as_str()).unwrap_or_default().to_string()
}

fn has_key(nodes: &Map<String, Value>, id: &str, key: &str) -> bool {
    nodes
        .get(id)
        .and_then(|node| node.get("inputs"))
        .and_then(|inputs| inputs.as_object())
        .is_some_and(|inputs| inputs.contains_key(key))
}

/// The id of the first node of a kind. Node ids are strings and a JSON map's order is
/// alphabetical, not the order the graph was drawn in, so numeric ids are ordered as numbers:
/// "the first KSampler" then means the first one a person would have meant in the editor.
fn first_node(nodes: &Map<String, Value>, wanted: impl Fn(&str) -> bool) -> Option<String> {
    let mut ids: Vec<&String> = nodes.keys().collect();
    ids.sort_by(|a, b| order_of(a).cmp(&order_of(b)));
    ids.iter()
        .map(|id| id.to_string())
        .find(|id| wanted(&class_of(&nodes[id])))
}

fn order_of(id: &str) -> (bool, i64, &str) {
    match id.parse::<i64>() {
        Ok(number) => (false, number, ""),
        Err(_) => (true, 0, id),
    }
}

/// The id of the node feeding one of the sampler's inputs, if that input is a link to a node
/// this graph actually holds.
fn linked_node(nodes: &Map<String, Value>, sampler: Option<&str>, input: &str) -> Option<String> {
    let link = nodes.get(sampler?)?.get("inputs")?.get(input)?;
    let id = link.as_array()?.first()?.as_str()?.to_string();
    nodes.contains_key(&id).then_some(id)
}

/// Write one value into one node's inputs, and say what happened either way.
fn set(nodes: &mut Map<String, Value>, id: &str, key: &str, value: Value, filled: &mut Filled, what: &str) {
    let Some(inputs) = nodes
        .get_mut(id)
        .and_then(|node| node.get_mut("inputs"))
        .and_then(|inputs| inputs.as_object_mut())
    else {
        // `api_format` checks every node has an inputs object, so this is only reachable for a
        // graph built in memory by a test. Reported rather than panicked: this runs on a worker.
        filled.missed.push(format!("node {id} has no inputs, so {what} was not applied"));
        return;
    };
    // Only a key the node already has. Adding one ComfyUI does not expect makes it refuse the
    // whole graph, which is a worse outcome than reporting that this part did not take.
    if inputs.contains_key(key) {
        inputs.insert(key.into(), value);
        filled.applied.push(what.to_string());
    } else {
        filled
            .missed
            .push(format!("node {id} has no `{key}` input, so {what} was not applied"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        Request {
            prompt: "a lighthouse in fog, oil painting".into(),
            negative: "blurry, watermark".into(),
            width: 1024,
            height: 768,
            steps: 28,
            seed: 4_294_967_296,
            cfg: 6.5,
        }
    }

    fn fill_built_in() -> Filled {
        let template = api_format(BUILT_IN).unwrap();
        fill(template, &request(), "dreamshaperXL_v21.safetensors", "Studio/2026-09-22").unwrap()
    }

    #[test]
    fn the_graph_that_ships_is_one_a_server_will_run() {
        let template = api_format(BUILT_IN).unwrap();
        let nodes = template.as_object().unwrap();
        let classes: Vec<String> = nodes.values().map(|n| class_of(n)).collect();
        assert!(classes.iter().any(|c| c == "KSampler"), "{classes:?}");
        assert!(classes.iter().any(|c| c == "CheckpointLoaderSimple"), "{classes:?}");
        assert!(classes.iter().any(|c| c == "EmptyLatentImage"), "{classes:?}");
        assert!(classes.iter().any(|c| c == "SaveImage"), "{classes:?}");
        assert!(classes.iter().filter(|c| *c == "CLIPTextEncode").count() == 2, "{classes:?}");
        // The shipped graph has to fill in completely: a placeholder that reports "the prompt was
        // not applied" would be the first thing a person hits.
        let filled = fill_built_in();
        assert!(filled.missed.is_empty(), "{:?}", filled.missed);
        assert!(filled.applied.len() >= 10, "{:?}", filled.applied);
    }

    #[test]
    fn the_explanation_in_the_shipped_file_never_reaches_a_server() {
        // It is a real key in the file, so a person reading the repository learns what the graph
        // is; ComfyUI would try to run it as a node, so it is stripped before posting.
        let raw: Value = serde_json::from_str(BUILT_IN).unwrap();
        assert!(raw.as_object().unwrap().contains_key("_comment"));
        let filled = fill_built_in();
        assert!(!filled.graph.as_object().unwrap().contains_key("_comment"));
    }

    #[test]
    fn every_part_of_a_request_lands_in_the_graph() {
        let filled = fill_built_in();
        let nodes = filled.graph.as_object().unwrap();

        let sampler = &nodes["3"]["inputs"];
        assert_eq!(sampler["seed"], json!(4_294_967_296_i64));
        assert_eq!(sampler["steps"], json!(28));
        assert_eq!(sampler["cfg"], json!(6.5));
        assert_eq!(sampler["denoise"], json!(1.0));

        // The prompt goes to the node wired to `positive`, which in the shipped graph is 6 and in
        // somebody else's may not be. Following the link is the point.
        assert_eq!(nodes["6"]["inputs"]["text"], json!("a lighthouse in fog, oil painting"));
        assert_eq!(nodes["7"]["inputs"]["text"], json!("blurry, watermark"));

        assert_eq!(nodes["5"]["inputs"]["width"], json!(1024));
        assert_eq!(nodes["5"]["inputs"]["height"], json!(768));
        assert_eq!(nodes["5"]["inputs"]["batch_size"], json!(1));
        assert_eq!(nodes["4"]["inputs"]["ckpt_name"], json!("dreamshaperXL_v21.safetensors"));
        assert_eq!(nodes["9"]["inputs"]["filename_prefix"], json!("Studio/2026-09-22"));
        assert_eq!(filled.size, Some((1024, 768)));
    }

    #[test]
    fn a_seed_too_big_for_a_signed_number_survives_into_the_graph() {
        // ComfyUI takes a 64-bit seed. Casting it to i64 on the way would turn 2^64-2 into -2 and
        // the picture could never be reproduced from its own sidecar, which is the one promise a
        // sidecar makes.
        let template = api_format(BUILT_IN).unwrap();
        let big = Request { seed: u64::MAX - 1, ..request() };
        let filled = fill(template, &big, "m", "Studio").unwrap();
        let written = &filled.graph["3"]["inputs"]["seed"];
        assert_eq!(written.as_u64(), Some(u64::MAX - 1), "{written}");
        assert!(filled.graph.to_string().contains("18446744073709551614"), "{}", filled.graph);
    }

    #[test]
    fn a_size_off_the_latent_grid_is_snapped_and_the_sidecar_is_told_what_was_sent() {
        let template = api_format(BUILT_IN).unwrap();
        let odd = Request { width: 1023, height: 769, ..request() };
        let filled = fill(template, &odd, "sd_xl_base_1.0.safetensors", "Studio").unwrap();
        assert_eq!(filled.size, Some((1024, 768)));
        let nodes = filled.graph.as_object().unwrap();
        assert_eq!(nodes["5"]["inputs"]["width"], json!(1024));
        assert_eq!(nodes["5"]["inputs"]["height"], json!(768));
        // Snapping counts as applied, not missed: the server did get a size, just not the one
        // asked for, and `sent` in the sidecar carries the difference.
        assert!(filled.missed.is_empty(), "{:?}", filled.missed);
        assert_eq!(snap_to_grid(1), 8);
        assert_eq!(snap_to_grid(0), 8);
        assert_eq!(snap_to_grid(1024), 1024);
        assert_eq!(snap_to_grid(1027), 1024);
        assert_eq!(snap_to_grid(1029), 1032);
    }

    #[test]
    fn a_graph_with_the_nodes_named_and_ordered_differently_is_filled_the_same_way() {
        // The shipped graph's ids happen to put the sampler at 3 and the encoders at 6 and 7.
        // A person's own graph will not, and filling by node number would have been the bug.
        let template = json!({
            "sampler": {
                "class_type": "KSampler",
                "inputs": {
                    "seed": 0, "steps": 20, "cfg": 7.0, "denoise": 1.0,
                    "model": ["loader", 0], "positive": ["good", 0],
                    "negative": ["bad", 0], "latent_image": ["canvas", 0]
                }
            },
            "loader": {
                "class_type": "CheckpointLoaderSimple",
                "inputs": { "ckpt_name": "sd_xl_base_1.0.safetensors" }
            },
            "bad": { "class_type": "CLIPTextEncode", "inputs": { "text": "old", "clip": ["loader", 1] } },
            "good": { "class_type": "CLIPTextEncode", "inputs": { "text": "old", "clip": ["loader", 1] } },
            "canvas": { "class_type": "EmptyLatentImage", "inputs": { "width": 512, "height": 512, "batch_size": 4 } },
            "decode": { "class_type": "VAEDecode", "inputs": { "samples": ["sampler", 0], "vae": ["loader", 2] } },
            "save": { "class_type": "SaveImage", "inputs": { "filename_prefix": "x", "images": ["decode", 0] } }
        });
        let filled = fill(template, &request(), "my-model.safetensors", "Studio").unwrap();
        assert!(filled.missed.is_empty(), "{:?}", filled.missed);
        let nodes = filled.graph.as_object().unwrap();
        assert_eq!(nodes["good"]["inputs"]["text"], json!("a lighthouse in fog, oil painting"));
        assert_eq!(nodes["bad"]["inputs"]["text"], json!("blurry, watermark"));
        assert_eq!(nodes["canvas"]["inputs"]["batch_size"], json!(1));
        assert_eq!(nodes["loader"]["inputs"]["ckpt_name"], json!("my-model.safetensors"));
        assert_eq!(nodes["sampler"]["inputs"]["seed"], json!(4_294_967_296_i64));
    }

    #[test]
    fn an_advanced_sampler_gets_a_whole_pass_rather_than_resuming_halfway() {
        let mut template = api_format(BUILT_IN).unwrap();
        template["3"]["class_type"] = json!("KSamplerAdvanced");
        template["3"]["inputs"] = json!({
            "add_noise": "enable", "noise_seed": 0, "steps": 20,
            "start_at_step": 10, "end_at_step": 20, "return_with_leftover_noise": "disable",
            "model": ["4", 0], "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["5", 0]
        });
        let filled = fill(template, &request(), "sd_xl_base_1.0.safetensors", "Studio").unwrap();
        let sampler = &filled.graph["3"]["inputs"];
        assert_eq!(sampler["noise_seed"], json!(4_294_967_296_i64));
        assert_eq!(sampler["start_at_step"], json!(0));
        assert_eq!(sampler["end_at_step"], json!(28));
        // No cfg on an advanced sampler, and saying so beats pretending it was set.
        assert!(filled.missed.iter().any(|m| m.contains("cfg")), "{:?}", filled.missed);
        assert_eq!(filled.size, Some((1024, 768)));
    }

    #[test]
    fn a_graph_that_cannot_take_part_of_the_request_says_which_part() {
        // Nothing wired to the negative input: the prompt still lands and the missing half is named.
        let mut template = api_format(BUILT_IN).unwrap();
        template["3"]["inputs"].as_object_mut().unwrap().remove("negative");
        let filled = fill(template, &request(), "sd_xl_base_1.0.safetensors", "Studio").unwrap();
        assert_eq!(filled.graph["6"]["inputs"]["text"], json!("a lighthouse in fog, oil painting"));
        assert!(
            filled.missed.iter().any(|m| m.contains("negative prompt")),
            "{:?}",
            filled.missed
        );

        // No sampler at all: nothing about the seed or the steps could be honoured, and the
        // answer says that rather than returning a graph that quietly ignores both. The
        // checkpoint and the size are still found by their classes.
        let mut no_sampler = api_format(BUILT_IN).unwrap();
        no_sampler.as_object_mut().unwrap().remove("3");
        let filled = fill(no_sampler, &request(), "my-model.safetensors", "Studio").unwrap();
        assert!(filled.missed.iter().any(|m| m.contains("KSampler")), "{:?}", filled.missed);
        assert_eq!(filled.graph["4"]["inputs"]["ckpt_name"], json!("my-model.safetensors"));
        assert_eq!(filled.size, Some((1024, 768)));

        // A positive input wired to something that takes no text.
        let mut wired_wrong = api_format(BUILT_IN).unwrap();
        wired_wrong["3"]["inputs"]["positive"] = json!(["5", 0]);
        let filled = fill(wired_wrong, &request(), "sd_xl_base_1.0.safetensors", "Studio").unwrap();
        assert!(
            filled.missed.iter().any(|m| m.contains("prompt was not applied")),
            "{:?}",
            filled.missed
        );

        // A link to a node the graph does not hold falls back to the class scan instead of
        // writing into a node that is not there.
        let mut dangling = api_format(BUILT_IN).unwrap();
        dangling["3"]["inputs"]["positive"] = json!(["99", 0]);
        let filled = fill(dangling, &request(), "sd_xl_base_1.0.safetensors", "Studio").unwrap();
        assert_eq!(filled.graph["6"]["inputs"]["text"], json!("a lighthouse in fog, oil painting"));
        assert!(filled.missed.iter().any(|m| m.contains("first text encoder")), "{:?}", filled.missed);
    }

    #[test]
    fn a_graph_with_no_checkpoint_says_the_model_was_not_applied() {
        let template = json!({
            "3": {
                "class_type": "KSampler",
                "inputs": {
                    "seed": 0, "steps": 20, "cfg": 7.0, "denoise": 1.0,
                    "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["5", 0]
                }
            },
            "6": { "class_type": "CLIPTextEncode", "inputs": { "text": "" } },
            "7": { "class_type": "CLIPTextEncode", "inputs": { "text": "" } },
            "5": { "class_type": "EmptyLatentImage", "inputs": { "width": 512, "height": 512, "batch_size": 1 } }
        });
        let filled = fill(template, &request(), "my-model", "Studio").unwrap();
        assert!(filled.missed.iter().any(|m| m.contains("my-model")), "{:?}", filled.missed);
    }

    #[test]
    fn a_graph_that_loads_a_model_without_a_ckpt_name_is_not_given_one() {
        // Adding the key would make ComfyUI refuse the whole graph. Reporting is the alternative
        // that still produces a picture.
        let mut template = api_format(BUILT_IN).unwrap();
        template["4"]["class_type"] = json!("UNETLoader");
        template["4"]["inputs"] = json!({ "unet_name": "flux.safetensors", "weight_dtype": "default" });
        let filled = fill(template, &request(), "my-model", "Studio").unwrap();
        assert_eq!(filled.graph["4"]["inputs"]["unet_name"], json!("flux.safetensors"));
        assert!(filled.graph["4"]["inputs"].get("ckpt_name").is_none());
        assert!(filled.missed.iter().any(|m| m.contains("ckpt_name")), "{:?}", filled.missed);
    }

    #[test]
    fn an_empty_model_leaves_the_graphs_own_checkpoint_alone() {
        let filled = fill(api_format(BUILT_IN).unwrap(), &request(), "", "Studio").unwrap();
        assert_eq!(filled.graph["4"]["inputs"]["ckpt_name"], json!("sd_xl_base_1.0.safetensors"));
        assert!(!filled.missed.iter().any(|m| m.contains("checkpoint")), "{:?}", filled.missed);
    }

    #[test]
    fn the_editors_own_save_is_refused_with_the_words_that_fix_it() {
        let editor_export = json!({
            "last_node_id": 9,
            "nodes": [{ "id": 3, "type": "KSampler", "pos": [100, 200] }],
            "links": [[1, 4, 0, 3, 0, "MODEL"]],
            "version": 0.4
        });
        let problem = api_format(&editor_export.to_string()).unwrap_err();
        assert!(problem.contains("API Format"), "{problem}");
        assert!(problem.contains("Dev mode"), "{problem}");
    }

    #[test]
    fn a_graph_that_is_not_a_graph_is_refused_before_it_is_sent() {
        assert!(api_format("{ not json").unwrap_err().contains("JSON"));
        assert!(api_format("[]").unwrap_err().contains("map of nodes"));
        assert!(api_format("{}").unwrap_err().contains("no nodes"));
        // A node with no class is what ComfyUI would refuse, with a message about a node id that
        // means nothing to the person who exported it. Naming the node here is cheaper.
        assert!(api_format(r#"{"1":{"inputs":{}}}"#).unwrap_err().contains("class_type"));
        assert!(api_format(r#"{"1":{"class_type":"KSampler"}}"#).unwrap_err().contains("no inputs"));
        assert!(api_format(r#"{"1":"KSampler"}"#).unwrap_err().contains("not an object"));
        // A graph that is only notes holds no nodes, and that is the part worth saying.
        assert!(api_format(r#"{"_comment":["hi"]}"#).unwrap_err().contains("no nodes"));
    }

    #[test]
    fn a_saved_request_body_is_accepted_as_the_graph_inside_it() {
        let body = json!({ "prompt": api_format(BUILT_IN).unwrap(), "client_id": "someone" });
        let graph = api_format(&body.to_string()).unwrap();
        assert!(graph.get("3").is_some(), "{graph}");
        assert!(graph.get("client_id").is_none());
    }

    #[test]
    fn a_missing_override_names_the_file_and_the_setting_that_points_at_it() {
        let problem = graph("/no/such/graph.json").unwrap_err();
        assert!(problem.contains("/no/such/graph.json"), "{problem}");
        assert!(problem.contains("studio.json"), "{problem}");
        // An empty override is the normal case: it means "use the graph that ships".
        assert!(graph("").is_ok());
        assert!(graph("   ").is_ok());
    }

    #[test]
    fn an_override_that_is_the_editors_own_save_says_so_at_the_place_it_was_named() {
        let dir = std::env::temp_dir().join(format!("studio-graph-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mine.json");
        std::fs::write(&path, r#"{"nodes":[],"links":[]}"#).unwrap();
        let problem = graph(path.to_str().unwrap()).unwrap_err();
        assert!(problem.contains("mine.json"), "{problem}");
        assert!(problem.contains("API Format"), "{problem}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_tilde_in_a_configured_path_is_the_persons_home() {
        // Only the expansion is checked: a test has no graphs in anybody's home, and inventing one
        // there would be worse than asserting the join.
        //
        // Against this machine's home rather than a HOME set here. The environment is process-wide
        // and the tests in one binary run at the same time, so moving HOME to check a tilde would
        // change what every other test saw — including the ones asserting that a path is written
        // with `~/…` in it.
        let home = crate::gallery::home();
        assert_eq!(expand_home("~/graphs/mine.json"), home.join("graphs/mine.json"));
        assert_eq!(expand_home("/srv/graphs/mine.json"), Path::new("/srv/graphs/mine.json"));
        assert_eq!(expand_home("~/"), home);
    }
}
