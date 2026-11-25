use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use serde::Deserialize;

use crate::{
    Result, bail_other_err,
    cook::{
        fs as cook_fs,
        pty::{PtyOut, log_to_pty},
    },
    wrap_io_err,
};

pub const MAX_CRATE_PATCHES_PER_RECIPE: usize = 16;
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_MANIFEST_VARIANTS: usize = 64;
pub const MAX_PATCH_BYTES: usize = 256 * 1024;
pub const MAX_LOCK_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_LOCK_VERSIONS_PER_CRATE: usize = 16;
pub const MAX_COPY_DEPTH: usize = 32;
pub const MAX_COPY_ENTRIES: usize = 8192;
pub const MAX_OVERLAY_REGISTRY_ENTRIES: usize = 65536;

const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const CARGO_OK_CONTENT: &str = "{\"v\":1}";

#[derive(Clone, Debug, Eq, PartialEq)]
struct SemVer {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Option<String>,
}

impl SemVer {
    fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            bail_other_err!("Empty crate version string");
        }
        let (core, pre) = match trimmed.split_once('-') {
            Some((c, p)) => {
                if p.is_empty() {
                    bail_other_err!("Malformed prerelease in version {:?}", raw);
                }
                (c, Some(p.to_string()))
            }
            None => (trimmed, None),
        };
        let mut parts = core.split('.');
        let major = parse_num(parts.next(), raw)?;
        let minor = parse_num(parts.next(), raw)?;
        let patch = parse_num(parts.next(), raw)?;
        if parts.next().is_some() {
            bail_other_err!("Malformed version {:?}", raw);
        }
        Ok(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    fn core_tuple(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
}

fn parse_num(part: Option<&str>, full: &str) -> Result<u64> {
    let Some(s) = part else {
        bail_other_err!("Malformed version {:?}", full);
    };
    s.parse::<u64>()
        .map_err(|_| crate::Error::Other(format!("Malformed version component in {:?}", full)))
}

#[derive(Debug, Deserialize)]
struct PatchManifest {
    #[serde(rename = "crate")]
    crate_name: String,
    #[serde(default)]
    #[allow(dead_code)]
    description: Option<String>,
    #[serde(default, rename = "variant")]
    variants: Vec<PatchVariant>,
}

#[derive(Debug, Deserialize)]
struct PatchVariant {
    min: String,
    max: String,
    #[serde(default)]
    rev: Option<String>,
    patch: String,
    blake3: String,
}

#[derive(Debug, Deserialize)]
struct CargoLock {
    #[serde(default, rename = "package")]
    packages: Vec<CargoLockPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoLockPackage {
    name: String,
    version: String,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CrateCargoToml {
    package: Option<CrateCargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CrateCargoPackage {
    name: Option<String>,
    version: Option<String>,
}

fn validate_safe_name(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail_other_err!("Invalid {label} name {:?}", value);
    }
    Ok(())
}

fn validate_safe_filename(value: &str, label: &str) -> Result<()> {
    if value.is_empty() {
        bail_other_err!("Empty {label} filename");
    }
    let path = Path::new(value);
    let mut comps = path.components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => bail_other_err!("Invalid {label} filename {:?}", value),
    }
}

fn validate_blake3_hex(hex: &str, context: &Path) -> Result<()> {
    if hex.len() != blake3::OUT_LEN * 2
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        bail_other_err!(
            "Invalid blake3 hex {:?} in manifest '{}'",
            hex,
            context.display()
        );
    }
    Ok(())
}

fn read_bounded_bytes(path: &Path, max_bytes: usize, label: &'static str) -> Result<Vec<u8>> {
    let file = fs::File::open(path).map_err(wrap_io_err!(path, label))?;
    let mut buf = Vec::new();
    let limit = (max_bytes as u64).saturating_add(1);
    file.take(limit)
        .read_to_end(&mut buf)
        .map_err(wrap_io_err!(path, label))?;
    if buf.len() > max_bytes {
        bail_other_err!(
            "{label} '{}' exceeds maximum size of {} bytes",
            path.display(),
            max_bytes
        );
    }
    Ok(buf)
}

fn read_bounded_string(path: &Path, max_bytes: usize, label: &'static str) -> Result<String> {
    let bytes = read_bounded_bytes(path, max_bytes, label)?;
    String::from_utf8(bytes)
        .map_err(|err| crate::Error::Other(format!("Invalid UTF-8 in '{}': {err}", path.display())))
}

fn load_manifest(cookbook_root: &Path, crate_name: &str) -> Result<(PathBuf, PatchManifest)> {
    validate_safe_name(crate_name, "crate")?;
    let crate_patch_dir = cookbook_root.join("patches/crates").join(crate_name);
    let manifest_path = crate_patch_dir.join("manifest.toml");
    let raw = read_bounded_string(
        &manifest_path,
        MAX_MANIFEST_BYTES,
        "Reading crate patch manifest",
    )?;
    let manifest: PatchManifest = toml::from_str(&raw).map_err(|err| {
        crate::Error::Other(format!(
            "Parsing crate patch manifest '{}' failed: {err}",
            manifest_path.display()
        ))
    })?;
    if manifest.crate_name != crate_name {
        bail_other_err!(
            "Crate patch manifest '{}' declares crate {:?}, expected {:?}",
            manifest_path.display(),
            manifest.crate_name,
            crate_name
        );
    }
    if manifest.variants.is_empty() {
        bail_other_err!(
            "Crate patch manifest '{}' defines no variants",
            manifest_path.display()
        );
    }
    if manifest.variants.len() > MAX_MANIFEST_VARIANTS {
        bail_other_err!(
            "Crate patch manifest '{}' has {} variants (max {})",
            manifest_path.display(),
            manifest.variants.len(),
            MAX_MANIFEST_VARIANTS
        );
    }
    for variant in &manifest.variants {
        let min = SemVer::parse(&variant.min)?;
        let max = SemVer::parse(&variant.max)?;
        if min.pre.is_some() || max.pre.is_some() {
            if min != max {
                bail_other_err!(
                    "Prerelease variant in '{}' must pin exact version (min={:?}, max={:?})",
                    manifest_path.display(),
                    variant.min,
                    variant.max
                );
            }
        } else if min.core_tuple() > max.core_tuple() {
            bail_other_err!(
                "Invalid variant range in '{}': min {:?} > max {:?}",
                manifest_path.display(),
                variant.min,
                variant.max
            );
        }
        validate_safe_filename(&variant.patch, "patch")?;
        validate_blake3_hex(&variant.blake3, &manifest_path)?;
    }
    Ok((crate_patch_dir, manifest))
}

fn variant_matches(variant: &PatchVariant, version: &SemVer, rev: Option<&str>) -> Result<bool> {
    if variant.rev.as_deref() != rev {
        return Ok(false);
    }
    let min = SemVer::parse(&variant.min)?;
    let max = SemVer::parse(&variant.max)?;
    if version.pre.is_some() || min.pre.is_some() || max.pre.is_some() {
        Ok(*version == min && *version == max)
    } else {
        Ok(version.core_tuple() >= min.core_tuple() && version.core_tuple() <= max.core_tuple())
    }
}

fn describe_variants(manifest: &PatchManifest) -> String {
    let mut parts = Vec::with_capacity(manifest.variants.len());
    for v in &manifest.variants {
        match &v.rev {
            Some(r) => parts.push(format!("{}..={} (rev {})", v.min, v.max, r)),
            None => parts.push(format!("{}..={}", v.min, v.max)),
        }
    }
    parts.join(", ")
}

fn select_verified_patch(
    cookbook_root: &Path,
    crate_name: &str,
    version_str: &str,
    rev: Option<&str>,
) -> Result<(PathBuf, Vec<u8>)> {
    let (crate_patch_dir, manifest) = load_manifest(cookbook_root, crate_name)?;
    let version = SemVer::parse(version_str)?;
    let mut matched: Option<&PatchVariant> = None;
    for variant in &manifest.variants {
        if variant_matches(variant, &version, rev)? {
            if matched.is_some() {
                bail_other_err!(
                    "Ambiguous crate patch variants for {} {} (rev {:?}) in '{}'",
                    crate_name,
                    version_str,
                    rev,
                    crate_patch_dir.join("manifest.toml").display()
                );
            }
            matched = Some(variant);
        }
    }
    let Some(variant) = matched else {
        bail_other_err!(
            "No crate patch variant for {} {} (rev {:?}); covered: [{}]",
            crate_name,
            version_str,
            rev,
            describe_variants(&manifest)
        );
    };
    let patch_path = crate_patch_dir.join(&variant.patch);
    let patch_bytes = read_bounded_bytes(&patch_path, MAX_PATCH_BYTES, "Reading crate patch")?;
    let actual_hash = blake3::hash(&patch_bytes).to_hex().to_string();
    if actual_hash != variant.blake3 {
        bail_other_err!(
            "Crate patch '{}' blake3 mismatch: expected {}, got {}",
            patch_path.display(),
            variant.blake3,
            actual_hash
        );
    }
    Ok((patch_path, patch_bytes))
}

fn read_crate_version_from_dir(crate_dir: &Path, expected_crate: &str) -> Result<String> {
    let cargo_toml_path = crate_dir.join("Cargo.toml");
    let raw = read_bounded_string(
        &cargo_toml_path,
        MAX_MANIFEST_BYTES,
        "Reading crate Cargo.toml",
    )?;
    let parsed: CrateCargoToml = toml::from_str(&raw).map_err(|err| {
        crate::Error::Other(format!(
            "Parsing '{}' failed: {err}",
            cargo_toml_path.display()
        ))
    })?;
    let Some(pkg) = parsed.package else {
        bail_other_err!(
            "Missing [package] section in '{}'",
            cargo_toml_path.display()
        );
    };
    if let Some(name) = pkg.name.as_deref()
        && name != expected_crate
    {
        bail_other_err!(
            "Crate directory '{}' has package name {:?}, expected {:?}",
            crate_dir.display(),
            name,
            expected_crate
        );
    }
    pkg.version.ok_or_else(|| {
        crate::Error::Other(format!(
            "Missing [package].version in '{}'",
            cargo_toml_path.display()
        ))
    })
}

pub fn apply_crate_patch(
    cookbook_root: &Path,
    crate_name: &str,
    version_override: Option<&str>,
    rev: Option<&str>,
    crate_dir: &Path,
    logger: &PtyOut,
) -> Result<()> {
    if !crate_dir.is_dir() {
        bail_other_err!("Crate directory '{}' does not exist", crate_dir.display());
    }
    let version = match version_override {
        Some(v) => v.to_string(),
        None => read_crate_version_from_dir(crate_dir, crate_name)?,
    };
    let (patch_path, patch_bytes) =
        select_verified_patch(cookbook_root, crate_name, &version, rev)?;
    log_to_pty!(
        logger,
        "DEBUG: applying crate patch {} to {} ({} {})",
        patch_path.display(),
        crate_dir.display(),
        crate_name,
        version
    );
    let mut command = Command::new("patch");
    command.arg("-d").arg(crate_dir);
    command.arg("-p1");
    command.arg("-F0");
    command.arg("-N");
    command.arg("-t");
    cook_fs::run_command_stdin(command, &patch_bytes, logger)
}

fn find_cargo_lock(source_dir: &Path, cargopath: Option<&str>) -> Result<PathBuf> {
    if let Some(rel) = cargopath {
        let candidate = source_dir.join(rel).join("Cargo.lock");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    let root_lock = source_dir.join("Cargo.lock");
    if root_lock.is_file() {
        return Ok(root_lock);
    }
    bail_other_err!(
        "Cargo.lock not found under '{}' for crate_patches",
        source_dir.display()
    );
}

fn parse_cargo_lock(lock_path: &Path) -> Result<CargoLock> {
    let raw = read_bounded_string(lock_path, MAX_LOCK_BYTES, "Reading Cargo.lock")?;
    toml::from_str(&raw).map_err(|err| {
        crate::Error::Other(format!(
            "Parsing Cargo.lock '{}' failed: {err}",
            lock_path.display()
        ))
    })
}

pub fn resolve_shared_cargo_home() -> Result<PathBuf> {
    if let Some(val) = env::var_os("CARGO_HOME")
        && !val.is_empty()
    {
        return Ok(PathBuf::from(val));
    }
    if let Some(home) = env::var_os("HOME")
        && !home.is_empty()
    {
        return Ok(PathBuf::from(home).join(".cargo"));
    }
    bail_other_err!("Cannot determine shared CARGO_HOME (neither CARGO_HOME nor HOME is set)");
}

fn list_registry_index_dirs(shared_cargo_home: &Path) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    for sub in ["registry/src", "registry/cache"] {
        let parent = shared_cargo_home.join(sub);
        if !parent.is_dir() {
            continue;
        }
        for entry_res in
            fs::read_dir(&parent).map_err(wrap_io_err!(&parent, "Reading cargo registry dir"))?
        {
            let entry = entry_res.map_err(wrap_io_err!(&parent, "Reading cargo registry entry"))?;
            if entry
                .file_type()
                .map_err(wrap_io_err!(entry.path(), "Reading entry type"))?
                .is_dir()
                && let Some(name) = entry.file_name().to_str()
                && name.starts_with("index.crates.io-")
            {
                names.insert(name.to_string());
            }
        }
    }
    Ok(names.into_iter().collect())
}

fn copy_dir_bounded(
    src: &Path,
    dst: &Path,
    depth: usize,
    entries_copied: &mut usize,
) -> Result<()> {
    if depth > MAX_COPY_DEPTH {
        bail_other_err!(
            "Crate copy exceeded maximum depth ({MAX_COPY_DEPTH}) at '{}'",
            src.display()
        );
    }
    cook_fs::create_dir(dst)?;
    for entry_res in fs::read_dir(src).map_err(wrap_io_err!(src, "Reading crate source dir"))? {
        let entry = entry_res.map_err(wrap_io_err!(src, "Reading crate source entry"))?;
        *entries_copied += 1;
        if *entries_copied > MAX_COPY_ENTRIES {
            bail_other_err!(
                "Crate copy exceeded maximum entry count ({MAX_COPY_ENTRIES}) at '{}'",
                src.display()
            );
        }
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(wrap_io_err!(&src_path, "Reading entry file type"))?;
        if file_type.is_dir() {
            copy_dir_bounded(&src_path, &dst_path, depth + 1, entries_copied)?;
        } else if file_type.is_symlink() {
            let target =
                fs::read_link(&src_path).map_err(wrap_io_err!(&src_path, "Reading symlink"))?;
            cook_fs::symlink(&target, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path).map_err(wrap_io_err!(
                &src_path,
                &dst_path,
                "Copying crate file"
            ))?;
        }
    }
    Ok(())
}

fn extract_crate_archive(
    archive: &Path,
    overlay_src_index: &Path,
    crate_dir_name: &str,
    logger: &PtyOut,
) -> Result<PathBuf> {
    let mut command = Command::new("tar");
    command
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(overlay_src_index);
    cook_fs::run_command(command, logger)?;
    let dst_dir = overlay_src_index.join(crate_dir_name);
    if !dst_dir.is_dir() {
        bail_other_err!(
            "Extracting '{}' did not produce '{}'",
            archive.display(),
            dst_dir.display()
        );
    }
    let cargo_ok = dst_dir.join(".cargo-ok");
    fs::write(&cargo_ok, CARGO_OK_CONTENT).map_err(wrap_io_err!(&cargo_ok, "Writing .cargo-ok"))?;
    Ok(dst_dir)
}

fn validate_requested_crates(crate_patches: &[String]) -> Result<BTreeSet<String>> {
    if crate_patches.len() > MAX_CRATE_PATCHES_PER_RECIPE {
        bail_other_err!(
            "Recipe lists {} crate_patches (max {})",
            crate_patches.len(),
            MAX_CRATE_PATCHES_PER_RECIPE
        );
    }
    let mut unique = BTreeSet::new();
    for crate_name in crate_patches {
        validate_safe_name(crate_name, "crate")?;
        if !unique.insert(crate_name.clone()) {
            bail_other_err!("Duplicate crate {:?} in crate_patches", crate_name);
        }
    }
    Ok(unique)
}

fn collect_locked_versions(
    lock: &CargoLock,
    requested: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<String>>> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for crate_name in requested {
        found.insert(crate_name.clone(), BTreeSet::new());
    }
    for pkg in &lock.packages {
        if pkg.source.as_deref() != Some(CRATES_IO_SOURCE) {
            continue;
        }
        if let Some(versions) = found.get_mut(&pkg.name) {
            versions.insert(pkg.version.clone());
            if versions.len() > MAX_LOCK_VERSIONS_PER_CRATE {
                bail_other_err!(
                    "Cargo.lock has more than {} locked versions for crate {:?}",
                    MAX_LOCK_VERSIONS_PER_CRATE,
                    pkg.name
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    for (crate_name, versions) in found {
        if versions.is_empty() {
            bail_other_err!(
                "Crate {:?} requested in crate_patches was not found as a crates.io dependency in Cargo.lock",
                crate_name
            );
        }
        out.insert(crate_name, versions.into_iter().collect());
    }
    Ok(out)
}

pub fn validate_locked_crate_patches(
    cookbook_root: &Path,
    source_dir: &Path,
    cargopath: Option<&str>,
    crate_patches: &[String],
) -> Result<()> {
    if crate_patches.is_empty() {
        return Ok(());
    }
    let requested = validate_requested_crates(crate_patches)?;
    let lock_path = find_cargo_lock(source_dir, cargopath)?;
    let lock = parse_cargo_lock(&lock_path)?;
    let locked_versions = collect_locked_versions(&lock, &requested)?;
    for (crate_name, versions) in &locked_versions {
        for version in versions {
            let _ = select_verified_patch(cookbook_root, crate_name, version, None)?;
        }
    }
    Ok(())
}

pub fn patches_dir_modified(cookbook_root: &Path, crate_patches: &[String]) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    for crate_name in crate_patches {
        let dir = cookbook_root.join("patches/crates").join(crate_name);
        if let Ok(ts) = cook_fs::modified_dir_ignore_git(&dir)
            && newest.is_none_or(|cur| ts > cur)
        {
            newest = Some(ts);
        }
    }
    newest
}

pub fn prepare_crate_patches(
    cookbook_root: &Path,
    source_dir: &Path,
    build_dir: &Path,
    cargopath: Option<&str>,
    crate_patches: &[String],
    logger: &PtyOut,
) -> Result<Option<PathBuf>> {
    if crate_patches.is_empty() {
        return Ok(None);
    }
    let lock_path = find_cargo_lock(source_dir, cargopath)?;
    let shared_cargo_home = resolve_shared_cargo_home()?;
    let overlay_dir = build_dir.join(".cargo-home");
    let prepared = prepare_crate_patches_from_lock(
        cookbook_root,
        &lock_path,
        &overlay_dir,
        &shared_cargo_home,
        crate_patches,
        logger,
    )?;
    Ok(Some(prepared))
}

pub fn prepare_crate_patches_from_lock(
    cookbook_root: &Path,
    lock_path: &Path,
    overlay_dir: &Path,
    shared_cargo_home: &Path,
    crate_patches: &[String],
    logger: &PtyOut,
) -> Result<PathBuf> {
    if crate_patches.is_empty() {
        bail_other_err!("prepare_crate_patches_from_lock requires at least one crate");
    }
    let requested = validate_requested_crates(crate_patches)?;
    let lock = parse_cargo_lock(lock_path)?;
    let locked_versions = collect_locked_versions(&lock, &requested)?;

    for (crate_name, versions) in &locked_versions {
        for version in versions {
            let _ = select_verified_patch(cookbook_root, crate_name, version, None)?;
        }
    }

    if !shared_cargo_home.is_dir() {
        bail_other_err!(
            "Shared CARGO_HOME '{}' does not exist",
            shared_cargo_home.display()
        );
    }
    let index_dirs = list_registry_index_dirs(shared_cargo_home)?;
    if index_dirs.is_empty() {
        bail_other_err!(
            "No crates.io registry directory found under '{}'",
            shared_cargo_home.join("registry").display()
        );
    }

    let mut targets_by_dirname: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (crate_name, versions) in &locked_versions {
        for version in versions {
            targets_by_dirname.insert(
                format!("{crate_name}-{version}"),
                (crate_name.clone(), version.clone()),
            );
        }
    }

    cook_fs::create_dir_clean(overlay_dir)?;
    let overlay_registry = overlay_dir.join("registry");
    let overlay_registry_src = overlay_registry.join("src");
    cook_fs::create_dir(&overlay_registry_src)?;

    for item in [
        "bin",
        "env",
        "git",
        "config.toml",
        "config",
        "credentials.toml",
        "credentials",
    ] {
        let src_item = shared_cargo_home.join(item);
        if src_item.exists() {
            cook_fs::symlink(&src_item, overlay_dir.join(item))?;
        }
    }
    let cache_src = shared_cargo_home.join("registry/cache");
    if cache_src.exists() {
        cook_fs::symlink(&cache_src, overlay_registry.join("cache"))?;
    }
    let shared_index_root = shared_cargo_home.join("registry/index");
    if shared_index_root.is_dir() {
        let overlay_index_root = overlay_registry.join("index");
        cook_fs::create_dir(&overlay_index_root)?;
        for idx_res in fs::read_dir(&shared_index_root).map_err(wrap_io_err!(
            &shared_index_root,
            "Reading shared registry index"
        ))? {
            let idx_entry = idx_res.map_err(wrap_io_err!(
                &shared_index_root,
                "Reading shared registry index entry"
            ))?;
            let idx_src = idx_entry.path();
            let idx_dst = overlay_index_root.join(idx_entry.file_name());
            if idx_entry
                .file_type()
                .map_err(wrap_io_err!(&idx_src, "Reading index entry type"))?
                .is_dir()
            {
                cook_fs::create_dir(&idx_dst)?;
                let mut idx_count: usize = 0;
                for child_res in
                    fs::read_dir(&idx_src).map_err(wrap_io_err!(&idx_src, "Reading index dir"))?
                {
                    let child =
                        child_res.map_err(wrap_io_err!(&idx_src, "Reading index dir entry"))?;
                    idx_count += 1;
                    if idx_count > MAX_OVERLAY_REGISTRY_ENTRIES {
                        bail_other_err!(
                            "Shared registry index '{}' exceeded {} entries",
                            idx_src.display(),
                            MAX_OVERLAY_REGISTRY_ENTRIES
                        );
                    }
                    let child_src = child.path();
                    let child_dst = idx_dst.join(child.file_name());
                    if child
                        .file_type()
                        .map_err(wrap_io_err!(&child_src, "Reading index child type"))?
                        .is_file()
                    {
                        fs::copy(&child_src, &child_dst).map_err(wrap_io_err!(
                            &child_src,
                            &child_dst,
                            "Copying index file"
                        ))?;
                    } else {
                        cook_fs::symlink(&child_src, &child_dst)?;
                    }
                }
            } else {
                cook_fs::symlink(&idx_src, &idx_dst)?;
            }
        }
    }

    let mut patched_dirnames: BTreeSet<String> = BTreeSet::new();
    for idx in &index_dirs {
        let overlay_idx_src = overlay_registry_src.join(idx);
        cook_fs::create_dir(&overlay_idx_src)?;
        let shared_idx_src = shared_cargo_home.join("registry/src").join(idx);
        let shared_idx_cache = shared_cargo_home.join("registry/cache").join(idx);

        if shared_idx_src.is_dir() {
            let mut count: usize = 0;
            for entry_res in fs::read_dir(&shared_idx_src)
                .map_err(wrap_io_err!(&shared_idx_src, "Reading shared registry src"))?
            {
                let entry = entry_res.map_err(wrap_io_err!(
                    &shared_idx_src,
                    "Reading shared registry entry"
                ))?;
                count += 1;
                if count > MAX_OVERLAY_REGISTRY_ENTRIES {
                    bail_other_err!(
                        "Shared registry src '{}' exceeded {} entries",
                        shared_idx_src.display(),
                        MAX_OVERLAY_REGISTRY_ENTRIES
                    );
                }
                let entry_name = entry.file_name();
                let Some(entry_str) = entry_name.to_str() else {
                    continue;
                };
                if let Some((crate_name, version)) = targets_by_dirname.get(entry_str) {
                    let dst_crate = overlay_idx_src.join(entry_str);
                    let mut copied: usize = 0;
                    copy_dir_bounded(&entry.path(), &dst_crate, 0, &mut copied)?;
                    if !dst_crate.join(".cargo-ok").is_file() {
                        let cargo_ok = dst_crate.join(".cargo-ok");
                        fs::write(&cargo_ok, CARGO_OK_CONTENT)
                            .map_err(wrap_io_err!(&cargo_ok, "Writing .cargo-ok"))?;
                    }
                    apply_crate_patch(
                        cookbook_root,
                        crate_name,
                        Some(version.as_str()),
                        None,
                        &dst_crate,
                        logger,
                    )?;
                    patched_dirnames.insert(entry_str.to_string());
                } else {
                    cook_fs::symlink(entry.path(), overlay_idx_src.join(&entry_name))?;
                }
            }
        }

        for (dir_name, (crate_name, version)) in &targets_by_dirname {
            if overlay_idx_src.join(dir_name).exists() {
                continue;
            }
            let archive = shared_idx_cache.join(format!("{dir_name}.crate"));
            if archive.is_file() {
                let dst_crate =
                    extract_crate_archive(&archive, &overlay_idx_src, dir_name, logger)?;
                apply_crate_patch(
                    cookbook_root,
                    crate_name,
                    Some(version.as_str()),
                    None,
                    &dst_crate,
                    logger,
                )?;
                patched_dirnames.insert(dir_name.clone());
            }
        }
    }

    for (dir_name, (crate_name, version)) in &targets_by_dirname {
        if !patched_dirnames.contains(dir_name) {
            bail_other_err!(
                "Locked crate {} {} ({}) is not available locally in shared CARGO_HOME '{}'; run fetch before building",
                crate_name,
                version,
                dir_name,
                shared_cargo_home.display()
            );
        }
    }

    assert_eq!(patched_dirnames.len(), targets_by_dirname.len());
    Ok(overlay_dir.to_path_buf())
}
