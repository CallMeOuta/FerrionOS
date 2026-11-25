#![forbid(unsafe_code)]

use std::io::Write;
use std::path::{Path, PathBuf};

use rune::time::Instant;

use rune::text::{
    Direction, FontDirError, MAX_FONT_DIRS, MAX_FONT_FILE_BYTES, MAX_FONT_FILES, MAX_SCAN_DEPTH,
    collect_font_files, configured_font_dirs, fontconfig_files_loaded, fontconfig_system_families,
};
use rune::tok::{self, type_};
use rune::{App, AppSpec, Cx, El, col, text};

const FULL_ROWS: &[(&str, &str)] = &[
    ("row-latn", "hello.txt"),
    ("row-ar", "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}.txt"),
    ("row-he", "\u{05E9}\u{05DC}\u{05D5}\u{05DD}.txt"),
    ("row-deva", "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}.txt"),
    ("row-thai", "\u{0E2A}\u{0E27}\u{0E31}\u{0E2A}\u{0E14}\u{0E35}.txt"),
    ("row-ja", "\u{65E5}\u{672C}\u{8A9E}\u{306E}\u{30D5}\u{30A1}\u{30A4}\u{30EB}.txt"),
    ("row-zh", "\u{4E2D}\u{6587}\u{6587}\u{4EF6}.txt"),
    ("row-ko", "\u{D55C}\u{AD6D}\u{C5B4}\u{D30C}\u{C77C}.txt"),
    ("row-cjk-mix", "\u{65E5}\u{672C}\u{8A9E}_\u{4E2D}\u{6587}_\u{D55C}\u{AD6D}\u{C5B4}.txt"),
    ("row-sym", "\u{2605}\u{2713}\u{2318}\u{2709}.txt"),
];

const LATIN_ROWS: &[(&str, &str)] = &[
    ("row-latn-0", "hello.txt"),
    ("row-latn-1", "readme.md"),
    ("row-latn-2", "config.toml"),
    ("row-latn-3", "main.rs"),
];

struct State {
    latin_only: bool,
}

fn parse_config() -> (bool, bool, Option<PathBuf>) {
    let mut latin_only = false;
    let mut exit_on_ready = false;
    let mut check_bounds = None;
    let args = rune::tooling::extra_args();
    for arg in args {
        if arg == "--latin-only" {
            latin_only = true;
        } else if arg == "--exit-on-ready" {
            exit_on_ready = true;
        } else if let Some(rest) = arg.strip_prefix("--check-bounds=") {
            check_bounds = Some(PathBuf::from(rest));
        }
    }
    (latin_only, exit_on_ready, check_bounds)
}

fn read_vm_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let token = rest.split_whitespace().next()?;
            return token.parse::<u64>().ok();
        }
    }
    None
}

fn count_mapped_font_files() -> usize {
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return 0;
    };
    maps.lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.ends_with(".ttf")
                || lower.ends_with(".otf")
                || lower.ends_with(".ttc")
                || lower.ends_with(".otc")
        })
        .count()
}

fn count_vulkan_maps() -> usize {
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return 0;
    };
    maps.lines().filter(|line| line.contains("libvulkan")).count()
}

fn run_bounds_check(base: &Path) -> i32 {
    let mut ok = true;

    let too_many_dirs: Vec<PathBuf> = (0..=MAX_FONT_DIRS)
        .map(|i| base.join(format!("d{i}")))
        .collect();
    match collect_font_files(&too_many_dirs) {
        Err(FontDirError::TooManyDirs { count, limit }) => {
            println!("BOUNDS_TOO_MANY_DIRS:ok|count={count}|limit={limit}");
        }
        other => {
            println!("BOUNDS_TOO_MANY_DIRS:fail|{other:?}");
            ok = false;
        }
    }

    let deep_root = base.join("deep");
    let mut deep_cur = deep_root.clone();
    for i in 0..=MAX_SCAN_DEPTH {
        deep_cur = deep_cur.join(format!("l{i}"));
    }
    if std::fs::create_dir_all(&deep_cur).is_err() {
        println!("BOUNDS_DEPTH:fail|mkdir");
        return 1;
    }
    match collect_font_files(&[deep_root]) {
        Err(FontDirError::DepthExceeded { depth, limit, .. }) => {
            println!("BOUNDS_DEPTH:ok|depth={depth}|limit={limit}");
        }
        other => {
            println!("BOUNDS_DEPTH:fail|{other:?}");
            ok = false;
        }
    }

    let many_root = base.join("many");
    if std::fs::create_dir_all(&many_root).is_err() {
        println!("BOUNDS_TOO_MANY_FILES:fail|mkdir");
        return 1;
    }
    for i in 0..=MAX_FONT_FILES {
        let p = many_root.join(format!("f{i:04}.ttf"));
        if std::fs::write(&p, b"font").is_err() {
            println!("BOUNDS_TOO_MANY_FILES:fail|write");
            return 1;
        }
    }
    match collect_font_files(&[many_root]) {
        Err(FontDirError::TooManyFiles { count, limit }) => {
            println!("BOUNDS_TOO_MANY_FILES:ok|count={count}|limit={limit}");
        }
        other => {
            println!("BOUNDS_TOO_MANY_FILES:fail|{other:?}");
            ok = false;
        }
    }

    let large_root = base.join("large");
    if std::fs::create_dir_all(&large_root).is_err() {
        println!("BOUNDS_FILE_TOO_LARGE:fail|mkdir");
        return 1;
    }
    let big_file = large_root.join("oversized.ttc");
    let Ok(f) = std::fs::File::create(&big_file) else {
        println!("BOUNDS_FILE_TOO_LARGE:fail|create");
        return 1;
    };
    if f.set_len(MAX_FONT_FILE_BYTES + 1).is_err() {
        println!("BOUNDS_FILE_TOO_LARGE:fail|set_len");
        return 1;
    }
    match collect_font_files(&[large_root]) {
        Err(FontDirError::FileTooLarge { bytes, limit, .. }) => {
            println!("BOUNDS_FILE_TOO_LARGE:ok|bytes={bytes}|limit={limit}");
        }
        other => {
            println!("BOUNDS_FILE_TOO_LARGE:fail|{other:?}");
            ok = false;
        }
    }

    if ok { 0 } else { 1 }
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let t = tok::theme(true);
    let rows = if s.latin_only { LATIN_ROWS } else { FULL_ROWS };
    let mut list = col().w_full().h_full().padding(16.0).gap(10.0).fill(t.bg);
    for &(key, label) in rows {
        let row = text(label)
            .key(key)
            .w_full()
            .dir(Direction::Auto)
            .font(type_::BODY)
            .color(t.text);
        list = list.child(row);
    }
    list
}

fn print_stats(latin_only: bool, start: Instant) {
    let startup_ms = start.elapsed().as_secs_f64() * 1000.0;
    let rss = read_vm_rss_kb().unwrap_or(0);
    let mapped_fonts = count_mapped_font_files();
    let vulkan = count_vulkan_maps();
    let dirs = configured_font_dirs().len();
    let fc_files = fontconfig_files_loaded();
    let fc_families = fontconfig_system_families();
    println!(
        "STATS:latin_only={latin_only}|dirs={dirs}|startup_ms={startup_ms:.3}|rss_kb={rss}|mapped_fonts={mapped_fonts}|vulkan_maps={vulkan}|fc_files={fc_files}|fc_families={fc_families}"
    );
    std::io::stdout().flush().ok();
}

fn main() {
    let start = rune::time::now();
    let (latin_only, exit_on_ready, check_bounds) = parse_config();
    if let Some(dir) = check_bounds {
        std::process::exit(run_bounds_check(&dir));
    }
    let init = if latin_only {
        || State { latin_only: true }
    } else {
        || State { latin_only: false }
    };
    let spec = AppSpec::new("Scripts Demo", (480.0, 420.0), init, view);
    App::new(spec)
        .on_ready(move |_| {
            print_stats(latin_only, start);
            if exit_on_ready {
                std::process::exit(0);
            }
        })
        .run();
}
