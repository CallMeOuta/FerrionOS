use std::fs::{self, OpenOptions};
use std::io::{self, Write};

use serde_json::{Value, json};

use crate::{Result, fail, io_failure, uid};

const STATUS_PATH: &str = "/scheme/netstatus/status";
const CTL_PATH: &str = "/scheme/netctl/ctl";
const MAX_STATUS_BYTES: usize = 16 * 1024;

#[derive(Debug, PartialEq)]
pub enum Action {
    Status,
    Up(String),
    Down(String),
}

fn valid_iface(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 15
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn parse(words: &[&str]) -> Result<Action> {
    match words {
        ["status"] => Ok(Action::Status),
        ["up"] => Ok(Action::Up("eth0".to_owned())),
        ["up", iface] if valid_iface(iface) => Ok(Action::Up((*iface).to_owned())),
        ["down"] => Ok(Action::Down("eth0".to_owned())),
        ["down", iface] if valid_iface(iface) => Ok(Action::Down((*iface).to_owned())),
        _ => Err(fail(
            2,
            "Use ferrion network status, up [IFACE] or down [IFACE]. See ferrion help.",
        )),
    }
}

pub fn name(action: &Action) -> &'static str {
    match action {
        Action::Status => "network status",
        Action::Up(_) => "network up",
        Action::Down(_) => "network down",
    }
}

fn read_status() -> Result<Value> {
    let bytes = match fs::read(STATUS_PATH) {
        Ok(b) => b,
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) || err.raw_os_error() == Some(libc::ENODEV) =>
        {
            return Ok(json!({"online": false, "interfaces": []}));
        }
        Err(err) => return Err(io_failure(err)),
    };
    if bytes.len() > MAX_STATUS_BYTES {
        return Err(fail(5, "Network service status exceeded size limit."));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| fail(5, "Network service returned non-UTF-8 status."))?;
    let parsed: Value = serde_json::from_str(text)
        .map_err(|_| fail(5, "Network service returned invalid JSON."))?;
    let interfaces = parsed["interfaces"].as_array().cloned().unwrap_or_default();
    let online = interfaces.iter().any(|iface| iface["ipv4"].is_string());
    Ok(json!({"online": online, "interfaces": interfaces}))
}

fn write_ctl(command: &str) -> Result<()> {
    if uid() != 0 {
        return Err(fail(4, "Changing network link state requires root."));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .open(CTL_PATH)
        .map_err(io_failure)?;
    file.write_all(command.as_bytes()).map_err(io_failure)?;
    Ok(())
}

pub fn execute(action: &Action, yes: bool) -> Result<Value> {
    match action {
        Action::Status => read_status(),
        Action::Up(iface) => {
            if iface != "eth0" {
                return Err(fail(2, format!("Unknown interface {iface}.")));
            }
            crate::confirm(yes, &format!("Bring network interface {iface} up"))?;
            write_ctl(&format!("up {iface}\n"))?;
            read_status()
        }
        Action::Down(iface) => {
            if iface != "eth0" {
                return Err(fail(2, format!("Unknown interface {iface}.")));
            }
            crate::confirm(yes, &format!("Bring network interface {iface} down"))?;
            write_ctl(&format!("down {iface}\n"))?;
            read_status()
        }
    }
}

pub fn print(_action: &Action, data: &Value) {
    let Some(ifaces) = data["interfaces"].as_array() else {
        println!("Network: offline");
        return;
    };
    if ifaces.is_empty() {
        println!("Network: offline");
        return;
    }
    for iface in ifaces {
        let name = iface["name"].as_str().unwrap_or("eth0");
        let state = iface["state"].as_str().unwrap_or("unknown");
        let mode = iface["mode"].as_str().unwrap_or("unknown");
        let link = iface["link"].as_str().unwrap_or("unknown");
        let addr = iface["ipv4"].as_str().unwrap_or("none");
        let mtu = iface["mtu"].as_u64().unwrap_or(1500);
        println!("{name}: {state} ({mode})  link {link}  ipv4 {addr}  mtu {mtu}");
        if let Some(dns) = iface["dns"].as_array() {
            if !dns.is_empty() {
                let list: Vec<&str> = dns.iter().filter_map(Value::as_str).collect();
                println!("  dns: {}", list.join(", "));
            }
        }
        if let Some(rem) = iface["lease"]["remaining_secs"].as_u64() {
            let server = iface["lease"]["server"].as_str().unwrap_or("");
            println!("  lease: {rem}s remaining (server {server})");
        }
    }
}
