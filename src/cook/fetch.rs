use crate::cook::{
    fetch_repo::{self, PlainPtyCallback},
    fs::*,
    package::{get_package_name, package_source_paths},
    pty::PtyOut,
    script::*,
};
use crate::{
    Error, Result, bail_other_err,
    config::translate_mirror,
    is_redox, log_to_pty,
    recipe::{BuildKind, CookRecipe, SourceRecipe},
    wrap_io_err, wrap_other_err,
};
use pkg::{SourceIdentifier, net_backend::DownloadBackendWriter};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
};

pub struct FetchResult {
    pub source_dir: PathBuf,
    pub source_ident: String,
    pub patches_ident: String,
    pub cached: bool,
}

impl FetchResult {
    pub fn new(
        source_dir: PathBuf,
        source_ident: String,
        patches_ident: String,
        cached: bool,
    ) -> Self {
        Self {
            source_dir,
            source_ident,
            patches_ident,
            cached,
        }
    }

    pub fn cached(source_dir: PathBuf, source_ident: String, patches_ident: String) -> Self {
        Self {
            source_dir,
            source_ident,
            patches_ident,
            cached: true,
        }
    }

    pub fn apply_info(mut self, recipe: &CookRecipe) -> Result<Self> {
        let (source, patch) =
            fetch_apply_source_info(recipe, self.source_ident, self.patches_ident)?;
        self.source_ident = source;
        self.patches_ident = patch;
        Ok(self)
    }
}

/// Fetch using "offline mode". It is equivalent of using "local" rule to all recipes.
pub fn fetch_offline(recipe: &CookRecipe, logger: &PtyOut) -> Result<FetchResult> {
    let recipe_dir = &recipe.dir;
    let source_dir = recipe_dir.join("source");
    match recipe.recipe.build.kind {
        BuildKind::None => {
            // the build function doesn't need source dir exists
            let (source, patch) = ("".to_string(), "".to_string());
            return FetchResult::cached(source_dir, source, patch).apply_info(recipe);
        }
        BuildKind::Remote => {
            return fetch_remote(recipe_dir, recipe, true, source_dir, logger);
        }
        _ => {}
    }

    let result = match &recipe.recipe.source {
        Some(SourceRecipe::Path { path: _ }) | None => fetch(recipe, true, logger)?,
        Some(SourceRecipe::SameAs { same_as }) => {
            let recipe = fetch_resolve_canon(same_as, recipe)?;
            // recursively fetch
            let r = fetch_offline(&recipe, logger)?;
            fetch_make_symlink(&source_dir, same_as)?;
            r
        }
        Some(SourceRecipe::Git {
            git: _,
            upstream: _,
            branch: _,
            rev: _,
            patches,
            script,
            shallow_clone: _,
        }) => {
            offline_check_exists(&source_dir)?;
            let (head_rev, _) = get_git_head_rev(&source_dir)?;
            let patches_blake3 = get_patches_blake3(recipe_dir, patches, script)?;
            FetchResult::cached(source_dir, head_rev, patches_blake3)
        }
        Some(SourceRecipe::Tar {
            tar: _,
            blake3,
            patches,
            script,
        }) => {
            let ident = blake3.clone().unwrap_or("no_tar_blake3_hash_info".into());
            let patches_blake3 = get_patches_blake3(recipe_dir, patches, script)?;
            let cached = source_dir.is_dir();
            if !cached {
                let source_tar = recipe_dir.join("source.tar");
                let source_tar_blake3 = get_file_blake3(&source_tar)?;
                if source_tar.exists() {
                    if let Some(blake3) = blake3 {
                        if source_tar_blake3 != *blake3 {
                            bail_other_err!(
                                "The downloaded tar blake3 {source_tar_blake3:?} is not equal to blake3 in recipe.toml"
                            );
                        }
                        create_dir(&source_dir)?;
                        fetch_extract_tar(source_tar, &source_dir, logger)?;
                        fetch_apply_patches(recipe_dir, patches, script, &source_dir, logger)?;
                    } else {
                        // need to trust this tar file
                        bail_other_err!(
                            "Please add blake3 = {source_tar_blake3:?} to {recipe:?}",
                            recipe = recipe_dir.join("recipe.toml").display(),
                        );
                    }
                }
            }
            offline_check_exists(&source_dir)?;
            FetchResult::new(source_dir, ident, patches_blake3, cached)
        }
    };

    result.apply_info(recipe)
}

pub fn fetch(recipe: &CookRecipe, check_source: bool, logger: &PtyOut) -> Result<FetchResult> {
    let recipe_dir = &recipe.dir;
    let source_dir = recipe_dir.join("source");
    let mut recipe = recipe;
    let mut recipe_owned;
    if recipe.rule == "local" && !source_dir.exists() {
        // local source, but the source is missing
        recipe_owned = recipe.clone();
        recipe_owned.rule = "source".into();
        recipe_owned.reload_recipe()?;
        recipe = &recipe_owned;
    }
    match recipe.recipe.build.kind {
        BuildKind::None => {
            // the build function doesn't need source dir exists
            let (source, patch) = ("".to_string(), "".to_string());
            return FetchResult::cached(source_dir, source, patch).apply_info(recipe);
        }
        BuildKind::Remote => {
            return fetch_remote(recipe_dir, recipe, false, source_dir, logger);
        }
        _ => {}
    }

    let cached_info = fetch_get_source_info(recipe).ok();
    let result = match &recipe.recipe.source {
        Some(SourceRecipe::SameAs { same_as }) => {
            let recipe = fetch_resolve_canon(same_as, recipe)?;
            // recursively fetch
            let r = fetch(&recipe, check_source, logger)?;
            fetch_make_symlink(&source_dir, same_as)?;
            r
        }
        Some(SourceRecipe::Path { path }) => {
            let path = Path::new(&path);
            let cached = source_dir.is_dir() && modified_dir(path)? <= modified_dir(&source_dir)?;
            if !cached {
                log_to_pty!(
                    logger,
                    "[DEBUG]: {:?} is newer than {:?}",
                    path.display(),
                    source_dir.display()
                );
                copy_dir_all(path, &source_dir).map_err(wrap_io_err!(
                    path,
                    source_dir,
                    "Copying source"
                ))?;
            }
            FetchResult::new(
                source_dir,
                "local_source".to_string(),
                "".to_string(),
                cached,
            )
        }
        Some(SourceRecipe::Git {
            git,
            upstream,
            branch,
            rev,
            patches,
            script,
            shallow_clone,
        }) => {
            //TODO: use libgit?
            let shallow_clone =
                shallow_clone.unwrap_or_else(|| crate::config::get_config().cook.git_treeless);
            let mut fetch_is_ran = false;
            let patches_ident = get_patches_blake3(recipe_dir, patches, script)?;
            let cached = if !source_dir.is_dir() {
                // Create source.tmp
                let source_dir_tmp = recipe_dir.join("source.tmp");
                create_dir_clean(&source_dir_tmp)?;

                // Clone the repository to source.tmp
                let mut command = Command::new("git");
                command
                    .arg("clone")
                    .arg("--recursive")
                    .arg(translate_mirror(git));
                if let Some(branch) = branch {
                    command.arg("--branch").arg(branch);
                }
                if shallow_clone {
                    command
                        .arg("--filter=tree:0")
                        .arg("--also-filter-submodules");
                }
                command.arg(&source_dir_tmp);
                if let Err(e) = run_command(command, logger) {
                    if !is_redox() {
                        return Err(e);
                    }
                    // TODO: RedoxFS has a race condition problem with `--recursive` and running in multi CPU.
                    //       It is appear that running the submodule update separately fixes it. Remove this when
                    //       a recursive clone of relibc (`git clone --recursive`) is proven to work in FerrionOS.
                    let mut cmds = vec!["update", "--init"];
                    if shallow_clone {
                        cmds.push("--filter=tree:0");
                    }
                    manual_git_recursive_submodule(logger, &source_dir_tmp, cmds)?;
                }

                // Move source.tmp to source atomically
                rename(&source_dir_tmp, &source_dir)?;

                false
            } else if !check_source {
                true
            } else {
                if !source_dir.join(".git").is_dir() {
                    bail_other_err!(
                        "{:?} is not a git repository, but recipe indicated git source",
                        source_dir.display()
                    );
                }

                let (head_rev, detached_rev) = get_git_head_rev(&source_dir)?;
                match (cached_info, rev, detached_rev) {
                    (None, _, _) => false,
                    (Some(s), _, _) if !s.is_updated(&head_rev, &patches_ident) => false,
                    (_, Some(rev), true) => get_git_tag_rev(&source_dir, rev, logger)
                        .is_ok_and(|exp_rev| exp_rev == head_rev),
                    (_, None, false) => match get_git_remote_tracking(&source_dir) {
                        Ok(remote) if !remote.check_updated(git, branch) => false,
                        Ok(remote) => {
                            git_run_fetch(logger, &source_dir, git)?;
                            fetch_is_ran = true;
                            match get_git_fetch_rev(
                                &source_dir,
                                &remote.remote_url,
                                branch.as_ref().unwrap_or(&remote.remote_branch),
                            ) {
                                Ok(fetch_rev) => fetch_rev == head_rev,
                                Err(e) => {
                                    log_to_pty!(logger, "{}", e);
                                    false
                                }
                            }
                        }
                        Err(e) => {
                            log_to_pty!(logger, "{}", e);
                            false
                        }
                    },
                    _ => false,
                }
            };

            if !cached {
                if !fetch_is_ran {
                    git_run_fetch(logger, &source_dir, git)?;
                }
                if let Some(_upstream) = upstream {
                    //TODO: set upstream URL (is this needed?)
                    // git remote set-url upstream "$GIT_UPSTREAM" &> /dev/null ||
                    // git remote add upstream "$GIT_UPSTREAM"
                    // git fetch upstream
                }

                if !patches.is_empty() || script.is_some() {
                    git_reset_clean(logger, &source_dir)?;
                }

                if let Some(rev) = rev {
                    // Check out specified revision
                    let mut command = Command::new("git");
                    command.arg("-C").arg(&source_dir);
                    command.arg("checkout").arg(rev);
                    run_command(command, logger)?;
                } else {
                    let branch = match branch {
                        Some(branch) => branch.clone(),
                        // TODO: Reuse default branch name from get_git_remote_tracking?
                        None => get_git_remote_branch(&source_dir, "origin")?,
                    };
                    let mut command = Command::new("git");
                    command
                        .arg("checkout")
                        .arg("-B")
                        .arg(&branch)
                        .arg(format!("remotes/origin/{branch}"));
                    command.current_dir(&source_dir);
                    run_command(command, logger)?;
                }

                // Sync submodules URL
                let mut command = Command::new("git");
                command.arg("-C").arg(&source_dir);
                command.arg("submodule").arg("sync").arg("--recursive");

                if let Err(e) = run_command(command, logger) {
                    if !is_redox() {
                        return Err(e);
                    }
                    manual_git_recursive_submodule(logger, &source_dir, vec!["sync"])?;
                }

                // Update submodules
                let mut command = Command::new("git");
                command.arg("-C").arg(&source_dir);
                command
                    .arg("submodule")
                    .arg("update")
                    .arg("--init")
                    .arg("--recursive");
                if shallow_clone {
                    command.arg("--filter=tree:0");
                }
                if let Err(e) = run_command(command, logger) {
                    if !is_redox() {
                        return Err(e);
                    }
                    let mut cmds = vec!["update", "--init"];
                    if shallow_clone {
                        cmds.push("--filter=tree:0");
                    }
                    manual_git_recursive_submodule(logger, &source_dir, cmds)?;
                }

                fetch_apply_patches(recipe_dir, patches, script, &source_dir, logger)?;
            }

            let (head_rev, _) = get_git_head_rev(&source_dir)?;
            FetchResult::new(source_dir, head_rev, patches_ident, cached)
        }
        Some(SourceRecipe::Tar {
            tar,
            blake3,
            patches,
            script,
        }) => {
            let source_tar = recipe_dir.join("source.tar");
            let source_ident = blake3.clone().unwrap_or("no_tar_blake3_hash_info".into());
            let patches_ident = get_patches_blake3(recipe_dir, patches, script)?;
            let mut tar_updated = false;
            loop {
                if !source_tar.is_file() {
                    tar_updated = true;
                    download_wget(tar, &source_tar, logger)?;
                }
                if !check_source {
                    break;
                }
                let source_tar_blake3 = get_file_blake3(&source_tar)?;
                if let Some(blake3) = blake3 {
                    if source_tar_blake3 == *blake3 {
                        break;
                    }
                    if tar_updated {
                        bail_other_err!(
                            "The downloaded tar blake3 {source_tar_blake3:?} is not equal to blake3 in recipe.toml"
                        )
                    } else {
                        log_to_pty!(
                            logger,
                            "DEBUG: source tar blake3 is different and need redownload"
                        );
                        remove_all(&source_tar)?;
                    }
                } else {
                    //TODO: set blake3 hash on the recipe with something like "cook fix"
                    log_to_pty!(
                        logger,
                        "WARNING: set blake3 for {:?} to {:?}",
                        source_tar.display(),
                        source_tar_blake3
                    );
                    break;
                }
            }
            let mut cached = true;
            if source_dir.is_dir() {
                let should_update = if tar_updated {
                    log_to_pty!(logger, "DEBUG: updating source: tar is newer");
                    true
                } else if cached_info.is_none() {
                    log_to_pty!(logger, "DEBUG: updating source: cached info is missing");
                    true
                } else if !cached_info
                    .as_ref()
                    .unwrap()
                    .is_updated(&source_ident, &patches_ident)
                {
                    if cached_info.unwrap().source_identifier != source_ident {
                        log_to_pty!(
                            logger,
                            "DEBUG: updating source: tar is newer than cached info"
                        );
                    } else {
                        log_to_pty!(
                            logger,
                            "DEBUG: updating source: patches are newer than cached info"
                        );
                    }
                    true
                } else {
                    false
                };
                if should_update {
                    remove_all(&source_dir)?
                }
            }
            if !source_dir.is_dir() {
                // Create source.tmp
                let source_dir_tmp = recipe_dir.join("source.tmp");
                create_dir_clean(&source_dir_tmp)?;
                fetch_extract_archive(source_tar, &source_dir_tmp, logger)?;
                fetch_apply_patches(recipe_dir, patches, script, &source_dir_tmp, logger)?;

                // Move source.tmp to source atomically
                rename(&source_dir_tmp, &source_dir)?;
                cached = false;
            }
            FetchResult::new(source_dir, source_ident, patches_ident, cached)
        }
        // Local Sources
        None => {
            if !source_dir.is_dir() {
                log_to_pty!(
                    logger,
                    "WARNING: Recipe without source section expected source dir at '{}'",
                    source_dir.display(),
                );
                create_dir(&source_dir)?;
            }
            FetchResult::cached(source_dir, "local_source".into(), "".into())
        }
    };

    if let BuildKind::Cargo { cargopath, .. } = &recipe.recipe.build.kind
        && !result.cached
    {
        fetch_cargo(&result.source_dir, cargopath.as_ref(), logger)?;
    }

    result.apply_info(recipe)
}

fn git_run_fetch(logger: &PtyOut, source_dir: &PathBuf, git: &String) -> Result<()> {
    let mut command = Command::new("git");
    command.arg("-C").arg(source_dir);
    command.arg("remote").arg("set-url").arg("origin").arg(git);
    run_command(command, logger)?;
    let mut command = Command::new("git");
    command.arg("-C").arg(source_dir);
    command.arg("fetch").arg("origin");
    run_command(command, logger)?;
    Ok(())
}

/// Discard every local change in a git source before patches are applied again:
/// tracked edits, untracked files (such as files an earlier patch created) and ignored files, in submodules too.
/// Ignored files go as well because a patch can create a path the upstream .gitignore matches, and in-tree build outputs from the old patch set would be stale.
fn git_reset_clean(logger: &PtyOut, source_dir: &PathBuf) -> Result<()> {
    let mut command = Command::new("git");
    command.arg("-C").arg(source_dir);
    command
        .arg("reset")
        .arg("--hard")
        .arg("--recurse-submodules");
    run_command(command, logger)?;

    // -f twice also removes untracked nested repositories.
    let clean = ["clean", "-d", "-f", "-f", "-x"];
    let mut command = Command::new("git");
    command.arg("-C").arg(source_dir);
    command.args(clean);
    run_command(command, logger)?;

    let mut command = Command::new("git");
    command.arg("-C").arg(source_dir);
    command
        .arg("submodule")
        .arg("foreach")
        .arg("--recursive")
        .arg("git")
        .args(clean);
    if let Err(e) = run_command(command, logger) {
        if !is_redox() {
            return Err(e);
        }
        let mut cmds = vec!["foreach", "git"];
        cmds.extend(clean);
        manual_git_recursive_submodule(logger, source_dir, cmds)?;
    }
    Ok(())
}

fn manual_git_recursive_submodule(
    logger: &PtyOut,
    source_dir: &PathBuf,
    cmd: Vec<&str>,
) -> Result<()> {
    log_to_pty!(
        logger,
        "Git submodule {} failed, might be caused by race condition in RedoxFS, retrying without --recursive.",
        cmd[0]
    );

    let mut repo_registry: BTreeMap<PathBuf, bool> = BTreeMap::new();

    loop {
        let mut dirty_git = false;

        let output = Command::new("find")
            .args([".", "-name", ".git"])
            .current_dir(source_dir)
            .output()
            .map_err(wrap_io_err!("Failed to execute find"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);

        for line in stdout.lines() {
            let git_path = PathBuf::from(line);
            if let Some(repo_root) = git_path.parent() {
                let repo_root_buf = repo_root.to_path_buf();

                if !repo_registry.contains_key(&repo_root_buf) {
                    repo_registry.insert(repo_root_buf.clone(), false);
                    dirty_git = true;
                }
            }
        }

        if !dirty_git {
            // completed
            return Ok(());
        }

        let pending_repos: Vec<PathBuf> = repo_registry
            .iter()
            .filter(|&(_, &synced)| !synced)
            .map(|(path, _)| path.clone())
            .collect();

        if pending_repos.is_empty() {
            bail_other_err!("No pending repos but dirty");
        }

        for repo in pending_repos {
            println!("==> Processing: {:?}", repo);

            let mut command = Command::new("git");
            command.arg("-C").arg(&repo).current_dir(source_dir);
            command.arg("submodule");

            for cmd in &cmd {
                command.arg(cmd);
            }
            run_command(command, logger)?;

            repo_registry.insert(repo, true);
        }
    }
}

fn get_patches_blake3(
    dir: &PathBuf,
    patches: &Vec<String>,
    script: &Option<String>,
) -> Result<String> {
    get_combined_blake3(
        dir,
        patches,
        &match script {
            Some(s) => vec![s.to_string()],
            None => Vec::new(),
        },
    )
}

pub(crate) fn fetch_make_symlink(source_dir: &PathBuf, same_as: &String) -> Result<()> {
    let target_dir = Path::new(same_as).join("source");
    if !source_dir.is_symlink() {
        if source_dir.is_dir() {
            bail_other_err!(
                "{dir:?} is a directory, but recipe indicated a symlink. \n\
                        try removing {dir:?} if you haven't made any changes",
                dir = source_dir.display(),
            )
        }
        std::os::unix::fs::symlink(&target_dir, source_dir).map_err(|err| {
            format!(
                "failed to symlink {:?} to {:?}: {}\n{:?}",
                target_dir.display(),
                source_dir.display(),
                err,
                err
            )
        })?;
    }
    Ok(())
}

pub(crate) fn fetch_resolve_canon(
    same_as: &String,
    source_recipe: &CookRecipe,
) -> Result<CookRecipe> {
    let canon_dir = &source_recipe.dir.join(same_as);
    if canon_dir
        .to_str()
        .unwrap()
        .chars()
        .filter(|c| *c == '/')
        .count()
        > 50
    {
        bail_other_err!("Infinite loop detected");
    }
    if !canon_dir.exists() {
        bail_other_err!("{dir:?} is not exists", dir = canon_dir.display());
    }
    let mut recipe = CookRecipe::from_path(canon_dir.as_path(), true, source_recipe.name.is_host())
        .map_err(Error::from)?;
    if !source_recipe.rule.is_empty() {
        recipe.apply_filesystem_config(&source_recipe.rule)?;
    }
    // Copying from repo.rs, not ideal, but works
    if let Some(gitrev) = crate::config::get_config()
        .recipe_lock
        .get(recipe.name.without_prefix())
        .and_then(|r| r.gitrev.clone())
        && let Some(SourceRecipe::Git { rev, branch, .. }) = &mut recipe.recipe.source
    {
        *rev = Some(gitrev.clone());
        *branch = None;
    }
    Ok(recipe)
}

fn fetch_extract_archive(archive: PathBuf, destination: &PathBuf, logger: &PtyOut) -> Result<()> {
    let mut magic = [0; 4];
    File::open(&archive)
        .and_then(|mut file| file.read_exact(&mut magic))
        .map_err(wrap_io_err!(archive, "Reading archive signature"))?;
    if matches!(&magic, b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x07\x08") {
        let mut command = Command::new("unzip");
        command.arg("-o");
        if !crate::config::get_config().cook.verbose {
            command.arg("-q");
        }
        command.arg(&archive).arg("-d").arg(destination);
        run_command(command, logger)
    } else {
        fetch_extract_tar(archive, destination, logger)
    }
}

pub(crate) fn fetch_extract_tar(
    source_tar: PathBuf,
    source_dir_tmp: &PathBuf,
    logger: &PtyOut,
) -> Result<()> {
    let mut command = Command::new("tar");
    let verbose = crate::config::get_config().cook.verbose;
    if is_redox() {
        command.arg(if verbose { "xvf" } else { "xf" });
    } else {
        command.arg("--extract");
        command.arg("--no-same-owner");
        if verbose {
            command.arg("--verbose");
        }
        command.arg("--file");
    }
    command.arg(&source_tar);
    command.arg("--directory").arg(source_dir_tmp);
    command.arg("--strip-components").arg("1");
    run_command(command, logger)?;
    Ok(())
}

pub(crate) fn fetch_cargo(
    source_dir: &PathBuf,
    cargopath: Option<&String>,
    logger: &PtyOut,
) -> Result<()> {
    if !check_cargo_available() {
        return Ok(());
    }

    let mut source_dir = source_dir.clone();
    if let Some(cargopath) = cargopath {
        source_dir = source_dir.join(cargopath);
    }

    let command_path = get_relative_exe("cookbook_redoxer").unwrap_or(PathBuf::from("cargo"));
    let mut command = Command::new(command_path);
    command.arg("fetch");
    command.arg("--manifest-path");
    command.arg(source_dir.join("Cargo.toml").into_os_string());
    run_command(command, logger)?;
    Ok(())
}

/// Check if "$REDOXER_TOOLCHAIN/bin/cargo" is available.
fn check_cargo_available() -> bool {
    let Ok(dir) = std::env::var("REDOXER_TOOLCHAIN") else {
        return false;
    };
    PathBuf::from(dir).join("bin/cargo").is_file()
}

pub fn fetch_remote(
    recipe_dir: &Path,
    recipe: &CookRecipe,
    offline_mode: bool,
    source_dir: PathBuf,
    logger: &PtyOut,
) -> Result<FetchResult> {
    let (mut manager, repository) = fetch_repo::get_binary_repo();
    let target_dir = create_target_dir(recipe_dir, recipe.target)?;
    if logger.is_some() {
        let writer = logger.as_ref().unwrap().1.try_clone().unwrap();
        manager.set_callback(Rc::new(RefCell::new(PlainPtyCallback::new(writer))));
    }
    let packages = recipe.recipe.get_packages_list();

    let name = recipe_dir
        .file_name()
        .ok_or("Unable to get recipe name")?
        .to_str()
        .unwrap();

    let mut result = None;
    let mut cached = true;

    for package in packages {
        let (_, source_pkgar, source_toml) = package_source_paths(package, &target_dir);
        let (tmp_pkgar, tmp_toml) = (
            source_pkgar.with_added_extension("tmp"),
            source_toml.with_added_extension("tmp"),
        );
        let source_name = get_package_name(name, package);
        let Some(repo_blake3) = repository.packages.get(&source_name) else {
            bail_other_err!("Package {source_name} does not exist in server repository")
        };

        if !offline_mode {
            if source_toml.is_file() {
                let pkg_toml = read_source_toml(&source_toml)?;
                if &pkg_toml.blake3 != repo_blake3 {
                    log_to_pty!(logger, "DEBUG: Updating source binaries");
                    remove_all(&source_toml)?;
                    if source_pkgar.is_file() {
                        remove_all(&source_pkgar)?;
                    }
                }
            }

            if !source_toml.is_file() {
                let pkg_toml = {
                    if tmp_toml.is_file() {
                        remove_all(&tmp_toml)?;
                    }
                    let toml_file =
                        File::create(&tmp_toml).map_err(wrap_io_err!(tmp_toml, "Creating file"))?;
                    let mut writer = DownloadBackendWriter::ToFile(toml_file);
                    manager.download(&format!("{}.toml", source_name), None, None, &mut writer)?;
                    read_source_toml(&tmp_toml)?
                };

                if tmp_pkgar.is_file() {
                    remove_all(&tmp_pkgar)?;
                }
                let pkgar_file =
                    File::create(&tmp_pkgar).map_err(wrap_io_err!(tmp_pkgar, "Creating file"))?;
                let mut writer = DownloadBackendWriter::ToFile(pkgar_file);
                manager.download(
                    &format!("{}.pkgar", source_name),
                    Some(pkg_toml.network_size),
                    None,
                    &mut writer,
                )?;
                rename(&tmp_pkgar, &source_pkgar)?;
                rename(&tmp_toml, &source_toml)?;
                cached = false;
            }

            // manager.download(file, 0, dest)
        } else {
            offline_check_exists(&source_pkgar)?;
            offline_check_exists(&source_toml)?;
        }

        // guaranteed to have `None` once and last in iteration
        if package.is_none() {
            let pkg_toml = read_source_toml(&source_toml)?;

            fetch_apply_source_info_from_remote(recipe, &pkg_toml.to_ident())?;

            result = Some(FetchResult::new(
                source_dir.clone(),
                pkg_toml.source_identifier,
                pkg_toml.patch_identifier,
                cached,
            ));
        }
    }

    result.ok_or_else(wrap_other_err!("There's no mandatory package in remote"))
}

fn read_source_toml(source_toml: &Path) -> Result<pkg::Package> {
    let mut file = File::open(source_toml).map_err(wrap_io_err!(source_toml, "Opening file"))?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(wrap_io_err!(source_toml, "Reading file"))?;
    let pkg_toml = pkg::Package::from_toml(&contents)?;
    Ok(pkg_toml)
}

pub(crate) fn fetch_apply_patches(
    recipe_dir: &Path,
    patches: &Vec<String>,
    script: &Option<String>,
    source_dir_tmp: &PathBuf,
    logger: &PtyOut,
) -> Result<()> {
    for patch_name in patches {
        let patch_file = recipe_dir.join(patch_name);
        if !patch_file.is_file() {
            bail_other_err!("Failed to find patch file {:?}", patch_file.display());
        }

        let patch = fs::read_to_string(&patch_file).map_err(|err| {
            format!(
                "failed to read patch file '{}': {}\n{:#?}",
                patch_file.display(),
                err,
                err
            )
        })?;

        let mut command = Command::new("patch");
        command.arg("--directory").arg(source_dir_tmp);
        command.arg("--strip=1");
        run_command_stdin(command, patch.as_bytes(), logger)?;
    }
    let _: () = if let Some(script) = script {
        let mut command = Command::new("bash");
        command.arg("-ex");
        command.env(
            "COOKBOOK_ROOT",
            std::env::current_dir()
                .map_err(wrap_io_err!(Path::new("."), "Finding cookbook root"))?,
        );
        command.current_dir(source_dir_tmp);
        run_command_stdin(
            command,
            format!("{SHARED_PRESCRIPT}\n{script}").as_bytes(),
            logger,
        )?;
    };
    Ok(())
}

pub(crate) fn fetch_apply_source_info(
    recipe: &CookRecipe,
    source_identifier: String,
    patch_identifier: String,
) -> Result<(String, String)> {
    let ident = crate::cook::ident::get_ident();
    let info = SourceIdentifier {
        commit_identifier: ident.commit.to_string(),
        time_identifier: ident.time.to_string(),
        source_identifier,
        patch_identifier,
    };

    fetch_apply_source_info_from_remote(recipe, &info)?;

    Ok((info.source_identifier, info.patch_identifier))
}

pub(crate) fn fetch_apply_source_info_from_remote(
    recipe: &CookRecipe,
    info: &SourceIdentifier,
) -> Result<()> {
    let target_dir = create_target_dir(&recipe.dir, recipe.target)?;
    let source_toml_path = target_dir.join("source_info.toml");
    serialize_and_write(&source_toml_path, &info)?;
    Ok(())
}

pub fn fetch_get_source_info(recipe: &CookRecipe) -> Result<SourceIdentifier> {
    let target_dir = recipe.target_dir();
    let source_toml_path = target_dir.join("source_info.toml");
    read_toml(&source_toml_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn reapplying_patches_after_reset_clean() {
        let root = std::env::temp_dir().join(format!("cookbook-fetch-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let sub = root.join("sub");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&sub).unwrap();

        git(&sub, &["init", "-q"]);
        fs::write(sub.join("a"), "a\n").unwrap();
        git(&sub, &["add", "a"]);
        git(&sub, &["commit", "-q", "-m", "sub"]);

        git(&source, &["init", "-q"]);
        fs::write(source.join(".gitignore"), "ignored.txt\n").unwrap();
        git(&source, &["add", ".gitignore"]);
        git(
            &source,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub.to_str().unwrap(),
                "sub",
            ],
        );
        git(&source, &["commit", "-q", "-m", "base"]);

        let patch = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+new\n--- /dev/null\n+++ b/ignored.txt\n@@ -0,0 +1 @@\n+ignored\n";
        fs::write(root.join("create.patch"), patch).unwrap();
        let patches = vec!["create.patch".to_string()];

        fetch_apply_patches(&root, &patches, &None, &source, &None).unwrap();
        fs::write(source.join("sub/untracked"), "x\n").unwrap();

        git_reset_clean(&None, &source).unwrap();
        fetch_apply_patches(&root, &patches, &None, &source, &None).unwrap();
        assert_eq!(fs::read_to_string(source.join("new.txt")).unwrap(), "new\n");
        assert!(!source.join("sub/untracked").exists());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn zip_source_preserves_root_files_and_directories() {
        crate::config::init_config();
        let root = std::env::temp_dir().join(format!("cookbook-zip-test-{}", std::process::id()));
        fs::create_dir_all(root.join("source")).unwrap();
        let archive = root.join("source.tar");
        fs::write(
            &archive,
            b"\x50\x4b\x03\x04\x14\x00\x00\x00\x00\x00\x00\x00\x21\x50\x5b\xf9\xf4\x16\x04\x00\x00\x00\x04\x00\x00\x00\x08\x00\x00\x00root.txtroot\
              \x50\x4b\x03\x04\x14\x00\x00\x00\x00\x00\x00\x00\x21\x50\xe9\xc2\xc9\xaa\x06\x00\x00\x00\x06\x00\x00\x00\x0f\x00\x00\x00nested/file.txtnested\
              \x50\x4b\x01\x02\x14\x03\x14\x00\x00\x00\x00\x00\x00\x00\x21\x50\x5b\xf9\xf4\x16\x04\x00\x00\x00\x04\x00\x00\x00\x08\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x80\x01\x00\x00\x00\x00root.txt\
              \x50\x4b\x01\x02\x14\x03\x14\x00\x00\x00\x00\x00\x00\x00\x21\x50\xe9\xc2\xc9\xaa\x06\x00\x00\x00\x06\x00\x00\x00\x0f\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x80\x01\x2a\x00\x00\x00nested/file.txt\
              \x50\x4b\x05\x06\x00\x00\x00\x00\x02\x00\x02\x00\x73\x00\x00\x00\x5d\x00\x00\x00\x00\x00",
        )
        .unwrap();
        fetch_extract_archive(archive, &root.join("source"), &None).unwrap();
        assert_eq!(fs::read(root.join("source/root.txt")).unwrap(), b"root");
        assert_eq!(
            fs::read(root.join("source/nested/file.txt")).unwrap(),
            b"nested"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
