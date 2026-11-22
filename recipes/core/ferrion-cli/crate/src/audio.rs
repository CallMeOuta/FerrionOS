use ferrion_audio::{self as client, GainTarget};
use serde_json::{Value, json};

use crate::{Result, fail, io_failure};

#[derive(Debug, PartialEq)]
pub enum Action {
    Status,
    Apps,
    Volume {
        target: Option<(bool, String)>,
        percent: Option<u8>,
    },
}

pub fn parse(words: &[&str]) -> Result<Action> {
    match words {
        ["status"] => Ok(Action::Status),
        ["apps"] => Ok(Action::Apps),
        ["volume", rest @ ..] => {
            let mut target = None;
            let mut percent = None;
            let mut args = rest.iter().copied();
            while let Some(arg) = args.next() {
                if matches!(arg, "--app" | "--session") {
                    let id = args
                        .next()
                        .ok_or_else(|| fail(2, "Provide an audio identity."))?;
                    if target.is_some() || !client::valid_identity(id) {
                        return Err(fail(
                            2,
                            "Use one app or session identity: 1–64 ASCII letters, digits, '.', '_' or '-'.",
                        ));
                    }
                    target = Some((arg == "--app", id.to_owned()));
                } else {
                    if percent.is_some() {
                        return Err(fail(2, "Provide one volume percentage."));
                    }
                    percent = Some(
                        arg.parse::<u8>()
                            .ok()
                            .filter(|v| *v <= 100)
                            .ok_or_else(|| fail(2, "Volume must be an integer from 0 to 100."))?,
                    );
                }
            }
            Ok(Action::Volume { target, percent })
        }
        _ => Err(fail(
            2,
            "Use ferrion audio status, volume or apps. See ferrion help.",
        )),
    }
}

pub fn name(action: &Action) -> &'static str {
    match action {
        Action::Status => "audio status",
        Action::Apps => "audio apps",
        Action::Volume { .. } => "audio volume",
    }
}

pub fn execute(action: &Action, yes: bool) -> Result<Value> {
    match action {
        Action::Status | Action::Apps => {
            let text = match action {
                Action::Status => client::status_json(),
                _ => client::apps_json(),
            }
            .map_err(io_failure)?;
            serde_json::from_str(&text).map_err(|_| fail(5, "Audio service returned invalid JSON."))
        }
        Action::Volume { target, percent } => {
            if let Some(percent) = percent {
                crate::confirm(yes, &format!("Set audio volume to {percent}%"))?;
            }
            match target {
                None => {
                    if let Some(percent) = percent {
                        client::set_volume(*percent).map_err(io_failure)?;
                    }
                    Ok(json!({"volume": client::volume().map_err(io_failure)?}))
                }
                Some((app, id)) => {
                    let target = if *app {
                        GainTarget::App(id)
                    } else {
                        GainTarget::Session(id)
                    };
                    if let Some(percent) = percent {
                        target
                            .set_gain_db(client::percent_to_db(*percent).map_err(io_failure)?)
                            .map_err(io_failure)?;
                    }
                    let db = target.gain_db().map_err(io_failure)?;
                    Ok(
                        json!({"target": if *app { "app" } else { "session" }, "identity": id,
                        "gain_db": if db.is_finite() { Some(db) } else { None },
                        "percent": if db.is_finite() { 100.0 * 10.0_f32.powf(db / 20.0) } else { 0.0 }}),
                    )
                }
            }
        }
    }
}

pub fn print(action: &Action, data: &Value) {
    match action {
        Action::Apps => {
            if let Some(apps) = data["apps"].as_array() {
                if apps.is_empty() {
                    println!("No audio apps.");
                }
                for app in apps {
                    println!(
                        "{}  {}  {} streams",
                        app["identity"].as_str().unwrap_or(""),
                        gain(&app["gain_db"]),
                        app["streams"]
                    );
                }
            }
        }
        Action::Status => {
            println!(
                "{} Hz, {}, {} channels",
                data["rate"],
                data["format"].as_str().unwrap_or(""),
                data["channels"]
            );
            println!("Volume: {}%", data["volume"]);
            println!("Streams: {}", data["streams"]);
            println!(
                "Bit-perfect: {} ({})",
                data["bit_perfect"],
                data["reason"].as_str().unwrap_or("")
            );
            println!("Limiter frames: {}", data["limiter_frames"]);
        }
        Action::Volume {
            target: Some(_), ..
        } => {
            println!(
                "{} {}: {}",
                data["target"].as_str().unwrap_or(""),
                data["identity"].as_str().unwrap_or(""),
                gain(&data["gain_db"])
            );
        }
        Action::Volume { target: None, .. } => {
            println!("Volume: {}%", data["volume"]);
        }
    }
}

fn gain(value: &Value) -> String {
    match value.as_f64() {
        Some(db) => format!("{db:.4} dB"),
        None => "mute".into(),
    }
}
