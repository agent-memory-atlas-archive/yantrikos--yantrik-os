//! Transcribe raw PCM to text with the bundled Whisper model.
//!
//! Reads 16 kHz mono f32 little-endian PCM — the format `ffmpeg -f f32le -ar 16000
//! -ac 1` emits — from a file or stdin, and prints the transcript. Keeping the
//! resampling in ffmpeg rather than here means this binary never has to know about
//! WAV headers, channel layouts or sample-rate conversion.
//!
//! Whisper's encoder is fixed at 30 seconds of audio, so anything longer is cut
//! into windows and transcribed one at a time. The windows overlap slightly, since
//! a hard cut lands mid-word often enough to matter.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use yantrik_ml::stt::CandleWhisper;

const SAMPLE_RATE: usize = 16_000;
const WINDOW: usize = 30 * SAMPLE_RATE;
/// A word straddling a window boundary is lost from both sides unless the windows
/// overlap. One second is enough for a word and cheap enough not to matter.
const OVERLAP: usize = SAMPLE_RATE;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut model_dir = PathBuf::from("/opt/yantrik/models/whisper");
    let mut input: Option<PathBuf> = None;
    let mut timestamps = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" => model_dir = args.next().context("--model needs a path")?.into(),
            "--timestamps" => timestamps = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: transcribe [--model DIR] [--timestamps] [FILE]\n\
                     \n\
                     FILE is raw 16kHz mono f32le PCM, or stdin if omitted. To make one:\n\
                     \n    ffmpeg -i in.wav -f f32le -ar 16000 -ac 1 - | transcribe\n"
                );
                return Ok(());
            }
            other => input = Some(other.into()),
        }
    }

    let mut raw = Vec::new();
    match &input {
        Some(path) => {
            raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?
        }
        None => {
            std::io::stdin()
                .read_to_end(&mut raw)
                .context("reading PCM from stdin")?;
        }
    }

    // Trailing bytes mean the input was truncated mid-sample; drop them rather than
    // reading past the end, and say so, because silent truncation looks like a short clip.
    let remainder = raw.len() % 4;
    if remainder != 0 {
        eprintln!("warning: input is not a whole number of f32 samples, dropping {remainder} byte(s)");
        raw.truncate(raw.len() - remainder);
    }

    let pcm: Vec<f32> = raw
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    if pcm.is_empty() {
        anyhow::bail!("no audio on the input");
    }

    let seconds = pcm.len() as f64 / SAMPLE_RATE as f64;
    eprintln!(
        "{:.1}s of audio, loading model from {}",
        seconds,
        model_dir.display()
    );

    let started = std::time::Instant::now();
    let whisper = CandleWhisper::from_dir(&model_dir)
        .with_context(|| format!("loading Whisper from {}", model_dir.display()))?;
    eprintln!("model loaded in {:.1}s", started.elapsed().as_secs_f64());

    let started = std::time::Instant::now();
    let mut offset = 0usize;
    while offset < pcm.len() {
        let end = (offset + WINDOW).min(pcm.len());
        let window = &pcm[offset..end];

        // Silence transcribes as a hallucinated caption ("Thank you.", subtitle credits)
        // rather than as nothing, so drop windows with no signal before asking.
        let peak = window.iter().fold(0f32, |acc, s| acc.max(s.abs()));
        if peak > 0.005 {
            let out = whisper.transcribe(window).context("transcribing window")?;
            let text = out.text.trim();
            if !text.is_empty() {
                if timestamps {
                    let at = offset / SAMPLE_RATE;
                    println!("[{:02}:{:02}] {}", at / 60, at % 60, text);
                } else {
                    println!("{text}");
                }
            }
        }

        if end == pcm.len() {
            break;
        }
        offset += WINDOW - OVERLAP;
    }

    eprintln!(
        "transcribed {:.1}s of audio in {:.1}s",
        seconds,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
