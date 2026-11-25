use pkg::{Package, PackageError, PackageName};
use pkgar::{PackageFile, Transaction, ext::PackageSrcExt};
use pkgar_core::PackageSrc;
use pkgar_keys::PublicKeyFile;

use crate::config::CookConfig;
use crate::cook::fetch_repo;
use crate::cook::package::{package_source_paths, package_target};
use crate::cook::{crate_patches, fetch, fs, pty::PtyOut, script::*};
use crate::recipe::BuildCompiler;
use crate::recipe::{AutoDeps, BuildKind, CookRecipe, OptionalPackageRecipe, Recipe};
use std::{
    collections::{BTreeSet, VecDeque},
    io::{Cursor, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
    str,
    sync::OnceLock,
};

use crate::{Error, Result, bail_other_err, log_to_pty, wrap_io_err};

fn stage_app_manifest(recipe_dir: &Path, package_name: &str, stage_dir: &Path) -> Result<()> {
    let manifest_src = recipe_dir.join("manifest.toml");
    if !manifest_src.is_file() {
        return Ok(());
    }
    let file = std::fs::File::open(&manifest_src)
        .map_err(wrap_io_err!(manifest_src, "Opening app manifest"))?;
    let mut bytes = Vec::new();
    let max_read = (ferrion_manifest::MAX_MANIFEST_BYTES as u64).saturating_add(1);
    file.take(max_read)
        .read_to_end(&mut bytes)
        .map_err(wrap_io_err!(manifest_src, "Reading app manifest"))?;
    let manifest = ferrion_manifest::parse_manifest_bytes(
        &bytes,
        ferrion_manifest::Origin::Repository {
            package: package_name,
        },
    )
    .map_err(|err| {
        Error::Other(format!(
            "App manifest '{}' failed validation ({}): {err}",
            manifest_src.display(),
            err.reason_code()
        ))
    })?;
    let rel_exec = manifest
        .exec
        .strip_prefix('/')
        .expect("validated exec starts with '/'");
    let direct = stage_dir.join(rel_exec);
    let usrmerge = rel_exec
        .strip_prefix("bin/")
        .map(|tail| stage_dir.join("usr/bin").join(tail));
    let exec_exists = std::fs::symlink_metadata(&direct).is_ok()
        || usrmerge
            .as_ref()
            .is_some_and(|alt| std::fs::symlink_metadata(alt).is_ok());
    if !exec_exists {
        bail_other_err!(
            "App manifest '{}' failed validation (missing-exec): exec {:?} not found in staged tree '{}'",
            manifest_src.display(),
            manifest.exec,
            stage_dir.display()
        );
    }
    let apps_dir = stage_dir.join("usr/share/ferrion/apps");
    fs::create_dir(&apps_dir)?;
    let dst = apps_dir.join(format!("{package_name}.toml"));
    std::fs::write(&dst, &bytes).map_err(wrap_io_err!(dst, "Writing staged app manifest"))?;
    Ok(())
}

const RELIBC_STATIC_TAG_FILE: &str = "relibc_static.blake3";
const BUILD_ENV_TAG_FILE: &str = "build_env.blake3";
const BUILD_ENV_DOMAIN: &[u8] = b"ferrion-build-env-v1\0";
const RELIBC_STATIC_NONE: &str = "none";
const RELIBC_START_MARKER: &[u8] = b"no thread fd present";
const RELIBC_STATIC_OBJECTS: &[&str] = &["libc.a", "crt0.o", "crti.o", "crtn.o"];
const MAX_RELIBC_OBJECT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_STAGE_SCAN_DIRS: usize = 4096;
const MAX_STAGE_SCAN_FILES: usize = 16_384;
const MAX_ELF_PHDRS: usize = 256;
const MAX_STATIC_ELF_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PKG_SCAN_ENTRIES: usize = 16_384;
const MAX_PKG_STATIC_ELF_BYTES: u64 = 64 * 1024 * 1024;
const SCAN_CHUNK_BYTES: usize = 64 * 1024;
const ELF64_HEADER_BYTES: usize = 64;
const ELF64_PHDR_BYTES: usize = 56;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;

fn auto_deps_from_dynamic_linking(
    stage_dirs: &[PathBuf],
    dep_pkgars: &BTreeSet<(PackageName, PathBuf)>,
    logger: &PtyOut,
) -> BTreeSet<PackageName> {
    let mut paths = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let verbose = crate::config::try_get_config()
        .map(|s| s.cook.verbose)
        .unwrap_or(true);
    let mut walk = VecDeque::new();

    for stage_dir in stage_dirs {
        walk.push_back((stage_dir, stage_dir.join("usr/bin")));
        walk.push_back((stage_dir, stage_dir.join("usr/games")));
        walk.push_back((stage_dir, stage_dir.join("usr/lib")));
        walk.push_back((stage_dir, stage_dir.join("usr/libexec")));
    }

    while let Some((rel_path, dir)) = walk.pop_front() {
        if visited.contains(&dir) {
            #[cfg(debug_assertions)]
            log_to_pty!(
                logger,
                "DEBUG: auto_deps => Skipping `{dir:?}` (already visited)"
            );
            continue;
        }
        assert!(
            visited.insert(dir.clone()),
            "Directory `{:?}` should not be in visited\nVisited: {:#?}",
            dir,
            visited
        );

        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry_res in read_dir {
            let Ok(entry) = entry_res else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_file() {
                paths.insert((rel_path, entry.path()));
            } else if file_type.is_dir() {
                walk.push_front((rel_path, entry.path()));
            }
        }
    }

    let mut needed = BTreeSet::new();
    for (rel_path, path) in paths {
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let read_cache = object::ReadCache::new(file);
        let Ok(object) = object::build::elf::Builder::read(&read_cache) else {
            continue;
        };
        let Some(dynamic_data) = object.dynamic_data() else {
            continue;
        };
        for dynamic in dynamic_data {
            let object::build::elf::Dynamic::String { tag, val } = dynamic else {
                continue;
            };
            let Ok(name) = str::from_utf8(val) else {
                continue;
            };
            let path_rel = path
                .strip_prefix(rel_path)
                .ok()
                .filter(|_| verbose)
                .map(|s| s.display());
            match *tag {
                object::elf::DT_NEEDED => {
                    if let Some(path_rel) = path_rel {
                        log_to_pty!(logger, "DEBUG: {} needs {}", path_rel, name);
                    }
                    needed.insert(name.to_string());
                }
                object::elf::DT_RPATH | object::elf::DT_RUNPATH => {
                    if let Some(path_rel) = path_rel {
                        log_to_pty!(logger, "DEBUG: {} uses runpath {}", path_rel, name);
                    }
                }
                _ => {}
            }
        }
    }

    let mut missing = needed.clone();
    for preinstalled in &["libc.so.6", "libgcc_s.so.1", "libstdc++.so.6"] {
        missing.remove(*preinstalled);
    }

    let mut deps = BTreeSet::new();
    if let Ok(key_file) = pkgar_keys::PublicKeyFile::open("build/id_ed25519.pub.toml") {
        for (dep, archive_path) in dep_pkgars.iter() {
            let Ok(mut package) = pkgar::PackageFile::new(archive_path, &key_file.pkey) else {
                continue;
            };
            let Ok(entries) = pkgar_core::PackageSrc::read_entries(&mut package) else {
                continue;
            };
            for entry in entries {
                let Ok(entry_path) = pkgar::ext::EntryExt::check_path(&entry) else {
                    continue;
                };
                for prefix in &["lib", "usr/lib"] {
                    let Ok(child_path) = entry_path.strip_prefix(prefix) else {
                        continue;
                    };
                    let Some(child_name) = child_path.to_str() else {
                        continue;
                    };
                    if needed.contains(child_name) {
                        if verbose {
                            log_to_pty!(logger, "DEBUG: {} provides {}", dep, child_name);
                        }
                        deps.insert(dep.with_prefix(pkg::PackagePrefix::Any));
                        missing.remove(child_name);
                    }
                }
            }
        }
    }

    if verbose {
        for name in missing {
            log_to_pty!(logger, "INFO: missing {}", name);
        }
    }

    deps
}

fn auto_deps_from_static_package_deps(
    build_dep_pkgars: &BTreeSet<(PackageName, PathBuf)>,
    dynamic_dep_pkgars: &BTreeSet<PackageName>,
) -> std::result::Result<BTreeSet<PackageName>, PackageError> {
    let static_dep_pkgars: Vec<PackageName> = build_dep_pkgars
        .iter()
        .map(|x| x.0.clone())
        .filter(|x| !dynamic_dep_pkgars.contains(x))
        .collect();
    let pkgs = CookRecipe::get_package_deps_recursive(&static_dep_pkgars, false)?;

    Ok(pkgs.into_iter().collect())
}

pub struct BuildResult {
    pub stage_dirs: Vec<PathBuf>,
    pub auto_deps: BTreeSet<PackageName>,
    pub cached: bool,
}

impl BuildResult {
    pub fn new(stage_dirs: Vec<PathBuf>, auto_deps: BTreeSet<PackageName>) -> Self {
        BuildResult {
            stage_dirs,
            auto_deps,
            cached: false,
        }
    }

    pub fn cached(stage_dirs: Vec<PathBuf>, auto_deps: BTreeSet<PackageName>) -> Self {
        BuildResult {
            stage_dirs,
            auto_deps,
            cached: true,
        }
    }
}

fn all_stage_packages_committed(recipe: &Recipe, target_dir: &Path) -> bool {
    let public_path = Path::new("build/id_ed25519.pub.toml");
    recipe.get_packages_list().into_iter().all(|package| {
        let (_, package_file, package_meta) =
            crate::cook::package::package_stage_paths(package, target_dir);
        crate::cook::package::is_stage_package_committed(&package_file, &package_meta, public_path)
    })
}

pub fn build(
    recipe_dir: &Path,
    source_dir: &Path,
    target_dir: &Path,
    cook_recipe: &CookRecipe,
    cook_config: &CookConfig,
    logger: &PtyOut,
) -> Result<BuildResult> {
    let recipe = &cook_recipe.recipe;
    let name = &cook_recipe.name;
    let check_source = !cook_recipe.is_deps;
    let sysroot_dir = get_sub_target_dir(target_dir, "sysroot");
    let toolchain_dir = get_sub_target_dir(target_dir, "toolchain");
    let auto_deps_file = get_sub_target_dir(target_dir, "auto_deps.toml");
    let stage_dirs = get_stage_dirs(&recipe.optional_packages, target_dir);
    let stage_pkgars: Vec<PathBuf> = stage_dirs
        .iter()
        .map(|p| p.with_added_extension("pkgar"))
        .collect();
    let cli_jobs = cook_config.jobs;
    if recipe.build.kind == BuildKind::None {
        return Ok(BuildResult::new(stage_dirs, BTreeSet::new()));
    }

    let mut dep_pkgars = BTreeSet::new();
    let mut dep_host_pkgars = BTreeSet::new();
    let build_deps = CookRecipe::get_build_deps_recursive(
        &[
            &recipe.build.dependencies[..],
            &recipe.build.dev_dependencies[..],
        ]
        .concat(),
        false,
    )?;
    for dependency in build_deps.iter() {
        let (_, pkgar, _) = dependency.stage_paths();
        if dependency.name.is_host() {
            dep_host_pkgars.insert((dependency.name.clone(), pkgar));
        } else {
            dep_pkgars.insert((dependency.name.clone(), pkgar));
        }
    }

    macro_rules! make_auto_deps {
        ($cached:expr) => {
            build_auto_deps(
                recipe,
                &auto_deps_file,
                &stage_dirs,
                $cached,
                cook_config,
                dep_pkgars,
                logger,
            )
        };
    }

    let relibc_tag_file = get_sub_target_dir(target_dir, RELIBC_STATIC_TAG_FILE);
    let static_relibc_digest = if recipe.build.kind == BuildKind::Remote {
        None
    } else {
        current_static_relibc_digest(name)?
    };
    let static_relibc_ok = match &static_relibc_digest {
        Some(digest) => {
            static_relibc_tag_current(logger, &relibc_tag_file, digest, &stage_dirs, &stage_pkgars)?
        }
        None => true,
    };

    let build_env_tag_file = get_sub_target_dir(target_dir, BUILD_ENV_TAG_FILE);
    let build_env_digest =
        if recipe.build.kind == BuildKind::Remote || recipe.build.env_vars.is_empty() {
            None
        } else {
            Some(compute_build_env_digest(&recipe.build.env_vars))
        };
    let build_env_ok = match &build_env_digest {
        Some(digest) => build_env_tag_current(logger, &build_env_tag_file, digest)?,
        None => true,
    };

    if !check_source {
        let stage_present = all_stage_packages_committed(recipe, target_dir);
        if stage_present && auto_deps_file.is_file() && static_relibc_ok && build_env_ok {
            log_to_pty!(logger, "DEBUG: using cached build, not checking source");
            let auto_deps = make_auto_deps!(true)?;
            return Ok(BuildResult::cached(stage_dirs, auto_deps));
        }
    }

    if recipe.build.kind == BuildKind::Remote {
        return build_remote(stage_dirs, stage_pkgars, recipe, target_dir, logger);
    }

    let deps_sysroot = if name.is_host() {
        &dep_host_pkgars
    } else {
        &dep_pkgars
    };
    let have_toolchain = !name.is_host() && !dep_host_pkgars.is_empty();
    let sysroot_tags_ok = deps_tags_current(logger, &sysroot_dir, deps_sysroot)?;
    let toolchain_tags_ok = if have_toolchain {
        deps_tags_current(logger, &toolchain_dir, &dep_host_pkgars)?
    } else {
        true
    };

    if !sysroot_tags_ok
        || !toolchain_tags_ok
        || !static_relibc_ok
        || !build_env_ok
        || !all_stage_packages_committed(recipe, target_dir)
        || build_is_source_newer(
            logger,
            recipe_dir,
            source_dir,
            &auto_deps_file,
            stage_pkgars,
            &recipe.build.crate_patches,
        )
    {
        if auto_deps_file.is_file() {
            fs::remove_all(&auto_deps_file)?;
        }
        if relibc_tag_file.is_file() {
            fs::remove_all(&relibc_tag_file)?;
        }
        if build_env_tag_file.is_file() {
            fs::remove_all(&build_env_tag_file)?;
        }
        for stage_dir in &stage_dirs {
            remove_stage_dir(stage_dir)?;
        }
        build_deps_dir(logger, &sysroot_dir, deps_sysroot)?;
        if have_toolchain {
            build_deps_dir(logger, &toolchain_dir, &dep_host_pkgars)?;
        }
    } else {
        log_to_pty!(logger, "DEBUG: using cached build");
        let auto_deps = make_auto_deps!(true)?;
        return Ok(BuildResult::cached(stage_dirs, auto_deps));
    }

    let stage_dir = stage_dirs
        .last()
        .expect("Should have atleast one stage dir");

    let build_dir = get_sub_target_dir(target_dir, "build");
    if !stage_dir.is_dir() {
        let stage_dir_tmp = target_dir.join("stage.tmp");
        fs::create_dir_clean(&stage_dir_tmp)?;

        if cook_config.clean_build || !static_relibc_ok || !build_env_ok || !build_dir.is_dir() {
            fs::create_dir_clean(&build_dir)?;
        }

        let flags_fn = |name, flags: &Vec<String>| {
            format!(
                "{name}+=(\n{}\n)\n",
                flags
                    .iter()
                    .map(|s| format!("  \"{s}\""))
                    .collect::<Vec<String>>()
                    .join("\n")
            )
        };

        let bool_fn = |name, flag: &bool| {
            if *flag { name } else { "" }
        };

        let mut allow_cargo_offline = false;
        let script = match &recipe.build.kind {
            BuildKind::Cargo {
                cargopath,
                cargoflags,
                cargopackages,
                cargoexamples,
                clearlocked,
                cargopackagesprefixed,
            } => {
                allow_cargo_offline = true;
                let mut script = format!(
                    "DYNAMIC_INIT\n{}{}\nCOOKBOOK_CARGO_PATH={} ",
                    bool_fn("COOKBOOK_CARGO_FLAGS=()\n", clearlocked),
                    flags_fn("COOKBOOK_CARGO_FLAGS", cargoflags),
                    cargopath.as_deref().unwrap_or(".")
                );
                if cargopackages.is_empty() && cargoexamples.is_empty() {
                    script += "cookbook_cargo\n"
                } else {
                    if !cargopackages.is_empty() {
                        script += if *cargopackagesprefixed {
                            "cookbook_cargo_packages_prefixed"
                        } else {
                            "cookbook_cargo_packages"
                        };
                        for package in cargopackages {
                            script += " ";
                            script += package;
                        }
                        script += "\n";
                    }
                    if !cargoexamples.is_empty() {
                        script += "cookbook_cargo_examples";
                        for example in cargoexamples {
                            script += " ";
                            script += example;
                        }
                        script += "\n";
                    }
                }

                script
            }
            BuildKind::Configure { configureflags } => format!(
                "DYNAMIC_INIT\n{}cookbook_configure",
                flags_fn("COOKBOOK_CONFIGURE_FLAGS", configureflags),
            ),
            BuildKind::Cmake { cmakeflags } => format!(
                "DYNAMIC_INIT\n{}cookbook_cmake",
                flags_fn("COOKBOOK_CMAKE_FLAGS", cmakeflags),
            ),
            BuildKind::Meson { mesonflags } => format!(
                "DYNAMIC_INIT\n{}cookbook_meson",
                flags_fn("COOKBOOK_MESON_FLAGS", mesonflags),
            ),
            BuildKind::Python {
                pyprojectpath,
                legacysetup,
                pipflags,
            } => format!(
                "DYNAMIC_INIT\n{}{}{}cookbook_python",
                bool_fn("COOKBOOK_PYTHON_LEGACY_SETUP=1\n", legacysetup),
                pyprojectpath
                    .as_ref()
                    .map(|s| format!("COOKBOOK_PYTHON_PYPROJECT_PATH={s}\n"))
                    .unwrap_or("".into()),
                flags_fn("COOKBOOK_PYTHON_PIP_FLAGS", pipflags),
            ),
            BuildKind::Custom { script } => script.clone(),
            BuildKind::Remote => unreachable!(),
            BuildKind::None => "".to_owned(),
        };

        let command = {
            let cookbook_build = build_dir.canonicalize().unwrap();
            let cookbook_recipe = recipe_dir.canonicalize().unwrap();
            let cookbook_root = Path::new(".").canonicalize().unwrap();
            let cookbook_stage = stage_dir_tmp.canonicalize().unwrap();
            let cookbook_source = source_dir.canonicalize().unwrap();
            let cookbook_sysroot = sysroot_dir.canonicalize().unwrap();
            let cookbook_toolchain = toolchain_dir.canonicalize().ok();
            let bash_args = if cook_config.verbose_cmd { "-ex" } else { "-e" };
            let cookbook_redoxer_bin = fs::get_relative_exe("cookbook_redoxer")?;
            let cookbook_repo_bin = fs::get_relative_exe("repo")?;
            let is_custom_crate_patches = !recipe.build.crate_patches.is_empty()
                && matches!(recipe.build.kind, BuildKind::Custom { .. });
            let cargo_home_overlay = if is_custom_crate_patches {
                None
            } else {
                let cargopath = match &recipe.build.kind {
                    BuildKind::Cargo { cargopath, .. } => cargopath.as_deref(),
                    _ => None,
                };
                crate_patches::prepare_crate_patches(
                    &cookbook_root,
                    &cookbook_source,
                    &cookbook_build,
                    cargopath,
                    &recipe.build.crate_patches,
                    logger,
                )?
            };
            let shared_cargo_home = if is_custom_crate_patches {
                Some(crate_patches::resolve_shared_cargo_home()?)
            } else {
                None
            };
            let mut command = Command::new(&cookbook_redoxer_bin);
            command.env("COOKBOOK_REDOXER", &cookbook_redoxer_bin);
            command.env("COOKBOOK_REPO", &cookbook_repo_bin);
            if let Some(overlay) = &cargo_home_overlay {
                command.env("CARGO_HOME", overlay);
                command.env("CARGO_NET_OFFLINE", "true");
            } else if let Some(shared) = &shared_cargo_home {
                command.env("COOKBOOK_SHARED_CARGO_HOME", shared);
                command.env("COOKBOOK_CRATE_OVERLAY", cookbook_build.join(".cargo-home"));
                command.env(
                    "COOKBOOK_CRATE_PATCHES",
                    recipe.build.crate_patches.join(","),
                );
                command.env("CARGO_HOME", cookbook_build.join(".cargo-home-unprepared"));
                command.env("CARGO_NET_OFFLINE", "true");
            }
            command.arg("env").arg("bash").arg(bash_args);
            command.current_dir(&cookbook_build);
            command.env("TARGET", package_target(name));
            command.env("COOKBOOK_BUILD", &cookbook_build);
            command.env("COOKBOOK_NAME", name.name());
            command.env("COOKBOOK_HOST_TARGET", redoxer::host_target());
            command.env("COOKBOOK_RECIPE", &cookbook_recipe);
            command.env("COOKBOOK_ROOT", &cookbook_root);
            command.env("COOKBOOK_STAGE", &cookbook_stage);
            command.env("COOKBOOK_SOURCE", &cookbook_source);
            command.env("COOKBOOK_SYSROOT", &cookbook_sysroot);
            if crate::is_hosted() {
                command.env("COOKBOOK_HOSTED", "1");
            }
            if let Some(cookbook_toolchain) = &cookbook_toolchain {
                command.env("COOKBOOK_TOOLCHAIN", cookbook_toolchain);
            } else if name.is_host() || crate::is_hosted() {
                command.env("COOKBOOK_TOOLCHAIN", &cookbook_sysroot);
            }
            if let Some(compiler) = recipe.build.compiler.as_ref() {
                match compiler {
                    BuildCompiler::Gcc | BuildCompiler::GccNoLTO => {
                        command.env("REDOXER_USE_CLANG", "0");
                    }
                    BuildCompiler::Clang | BuildCompiler::ClangNoLTO | BuildCompiler::Wasm => {
                        command.env("REDOXER_USE_CLANG", "1");
                    }
                    _ => {}
                }
                match compiler {
                    BuildCompiler::ClangNoLTO | BuildCompiler::GccNoLTO | BuildCompiler::NoLTO => {
                        command.env("REDOXER_USE_LTO", "0");
                    }
                    _ => {}
                }
                if matches!(compiler, BuildCompiler::Wasm) {
                    command.env("TARGET", "wasm32-wasip1");
                }
            };
            command.env("COOKBOOK_MAKE_JOBS", cli_jobs.to_string());
            if cook_config.verbose {
                command.env("COOKBOOK_VERBOSE", "true");
            } else {
                command.env_remove("COOKBOOK_VERBOSE");
            }
            if cook_config.verbose_cmd {
                command.env("COOKBOOK_VERBOSE_CMD", "true");
            } else {
                command.env_remove("COOKBOOK_VERBOSE_CMD");
            }
            if cook_config.offline && allow_cargo_offline {
                command.env("COOKBOOK_OFFLINE", "1");
            } else {
                command.env_remove("COOKBOOK_OFFLINE");
            }
            if let Ok(ident_source) = fetch::fetch_get_source_info(cook_recipe) {
                command.env("COOKBOOK_SOURCE_IDENT", ident_source.source_identifier);
                command.env("COOKBOOK_COMMIT_IDENT", ident_source.commit_identifier);
            }
            for var in &recipe.build.env_vars {
                match std::env::var_os(var) {
                    Some(val) => {
                        command.env(var, val);
                    }
                    None => {
                        command.env_remove(var);
                    }
                }
            }
            command
        };

        assert_deps_populated(&sysroot_dir, deps_sysroot)?;
        if have_toolchain {
            assert_deps_populated(&toolchain_dir, &dep_host_pkgars)?;
        }
        let full_script = format!(
            "{}\n{}\n{}\n{}",
            BUILD_PRESCRIPT, SHARED_PRESCRIPT, script, BUILD_POSTSCRIPT
        );
        fs::run_command_stdin(command, full_script.as_bytes(), logger)?;
        stage_app_manifest(recipe_dir, name.name(), &stage_dir_tmp)?;

        let mut globs = Vec::new();
        for (i, feat) in recipe.optional_packages.iter().enumerate() {
            let stage_dir = &stage_dirs[i];
            fs::create_dir_clean(stage_dir)?;
            for path in &feat.files {
                let glob = globset::Glob::new(path).map_err(|e| format!("{}", e))?;
                globs.push((glob.compile_matcher(), stage_dir.clone()));
            }
        }
        fs::move_dir_all_fn(
            &stage_dir_tmp,
            &Box::new(|path: PathBuf| {
                for (glob, dst) in &globs {
                    if glob.is_match(&path) {
                        return Some(dst.as_path());
                    }
                }
                None
            }),
        )
        .map_err(wrap_io_err!("Moving to stages dir"))?;

        fs::rename(&stage_dir_tmp, stage_dir)?;
    }

    if let Some(digest) = &static_relibc_digest {
        write_static_relibc_tag(logger, &relibc_tag_file, &stage_dirs, digest)?;
    }
    if let Some(digest) = &build_env_digest {
        write_build_env_tag(&build_env_tag_file, digest)?;
    }

    if cook_config.clean_target {
        fs::remove_all(&build_dir)?;
        clean_deps_dir(&sysroot_dir)?;
        if toolchain_dir.is_dir() {
            clean_deps_dir(&toolchain_dir)?;
        }
    }

    let auto_deps = make_auto_deps!(false)?;
    Ok(BuildResult::new(stage_dirs, auto_deps))
}

fn compute_build_env_digest(env_vars: &[String]) -> String {
    assert!(env_vars.len() <= crate::recipe::MAX_BUILD_ENV_VARS);
    let mut hasher = blake3::Hasher::new();
    hasher.update(BUILD_ENV_DOMAIN);
    hasher.update(&(env_vars.len() as u64).to_le_bytes());
    for var in env_vars {
        hasher.update(&(var.len() as u64).to_le_bytes());
        hasher.update(var.as_bytes());
        match std::env::var_os(var) {
            Some(val) => {
                let bytes = val.as_encoded_bytes();
                hasher.update(&[1u8]);
                hasher.update(&(bytes.len() as u64).to_le_bytes());
                hasher.update(bytes);
            }
            None => {
                hasher.update(&[0u8]);
            }
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn build_env_tag_current(logger: &PtyOut, tag_file: &Path, current_digest: &str) -> Result<bool> {
    if tag_file.is_file() {
        let recorded = fs::read_to_string(tag_file)?;
        if recorded.trim() == current_digest {
            return Ok(true);
        }
        log_to_pty!(logger, "DEBUG: updating build: declared env-vars changed");
        return Ok(false);
    }
    log_to_pty!(
        logger,
        "DEBUG: updating build: {} is absent",
        BUILD_ENV_TAG_FILE
    );
    Ok(false)
}

fn write_build_env_tag(tag_file: &Path, digest: &str) -> Result<()> {
    let contents = format!("{digest}\n");
    std::fs::write(tag_file, contents).map_err(wrap_io_err!(tag_file, "Writing build env tag"))
}

fn gnu_target_for(target: &str) -> &str {
    match target {
        "riscv64gc-unknown-redox" => "riscv64-unknown-redox",
        other => other,
    }
}

fn find_static_relibc_lib_dir() -> Option<PathBuf> {
    let sysroot = std::env::var_os("COOKBOOK_HOST_SYSROOT")?;
    if sysroot.is_empty() {
        return None;
    }
    let sysroot_path = PathBuf::from(sysroot);
    let gnu_target = std::env::var("GNU_TARGET")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| gnu_target_for(redoxer::target()).to_owned());
    let candidates = [
        sysroot_path.join(&gnu_target).join("lib"),
        sysroot_path.join("lib"),
    ];
    candidates
        .into_iter()
        .find(|dir| dir.join("libc.a").is_file())
}

static STATIC_RELIBC_DIGEST_CACHE: OnceLock<Option<String>> = OnceLock::new();

fn compute_static_relibc_digest() -> Result<Option<String>> {
    let Some(lib_dir) = find_static_relibc_lib_dir() else {
        return Ok(None);
    };
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; SCAN_CHUNK_BYTES];
    for obj_name in RELIBC_STATIC_OBJECTS {
        let path = lib_dir.join(obj_name);
        if !path.is_file() {
            continue;
        }
        let meta = std::fs::metadata(&path).map_err(wrap_io_err!(&path, "Reading metadata"))?;
        if meta.len() > MAX_RELIBC_OBJECT_BYTES {
            bail_other_err!(
                "Static relibc object {} size {} exceeds bound {}",
                path.display(),
                meta.len(),
                MAX_RELIBC_OBJECT_BYTES
            );
        }
        hasher.update(obj_name.as_bytes());
        hasher.update(&[0]);
        hasher.update(&meta.len().to_le_bytes());
        let mut file = std::fs::File::open(&path).map_err(wrap_io_err!(&path, "Opening file"))?;
        let mut read_total: u64 = 0;
        loop {
            let n = file
                .read(&mut buf)
                .map_err(wrap_io_err!(&path, "Reading file"))?;
            if n == 0 {
                break;
            }
            read_total = read_total.saturating_add(n as u64);
            if read_total > MAX_RELIBC_OBJECT_BYTES {
                bail_other_err!(
                    "Static relibc object {} read {} exceeds bound {}",
                    path.display(),
                    read_total,
                    MAX_RELIBC_OBJECT_BYTES
                );
            }
            hasher.update(&buf[..n]);
        }
    }
    Ok(Some(hasher.finalize().to_hex().to_string()))
}

fn current_static_relibc_digest(name: &PackageName) -> Result<Option<String>> {
    if name.is_host() || name.without_prefix() == "relibc" || crate::cross_target().is_some() {
        return Ok(None);
    }
    if let Some(cached) = STATIC_RELIBC_DIGEST_CACHE.get() {
        return Ok(cached.clone());
    }
    let digest = compute_static_relibc_digest()?;
    let _ = STATIC_RELIBC_DIGEST_CACHE.set(digest.clone());
    Ok(digest)
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        let n = reader.read(&mut buf[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

fn is_static_relibc_elf_reader<R: Read + Seek>(
    logger: &PtyOut,
    reader: &mut R,
    file_len: u64,
    label: &str,
) -> Result<bool> {
    if file_len < ELF64_HEADER_BYTES as u64 {
        return Ok(false);
    }
    let mut hdr = [0u8; ELF64_HEADER_BYTES];
    let n = read_exact_or_eof(reader, &mut hdr)
        .map_err(wrap_io_err!(Path::new(label), "Reading ELF header"))?;
    if n < ELF64_HEADER_BYTES || &hdr[0..4] != b"\x7fELF" {
        return Ok(false);
    }
    if hdr[4] != 2 || hdr[5] != 1 {
        return Ok(false);
    }
    let e_type = u16::from_le_bytes([hdr[16], hdr[17]]);
    let e_phoff = u64::from_le_bytes(hdr[32..40].try_into().unwrap());
    let e_phentsize = u16::from_le_bytes([hdr[54], hdr[55]]);
    let e_phnum = u16::from_le_bytes([hdr[56], hdr[57]]);
    if e_type == 0 || e_type == 1 || e_phnum == 0 || (e_phentsize as usize) < ELF64_PHDR_BYTES {
        return Ok(false);
    }
    if (e_phnum as usize) > MAX_ELF_PHDRS {
        log_to_pty!(
            logger,
            "WARN: static relibc scan hit MAX_ELF_PHDRS ({}) at {}; treating recipe as static",
            MAX_ELF_PHDRS,
            label
        );
        return Ok(true);
    }
    let mut loads: Vec<(u64, u64)> = Vec::new();
    let mut raw_phdr = [0u8; ELF64_PHDR_BYTES];
    for i in 0..(e_phnum as u64) {
        let Some(off) = e_phoff.checked_add(i.saturating_mul(e_phentsize as u64)) else {
            return Ok(false);
        };
        if off
            .checked_add(ELF64_PHDR_BYTES as u64)
            .is_none_or(|end| end > file_len)
        {
            return Ok(false);
        }
        reader
            .seek(SeekFrom::Start(off))
            .map_err(wrap_io_err!(Path::new(label), "Seeking ELF program header"))?;
        let pn = read_exact_or_eof(reader, &mut raw_phdr)
            .map_err(wrap_io_err!(Path::new(label), "Reading ELF program header"))?;
        if pn < ELF64_PHDR_BYTES {
            return Ok(false);
        }
        let p_type = u32::from_le_bytes(raw_phdr[0..4].try_into().unwrap());
        if p_type == PT_DYNAMIC {
            return Ok(false);
        }
        if p_type == PT_LOAD {
            let p_offset = u64::from_le_bytes(raw_phdr[8..16].try_into().unwrap());
            let p_filesz = u64::from_le_bytes(raw_phdr[32..40].try_into().unwrap());
            if p_filesz > 0 {
                loads.push((p_offset, p_filesz));
            }
        }
    }
    let overlap = RELIBC_START_MARKER.len() - 1;
    let mut buf = vec![0u8; overlap + SCAN_CHUNK_BYTES];
    let mut scanned: u64 = 0;
    for (p_offset, p_filesz) in loads {
        if p_offset
            .checked_add(p_filesz)
            .is_none_or(|end| end > file_len)
        {
            continue;
        }
        let next_scanned = scanned.checked_add(p_filesz);
        if next_scanned.is_none_or(|n| n > MAX_STATIC_ELF_SCAN_BYTES) {
            log_to_pty!(
                logger,
                "WARN: static relibc scan hit MAX_STATIC_ELF_SCAN_BYTES ({}) at {}; treating recipe as static",
                MAX_STATIC_ELF_SCAN_BYTES,
                label
            );
            return Ok(true);
        }
        scanned = next_scanned.unwrap();
        reader
            .seek(SeekFrom::Start(p_offset))
            .map_err(wrap_io_err!(Path::new(label), "Seeking ELF segment"))?;
        let mut remaining = p_filesz;
        let mut carry = 0usize;
        while remaining > 0 {
            let to_read = (remaining as usize).min(SCAN_CHUNK_BYTES);
            let got = read_exact_or_eof(reader, &mut buf[carry..carry + to_read])
                .map_err(wrap_io_err!(Path::new(label), "Reading ELF segment"))?;
            if got == 0 {
                break;
            }
            let total = carry + got;
            if contains_subslice(&buf[..total], RELIBC_START_MARKER) {
                return Ok(true);
            }
            let next_carry = total.min(overlap);
            buf.copy_within(total - next_carry..total, 0);
            carry = next_carry;
            remaining -= got as u64;
        }
    }
    Ok(false)
}

fn is_static_relibc_elf_stream<R: Read>(
    logger: &PtyOut,
    reader: &mut R,
    unpacked_size: u64,
    label: &str,
) -> Result<bool> {
    if unpacked_size < ELF64_HEADER_BYTES as u64 {
        return Ok(false);
    }
    let mut hdr = [0u8; ELF64_HEADER_BYTES];
    let n = read_exact_or_eof(reader, &mut hdr)
        .map_err(wrap_io_err!(Path::new(label), "Reading package ELF header"))?;
    if n < ELF64_HEADER_BYTES || &hdr[0..4] != b"\x7fELF" {
        return Ok(false);
    }
    if hdr[4] != 2 || hdr[5] != 1 {
        return Ok(false);
    }
    let e_type = u16::from_le_bytes([hdr[16], hdr[17]]);
    let e_phoff = u64::from_le_bytes(hdr[32..40].try_into().unwrap());
    let e_phentsize = u16::from_le_bytes([hdr[54], hdr[55]]);
    let e_phnum = u16::from_le_bytes([hdr[56], hdr[57]]);
    if e_type == 0 || e_type == 1 || e_phnum == 0 || (e_phentsize as usize) < ELF64_PHDR_BYTES {
        return Ok(false);
    }
    if (e_phnum as usize) > MAX_ELF_PHDRS {
        log_to_pty!(
            logger,
            "WARN: static relibc scan hit MAX_ELF_PHDRS ({}) at {}; treating recipe as static",
            MAX_ELF_PHDRS,
            label
        );
        return Ok(true);
    }
    let mut phdr_prefix = Vec::new();
    if e_phoff == ELF64_HEADER_BYTES as u64 {
        let phdr_bytes = (e_phnum as usize).saturating_mul(e_phentsize as usize);
        if (ELF64_HEADER_BYTES as u64)
            .checked_add(phdr_bytes as u64)
            .is_none_or(|end| end > unpacked_size)
        {
            return Ok(false);
        }
        phdr_prefix.resize(phdr_bytes, 0);
        let got = read_exact_or_eof(reader, &mut phdr_prefix)
            .map_err(wrap_io_err!(Path::new(label), "Reading package ELF phdrs"))?;
        if got < phdr_bytes {
            return Ok(false);
        }
        for i in 0..(e_phnum as usize) {
            let off = i * (e_phentsize as usize);
            let p_type = u32::from_le_bytes(phdr_prefix[off..off + 4].try_into().unwrap());
            if p_type == PT_DYNAMIC {
                return Ok(false);
            }
        }
    }
    if unpacked_size > MAX_PKG_STATIC_ELF_BYTES {
        log_to_pty!(
            logger,
            "WARN: static relibc scan hit MAX_PKG_STATIC_ELF_BYTES ({}) at {}; treating recipe as static",
            MAX_PKG_STATIC_ELF_BYTES,
            label
        );
        return Ok(true);
    }
    let mut full = Vec::with_capacity(unpacked_size as usize);
    full.extend_from_slice(&hdr);
    full.extend_from_slice(&phdr_prefix);
    let rest_cap = (unpacked_size as usize).saturating_sub(full.len());
    let mut limited = reader.take(rest_cap as u64);
    limited
        .read_to_end(&mut full)
        .map_err(wrap_io_err!(Path::new(label), "Reading package ELF"))?;
    let mut cursor = Cursor::new(full.as_slice());
    is_static_relibc_elf_reader(logger, &mut cursor, full.len() as u64, label)
}

fn stage_dirs_have_static_relibc(logger: &PtyOut, stage_dirs: &[PathBuf]) -> Result<bool> {
    let mut walk = VecDeque::new();
    for dir in stage_dirs {
        if dir.is_dir() {
            walk.push_back(dir.clone());
        }
    }
    let mut dirs_seen: usize = 0;
    let mut files_seen: usize = 0;
    while let Some(dir) = walk.pop_front() {
        dirs_seen += 1;
        if dirs_seen > MAX_STAGE_SCAN_DIRS {
            log_to_pty!(
                logger,
                "WARN: static relibc scan hit MAX_STAGE_SCAN_DIRS ({}) at {}; treating recipe as static",
                MAX_STAGE_SCAN_DIRS,
                dir.display()
            );
            return Ok(true);
        }
        let read_dir = std::fs::read_dir(&dir).map_err(wrap_io_err!(&dir, "Reading stage dir"))?;
        for entry_res in read_dir {
            let entry = entry_res.map_err(wrap_io_err!(&dir, "Reading stage entry"))?;
            let file_type = entry
                .file_type()
                .map_err(wrap_io_err!(entry.path(), "Reading stage entry type"))?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                walk.push_back(entry.path());
            } else if file_type.is_file() {
                files_seen += 1;
                if files_seen > MAX_STAGE_SCAN_FILES {
                    log_to_pty!(
                        logger,
                        "WARN: static relibc scan hit MAX_STAGE_SCAN_FILES ({}) at {}; treating recipe as static",
                        MAX_STAGE_SCAN_FILES,
                        entry.path().display()
                    );
                    return Ok(true);
                }
                let path = entry.path();
                let meta = entry
                    .metadata()
                    .map_err(wrap_io_err!(&path, "Reading stage file metadata"))?;
                let mut file = std::fs::File::open(&path)
                    .map_err(wrap_io_err!(&path, "Opening stage file"))?;
                let label = path.to_string_lossy();
                if is_static_relibc_elf_reader(logger, &mut file, meta.len(), &label)? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn stage_pkgars_have_static_relibc(logger: &PtyOut, stage_pkgars: &[PathBuf]) -> Result<bool> {
    if !stage_pkgars.iter().any(|p| p.is_file()) {
        return Ok(false);
    }
    let pkey_path = Path::new("build/id_ed25519.pub.toml");
    let key_file = PublicKeyFile::open(pkey_path)?;
    let mut total_entries: usize = 0;
    for pkgar_path in stage_pkgars {
        if !pkgar_path.is_file() {
            continue;
        }
        let mut package = PackageFile::new(pkgar_path, &key_file.pkey)?;
        let entries = package.read_entries()?;
        total_entries = total_entries.saturating_add(entries.len());
        if total_entries > MAX_PKG_SCAN_ENTRIES {
            log_to_pty!(
                logger,
                "WARN: static relibc scan hit MAX_PKG_SCAN_ENTRIES ({}) at {}; treating recipe as static",
                MAX_PKG_SCAN_ENTRIES,
                pkgar_path.display()
            );
            return Ok(true);
        }
        for entry in entries {
            let entry_label = pkgar::ext::EntryExt::check_path(&entry)
                .map(|p| format!("{}:{}", pkgar_path.display(), p.display()))
                .unwrap_or_else(|_| pkgar_path.display().to_string());
            let mut data_reader = package.data_reader(&entry)?;
            let unpacked_size = data_reader.unpacked_size;
            let scan_res =
                is_static_relibc_elf_stream(logger, &mut data_reader, unpacked_size, &entry_label);
            package.restore_reader(data_reader.into_inner())?;
            if scan_res? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn static_relibc_tag_current(
    logger: &PtyOut,
    tag_file: &Path,
    current_digest: &str,
    stage_dirs: &[PathBuf],
    stage_pkgars: &[PathBuf],
) -> Result<bool> {
    if tag_file.is_file() {
        let recorded = fs::read_to_string(tag_file)?;
        let recorded = recorded.trim();
        if recorded == RELIBC_STATIC_NONE || recorded == current_digest {
            return Ok(true);
        }
        log_to_pty!(
            logger,
            "DEBUG: updating build: static relibc sysroot changed"
        );
        return Ok(false);
    }
    if !stage_pkgars.iter().all(|p| p.is_file()) {
        return Ok(false);
    }
    let has_static = if stage_dirs.iter().all(|d| d.is_dir()) {
        stage_dirs_have_static_relibc(logger, stage_dirs)?
    } else {
        stage_pkgars_have_static_relibc(logger, stage_pkgars)?
    };
    if has_static {
        log_to_pty!(
            logger,
            "DEBUG: updating build: static relibc binary has no relibc tag"
        );
        return Ok(false);
    }
    std::fs::write(tag_file, RELIBC_STATIC_NONE)
        .map_err(wrap_io_err!(tag_file, "Writing static relibc tag"))?;
    Ok(true)
}

fn write_static_relibc_tag(
    logger: &PtyOut,
    tag_file: &Path,
    stage_dirs: &[PathBuf],
    current_digest: &str,
) -> Result<()> {
    let tag_val = if stage_dirs_have_static_relibc(logger, stage_dirs)? {
        current_digest
    } else {
        RELIBC_STATIC_NONE
    };
    std::fs::write(tag_file, tag_val)
        .map_err(wrap_io_err!(tag_file, "Writing static relibc tag"))?;
    Ok(())
}

fn build_is_source_newer(
    logger: &PtyOut,
    recipe_dir: &Path,
    source_dir: &Path,
    auto_deps_file: &Path,
    stage_pkgars: Vec<PathBuf>,
    crate_patches: &[String],
) -> bool {
    if !auto_deps_file.is_file() {
        return true;
    }
    let Ok(stage_modified) = fs::modified_all(&stage_pkgars, fs::modified) else {
        return true;
    };
    let Ok(mut source_modified) = fs::modified_dir_ignore_git(source_dir) else {
        return false;
    };
    let mut recipe_is_newest = false;
    if let Ok(recipe_modified) = fs::modified(&recipe_dir.join("recipe.toml"))
        && recipe_modified > source_modified
    {
        source_modified = recipe_modified;
        recipe_is_newest = true;
    }
    if let Ok(manifest_modified) = fs::modified(&recipe_dir.join("manifest.toml"))
        && manifest_modified > source_modified
    {
        source_modified = manifest_modified;
        recipe_is_newest = true;
    }
    if let Some(patches_modified) =
        crate_patches::patches_dir_modified(Path::new("."), crate_patches)
        && patches_modified > source_modified
    {
        source_modified = patches_modified;
        recipe_is_newest = true;
    }
    let newer = source_modified > stage_modified;
    if newer {
        log_to_pty!(
            logger,
            "DEBUG: updating build of {}: {} is newer",
            recipe_dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("??"),
            if recipe_is_newest { "recipe" } else { "source" }
        );
    }
    newer
}

pub fn remove_stage_dir(stage_dir: &PathBuf) -> crate::Result<()> {
    if stage_dir.is_dir() {
        fs::remove_all(stage_dir)?;
    }
    let stage_file = stage_dir.with_added_extension("pkgar");
    if stage_file.is_file() {
        fs::remove_all(&stage_file)?;
    }
    let stage_meta = stage_dir.with_added_extension("toml");
    if stage_meta.is_file() {
        fs::remove_all(&stage_meta)?;
    }
    let stage_files = stage_dir.with_added_extension("files");
    if stage_files.is_file() {
        fs::remove_all(&stage_files)?;
    }
    Ok(())
}

pub fn get_stage_dirs(features: &Vec<OptionalPackageRecipe>, target_dir: &Path) -> Vec<PathBuf> {
    let mut target_dir = target_dir.to_path_buf();
    if let Some(cross_target) = crate::cross_target() {
        target_dir = target_dir.join(cross_target)
    }
    let mut v = Vec::new();
    for f in features {
        v.push(target_dir.join(format!("stage.{}", f.name)));
    }
    v.push(target_dir.join("stage"));
    v
}

pub fn get_sub_target_dir(target_dir: &Path, sub_path: &str) -> PathBuf {
    let mut target_dir = target_dir.to_path_buf();
    if let Some(cross_target) = crate::cross_target() {
        target_dir = target_dir.join(cross_target)
    }
    target_dir.join(sub_path)
}

const DEPS_POPULATED_MARKER: &str = ".populated";

enum DepsDir {
    Absent,
    Populated { tags: BTreeSet<String> },
    Emptied { tags: BTreeSet<String> },
}

fn inspect_deps_dir(deps_dir: &Path) -> Result<DepsDir> {
    let tags_dir = deps_dir.join(".tags");
    if !tags_dir.is_dir() {
        return Ok(DepsDir::Absent);
    }
    let mut tags = BTreeSet::new();
    for entry in
        std::fs::read_dir(&tags_dir).map_err(wrap_io_err!(&tags_dir, "Reading tags dir"))?
    {
        let entry = entry.map_err(wrap_io_err!(&tags_dir, "Reading tags entry"))?;
        if let Some(name) = entry.file_name().to_str() {
            tags.insert(name.to_owned());
        }
    }
    if deps_dir.join(DEPS_POPULATED_MARKER).is_file() {
        Ok(DepsDir::Populated { tags })
    } else {
        Ok(DepsDir::Emptied { tags })
    }
}

fn expected_dep_tags(dep_pkgars: &BTreeSet<(PackageName, PathBuf)>) -> BTreeSet<String> {
    dep_pkgars
        .iter()
        .map(|(name, _)| name.without_prefix().to_owned())
        .collect()
}

fn assert_deps_populated(
    deps_dir: &Path,
    dep_pkgars: &BTreeSet<(PackageName, PathBuf)>,
) -> Result<()> {
    let state = inspect_deps_dir(deps_dir)?;
    let DepsDir::Populated { tags } = state else {
        panic!(
            "deps dir {} is not Populated before build",
            deps_dir.display()
        );
    };
    assert_eq!(tags, expected_dep_tags(dep_pkgars));
    Ok(())
}

fn open_deps_pkey(dep_pkgars: &BTreeSet<(PackageName, PathBuf)>) -> Result<[u8; 32]> {
    let pkey_path = "build/id_ed25519.pub.toml";
    match PublicKeyFile::open(pkey_path) {
        Ok(k) => Ok(k.pkey),
        Err(e) => {
            if !dep_pkgars.is_empty() {
                Err(Error::from(e))
            } else {
                Ok(Default::default())
            }
        }
    }
}

fn deps_hashes_match(
    logger: &PtyOut,
    deps_dir: &Path,
    tags: &BTreeSet<String>,
    dep_pkgars: &BTreeSet<(PackageName, PathBuf)>,
    pkey_file: &[u8; 32],
) -> Result<bool> {
    let expected = expected_dep_tags(dep_pkgars);
    if *tags != expected {
        return Ok(false);
    }
    let tags_dir = deps_dir.join(".tags");
    let mut cached = true;
    for (name, pkgar_path) in dep_pkgars {
        let tag_file = tags_dir.join(name.without_prefix());
        let Ok(tag_hash) = blake3::Hash::from_hex(fs::read_to_string(&tag_file)?) else {
            log_to_pty!(
                logger,
                "DEBUG: updating {:?}: {:?} is absent",
                deps_dir.file_name().unwrap().display(),
                name.as_str(),
            );
            cached = false;
            continue;
        };
        let pkgar_hash = PackageFile::new(pkgar_path, pkey_file)?.header().blake3;
        if *tag_hash.as_bytes() != pkgar_hash {
            log_to_pty!(
                logger,
                "DEBUG: updating {:?}: {:?} is updated",
                deps_dir.file_name().unwrap().display(),
                name.as_str(),
            );
            cached = false;
        }
    }
    Ok(cached)
}

fn deps_tags_current(
    logger: &PtyOut,
    deps_dir: &Path,
    dep_pkgars: &BTreeSet<(PackageName, PathBuf)>,
) -> Result<bool> {
    let pkey_file = open_deps_pkey(dep_pkgars)?;
    match inspect_deps_dir(deps_dir)? {
        DepsDir::Absent => Ok(false),
        DepsDir::Populated { tags } | DepsDir::Emptied { tags } => {
            deps_hashes_match(logger, deps_dir, &tags, dep_pkgars, &pkey_file)
        }
    }
}

fn build_deps_dir(
    logger: &PtyOut,
    deps_dir: &PathBuf,
    dep_pkgars: &BTreeSet<(PackageName, PathBuf)>,
) -> Result<bool> {
    let pkey_file = open_deps_pkey(dep_pkgars)?;
    match inspect_deps_dir(deps_dir)? {
        DepsDir::Populated { tags } => {
            if deps_hashes_match(logger, deps_dir, &tags, dep_pkgars, &pkey_file)? {
                return Ok(true);
            }
            fs::remove_all(deps_dir)?;
        }
        DepsDir::Emptied { .. } => {
            fs::remove_all(deps_dir)?;
        }
        DepsDir::Absent => {
            if deps_dir.exists() {
                fs::remove_all(deps_dir)?;
            }
        }
    }

    let deps_dir_tmp = deps_dir.with_added_extension("tmp");
    fs::create_dir_clean(&deps_dir_tmp)?;
    let tags_dir = deps_dir_tmp.join(".tags");
    let usr_dir = deps_dir_tmp.join("usr");
    fs::create_dir(&tags_dir)?;
    fs::create_dir(&usr_dir)?;

    for folder in &["bin", "include", "lib", "share"] {
        fs::create_dir(&usr_dir.join(folder))?;

        fs::symlink(Path::new("usr").join(folder), deps_dir_tmp.join(folder))?;
    }

    for (name, archive_path) in dep_pkgars {
        let tag_file = tags_dir.join(name.without_prefix());
        let mut package = PackageFile::new(archive_path, &pkey_file)?;
        Transaction::install(&mut package, &deps_dir_tmp)?.commit()?;
        let hash = blake3::Hash::from_bytes(package.header().blake3).to_hex();
        std::fs::write(&tag_file, hash.as_bytes()).map_err(wrap_io_err!("Writing tag"))?;
    }

    let populated_marker = deps_dir_tmp.join(DEPS_POPULATED_MARKER);
    std::fs::write(&populated_marker, "").map_err(wrap_io_err!("Writing populated marker"))?;

    fs::rename(&deps_dir_tmp, deps_dir)?;

    Ok(false)
}

fn clean_deps_dir(deps_dir: &PathBuf) -> Result<bool> {
    let marker = deps_dir.join(DEPS_POPULATED_MARKER);
    if marker.is_file() {
        fs::remove_all(&marker)?;
    }
    if deps_dir.is_dir() {
        for entry in
            std::fs::read_dir(deps_dir).map_err(wrap_io_err!(deps_dir, "Reading deps dir"))?
        {
            let entry = entry.map_err(wrap_io_err!(deps_dir, "Reading deps entry"))?;
            if entry.file_name() == ".tags" {
                continue;
            }
            fs::remove_all(&entry.path())?;
        }
    }
    Ok(false)
}

fn build_auto_deps(
    recipe: &Recipe,
    auto_deps_path: &Path,
    stage_dirs: &Vec<PathBuf>,
    cached: bool,
    cook_config: &CookConfig,
    mut dep_pkgars: BTreeSet<(PackageName, PathBuf)>,
    logger: &PtyOut,
) -> Result<BTreeSet<PackageName>> {
    if auto_deps_path.is_file() && !cached {
        if cook_config.verbose {
            log_to_pty!(logger, "DEBUG: updating {}", auto_deps_path.display());
        }
        fs::remove_all(auto_deps_path)?;
    }

    let auto_deps = if auto_deps_path.exists() {
        let wrapper: AutoDeps = fs::read_toml(auto_deps_path)?;
        wrapper.packages
    } else {
        let mut dynamic_deps = auto_deps_from_dynamic_linking(stage_dirs, &dep_pkgars, logger);
        dep_pkgars.retain(|x| recipe.build.dependencies.contains(&x.0));
        let package_deps =
            auto_deps_from_static_package_deps(&dep_pkgars, &dynamic_deps).unwrap_or_default();
        dynamic_deps.extend(package_deps);

        let wrapper = AutoDeps {
            packages: dynamic_deps,
        };
        fs::serialize_and_write(auto_deps_path, &wrapper)?;
        wrapper.packages
    };
    Ok(auto_deps)
}

pub fn build_remote(
    stage_dirs: Vec<PathBuf>,
    stage_pkgars: Vec<PathBuf>,
    recipe: &Recipe,
    target_dir: &Path,
    logger: &PtyOut,
) -> Result<BuildResult> {
    let source_toml = target_dir.join("source.toml");
    let auto_deps_path = target_dir.join("auto_deps.toml");
    let source_pubkey = fetch_repo::get_binary_pubkey();

    let packages = recipe.get_packages_list();
    let mut cached = auto_deps_path.is_file();
    for (i, package) in packages.iter().enumerate() {
        let stage_pkgar = &stage_pkgars[i];

        if !stage_pkgar.is_file() {
            cached = false;
            break;
        }

        let (_, source_pkgar, _) = package_source_paths(*package, target_dir);
        if fs::modified(&source_pkgar)? > fs::modified(stage_pkgar)? {
            cached = false;
            break;
        }
    }

    if cached {
        log_to_pty!(logger, "DEBUG: using cached build");
        let wrapper: AutoDeps = fs::read_toml(&auto_deps_path)?;
        return Ok(BuildResult::cached(stage_dirs, wrapper.packages));
    }

    for (i, package) in packages.into_iter().enumerate() {
        let stage_dir = &stage_dirs[i];
        let (_, source_pkgar, _) = package_source_paths(package, target_dir);
        fs::create_dir_clean(stage_dir)?;
        pkgar::extract(&source_pubkey, &source_pkgar, stage_dir).map_err(|e| {
            if matches!(e, pkgar::Error::Core(pkgar_core::Error::Dryoc(_))) {
                hint_incorrect_pkey(logger, &source_pubkey, &source_pkgar);
            }
            e
        })?
    }

    if auto_deps_path.is_file() {
        fs::remove_all(&auto_deps_path)?
    }

    let auto_deps = {
        let toml_content = fs::read_to_string(&source_toml)?;
        let pkg_toml = Package::from_toml(&toml_content)?;
        let wrapper = AutoDeps {
            packages: pkg_toml.depends.into_iter().collect(),
        };
        fs::serialize_and_write(&auto_deps_path, &wrapper)?;
        wrapper.packages
    };
    Ok(BuildResult::new(stage_dirs, auto_deps))
}

fn hint_incorrect_pkey(logger: &PtyOut, source_pubkey: &Path, source_pkgar: &Path) -> Option<()> {
    log_to_pty!(
        logger,
        "DEBUG: Pkgar key mismatch, hint:\n - expected: {:?}\n - current:  {:?}",
        blake3::Hash::from_bytes(
            pkgar_keys::PublicKeyFile::from_header_path(source_pkgar)
                .ok()?
                .pkey
        )
        .to_hex()
        .to_string(),
        blake3::Hash::from_bytes(PublicKeyFile::open(source_pubkey).ok()?.pkey)
            .to_hex()
            .to_string(),
    );

    Some(())
}

#[cfg(test)]
mod tests {
    use std::os::unix;

    #[test]
    fn file_system_loop_no_infinite_loop() {
        let mut root = std::env::temp_dir();
        root.push("temp_test_dir_file_system_loop_no_infinite_loop");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("Failed to create temporary root directory");

        let dir = root.join("loop");
        unix::fs::symlink(&root, &dir).expect("Linking {dir:?} to {root:?}");

        assert_eq!(
            root.canonicalize().unwrap(),
            dir.canonicalize().unwrap(),
            "Expected a loop where {dir:?} points to {root:?}"
        );

        let entries =
            super::auto_deps_from_dynamic_linking(&vec![root.clone()], &Default::default(), &None);
        assert!(
            entries.is_empty(),
            "auto_deps shouldn't have yielded any libraries"
        );
    }
}
