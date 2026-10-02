//! The volume and the backlight on the shell's control surface: what `describe shell` says
//! about them, and the three actions that move them.
//!
//! Whatever the slider in Quick Settings can do, a mind can ask for, and the answer is the
//! machine's own reading taken after the change, not the value that was asked for: a volume
//! the audio server refused, or a backlight the machine does not have, must not come back as
//! done. The machine is reached through closures so the rules here are tested without one.

use serde_json::{json, Value};
use yantrik_os::audio::AudioState;

/// `level` as a percent: a whole number from 0 to 100. Not clamped: a caller that asked for 150
/// should be told that is not a level, not handed 100 and left to think it got what it asked.
pub fn parse_level(args: &Value) -> Result<u8, String> {
    let level = args
        .get("level")
        .and_then(Value::as_i64)
        .ok_or("`level` must be a whole number from 0 to 100")?;
    u8::try_from(level)
        .ok()
        .filter(|l| *l <= 100)
        .ok_or_else(|| format!("`level` is {level}; it must be from 0 to 100"))
}

/// What `describe shell` says about sound: the machine's volume, or `null` when it has no audio
/// server to ask, so a reader never mistakes a missing mixer for a muted one.
pub fn audio_for_describe(available: bool, volume: i32, muted: bool) -> Value {
    if available {
        json!({ "volume": volume, "muted": muted })
    } else {
        Value::Null
    }
}

/// What `describe shell` says about the backlight. `level` is `null` when there is none.
pub fn brightness_for_describe(available: bool, level: i32) -> Value {
    json!({ "available": available, "level": if available { json!(level) } else { Value::Null } })
}

fn audio_answer(state: AudioState) -> Value {
    json!({ "volume": state.volume_pct, "muted": state.muted })
}

/// `set_volume`: set it, then answer with what the machine now reads.
pub fn set_volume(
    args: &Value,
    set: impl FnOnce(u8) -> Result<(), String>,
    read: impl FnOnce() -> Option<AudioState>,
) -> Result<Value, String> {
    let level = parse_level(args)?;
    set(level)?;
    let now = read().ok_or("the volume was set, but the audio server did not answer when asked for it back")?;
    Ok(audio_answer(now))
}

/// `set_mute`: mute or unmute, then answer with what the machine now reads.
pub fn set_mute(
    args: &Value,
    set: impl FnOnce(bool) -> Result<(), String>,
    read: impl FnOnce() -> Option<AudioState>,
) -> Result<Value, String> {
    let muted = args.get("muted").and_then(Value::as_bool).ok_or("`muted` must be true or false")?;
    set(muted)?;
    let now = read().ok_or("the mute was set, but the audio server did not answer when asked for it back")?;
    Ok(audio_answer(now))
}

/// `set_brightness`: refuses plainly when the machine has no backlight (a VM, a desktop
/// monitor), otherwise sets it and answers with the level the panel now reads.
pub fn set_brightness(
    args: &Value,
    available: bool,
    set: impl FnOnce(u8) -> Result<(), String>,
    read: impl FnOnce() -> Option<u8>,
) -> Result<Value, String> {
    if !available {
        return Err("this machine has no backlight, so there is no brightness to set (`describe shell` shows brightness.available: false)".into());
    }
    let level = parse_level(args)?;
    set(level)?;
    let now = read().ok_or("the brightness was set, but the backlight could not be read back")?;
    Ok(json!({ "available": true, "level": now }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(volume_pct: u8, muted: bool) -> Option<AudioState> {
        Some(AudioState { volume_pct, muted })
    }

    #[test]
    fn a_level_is_a_whole_percent_and_never_quietly_clamped() {
        assert_eq!(parse_level(&json!({ "level": 0 })), Ok(0));
        assert_eq!(parse_level(&json!({ "level": 100 })), Ok(100));
        for bad in [json!({ "level": 101 }), json!({ "level": -1 }), json!({ "level": 45.5 }), json!({ "level": "loud" }), json!({})] {
            assert!(parse_level(&bad).is_err(), "{bad}");
        }
        assert!(parse_level(&json!({ "level": 150 })).unwrap_err().contains("0 to 100"));
    }

    #[test]
    fn set_volume_answers_with_what_the_machine_reads_not_what_was_asked() {
        // The audio server capped it: the answer says 80, and does not echo the 95 asked for.
        let answer = set_volume(&json!({ "level": 95 }), |_| Ok(()), || state(80, false)).unwrap();
        assert_eq!(answer, json!({ "volume": 80, "muted": false }));
    }

    #[test]
    fn set_volume_hands_the_level_to_the_machine_and_reports_its_refusal() {
        let mut got = None;
        set_volume(&json!({ "level": 30 }), |l| { got = Some(l); Ok(()) }, || state(30, false)).unwrap();
        assert_eq!(got, Some(30));
        let err = set_volume(&json!({ "level": 30 }), |_| Err("wpctl could not be run".into()), || state(30, false)).unwrap_err();
        assert!(err.contains("wpctl"), "{err}");
        // A bad level never reaches the machine.
        let mut touched = false;
        assert!(set_volume(&json!({ "level": 200 }), |_| { touched = true; Ok(()) }, || state(0, false)).is_err());
        assert!(!touched);
    }

    #[test]
    fn a_volume_that_cannot_be_read_back_is_not_reported_as_done() {
        let err = set_volume(&json!({ "level": 30 }), |_| Ok(()), || None).unwrap_err();
        assert!(err.contains("did not answer"), "{err}");
    }

    #[test]
    fn set_mute_wants_a_real_boolean_and_reads_the_result_back() {
        let answer = set_mute(&json!({ "muted": true }), |m| { assert!(m); Ok(()) }, || state(45, true)).unwrap();
        assert_eq!(answer, json!({ "volume": 45, "muted": true }));
        assert!(set_mute(&json!({ "muted": "yes" }), |_| Ok(()), || state(45, true)).is_err());
        assert!(set_mute(&json!({}), |_| Ok(()), || state(45, true)).is_err());
    }

    #[test]
    fn set_brightness_refuses_clearly_without_a_backlight() {
        let mut touched = false;
        let err = set_brightness(&json!({ "level": 50 }), false, |_| { touched = true; Ok(()) }, || Some(50)).unwrap_err();
        assert!(err.contains("no backlight"), "{err}");
        assert!(!touched, "nothing was run against a machine with no backlight");
    }

    #[test]
    fn set_brightness_answers_with_the_panels_own_reading() {
        let answer = set_brightness(&json!({ "level": 1 }), true, |_| Ok(()), || Some(1)).unwrap();
        assert_eq!(answer, json!({ "available": true, "level": 1 }));
        assert!(set_brightness(&json!({ "level": 101 }), true, |_| Ok(()), || Some(100)).is_err());
        assert!(set_brightness(&json!({ "level": 50 }), true, |_| Err("no route".into()), || Some(50)).is_err());
    }

    #[test]
    fn describe_shows_no_number_for_hardware_that_is_not_there() {
        assert_eq!(audio_for_describe(false, 0, false), Value::Null, "no mixer is not a muted mixer");
        assert_eq!(audio_for_describe(true, 45, true), json!({ "volume": 45, "muted": true }));
        assert_eq!(brightness_for_describe(false, 0), json!({ "available": false, "level": null }));
        assert_eq!(brightness_for_describe(true, 70), json!({ "available": true, "level": 70 }));
    }
}
