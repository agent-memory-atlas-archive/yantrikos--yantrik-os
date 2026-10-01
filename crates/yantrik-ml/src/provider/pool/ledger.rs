//! What the pool used, per day, provider and model: for "today: 412 calls across 6 providers,
//! $0 spent", and so a restart does not forget the day's counts. Kept as one small JSON file,
//! written whole and replaced atomically, holding the last 14 days.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub requests: u64,
    pub tokens: u64,
    /// Refusals: 429s and errors.
    pub refused: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Ledger {
    /// "2026-09-30" → "groq|openai/gpt-oss-120b" → totals.
    days: BTreeMap<String, BTreeMap<String, Totals>>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Ledger {
    /// The ledger at `path`, or an empty one when there is none yet or it cannot be read.
    pub fn open(path: &Path) -> Ledger {
        let mut l: Ledger = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        l.path = Some(path.to_path_buf());
        l
    }

    pub fn add(&mut self, day: &str, provider: &str, model: &str, ok: bool, tokens: u64) {
        let t = self.days.entry(day.to_string()).or_default().entry(format!("{provider}|{model}")).or_default();
        t.requests += 1;
        t.tokens += tokens;
        if !ok {
            t.refused += 1;
        }
        while self.days.len() > 14 {
            let oldest = self.days.keys().next().cloned();
            if let Some(k) = oldest {
                self.days.remove(&k);
            }
        }
    }

    /// One day's totals per provider (models summed).
    pub fn day(&self, day: &str) -> BTreeMap<String, Totals> {
        let mut out: BTreeMap<String, Totals> = BTreeMap::new();
        for (k, t) in self.days.get(day).into_iter().flatten() {
            let provider = k.split('|').next().unwrap_or(k).to_string();
            let o = out.entry(provider).or_default();
            o.requests += t.requests;
            o.tokens += t.tokens;
            o.refused += t.refused;
        }
        out
    }

    /// Write it out: to a temporary file beside it, then renamed over it.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    }
}

/// "2026-09-30" for a unix time, in UTC.
pub fn day_name(now: i64) -> String {
    let (y, m, d) = super::quota::civil(now.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}
