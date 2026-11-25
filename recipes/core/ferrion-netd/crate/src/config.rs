use std::fs;
use std::io;
use std::net::Ipv4Addr;

pub const CONFIG_PATH: &str = "/etc/net/config.toml";
pub const MAX_CONFIG_BYTES: usize = 16 * 1024;
pub const MAX_CONFIG_LINES: usize = 256;
pub const MAX_DNS_SERVERS: usize = 8;
pub const MIN_MTU: u16 = 576;
pub const MAX_MTU: u16 = 1500;
pub const FALLBACK_DNS: Ipv4Addr = Ipv4Addr::new(9, 9, 9, 9);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkMode {
    Dhcp,
    Static(StaticConfig),
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticConfig {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
    pub gateway: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub mtu: u16,
}

fn unquote(raw: &str) -> Option<&str> {
    let s = raw.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        Some(&s[1..s.len() - 1])
    } else {
        None
    }
}

fn parse_cidr(raw: &str) -> Option<(Ipv4Addr, u8)> {
    let (ip_s, pfx_s) = raw.split_once('/')?;
    let ip: Ipv4Addr = ip_s.trim().parse().ok()?;
    let pfx: u8 = pfx_s.trim().parse().ok()?;
    if pfx == 0 || pfx > 32 || !is_usable_unicast(ip) {
        return None;
    }
    Some((ip, pfx))
}

pub fn is_usable_unicast(ip: Ipv4Addr) -> bool {
    !ip.is_unspecified() && !ip.is_broadcast() && !ip.is_multicast() && !ip.is_loopback()
}

fn parse_ip_array(raw: &str) -> Option<Vec<Ipv4Addr>> {
    let s = raw.trim();
    if !s.starts_with('[') || !s.ends_with(']') {
        return None;
    }
    let inner = s[1..s.len() - 1].trim();
    let mut out = Vec::new();
    if inner.is_empty() {
        return Some(out);
    }
    for part in inner.split(',') {
        let item = part.trim();
        if item.is_empty() {
            continue;
        }
        let text = unquote(item)?;
        let ip: Ipv4Addr = text.parse().ok()?;
        if !is_usable_unicast(ip) {
            return None;
        }
        if out.len() < MAX_DNS_SERVERS {
            out.push(ip);
        }
    }
    Some(out)
}

pub fn parse_config(text: &str) -> Result<LinkMode, String> {
    if text.len() > MAX_CONFIG_BYTES {
        return Err(format!("config exceeds {MAX_CONFIG_BYTES} bytes"));
    }
    let mut mode: Option<String> = None;
    let mut address: Option<(Ipv4Addr, u8)> = None;
    let mut gateway: Option<Ipv4Addr> = None;
    let mut dns: Vec<Ipv4Addr> = Vec::new();
    let mut mtu: u16 = MAX_MTU;

    for (idx, raw_line) in text.lines().enumerate() {
        if idx >= MAX_CONFIG_LINES {
            return Err(format!("config exceeds {MAX_CONFIG_LINES} lines"));
        }
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!("line {}: expected key = value", idx + 1));
        };
        let key = k.trim();
        let val = v.trim();
        match key {
            "mode" => {
                let s = unquote(val).ok_or_else(|| format!("line {}: invalid mode", idx + 1))?;
                mode = Some(s.to_owned());
            }
            "address" => {
                let s = unquote(val).ok_or_else(|| format!("line {}: invalid address", idx + 1))?;
                address = Some(
                    parse_cidr(s).ok_or_else(|| format!("line {}: invalid CIDR {s:?}", idx + 1))?,
                );
            }
            "gateway" => {
                let s = unquote(val).ok_or_else(|| format!("line {}: invalid gateway", idx + 1))?;
                let ip: Ipv4Addr = s
                    .parse()
                    .map_err(|_| format!("line {}: invalid gateway IP", idx + 1))?;
                if !is_usable_unicast(ip) {
                    return Err(format!("line {}: unusable gateway IP", idx + 1));
                }
                gateway = Some(ip);
            }
            "dns" => {
                dns = parse_ip_array(val)
                    .ok_or_else(|| format!("line {}: invalid dns list", idx + 1))?;
            }
            "mtu" => {
                let parsed: u16 = val
                    .parse()
                    .map_err(|_| format!("line {}: invalid mtu", idx + 1))?;
                if !(MIN_MTU..=MAX_MTU).contains(&parsed) {
                    return Err(format!(
                        "line {}: mtu must be in {MIN_MTU}..={MAX_MTU}",
                        idx + 1
                    ));
                }
                mtu = parsed;
            }
            _ => return Err(format!("line {}: unknown key {key:?}", idx + 1)),
        }
    }

    match mode.as_deref().unwrap_or("dhcp") {
        "dhcp" => Ok(LinkMode::Dhcp),
        "off" => Ok(LinkMode::Off),
        "static" => {
            let (addr, prefix_len) =
                address.ok_or_else(|| "static mode requires address".to_owned())?;
            Ok(LinkMode::Static(StaticConfig {
                address: addr,
                prefix_len,
                gateway,
                dns,
                mtu,
            }))
        }
        other => Err(format!("unknown mode {other:?}")),
    }
}

pub fn load_config() -> LinkMode {
    match fs::read(CONFIG_PATH) {
        Ok(bytes) if bytes.len() <= MAX_CONFIG_BYTES => match std::str::from_utf8(&bytes) {
            Ok(text) => match parse_config(text) {
                Ok(mode) => mode,
                Err(err) => {
                    eprintln!("ferrion-netd: {CONFIG_PATH}: {err}; using dhcp");
                    LinkMode::Dhcp
                }
            },
            Err(_) => {
                eprintln!("ferrion-netd: {CONFIG_PATH}: invalid UTF-8; using dhcp");
                LinkMode::Dhcp
            }
        },
        Ok(_) => {
            eprintln!("ferrion-netd: {CONFIG_PATH}: exceeds {MAX_CONFIG_BYTES} bytes; using dhcp");
            LinkMode::Dhcp
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => LinkMode::Dhcp,
        Err(err) => {
            eprintln!("ferrion-netd: cannot read {CONFIG_PATH}: {err}; using dhcp");
            LinkMode::Dhcp
        }
    }
}
