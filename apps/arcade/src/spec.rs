//! The two bounded grammars a mind writes: `character` and `game`.
//!
//! The kit is a capability kit, not free-form generation. Everything a spec can say is
//! enumerated here; everything the spec does not say, the compiler decides. That division is
//! what makes the output verifiable: a bounded grammar can be refused with a sentence naming
//! the field, and a deterministic compiler can be held to hard gates (see `verify.rs`).
//!
//! Parsing is serde, so a wrongly-typed or unknown field is refused with serde's own path
//! ("missing field `palette`", "unknown field `collor`"). What serde cannot judge — a hex
//! colour that is not hex, a head:body ratio of 9, a chaser faster than the player it is
//! supposed to be escapable from — `validate` refuses afterwards, one sentence at a time,
//! naming the field the way the JSON spells it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── The vocabularies, as the refusals spell them ────────────────────
//
// serde's own "unknown variant" sentence does not name the field, and the kit's
// promise is a refusal that does. So every enumerated field is checked against
// its vocabulary on the raw JSON before the typed parse, one sentence at a time.

const ARCHETYPES: [&str; 4] = ["blob", "critter", "brute", "sprite"];
const EAR_SHAPES: [&str; 4] = ["none", "round", "pointy", "long"];
const TAIL_SHAPES: [&str; 4] = ["none", "stump", "long", "curl"];
const EXPRESSIONS: [&str; 4] = ["cheerful", "sleepy", "fierce", "surprised"];
const STANCES: [&str; 3] = ["upright", "crouched", "bouncy"];
const THEMES: [&str; 5] = ["meadow", "dusk", "candy", "volcano", "ice"];
const COLLECTIBLE_KINDS: [&str; 4] = ["berry", "coin", "crystal", "star"];
const HAZARD_KINDS: [&str; 3] = ["chaser", "wanderer", "patrol"];
const MUSIC_MOODS: [&str; 4] = ["calm", "bouncy", "tense", "playful"];

/// Every enumerated field a spec can carry, and the words it accepts — read off the same
/// constants the validator checks against, so the two cannot drift.
///
/// This exists because the grammar used to be unreadable. `describe` said only "the validator
/// refuses with a sentence naming the field", which is true and is not documentation: the only
/// way to learn that `expression` is one of four words was to guess wrong and be told. Four
/// specs were refused before one was accepted, each refusal correct, each costing a round trip
/// (#94). Every other surface on this desktop states its enums inline — `direction: left | right`
/// — and Arcade's live in a JSON document, so they have to be published deliberately.
///
/// `character` and `game` are separate because the two actions take different documents; a game's
/// `player.character` may inline a whole character, so a caller writing one needs both lists.
pub fn vocabularies() -> serde_json::Value {
    serde_json::json!({
        "character": {
            "archetype": ARCHETYPES,
            "ears": EAR_SHAPES,
            "tail": TAIL_SHAPES,
            "expression": EXPRESSIONS,
            "stance": STANCES,
        },
        "game": {
            "arena.theme": THEMES,
            "collectible.kind": COLLECTIBLE_KINDS,
            "hazards[].kind": HAZARD_KINDS,
            "music": MUSIC_MOODS,
        },
    })
}

/// The numeric fields and the range each is held to, in the same shape as [`vocabularies`].
///
/// The other half of what a caller has to guess. `proportions` is the one that catches people:
/// the keys are not the words anyone reaches for first, and a value outside the range is refused
/// just as firmly as a misspelled enum.
pub fn ranges() -> serde_json::Value {
    serde_json::json!({
        "character": {
            "proportions.head_body": [0.5, 1.6],
            "proportions.limb_length": [0.4, 1.4],
            "proportions.width": [0.7, 1.4],
        },
        "game": {
            "arena.size": [12.0, 40.0],
            "player.speed": [4.0, 12.0],
            "hazards[].speed": [0.5, 12.0],
        },
    })
}

/// One line naming every enumerated field and its words, for an action's argument text.
///
/// The argument description is what a mind reads before its first call, so the words go there
/// rather than only into `describe`'s state — a caller should not have to make two reads to find
/// out what one of them will accept.
pub fn vocabulary_line(document: &str) -> String {
    let all = vocabularies();
    let Some(fields) = all.get(document).and_then(|v| v.as_object()) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for (field, words) in fields {
        let words: Vec<&str> = words.as_array().map_or_else(Vec::new, |a| {
            a.iter().filter_map(|w| w.as_str()).collect()
        });
        parts.push(format!("{field}: {}", words.join(" | ")));
    }
    parts.join("; ")
}

fn check_vocab(field: &str, value: Option<&Value>, allowed: &[&str]) -> Result<(), String> {
    if let Some(found) = value.and_then(|v| v.as_str()) {
        if !allowed.contains(&found) {
            return Err(format!(
                "`{field}` must be one of {}, but this spec says \"{found}\".",
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}

/// The five enumerated fields of a character spec, under whatever prefix they sit
/// (top level for `new_character`, `player.character.` inside a game spec).
fn check_character_vocabulary(prefix: &str, raw: &Value) -> Result<(), String> {
    check_vocab(&format!("{prefix}archetype"), raw.get("archetype"), &ARCHETYPES)?;
    check_vocab(&format!("{prefix}ears"), raw.get("ears"), &EAR_SHAPES)?;
    check_vocab(&format!("{prefix}tail"), raw.get("tail"), &TAIL_SHAPES)?;
    check_vocab(&format!("{prefix}expression"), raw.get("expression"), &EXPRESSIONS)?;
    check_vocab(&format!("{prefix}stance"), raw.get("stance"), &STANCES)?;
    Ok(())
}

fn check_game_vocabulary(raw: &Value) -> Result<(), String> {
    check_vocab("arena.theme", raw.pointer("/arena/theme"), &THEMES)?;
    check_vocab("collectible.kind", raw.pointer("/collectible/kind"), &COLLECTIBLE_KINDS)?;
    check_vocab("music", raw.get("music"), &MUSIC_MOODS)?;
    if let Some(hazards) = raw.get("hazards").and_then(|v| v.as_array()) {
        for (i, group) in hazards.iter().enumerate() {
            check_vocab(&format!("hazards[{i}].kind"), group.get("kind"), &HAZARD_KINDS)?;
        }
    }
    // An inline player character carries its own five enums; a name reference is
    // just a string and must not be judged as a vocabulary word.
    if let Some(character) = raw.pointer("/player/character").filter(|c| c.is_object()) {
        check_character_vocabulary("player.character.", character)?;
    }
    Ok(())
}

// ── Character ───────────────────────────────────────────────────────

/// What body shape the creature is assembled from. The archetypes differ in default
/// silhouette scaling inside the engine: a blob is one round body with a face, a brute is
/// wide with short legs, a sprite is small with big ears. Proportions still move every one
/// of them; the archetype only sets the starting posture of the parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Archetype {
    Blob,
    Critter,
    Brute,
    Sprite,
}

impl Default for Archetype {
    fn default() -> Self {
        Archetype::Critter
    }
}

impl Archetype {
    pub fn as_str(&self) -> &'static str {
        match self {
            Archetype::Blob => "blob",
            Archetype::Critter => "critter",
            Archetype::Brute => "brute",
            Archetype::Sprite => "sprite",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EarShape {
    None,
    #[default]
    Round,
    Pointy,
    Long,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TailShape {
    None,
    #[default]
    Stump,
    Long,
    Curl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Expression {
    #[default]
    Cheerful,
    Sleepy,
    Fierce,
    Surprised,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Stance {
    #[default]
    Upright,
    Crouched,
    Bouncy,
}

/// Part proportions, all multipliers on the archetype's default build. Bounded so a spec
/// cannot ask for a head the size of the arena or limbs that clip through the floor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Proportions {
    /// Head radius as a multiple of body radius: 0.5 is a small head, 1.6 is a Muppet.
    pub head_body: f64,
    /// Arm and leg length multiplier.
    pub limb_length: f64,
    /// Overall width multiplier — how chunky the creature is.
    pub width: f64,
}

impl Default for Proportions {
    fn default() -> Self {
        Self { head_body: 1.0, limb_length: 1.0, width: 1.0 }
    }
}

/// The five-colour palette, by role. The roles are the style the owner asked for: a base
/// coat, a lighter belly, one accent (ears, tail tip), a nose and an eye colour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Palette {
    pub base: String,
    pub belly: String,
    pub accent: String,
    pub nose: String,
    pub eye: String,
}

/// A creature, as the engine's character builder understands it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CharacterSpec {
    /// The one required field: what the mind calls this creature, and what a game spec
    /// names to cast it.
    #[serde(default = "String::new")]
    pub name: String,
    pub archetype: Archetype,
    pub proportions: Proportions,
    pub ears: EarShape,
    pub tail: TailShape,
    pub palette: Option<Palette>,
    pub expression: Expression,
    pub stance: Stance,
}

impl Default for CharacterSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            archetype: Archetype::default(),
            proportions: Proportions::default(),
            ears: EarShape::default(),
            tail: TailShape::default(),
            palette: None,
            expression: Expression::default(),
            stance: Stance::default(),
        }
    }
}

/// Parse one character spec, or refuse with a sentence naming the field.
pub fn parse_character(text: &str) -> Result<CharacterSpec, String> {
    let raw: Value = serde_json::from_str(text)
        .map_err(|e| format!("the character spec is not valid JSON: {e}"))?;
    check_character_vocabulary("", &raw)?;
    let spec: CharacterSpec = serde_json::from_value(raw).map_err(|e| format!(
        "the character spec is not valid JSON this grammar accepts: {e}"
    ))?;
    spec.validate()?;
    Ok(spec)
}

impl CharacterSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("the character spec needs `name`: what the creature is called.".into());
        }
        if self.name.chars().count() > 40 {
            return Err(format!(
                "`name` must be 40 characters or fewer, but \"{}\" is {}.",
                self.name,
                self.name.chars().count()
            ));
        }
        if self.name.chars().any(|c| c.is_control()) {
            return Err(format!("`name` must not contain control characters, but \"{}\" does.", self.name));
        }
        let Some(palette) = &self.palette else {
            return Err(
                "the character spec needs `palette`: the five colours base, belly, accent, \
                 nose and eye, each a hex colour like \"#44cc88\"."
                    .into(),
            );
        };
        for (role, colour) in [
            ("base", &palette.base),
            ("belly", &palette.belly),
            ("accent", &palette.accent),
            ("nose", &palette.nose),
            ("eye", &palette.eye),
        ] {
            check_hex(&format!("palette.{role}"), colour)?;
        }
        check_range("proportions.head_body", self.proportions.head_body, 0.5, 1.6)?;
        check_range("proportions.limb_length", self.proportions.limb_length, 0.4, 1.4)?;
        check_range("proportions.width", self.proportions.width, 0.7, 1.4)?;
        Ok(())
    }
}

/// A hex colour is `#rgb` or `#rrggbb`, because that is all the engine's materials take.
pub fn check_hex(field: &str, value: &str) -> Result<(), String> {
    let body = value.strip_prefix('#').unwrap_or(value);
    let ok = matches!(body.len(), 3 | 6)
        && body.chars().all(|c| c.is_ascii_hexdigit())
        && value.starts_with('#');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "`{field}` must be a hex colour like \"#44cc88\", but was \"{value}\"."
        ))
    }
}

/// One refusal sentence for a number outside its bounds, naming the field as the JSON
/// spells it and saying what the bounds are, so the next spec is a correction and not a guess.
pub fn check_range(field: &str, value: f64, min: f64, max: f64) -> Result<(), String> {
    if value.is_finite() && value >= min && value <= max {
        Ok(())
    } else {
        Err(format!("`{field}` must be between {min} and {max}, but was {value}."))
    }
}

pub fn check_count(field: &str, value: u32, min: u32, max: u32) -> Result<(), String> {
    if value >= min && value <= max {
        Ok(())
    } else {
        Err(format!("`{field}` must be between {min} and {max}, but was {value}."))
    }
}

// ── Game ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ArenaTheme {
    #[default]
    Meadow,
    Dusk,
    Candy,
    Volcano,
    Ice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CollectibleKind {
    #[default]
    Berry,
    Coin,
    Crystal,
    Star,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HazardKind {
    /// Steers toward the player. Must be slower than the player or the game cannot be won.
    Chaser,
    /// Walks its own random path and bounces off walls.
    Wanderer,
    /// Runs a fixed line back and forth.
    Patrol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MusicMood {
    Calm,
    #[default]
    Bouncy,
    Tense,
    Playful,
}

/// A player character is either the name of one already saved in the library or a full
/// inline spec. Both arrive in one game spec; the library resolves the name before compile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CharacterRef {
    Name(String),
    Inline(Box<CharacterSpec>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ArenaSpec {
    /// Side length of the square arena in metres.
    pub size: f64,
    pub theme: ArenaTheme,
}

impl Default for ArenaSpec {
    fn default() -> Self {
        Self { size: 18.0, theme: ArenaTheme::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerSpec {
    pub character: CharacterRef,
    #[serde(default = "PlayerSpec::default_speed")]
    pub speed: f64,
}

impl PlayerSpec {
    fn default_speed() -> f64 {
        7.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CollectibleSpec {
    pub kind: CollectibleKind,
    /// How many to collect to WIN.
    pub count: u32,
}

impl Default for CollectibleSpec {
    fn default() -> Self {
        Self { kind: CollectibleKind::default(), count: 10 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HazardGroup {
    pub kind: HazardKind,
    /// Metres per second.
    pub speed: f64,
    /// How many of this kind.
    pub count: u32,
}

/// Arena colours, all five or none. None means the theme's own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArenaPalette {
    pub ground: String,
    pub wall: String,
    pub sky: String,
    pub item: String,
    pub hazard: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GameSpec {
    #[serde(default = "String::new")]
    pub title: String,
    pub arena: ArenaSpec,
    /// Required, so it has no default: a game with no player is not this genre.
    pub player: Option<PlayerSpec>,
    pub collectible: CollectibleSpec,
    pub hazards: Vec<HazardGroup>,
    pub lives: u32,
    pub palette: Option<ArenaPalette>,
    pub music: MusicMood,
}

impl Default for GameSpec {
    fn default() -> Self {
        Self {
            title: String::new(),
            arena: ArenaSpec::default(),
            player: None,
            collectible: CollectibleSpec::default(),
            hazards: Vec::new(),
            lives: 3,
            palette: None,
            music: MusicMood::default(),
        }
    }
}

/// The most a game may field at once. Beyond this the software renderer the verifier runs
/// on stops holding its frame budget, and the arena stops being readable.
const MAX_TOTAL_HAZARDS: u32 = 12;

pub fn parse_game(text: &str) -> Result<GameSpec, String> {
    let raw: Value = serde_json::from_str(text)
        .map_err(|e| format!("the game spec is not valid JSON: {e}"))?;
    check_game_vocabulary(&raw)?;
    let spec: GameSpec = serde_json::from_value(raw)
        .map_err(|e| format!("the game spec is not valid JSON this grammar accepts: {e}"))?;
    spec.validate()?;
    Ok(spec)
}

impl GameSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() {
            return Err("the game spec needs `title`: what the game is called.".into());
        }
        if self.title.chars().count() > 40 {
            return Err(format!(
                "`title` must be 40 characters or fewer, but \"{}\" is {}.",
                self.title,
                self.title.chars().count()
            ));
        }
        let Some(player) = &self.player else {
            return Err(
                "the game spec needs `player`: a character (a saved character's name, or an \
                 inline character spec) and a speed."
                    .into(),
            );
        };
        check_range("player.speed", player.speed, 4.0, 12.0)?;
        match &player.character {
            CharacterRef::Name(name) => {
                if name.trim().is_empty() {
                    return Err(
                        "`player.character` names a saved character, but the name is empty."
                            .into(),
                    );
                }
            }
            CharacterRef::Inline(character) => {
                character.validate().map_err(|e| format!("in `player.character`: {e}"))?;
            }
        }
        check_range("arena.size", self.arena.size, 12.0, 40.0)?;
        check_count("collectible.count", self.collectible.count, 3, 30)?;
        check_count("lives", self.lives, 1, 5)?;
        if self.hazards.len() > 3 {
            return Err(format!(
                "`hazards` holds at most 3 groups, but this spec has {}.",
                self.hazards.len()
            ));
        }
        let mut total: u32 = 0;
        for (i, group) in self.hazards.iter().enumerate() {
            check_count(&format!("hazards[{i}].count"), group.count, 1, 8)?;
            check_range(&format!("hazards[{i}].speed"), group.speed, 0.5, 12.0)?;
            // A chaser the player cannot outrun makes WIN unreachable, and the verifier's
            // win-bot would fail a game the grammar should never have accepted.
            if group.kind == HazardKind::Chaser && group.speed >= player.speed {
                return Err(format!(
                    "`hazards[{i}].speed` ({}) must be below `player.speed` ({}): a chaser \
                     the player cannot outrun makes the game unwinnable.",
                    group.speed, player.speed
                ));
            }
            if group.kind != HazardKind::Chaser && group.speed > player.speed {
                return Err(format!(
                    "`hazards[{i}].speed` ({}) must not exceed `player.speed` ({}) — nothing \
                     in the arena may be faster than the player.",
                    group.speed, player.speed
                ));
            }
            total += group.count;
        }
        if total > MAX_TOTAL_HAZARDS {
            return Err(format!(
                "`hazards` fields {total} hazards in total; the arena holds at most \
                 {MAX_TOTAL_HAZARDS}."
            ));
        }
        // Enough room to place every item apart from the spawn and from each other.
        // Eight square metres per item is what the engine's scatterer can keep
        // visually distinct and what its win-bot can still path between.
        let room = self.arena.size * self.arena.size;
        if room / 8.0 < self.collectible.count as f64 {
            return Err(format!(
                "`collectible.count` ({}) does not fit `arena.size` ({}): the arena needs \
                 roughly 8 square metres per item.",
                self.collectible.count, self.arena.size
            ));
        }
        if let Some(palette) = &self.palette {
            for (role, colour) in [
                ("ground", &palette.ground),
                ("wall", &palette.wall),
                ("sky", &palette.sky),
                ("item", &palette.item),
                ("hazard", &palette.hazard),
            ] {
                check_hex(&format!("palette.{role}"), colour)?;
            }
        }
        Ok(())
    }
}

/// A game spec with its player character resolved to an inline spec — the exact form the
/// compiler takes. Resolution against the library is `library.rs`'s job; the compiler never
/// sees a name it has to look up.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedGame {
    pub spec: GameSpec,
    pub character: CharacterSpec,
}

impl ResolvedGame {
    pub fn resolve(spec: GameSpec, saved: &dyn Fn(&str) -> Result<CharacterSpec, String>) -> Result<Self, String> {
        let Some(player) = spec.player.clone() else {
            return Err("the game spec needs `player`.".into());
        };
        let character = match player.character {
            CharacterRef::Inline(c) => *c,
            CharacterRef::Name(name) => saved(&name).map_err(|e| {
                format!("`player.character` names \"{name}\", and {e}")
            })?,
        };
        Ok(Self { spec, character })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> serde_json::Value {
        serde_json::json!({
            "base": "#7bc47f", "belly": "#f2e9d8", "accent": "#e8734a",
            "nose": "#3a2e2a", "eye": "#20242c"
        })
    }

    fn character_json() -> serde_json::Value {
        serde_json::json!({
            "name": "Moss",
            "archetype": "blob",
            "proportions": { "head_body": 1.2, "limb_length": 0.7, "width": 1.1 },
            "ears": "none",
            "tail": "stump",
            "palette": palette(),
            "expression": "sleepy",
            "stance": "crouched"
        })
    }

    fn game_json() -> serde_json::Value {
        serde_json::json!({
            "title": "Nut Rush",
            "arena": { "size": 20, "theme": "meadow" },
            "player": { "character": character_json(), "speed": 8 },
            "collectible": { "kind": "berry", "count": 10 },
            "hazards": [
                { "kind": "chaser", "speed": 4.5, "count": 2 },
                { "kind": "patrol", "speed": 6, "count": 1 }
            ],
            "lives": 3,
            "music": "bouncy"
        })
    }

    /// The published grammar is the enforced grammar, field by field.
    ///
    /// Not "the lists match the constants" — that would only prove one file agrees with itself.
    /// For every field `vocabularies()` advertises, this pokes a word that is certainly not in
    /// it into a real spec and requires the refusal to name that field and to list exactly the
    /// words the grammar published. If someone adds a vocabulary to the validator and forgets
    /// the grammar, or renames a field on one side, this fails.
    #[test]
    fn every_field_the_grammar_publishes_is_a_field_the_validator_enforces() {
        fn set(doc: &mut serde_json::Value, path: &str, word: &str) {
            // The three shapes of path the grammar uses: `music`, `arena.theme`, `hazards[].kind`.
            if let Some((head, tail)) = path.split_once("[].") {
                doc[head][0][tail] = serde_json::json!(word);
            } else if let Some((head, tail)) = path.split_once('.') {
                doc[head][tail] = serde_json::json!(word);
            } else {
                doc[path] = serde_json::json!(word);
            }
        }

        let all = vocabularies();
        let mut checked = 0;
        for (document, fields) in all.as_object().expect("the grammar is an object") {
            for (field, words) in fields.as_object().expect("each document lists its fields") {
                let published: Vec<&str> =
                    words.as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
                assert!(!published.is_empty(), "{document}.{field} publishes no words");

                let mut doc = if document == "character" { character_json() } else { game_json() };
                set(&mut doc, field, "definitely-not-a-word");
                let err = if document == "character" {
                    parse_character(&doc.to_string()).unwrap_err()
                } else {
                    parse_game(&doc.to_string()).unwrap_err()
                };

                let as_refused = field.replace("[]", "[0]");
                assert!(
                    err.contains(&as_refused),
                    "the grammar publishes `{document}.{field}`, but refusing a bad value for it                      said: {err}"
                );
                for word in &published {
                    assert!(
                        err.contains(word),
                        "the grammar says `{field}` accepts `{word}`, and the validator's own                          refusal does not mention it: {err}"
                    );
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 9, "nine enumerated fields were published; the count moved");
    }

    /// Every range the grammar publishes is one the validator holds a value to.
    #[test]
    fn every_range_the_grammar_publishes_is_a_range_the_validator_enforces() {
        fn set(doc: &mut serde_json::Value, path: &str, value: f64) {
            if let Some((head, tail)) = path.split_once("[].") {
                doc[head][0][tail] = serde_json::json!(value);
            } else if let Some((head, tail)) = path.split_once('.') {
                doc[head][tail] = serde_json::json!(value);
            } else {
                doc[path] = serde_json::json!(value);
            }
        }

        let all = ranges();
        let mut checked = 0;
        for (document, fields) in all.as_object().expect("the ranges are an object") {
            for (field, bounds) in fields.as_object().expect("each document lists its fields") {
                let pair = bounds.as_array().expect("a range is [min, max]");
                let max = pair[1].as_f64().expect("the max is a number");

                let mut doc = if document == "character" { character_json() } else { game_json() };
                set(&mut doc, field, max + 1_000.0);
                let err = if document == "character" {
                    parse_character(&doc.to_string()).unwrap_err()
                } else {
                    parse_game(&doc.to_string()).unwrap_err()
                };
                let as_refused = field.replace("[]", "[0]");
                assert!(
                    err.contains(&as_refused),
                    "the grammar publishes the range for `{document}.{field}`, but a value far                      above its maximum was refused with: {err}"
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 6, "six ranged fields were published; the count moved");
    }

    /// The one-liner the argument text is built from names every field and every word.
    #[test]
    fn the_argument_line_carries_the_whole_vocabulary() {
        for document in ["character", "game"] {
            let line = vocabulary_line(document);
            let fields = vocabularies();
            for (field, words) in fields[document].as_object().unwrap() {
                assert!(line.contains(field), "`{document}` line omits {field}: {line}");
                for word in words.as_array().unwrap() {
                    let word = word.as_str().unwrap();
                    assert!(line.contains(word), "`{document}` line omits {word}: {line}");
                }
            }
        }
        assert_eq!(vocabulary_line("nonsense"), "", "an unknown document is empty, not a panic");
    }

    #[test]
    fn a_full_character_spec_parses() {
        let spec = parse_character(&character_json().to_string()).unwrap();
        assert_eq!(spec.name, "Moss");
        assert_eq!(spec.archetype, Archetype::Blob);
        assert_eq!(spec.proportions.head_body, 1.2);
        assert_eq!(spec.palette.as_ref().unwrap().accent, "#e8734a");
    }

    #[test]
    fn a_minimal_character_spec_gets_the_defaults() {
        // Only `name` and `palette` carry meaning a compiler cannot invent; everything else
        // has a default the grammar states, so a small spec is not a refused spec.
        let spec = parse_character(&serde_json::json!({ "name": "Pip", "palette": palette() }).to_string()).unwrap();
        assert_eq!(spec.archetype, Archetype::Critter);
        assert_eq!(spec.proportions, Proportions::default());
        assert_eq!(spec.expression, Expression::Cheerful);
    }

    #[test]
    fn a_bad_colour_is_refused_naming_the_field() {
        let mut bad = character_json();
        bad["palette"]["eye"] = serde_json::json!("green");
        let err = parse_character(&bad.to_string()).unwrap_err();
        assert!(err.contains("palette.eye"), "{err}");
        assert!(err.contains("green"), "the refusal quotes what it was given: {err}");

        let mut short = character_json();
        short["palette"]["base"] = serde_json::json!("#12345");
        let err = parse_character(&short.to_string()).unwrap_err();
        assert!(err.contains("palette.base"), "{err}");
    }

    #[test]
    fn three_digit_hex_is_accepted() {
        let mut ok = character_json();
        ok["palette"]["nose"] = serde_json::json!("#3a2");
        assert!(parse_character(&ok.to_string()).is_ok());
    }

    #[test]
    fn an_out_of_range_proportion_is_refused_naming_the_field() {
        let mut bad = character_json();
        bad["proportions"]["head_body"] = serde_json::json!(9.0);
        let err = parse_character(&bad.to_string()).unwrap_err();
        assert!(err.contains("proportions.head_body"), "{err}");
        assert!(err.contains("between 0.5 and 1.6"), "the refusal says the bounds: {err}");
    }

    #[test]
    fn a_missing_name_is_refused_with_a_sentence() {
        let mut bad = character_json();
        bad.as_object_mut().unwrap().remove("name");
        let err = parse_character(&bad.to_string()).unwrap_err();
        assert!(err.contains("name"), "{err}");
    }

    #[test]
    fn an_unknown_field_is_refused_not_dropped() {
        // The grammar is bounded on purpose: a field the kit does not know is a field whose
        // effect the mind believes in and the compiler will never apply. Silence here would
        // be a lie of omission.
        let mut bad = character_json();
        bad["collor"] = serde_json::json!("#fff");
        let err = parse_character(&bad.to_string()).unwrap_err();
        assert!(err.contains("collor"), "{err}");
    }

    #[test]
    fn a_full_game_spec_parses() {
        let spec = parse_game(&game_json().to_string()).unwrap();
        assert_eq!(spec.title, "Nut Rush");
        assert_eq!(spec.arena.size, 20.0);
        assert_eq!(spec.hazards.len(), 2);
        assert_eq!(spec.lives, 3);
    }

    #[test]
    fn a_game_needs_a_title_and_a_player() {
        let mut no_title = game_json();
        no_title.as_object_mut().unwrap().remove("title");
        let err = parse_game(&no_title.to_string()).unwrap_err();
        assert!(err.contains("title"), "{err}");

        let mut no_player = game_json();
        no_player.as_object_mut().unwrap().remove("player");
        let err = parse_game(&no_player.to_string()).unwrap_err();
        assert!(err.contains("player"), "{err}");
    }

    #[test]
    fn a_chaser_faster_than_the_player_is_refused_as_unwinnable() {
        let mut bad = game_json();
        bad["hazards"][0]["speed"] = serde_json::json!(9.0); // player.speed is 8
        let err = parse_game(&bad.to_string()).unwrap_err();
        assert!(err.contains("hazards[0].speed"), "{err}");
        assert!(err.contains("unwinnable"), "{err}");
    }

    #[test]
    fn a_game_missing_its_player_says_so_from_inside_the_character_validation() {
        let mut bad = game_json();
        bad["player"]["character"]["palette"]["belly"] = serde_json::json!("cream");
        let err = parse_game(&bad.to_string()).unwrap_err();
        assert!(err.contains("player.character"), "{err}");
        assert!(err.contains("palette.belly"), "{err}");
    }

    #[test]
    fn too_many_hazards_are_refused() {
        let mut bad = game_json();
        bad["hazards"] = serde_json::json!([
            { "kind": "chaser", "speed": 4, "count": 8 },
            { "kind": "wanderer", "speed": 4, "count": 8 }
        ]);
        let err = parse_game(&bad.to_string()).unwrap_err();
        assert!(err.contains("hazards"), "{err}");
        assert!(err.contains("12"), "{err}");
    }

    #[test]
    fn items_must_fit_the_arena() {
        let mut bad = game_json();
        bad["arena"]["size"] = serde_json::json!(12);
        bad["collectible"]["count"] = serde_json::json!(30);
        let err = parse_game(&bad.to_string()).unwrap_err();
        assert!(err.contains("collectible.count"), "{err}");
        assert!(err.contains("arena.size"), "{err}");
    }

    #[test]
    fn a_bad_music_mood_lists_the_vocabulary() {
        let mut bad = game_json();
        bad["music"] = serde_json::json!("jazz");
        let err = parse_game(&bad.to_string()).unwrap_err();
        assert!(err.contains("music"), "{err}");
        // serde names the variants it wanted, so the next spec is a correction.
        assert!(err.contains("calm") || err.contains("variant"), "{err}");
    }

    #[test]
    fn a_character_reference_by_name_needs_no_inline_validation() {
        let mut g = game_json();
        g["player"]["character"] = serde_json::json!("Moss");
        let spec = parse_game(&g.to_string()).unwrap();
        assert!(matches!(spec.player.unwrap().character, CharacterRef::Name(_)));
    }

    #[test]
    fn resolution_replaces_a_name_with_the_saved_spec() {
        let mut g = game_json();
        g["player"]["character"] = serde_json::json!("Moss");
        let spec = parse_game(&g.to_string()).unwrap();
        let moss = parse_character(&character_json().to_string()).unwrap();
        let resolved = ResolvedGame::resolve(spec, &|name| {
            if name == "Moss" {
                Ok(moss.clone())
            } else {
                Err(format!("no character called \"{name}\" is saved"))
            }
        })
        .unwrap();
        assert_eq!(resolved.character.name, "Moss");

        let mut g2 = game_json();
        g2["player"]["character"] = serde_json::json!("Ghost");
        let err = ResolvedGame::resolve(parse_game(&g2.to_string()).unwrap(), &|name| {
            Err(format!("no character called \"{name}\" is saved"))
        })
        .unwrap_err();
        assert!(err.contains("Ghost"), "{err}");
    }
}
