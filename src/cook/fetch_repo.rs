use std::{
    cell::RefCell,
    io::{PipeWriter, Write},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use crate::cook::fs;
use pkg::{
    PackageName, RepoManager, Repository,
    callback::{Callback, PlainCallback, SilentCallback},
    net_backend::{CurlBackend, DownloadBackend},
};

pub enum BinaryRepo {
    Fresh(Repository),
    Cached(Repository),
    Unavailable(String),
}

thread_local! {
static BINARY_REPO: RefCell<Option<(RepoManager, BinaryRepo)>> = const { RefCell::new(None) };
}

fn repo_cache_path(repo_path: &Path, remote: &str, target: &str) -> PathBuf {
    repo_path.join(format!("repo_{remote}_{target}.toml"))
}

fn load_cached_repo(path: &Path) -> Option<Repository> {
    let metadata = std::fs::metadata(path).ok()?;

    if !crate::config::get_config().cook.offline {
        let stale_time = std::time::SystemTime::now().checked_sub(Duration::from_secs(8 * 3600))?;
        if metadata.modified().ok()? < stale_time {
            let _ = std::fs::remove_file(path);
            return None;
        }
    }

    let toml_str = std::fs::read_to_string(path).ok()?;
    Repository::from_toml(&toml_str).ok()
}

fn init_binary_repo() -> (RepoManager, BinaryRepo) {
    let callback = Rc::new(RefCell::new(SilentCallback::new()));
    let download_backend = CurlBackend::new().expect("Curl not found");
    let mut repo = RepoManager::new(callback, Box::new(download_backend));
    let target = redoxer::target();
    let remote_source = crate::config::translate_mirror(crate::REMOTE_PKG_SOURCE);
    repo.add_remote(&remote_source, target)
        .expect("Unable to add remote");

    let repo_path = PathBuf::from("build/remotes");
    repo.set_download_path(repo_path.clone());
    let offline = crate::config::get_config().cook.offline;
    let cache_path = repo_cache_path(&repo_path, &repo.remotes[0], target);

    let state = if let Some(cached) = load_cached_repo(&cache_path) {
        if !offline {
            if let Err(e) = repo.sync_keys() {
                eprintln!("Unable to sync keys: {e}");
            }
        }
        BinaryRepo::Cached(cached)
    } else if offline {
        BinaryRepo::Unavailable(format!(
            "offline binary repository cache missing: {}",
            cache_path.display()
        ))
    } else if let Err(e) = repo.sync_keys() {
        BinaryRepo::Unavailable(format!("Unable to sync keys: {e}"))
    } else {
        match download_repo(&repo, &cache_path) {
            Ok(fresh) => BinaryRepo::Fresh(fresh),
            Err(e) => BinaryRepo::Unavailable(format!(
                "Unable to load server repo.toml, all recipes will build from source: {e}"
            )),
        }
    };
    repo.callback = Rc::new(RefCell::new(PlainCallback::new()));
    (repo, state)
}

fn download_repo(repo: &RepoManager, cache_path: &Path) -> crate::Result<Repository> {
    let (toml_str, _) = repo.get_package_toml(&PackageName::new("repo").unwrap())?;
    let repo = Repository::from_toml(&toml_str)?;
    fs::serialize_and_write(cache_path, &repo)?;
    Ok(repo)
}

pub fn try_get_binary_repo() -> crate::Result<(RepoManager, Repository)> {
    BINARY_REPO.with(|cell| {
        let mut opt = cell.borrow_mut();
        if opt.is_none() {
            *opt = Some(init_binary_repo());
        }
        let (repo, state) = opt.as_ref().unwrap();
        match state {
            BinaryRepo::Fresh(repo_toml) | BinaryRepo::Cached(repo_toml) => {
                Ok(((*repo).clone(), repo_toml.clone()))
            }
            BinaryRepo::Unavailable(reason) => {
                if crate::config::get_config().cook.offline {
                    Err(crate::Error::Other(reason.clone()))
                } else {
                    eprintln!("{reason}");
                    Ok(((*repo).clone(), Repository::default()))
                }
            }
        }
    })
}

pub fn get_binary_repo() -> (RepoManager, Repository) {
    match try_get_binary_repo() {
        Ok(pair) => pair,
        Err(reason) => {
            eprintln!("{reason}");
            BINARY_REPO.with(|cell| {
                let opt = cell.borrow();
                let (repo, _) = opt.as_ref().unwrap();
                ((*repo).clone(), Repository::default())
            })
        }
    }
}
pub fn get_binary_pubkey() -> PathBuf {
    BINARY_REPO.with(|cell| {
        let mut opt = cell.borrow_mut();
        if opt.is_none() {
            *opt = Some(init_binary_repo());
        }
        let (repo, _) = opt.as_ref().unwrap();
        let repo_path = PathBuf::from("build/remotes");
        repo_path.join(format!("pub_key_{}.toml", repo.remotes[0]))
    })
}

pub struct PlainPtyCallback {
    size: u64,
    unknown_size: bool,
    pos: u64,
    fetch_processed: usize,
    fetch_total: usize,
    interactive: bool,
    download_file: Option<String>,
    pty: PipeWriter,
}

impl PlainPtyCallback {
    pub fn new(pty: PipeWriter) -> Self {
        Self {
            size: 0,
            unknown_size: false,
            pos: 0,
            fetch_processed: 0,
            fetch_total: 0,
            interactive: false,
            download_file: None,
            pty,
        }
    }

    pub fn set_interactive(&mut self, enabled: bool) {
        self.interactive = enabled;
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }

    pub fn format_size(bytes: u64) -> String {
        if bytes == 0 {
            return "0 B".to_string();
        }
        const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
        let i = (bytes as f64).log(1024.0).floor() as usize;
        let size = bytes as f64 / 1024.0_f64.powi(i as i32);
        format!("{:.2} {}", size, UNITS[i])
    }

    fn downloading_str(&self) -> &'static str {
        "Downloading"
    }
}

const RESET_LINE: &str = "\r\x1b[2K";

impl Callback for PlainPtyCallback {
    fn fetch_start(&mut self, initial_count: usize) {
        self.fetch_total = 0;
        self.fetch_processed = 0;
        self.fetch_package_increment(0, initial_count);
    }

    fn fetch_package_name(&mut self, pkg_name: &PackageName) {
        let _ = write!(&self.pty, " {}", pkg_name.as_str());
        self.flush();
    }

    fn fetch_package_increment(&mut self, added_processed: usize, added_count: usize) {
        self.fetch_processed += added_processed;
        self.fetch_total += added_count;

        let _ = write!(
            &self.pty,
            "{RESET_LINE}Fetching: [{}/{}]",
            self.fetch_processed, self.fetch_total
        );
        self.flush();
    }

    fn fetch_end(&mut self) {
        if self.fetch_processed == self.fetch_total {
            let _ = writeln!(&self.pty, "{RESET_LINE}Fetch complete.");
        } else {
            let _ = writeln!(&self.pty, "{RESET_LINE}Fetch incomplete.");
        }
    }

    fn download_start(&mut self, length: u64, file: &str) {
        self.size = length;
        self.unknown_size = length == 0;
        self.pos = 0;
        if !self.unknown_size {
            let _ = write!(&self.pty, "{RESET_LINE}{} {file}", self.downloading_str());
            self.download_file = Some(file.to_string());
            self.flush();
        }
    }

    fn download_increment(&mut self, downloaded: u64) {
        self.pos += downloaded;
        if self.unknown_size {
            self.size += downloaded;
        }
        if self.unknown_size {
            return;
        }

        let pos_mb = self.pos as f64 / 1_048_576.0;
        let size_mb = self.size as f64 / 1_048_576.0;
        let file_name = self.download_file.as_deref().unwrap_or("");
        let _ = write!(
            &self.pty,
            "{RESET_LINE}{} {} [{:.2} MB / {:.2} MB]",
            self.downloading_str(),
            file_name,
            pos_mb,
            size_mb
        );
        self.flush();
    }

    fn download_end(&mut self) {
        if !self.unknown_size {
            let _ = writeln!(&self.pty);
            self.download_file = None;
        }
    }
}
