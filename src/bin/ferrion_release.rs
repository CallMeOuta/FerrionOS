use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

use ferrion_update::client::{
    ClientState, FetchRequest, Fetcher, check_and_verify, verify_index_bytes, verify_root_bytes,
};
use ferrion_update::crypto::{Blake3Hash, PublicKey};
use ferrion_update::error::FetchError;
use ferrion_update::format::{
    Arch, CalendarDate, Channel, IndexFile, KeysSection, MirrorEntry, PackageEntry, PackageSet,
    ReleaseEntry, ReleaseNumber, RootFile, parse_index_toml, parse_root_toml, render_index_toml,
    render_root_toml, valid_package_name,
};
use ferrion_update::pkgar::verify_pkgar;
use ferrion_update::{
    FORMAT_VERSION, MAX_FIXES_IN_INDEX, MAX_MIRRORS, MAX_OBJECT_BYTES, MAX_PACKAGES_IN_INDEX,
    SIGNATURE_BYTES,
};

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];
const MAX_CLI_ARGS: usize = 512;

const USAGE: &str = "Usage:
  ferrion-release keygen --out-dir <DIR>
  ferrion-release pubkey [--key <PEM_OR_PKCS11_URI>] [--dev-key-dir <DIR>]
  ferrion-release build --repo <DIR> --out <DIR> --release <YYYYMMDDNN> --serial <N> [options]
  ferrion-release sign-root --root <FILE> --out <DIR> --serial <N> [options]
  ferrion-release verify --dir <DIR> --pubkey <ed25519:HEX> --arch <ARCH> --channel <CH> --installed <YYYYMMDDNN> [--today <YYYY-MM-DD>]

Signing key resolution (in order):
  1. --key <PEM_PATH | pkcs11:...>
  2. FERRION_RELEASE_SIGNER environment variable
  3. VERIFIED_BOOT_SIGNER environment variable
  4. --dev-key-dir <DIR> (creates release.key if missing)

Build options:
  --arch <x86_64|aarch64|riscv64gc>    (default: x86_64)
  --channel <CH[=YYYYMMDDNN]>          (repeatable; default: dev=<release>)
  --floor <YYYYMMDDNN>                 (default: <release>)
  --signed <YYYY-MM-DD>                (default: UTC today)
  --fresh-days <N>                     (default: 45)
  --security                           (mark release security = true)
  --via <YYYYMMDDNN>                   (waypoint release)
  --fix <ADVISORY_ID>                  (repeatable)
  --mirror <URL>                       (repeatable)
  --next-key <ed25519:HEX>             (announce next release key)
  --package <NAME>                     (repeatable filter; default: all in repo)
  --system-package <NAME>              (repeatable; marks package set = system)
  --default-set <system|app>           (default: system)
  --previous-root <FILE>               (enforce strict monotonic serial/release and keep history)
  --previous-index <FILE>              (preserve rev for packages with unchanged head)
  --allow-same-release                 (allow signing another arch for the same release number)";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() > MAX_CLI_ARGS {
        eprintln!("ferrion-release: too many arguments (max {MAX_CLI_ARGS})");
        return ExitCode::from(2);
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("ferrion-release: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return Err("missing subcommand".to_string());
    };
    match cmd.as_str() {
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        "keygen" => cmd_keygen(rest),
        "pubkey" => cmd_pubkey(rest),
        "build" => cmd_build(rest),
        "sign-root" => cmd_sign_root(rest),
        "verify" => cmd_verify(rest),
        other => Err(format!("unknown subcommand {other:?}\n{USAGE}")),
    }
}

fn next_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn utc_today() -> Result<CalendarDate, String> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock before epoch: {e}"))?
        .as_secs();
    let days = (secs / 86_400) as i64;
    CalendarDate::from_days_since_epoch(days)
}

fn is_pkcs11_uri(key: &str) -> bool {
    key.starts_with("pkcs11:")
}

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Result<Self, String> {
        let base = env::temp_dir();
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for attempt in 0u32..100 {
            let candidate = base.join(format!("{prefix}-{pid}-{nanos}-{attempt}"));
            match fs::create_dir(&candidate) {
                Ok(()) => return Ok(Self { path: candidate }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("create temp dir {}: {e}", candidate.display())),
            }
        }
        Err("could not allocate temporary directory".to_string())
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct Signer {
    key: String,
    pkcs11_conf: Option<PathBuf>,
    _guard: Option<TempDirGuard>,
}

impl Signer {
    fn resolve(key_arg: Option<String>, dev_key_dir: Option<PathBuf>) -> Result<Self, String> {
        let key = if let Some(k) = key_arg {
            k
        } else if let Ok(v) = env::var("FERRION_RELEASE_SIGNER")
            && !v.is_empty()
        {
            v
        } else if let Ok(v) = env::var("VERIFIED_BOOT_SIGNER")
            && !v.is_empty()
        {
            v
        } else if let Some(dir) = dev_key_dir {
            ensure_dev_key(&dir)?;
            dir.join("release.key").to_string_lossy().into_owned()
        } else {
            return Err(
                "no signing key given; pass --key, --dev-key-dir, or set VERIFIED_BOOT_SIGNER"
                    .to_string(),
            );
        };

        if is_pkcs11_uri(&key) {
            let guard = TempDirGuard::new("ferrion-release-pkcs11")?;
            let conf = write_pkcs11_openssl_conf(&guard.path)?;
            Ok(Self {
                key,
                pkcs11_conf: Some(conf),
                _guard: Some(guard),
            })
        } else {
            if !Path::new(&key).is_file() {
                return Err(format!("signing key file not found: {key}"));
            }
            Ok(Self {
                key,
                pkcs11_conf: None,
                _guard: None,
            })
        }
    }

    fn openssl_cmd(&self) -> Command {
        let mut cmd = Command::new("openssl");
        if let Some(conf) = &self.pkcs11_conf {
            cmd.env("OPENSSL_CONF", conf);
        }
        cmd
    }

    fn public_key(&self) -> Result<PublicKey, String> {
        let output = self
            .openssl_cmd()
            .args(["pkey", "-in", &self.key, "-pubout", "-outform", "DER"])
            .output()
            .map_err(|e| format!("failed to run openssl pkey: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("openssl pkey failed: {}", stderr.trim()));
        }
        let der = output.stdout;
        if der.len() != 44 || der[0..12] != ED25519_SPKI_PREFIX {
            return Err(format!(
                "signing key is not an Ed25519 key (DER length {}, expected 44-byte Ed25519 SPKI)",
                der.len()
            ));
        }
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&der[12..44]);
        Ok(PublicKey(raw))
    }

    fn sign_file(&self, input: &Path, sig_out: &Path) -> Result<Vec<u8>, String> {
        let output = self
            .openssl_cmd()
            .args(["pkeyutl", "-sign", "-rawin", "-inkey", &self.key, "-in"])
            .arg(input)
            .arg("-out")
            .arg(sig_out)
            .output()
            .map_err(|e| format!("failed to run openssl pkeyutl: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("openssl pkeyutl -sign failed: {}", stderr.trim()));
        }
        let sig = fs::read(sig_out)
            .map_err(|e| format!("failed reading signature {}: {e}", sig_out.display()))?;
        if sig.len() != SIGNATURE_BYTES {
            return Err(format!(
                "signature {} has {} bytes, expected {SIGNATURE_BYTES}",
                sig_out.display(),
                sig.len()
            ));
        }
        Ok(sig)
    }
}

fn write_pkcs11_openssl_conf(dir: &Path) -> Result<PathBuf, String> {
    let modules_dir = if let Ok(v) = env::var("OPENSSL_MODULES")
        && !v.is_empty()
    {
        PathBuf::from(v)
    } else {
        let out = Command::new("openssl")
            .args(["version", "-m"])
            .output()
            .map_err(|e| format!("failed to run openssl version -m: {e}"))?;
        if !out.status.success() {
            return Err("openssl version -m failed".to_string());
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let trimmed = text.trim();
        let inner = trimmed
            .strip_prefix("MODULESDIR: \"")
            .and_then(|s| s.strip_suffix('"'))
            .ok_or_else(|| format!("unexpected openssl version -m output: {trimmed}"))?;
        PathBuf::from(inner)
    };
    let provider = modules_dir.join("pkcs11.so");
    if !provider.is_file() {
        return Err(format!(
            "OpenSSL pkcs11 provider not found at {}; install pkcs11-provider or set OPENSSL_MODULES",
            provider.display()
        ));
    }
    let conf_path = dir.join("openssl-pkcs11.cnf");
    let conf = format!(
        "openssl_conf = openssl_init\n\
         [openssl_init]\n\
         providers = provider_sect\n\
         [provider_sect]\n\
         default = default_sect\n\
         pkcs11 = pkcs11_sect\n\
         [default_sect]\n\
         activate = 1\n\
         [pkcs11_sect]\n\
         module = {}\n\
         pkcs11-module-block-operations = digest\n\
         pkcs11-module-quirks = no-deinit\n\
         activate = 1\n",
        provider.display()
    );
    fs::write(&conf_path, conf).map_err(|e| format!("writing {}: {e}", conf_path.display()))?;
    Ok(conf_path)
}

fn ensure_dev_key(dir: &Path) -> Result<PublicKey, String> {
    fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let key_path = dir.join("release.key");
    let pub_path = dir.join("release.pub");
    if !key_path.is_file() {
        let out = Command::new("openssl")
            .args(["genpkey", "-algorithm", "ed25519"])
            .output()
            .map_err(|e| format!("failed to run openssl genpkey: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!("openssl genpkey failed: {}", stderr.trim()));
        }
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&key_path)
            .map_err(|e| format!("creating {}: {e}", key_path.display()))?;
        f.write_all(&out.stdout)
            .map_err(|e| format!("writing {}: {e}", key_path.display()))?;
    }
    let signer = Signer::resolve(Some(key_path.to_string_lossy().into_owned()), None)?;
    let pubkey = signer.public_key()?;
    fs::write(&pub_path, pubkey.0).map_err(|e| format!("writing {}: {e}", pub_path.display()))?;
    Ok(pubkey)
}

fn cmd_keygen(args: &[String]) -> Result<(), String> {
    let mut out_dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out-dir" => out_dir = Some(PathBuf::from(next_value(args, &mut i, "--out-dir")?)),
            other => return Err(format!("unknown flag for keygen: {other}")),
        }
        i += 1;
    }
    let dir = out_dir.ok_or("keygen requires --out-dir <DIR>")?;
    let pubkey = ensure_dev_key(&dir)?;
    println!("{pubkey}");
    Ok(())
}

fn cmd_pubkey(args: &[String]) -> Result<(), String> {
    let mut key: Option<String> = None;
    let mut dev_key_dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--key" => key = Some(next_value(args, &mut i, "--key")?),
            "--dev-key-dir" => {
                dev_key_dir = Some(PathBuf::from(next_value(args, &mut i, "--dev-key-dir")?))
            }
            other => return Err(format!("unknown flag for pubkey: {other}")),
        }
        i += 1;
    }
    let signer = Signer::resolve(key, dev_key_dir)?;
    let pubkey = signer.public_key()?;
    println!("{pubkey}");
    Ok(())
}

fn parse_channel_spec(
    spec: &str,
    default_release: ReleaseNumber,
) -> Result<(Channel, ReleaseNumber), String> {
    if let Some((ch, rel)) = spec.split_once('=') {
        let channel = ch.parse::<Channel>()?;
        let release = rel.parse::<ReleaseNumber>()?;
        Ok((channel, release))
    } else {
        let channel = spec.parse::<Channel>()?;
        Ok((channel, default_release))
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("creating dir {}: {e}", parent.display()))?;
    }
    let tmp = path.with_added_extension("tmp");
    fs::write(&tmp, bytes).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path)
        .map_err(|e| format!("renaming {} to {}: {e}", tmp.display(), path.display()))?;
    Ok(())
}

fn resolve_repo_target_dir(repo: &Path, arch: Arch) -> PathBuf {
    let candidate = repo.join(arch.target_triple());
    if candidate.is_dir() {
        candidate
    } else {
        repo.to_path_buf()
    }
}

fn cmd_build(args: &[String]) -> Result<(), String> {
    let mut repo_dir: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut arch = Arch::X86_64;
    let mut release: Option<ReleaseNumber> = None;
    let mut serial: Option<u64> = None;
    let mut floor: Option<ReleaseNumber> = None;
    let mut signed: Option<CalendarDate> = None;
    let mut fresh_days: u32 = 45;
    let mut security = false;
    let mut via: Option<ReleaseNumber> = None;
    let mut fixes: Vec<String> = Vec::new();
    let mut mirrors: Vec<MirrorEntry> = Vec::new();
    let mut next_key: Option<PublicKey> = None;
    let mut channel_specs: Vec<String> = Vec::new();
    let mut package_filter: BTreeSet<String> = BTreeSet::new();
    let mut system_packages: BTreeSet<String> = BTreeSet::new();
    let mut default_set = PackageSet::System;
    let mut previous_root_path: Option<PathBuf> = None;
    let mut previous_index_path: Option<PathBuf> = None;
    let mut key: Option<String> = None;
    let mut dev_key_dir: Option<PathBuf> = None;
    let mut allow_same_release = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repo" => repo_dir = Some(PathBuf::from(next_value(args, &mut i, "--repo")?)),
            "--out" => out_dir = Some(PathBuf::from(next_value(args, &mut i, "--out")?)),
            "--arch" => arch = next_value(args, &mut i, "--arch")?.parse::<Arch>()?,
            "--release" => {
                release = Some(next_value(args, &mut i, "--release")?.parse::<ReleaseNumber>()?)
            }
            "--serial" => {
                let raw = next_value(args, &mut i, "--serial")?;
                serial = Some(
                    raw.parse::<u64>()
                        .map_err(|_| format!("invalid --serial {raw:?}"))?,
                );
            }
            "--floor" => {
                floor = Some(next_value(args, &mut i, "--floor")?.parse::<ReleaseNumber>()?)
            }
            "--signed" => {
                signed = Some(next_value(args, &mut i, "--signed")?.parse::<CalendarDate>()?)
            }
            "--fresh-days" => {
                let raw = next_value(args, &mut i, "--fresh-days")?;
                fresh_days = raw
                    .parse::<u32>()
                    .map_err(|_| format!("invalid --fresh-days {raw:?}"))?;
            }
            "--security" => security = true,
            "--via" => via = Some(next_value(args, &mut i, "--via")?.parse::<ReleaseNumber>()?),
            "--fix" => fixes.push(next_value(args, &mut i, "--fix")?),
            "--mirror" => mirrors.push(MirrorEntry {
                url: next_value(args, &mut i, "--mirror")?,
            }),
            "--next-key" => {
                next_key = Some(next_value(args, &mut i, "--next-key")?.parse::<PublicKey>()?)
            }
            "--channel" => channel_specs.push(next_value(args, &mut i, "--channel")?),
            "--package" => {
                let val = next_value(args, &mut i, "--package")?;
                for name in val.split(',').filter(|s| !s.is_empty()) {
                    package_filter.insert(name.to_string());
                }
            }
            "--system-package" => {
                let val = next_value(args, &mut i, "--system-package")?;
                for name in val.split(',').filter(|s| !s.is_empty()) {
                    system_packages.insert(name.to_string());
                }
            }
            "--default-set" => {
                default_set = match next_value(args, &mut i, "--default-set")?.as_str() {
                    "system" => PackageSet::System,
                    "app" => PackageSet::App,
                    other => return Err(format!("unknown --default-set {other:?}")),
                };
            }
            "--previous-root" => {
                previous_root_path =
                    Some(PathBuf::from(next_value(args, &mut i, "--previous-root")?))
            }
            "--previous-index" => {
                previous_index_path =
                    Some(PathBuf::from(next_value(args, &mut i, "--previous-index")?))
            }
            "--key" => key = Some(next_value(args, &mut i, "--key")?),
            "--dev-key-dir" => {
                dev_key_dir = Some(PathBuf::from(next_value(args, &mut i, "--dev-key-dir")?))
            }
            "--allow-same-release" => allow_same_release = true,
            other => return Err(format!("unknown flag for build: {other}")),
        }
        i += 1;
    }

    let repo_dir = repo_dir.ok_or("build requires --repo <DIR>")?;
    let out_dir = out_dir.ok_or("build requires --out <DIR>")?;
    let release = release.ok_or("build requires --release <YYYYMMDDNN>")?;
    let serial = serial.ok_or("build requires --serial <N>")?;
    if serial == 0 {
        return Err("--serial must be >= 1".to_string());
    }
    let floor = floor.unwrap_or(release);
    if floor > release {
        return Err(format!("--floor {floor} exceeds --release {release}"));
    }
    if let Some(v) = via {
        if v >= release {
            return Err(format!("--via {v} must be below --release {release}"));
        }
    }
    if fixes.len() > MAX_FIXES_IN_INDEX {
        return Err(format!("too many --fix values (max {MAX_FIXES_IN_INDEX})"));
    }
    if mirrors.len() > MAX_MIRRORS {
        return Err(format!("too many --mirror values (max {MAX_MIRRORS})"));
    }
    let signed = match signed {
        Some(d) => d,
        None => utc_today()?,
    };

    let signer = Signer::resolve(key, dev_key_dir)?;
    let pubkey = signer.public_key()?;

    let prev_root = match previous_root_path {
        Some(path) => {
            let bytes = fs::read(&path)
                .map_err(|e| format!("reading previous root {}: {e}", path.display()))?;
            let parsed = parse_root_toml(&bytes, None)
                .map_err(|e| format!("parsing previous root {}: {e}", path.display()))?;
            if serial <= parsed.serial {
                return Err(format!(
                    "refusing --serial {serial}: must be strictly greater than previous serial {}",
                    parsed.serial
                ));
            }
            if let Some(&max_rel) = parsed.releases.keys().next_back() {
                if !allow_same_release && release <= max_rel {
                    return Err(format!(
                        "refusing --release {release}: must be strictly greater than previous release {max_rel}"
                    ));
                }
            }
            Some(parsed)
        }
        None => None,
    };

    let prev_pkg_revs: BTreeMap<String, (Blake3Hash, ReleaseNumber)> = match previous_index_path {
        Some(path) => {
            let bytes = fs::read(&path)
                .map_err(|e| format!("reading previous index {}: {e}", path.display()))?;
            let parsed = parse_index_toml(&bytes, None)
                .map_err(|e| format!("parsing previous index {}: {e}", path.display()))?;
            let mut map = BTreeMap::new();
            for p in parsed.package {
                map.insert(p.name, (p.head, p.rev));
            }
            map
        }
        None => BTreeMap::new(),
    };

    let target_dir = resolve_repo_target_dir(&repo_dir, arch);
    if !target_dir.is_dir() {
        return Err(format!(
            "repo directory not found: {}",
            target_dir.display()
        ));
    }

    let mut pkg_names: Vec<String> = Vec::new();
    for entry in
        fs::read_dir(&target_dir).map_err(|e| format!("listing {}: {e}", target_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("reading dir entry: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("pkgar") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !package_filter.is_empty() && !package_filter.contains(stem) {
            continue;
        }
        if !valid_package_name(stem) {
            return Err(format!(
                "invalid package name {stem:?} in {}",
                target_dir.display()
            ));
        }
        pkg_names.push(stem.to_string());
        if pkg_names.len() > MAX_PACKAGES_IN_INDEX {
            return Err(format!(
                "repository exceeds maximum of {MAX_PACKAGES_IN_INDEX} packages"
            ));
        }
    }
    pkg_names.sort();

    if !package_filter.is_empty() {
        for wanted in &package_filter {
            if !pkg_names.iter().any(|n| n == wanted) {
                return Err(format!(
                    "requested --package {wanted:?} not found in {}",
                    target_dir.display()
                ));
            }
        }
    }
    if pkg_names.is_empty() {
        return Err(format!(
            "no .pkgar packages found in {}",
            target_dir.display()
        ));
    }

    let mut packages = Vec::with_capacity(pkg_names.len());
    let mut system_rev: Option<ReleaseNumber> = None;

    for name in &pkg_names {
        let pkgar_path = target_dir.join(format!("{name}.pkgar"));
        let meta_path = target_dir.join(format!("{name}.toml"));
        let bytes =
            fs::read(&pkgar_path).map_err(|e| format!("reading {}: {e}", pkgar_path.display()))?;
        let size = bytes.len() as u64;
        if size == 0 || size > MAX_OBJECT_BYTES {
            return Err(format!(
                "package {name} size {size} out of bounds 1..={MAX_OBJECT_BYTES}"
            ));
        }
        let head_info = ferrion_update::pkgar::inspect_pkgar_head(name, &bytes)
            .map_err(|e| format!("inspecting {}: {e}", pkgar_path.display()))?;
        let _ = verify_pkgar(
            name,
            &bytes,
            &head_info.head_hash,
            Some(&head_info.public_key),
        )
        .map_err(|e| format!("verifying {}: {e}", pkgar_path.display()))?;

        let mut needs: Vec<String> = Vec::new();
        if meta_path.is_file() {
            let meta_text = fs::read_to_string(&meta_path)
                .map_err(|e| format!("reading {}: {e}", meta_path.display()))?;
            let meta: toml::Table = toml::from_str(&meta_text)
                .map_err(|e| format!("parsing {}: {e}", meta_path.display()))?;
            if let Some(meta_b3) = meta.get("blake3").and_then(|v| v.as_str())
                && !meta_b3.is_empty()
            {
                let expected = Blake3Hash::from_hex(meta_b3)
                    .map_err(|e| format!("{}: invalid blake3: {e}", meta_path.display()))?;
                if expected != head_info.head_hash {
                    return Err(format!(
                        "{}: blake3 {expected} disagrees with pkgar header {}",
                        meta_path.display(),
                        head_info.head_hash
                    ));
                }
            }
            if let Some(deps) = meta.get("depends").and_then(|v| v.as_array()) {
                for dep in deps {
                    if let Some(s) = dep.as_str()
                        && valid_package_name(s)
                    {
                        needs.push(s.to_string());
                    }
                }
                needs.sort();
                needs.dedup();
            }
        }

        let object_hash = Blake3Hash::hash_bytes(&bytes);
        let obj_dst = out_dir
            .join("objects")
            .join(object_hash.prefix_two())
            .join(object_hash.to_hex());
        if !obj_dst.is_file() {
            write_atomic(&obj_dst, &bytes)?;
        }

        let set = if system_packages.contains(name) {
            PackageSet::System
        } else {
            default_set
        };
        let rev = match prev_pkg_revs.get(name) {
            Some((prev_head, prev_rev)) if *prev_head == head_info.head_hash => *prev_rev,
            _ => release,
        };
        if set == PackageSet::System {
            system_rev = Some(match system_rev {
                Some(cur) if cur >= rev => cur,
                _ => rev,
            });
        }

        packages.push(PackageEntry {
            name: name.clone(),
            set,
            rev,
            head: head_info.head_hash,
            object: object_hash,
            size,
            needs,
            patch: Vec::new(),
        });
    }

    let index = IndexFile {
        format: FORMAT_VERSION,
        release,
        system: system_rev.unwrap_or(release),
        fixes,
        package: packages,
    };
    let index_text = render_index_toml(&index);
    let index_bytes = index_text.as_bytes();
    let index_hash = Blake3Hash::hash_bytes(index_bytes);

    let snap_dir = out_dir
        .join("snap")
        .join(release.to_string())
        .join(arch.as_str());
    let index_path = snap_dir.join("index.toml");
    let index_sig_path = snap_dir.join("index.toml.sig");
    write_atomic(&index_path, index_bytes)?;
    let index_sig = signer.sign_file(&index_path, &index_sig_path)?;
    verify_index_bytes(index_bytes, &index_sig, &index_hash, &pubkey, release, via)
        .map_err(|e| format!("self-verification of index.toml failed: {e}"))?;

    let mut channels = prev_root
        .as_ref()
        .map(|r| r.channels.clone())
        .unwrap_or_default();
    if channel_specs.is_empty() {
        channels.set(Channel::Dev, release);
    } else {
        for spec in &channel_specs {
            let (ch, rel) = parse_channel_spec(spec, release)?;
            channels.set(ch, rel);
        }
    }

    let mut releases = prev_root
        .as_ref()
        .map(|r| r.releases.clone())
        .unwrap_or_default();
    let entry = releases.entry(release).or_insert_with(|| ReleaseEntry {
        index: BTreeMap::new(),
        floor,
        security,
        via,
        withdrawn: false,
        rollout: Vec::new(),
    });
    entry.index.insert(arch, index_hash);
    entry.floor = floor;
    entry.security = security;
    entry.via = via;

    let keys = match next_key {
        Some(nk) => Some(KeysSection { next: Some(nk) }),
        None => prev_root.as_ref().and_then(|r| r.keys.clone()),
    };
    let final_mirrors = if !mirrors.is_empty() {
        mirrors
    } else {
        prev_root
            .as_ref()
            .map(|r| r.mirrors.clone())
            .unwrap_or_default()
    };

    let root = RootFile {
        format: FORMAT_VERSION,
        serial,
        signed,
        fresh_days,
        channels,
        releases,
        keys,
        mirrors: final_mirrors,
    };
    let root_text = render_root_toml(&root);
    let root_bytes = root_text.as_bytes();
    let root_path = out_dir.join("root.toml");
    let root_sig_path = out_dir.join("root.toml.sig");
    write_atomic(&root_path, root_bytes)?;
    let root_sig = signer.sign_file(&root_path, &root_sig_path)?;
    verify_root_bytes(root_bytes, &root_sig, &pubkey, via)
        .map_err(|e| format!("self-verification of root.toml failed: {e}"))?;

    println!(
        "ferrion-release: signed release {release} (serial {serial}, arch {arch}, {} packages, key {pubkey})",
        index.package.len()
    );
    Ok(())
}

fn cmd_sign_root(args: &[String]) -> Result<(), String> {
    let mut root_in: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut serial: Option<u64> = None;
    let mut signed: Option<CalendarDate> = None;
    let mut fresh_days: Option<u32> = None;
    let mut channel_specs: Vec<String> = Vec::new();
    let mut withdraw: Vec<ReleaseNumber> = Vec::new();
    let mut next_key: Option<PublicKey> = None;
    let mut key: Option<String> = None;
    let mut dev_key_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => root_in = Some(PathBuf::from(next_value(args, &mut i, "--root")?)),
            "--out" => out_dir = Some(PathBuf::from(next_value(args, &mut i, "--out")?)),
            "--serial" => {
                let raw = next_value(args, &mut i, "--serial")?;
                serial = Some(
                    raw.parse::<u64>()
                        .map_err(|_| format!("invalid --serial {raw:?}"))?,
                );
            }
            "--signed" => {
                signed = Some(next_value(args, &mut i, "--signed")?.parse::<CalendarDate>()?)
            }
            "--fresh-days" => {
                let raw = next_value(args, &mut i, "--fresh-days")?;
                fresh_days = Some(
                    raw.parse::<u32>()
                        .map_err(|_| format!("invalid --fresh-days {raw:?}"))?,
                );
            }
            "--channel" => channel_specs.push(next_value(args, &mut i, "--channel")?),
            "--withdraw" => {
                withdraw.push(next_value(args, &mut i, "--withdraw")?.parse::<ReleaseNumber>()?)
            }
            "--next-key" => {
                next_key = Some(next_value(args, &mut i, "--next-key")?.parse::<PublicKey>()?)
            }
            "--key" => key = Some(next_value(args, &mut i, "--key")?),
            "--dev-key-dir" => {
                dev_key_dir = Some(PathBuf::from(next_value(args, &mut i, "--dev-key-dir")?))
            }
            other => return Err(format!("unknown flag for sign-root: {other}")),
        }
        i += 1;
    }

    let root_in = root_in.ok_or("sign-root requires --root <FILE>")?;
    let out_dir = out_dir.ok_or("sign-root requires --out <DIR>")?;
    let serial = serial.ok_or("sign-root requires --serial <N>")?;
    let bytes = fs::read(&root_in).map_err(|e| format!("reading {}: {e}", root_in.display()))?;
    let mut root =
        parse_root_toml(&bytes, None).map_err(|e| format!("parsing {}: {e}", root_in.display()))?;
    if serial <= root.serial {
        return Err(format!(
            "refusing --serial {serial}: must be strictly greater than existing serial {}",
            root.serial
        ));
    }
    root.serial = serial;
    root.signed = match signed {
        Some(d) => d,
        None => utc_today()?,
    };
    if let Some(fd) = fresh_days {
        root.fresh_days = fd;
    }
    for spec in &channel_specs {
        let (ch, rel_str) = spec
            .split_once('=')
            .ok_or_else(|| format!("sign-root --channel requires CH=YYYYMMDDNN, got {spec:?}"))?;
        let channel = ch.parse::<Channel>()?;
        let release = rel_str.parse::<ReleaseNumber>()?;
        if !root.releases.contains_key(&release) {
            return Err(format!(
                "channel {channel} target {release} is not in [releases]"
            ));
        }
        root.channels.set(channel, release);
    }
    for rel in withdraw {
        let Some(entry) = root.releases.get_mut(&rel) else {
            return Err(format!("cannot withdraw {rel}: not in [releases]"));
        };
        entry.withdrawn = true;
    }
    if let Some(nk) = next_key {
        root.keys = Some(KeysSection { next: Some(nk) });
    }

    let signer = Signer::resolve(key, dev_key_dir)?;
    let pubkey = signer.public_key()?;

    let root_text = render_root_toml(&root);
    let root_bytes = root_text.as_bytes();
    let root_path = out_dir.join("root.toml");
    let root_sig_path = out_dir.join("root.toml.sig");
    write_atomic(&root_path, root_bytes)?;
    let root_sig = signer.sign_file(&root_path, &root_sig_path)?;
    verify_root_bytes(root_bytes, &root_sig, &pubkey, None)
        .map_err(|e| format!("self-verification of root.toml failed: {e}"))?;

    println!("ferrion-release: signed root.toml serial {serial} with {pubkey}");
    Ok(())
}

struct LocalDirFetcher {
    root: PathBuf,
}

impl Fetcher for LocalDirFetcher {
    fn fetch(&mut self, request: &FetchRequest) -> Result<Vec<u8>, FetchError> {
        let rel = request
            .path
            .strip_prefix('/')
            .ok_or_else(|| FetchError::Transport("path missing leading /".to_string()))?;
        let path = self.root.join(rel);
        let meta = fs::metadata(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FetchError::NotFound(request.path.clone())
            } else {
                FetchError::Transport(e.to_string())
            }
        })?;
        if meta.len() > request.max_bytes {
            return Err(FetchError::SizeLimitExceeded {
                path: request.path.clone(),
                limit: request.max_bytes,
            });
        }
        fs::read(&path).map_err(|e| FetchError::Transport(e.to_string()))
    }
}

fn cmd_verify(args: &[String]) -> Result<(), String> {
    let mut dir: Option<PathBuf> = None;
    let mut pubkey: Option<PublicKey> = None;
    let mut arch = Arch::X86_64;
    let mut channel = Channel::Dev;
    let mut installed: Option<ReleaseNumber> = None;
    let mut today: Option<CalendarDate> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => dir = Some(PathBuf::from(next_value(args, &mut i, "--dir")?)),
            "--pubkey" => {
                pubkey = Some(next_value(args, &mut i, "--pubkey")?.parse::<PublicKey>()?)
            }
            "--arch" => arch = next_value(args, &mut i, "--arch")?.parse::<Arch>()?,
            "--channel" => channel = next_value(args, &mut i, "--channel")?.parse::<Channel>()?,
            "--installed" => {
                installed = Some(next_value(args, &mut i, "--installed")?.parse::<ReleaseNumber>()?)
            }
            "--today" => {
                today = Some(next_value(args, &mut i, "--today")?.parse::<CalendarDate>()?)
            }
            other => return Err(format!("unknown flag for verify: {other}")),
        }
        i += 1;
    }

    let dir = dir.ok_or("verify requires --dir <DIR>")?;
    let pubkey = pubkey.ok_or("verify requires --pubkey <ed25519:HEX>")?;
    let installed = installed.ok_or("verify requires --installed <YYYYMMDDNN>")?;
    let today = match today {
        Some(d) => d,
        None => utc_today()?,
    };

    let state = ClientState {
        release_key: pubkey,
        emergency_key: None,
        arch,
        channel,
        installed_release: installed,
        trusted_serial: 0,
        last_waypoint: None,
        installed_packages: BTreeMap::new(),
    };
    let mut fetcher = LocalDirFetcher { root: dir };
    let outcome = check_and_verify(&state, today, &mut fetcher, true)
        .map_err(|e| format!("verification failed [{}]: {e}", e.reason_code()))?;
    if let Some(notice) = &outcome.stale_notice {
        println!(
            "notice: root.toml signed {} is {} days old (fresh_days {})",
            notice.signed, notice.age_days, notice.fresh_days
        );
    }
    println!(
        "verified release {} (serial {}, {} packages)",
        outcome.target_release,
        outcome.root.serial,
        outcome.verified_packages.len()
    );
    Ok(())
}
