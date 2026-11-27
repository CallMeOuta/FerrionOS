mod check;
mod disk;
mod engine;
mod generate;
mod keyroll;
mod maps;
mod model;
mod op;
mod readonly;
mod ring;
mod rng;

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use engine::{Engine, Profile, Stats};
use op::{Step, parse_step};
use rng::Rng;

const MAX_SHRINK_RUNS: usize = 600;
const SHRINK_BUDGET: Duration = Duration::from_secs(300);
const PROGRESS_EVERY: Duration = Duration::from_secs(15);
const MAX_THREADS: usize = 64;

thread_local! {
    static PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

struct Failure {
    step: usize,
    message: String,
}

struct Run {
    steps: Vec<Step>,
    stats: Stats,
    failure: Option<Failure>,
}

fn guarded<T>(work: impl FnOnce() -> Result<T, Failure>) -> Result<T, Failure> {
    PANIC.with(|panic| *panic.borrow_mut() = None);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
        Ok(result) => result,
        Err(_) => Err(Failure {
            step: usize::MAX,
            message: format!(
                "RedoxFS panicked: {}",
                PANIC.with(|panic| panic.borrow_mut().take()).unwrap_or_default()
            ),
        }),
    }
}

fn generate_and_run(seed: u64, ops: u64, barriers: bool) -> Run {
    let mut steps = Vec::new();
    let mut stats = Stats::default();
    let result = guarded(|| {
        let mut engine = Engine::new(Profile::for_seed(seed, barriers), seed).map_err(|message| Failure {
            step: 0,
            message,
        })?;
        let mut rng = Rng::new(seed);
        while engine.stats.ops < ops {
            let start = steps.len();
            generate::generate(&engine, &mut rng, &mut steps);
            for index in start..steps.len() {
                let outcome = engine.step(&steps[index]);
                stats = engine.stats.clone();
                outcome.map_err(|message| Failure {
                    step: index,
                    message,
                })?;
            }
        }
        engine.finish().map_err(|message| Failure {
            step: steps.len().saturating_sub(1),
            message: format!("final check: {message}"),
        })?;
        stats = engine.stats.clone();
        Ok(())
    });
    let failure = result.err().map(|mut failure| {
        if failure.step == usize::MAX {
            failure.step = steps.len().saturating_sub(1);
        }
        failure
    });
    Run {
        steps,
        stats,
        failure,
    }
}

fn replay(seed: u64, barriers: bool, steps: &[Step]) -> Result<Stats, Failure> {
    let mut last = 0;
    let result = guarded(|| {
        let mut engine = Engine::new(Profile::for_seed(seed, barriers), seed).map_err(|message| Failure {
            step: 0,
            message,
        })?;
        for (index, step) in steps.iter().enumerate() {
            last = index;
            engine.step(step).map_err(|message| Failure {
                step: index,
                message,
            })?;
        }
        engine.finish().map_err(|message| Failure {
            step: steps.len().saturating_sub(1),
            message: format!("final check: {message}"),
        })?;
        Ok(engine.stats.clone())
    });
    result.map_err(|mut failure| {
        if failure.step == usize::MAX {
            failure.step = last;
        }
        failure
    })
}

fn shrink(seed: u64, barriers: bool, steps: &[Step], failure: &Failure) -> (Vec<Step>, String) {
    let started = Instant::now();
    let mut current: Vec<Step> = steps[..=failure.step.min(steps.len() - 1)].to_vec();
    let mut message = failure.message.clone();
    let mut chunk = current.len().div_ceil(2).max(1);
    let mut runs = 0;
    loop {
        let mut index = 0;
        let mut removed = false;
        while index < current.len() && runs < MAX_SHRINK_RUNS && started.elapsed() < SHRINK_BUDGET {
            let end = (index + chunk).min(current.len());
            let mut candidate = current[..index].to_vec();
            candidate.extend_from_slice(&current[end..]);
            runs += 1;
            match replay(seed, barriers, &candidate) {
                Err(found) => {
                    candidate.truncate(found.step + 1);
                    current = candidate;
                    message = found.message;
                    removed = true;
                }
                Ok(_) => index += chunk,
            }
        }
        if runs >= MAX_SHRINK_RUNS || started.elapsed() >= SHRINK_BUDGET {
            break;
        }
        if !removed {
            if chunk == 1 {
                break;
            }
            chunk = chunk.div_ceil(2);
        }
    }
    (current, message)
}

fn failures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("failures")
}

fn report(seed: u64, ops: u64, barriers: bool, run: &Run, shrink_allowed: bool) -> String {
    let failure = run.failure.as_ref().expect("failure");
    let mut text = format!(
        "FAIL seed {seed} ({ops} operations, profile {:?})\n  at step {}: {}\n  failing step: {}\n",
        Profile::for_seed(seed, barriers),
        failure.step,
        failure.message,
        run.steps.get(failure.step).map(|step| step.to_string()).unwrap_or_default()
    );
    if !shrink_allowed || run.steps.is_empty() {
        return text;
    }
    let (minimal, message) = shrink(seed, barriers, &run.steps, failure);
    let dir = failures_dir();
    let _ = std::fs::create_dir_all(&dir);
    let (suffix, mode) = if barriers { ("", "") } else { ("-flush", " flush-on-unmount") };
    let path = dir.join(format!("seed-{seed}{suffix}.txt"));
    let mut file = format!("seed {seed}{mode}\n");
    for step in minimal.iter() {
        file.push_str(&step.to_string());
        file.push('\n');
    }
    let _ = std::fs::write(&path, &file);
    text.push_str(&format!(
        "  minimal replay ({} steps, {}): sim/redoxfs/run.sh replay {}\n",
        minimal.len(),
        message,
        path.display()
    ));
    for step in minimal.iter() {
        text.push_str(&format!("    {step}\n"));
    }
    text
}

fn read_regressions() -> Vec<(u64, u64, String)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("regressions.txt");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.splitn(3, ' ');
        let seed = words.next().and_then(|word| word.parse().ok());
        let ops = words.next().and_then(|word| word.parse().ok());
        if let (Some(seed), Some(ops)) = (seed, ops) {
            result.push((seed, ops, words.next().unwrap_or("").to_string()));
        }
    }
    result
}

fn run_many(
    jobs: Vec<(u64, u64)>,
    barriers: bool,
    threads: usize,
    shrink_limit: usize,
    label: &str,
) -> (Stats, Vec<String>) {
    let jobs = Arc::new(jobs);
    let next = Arc::new(AtomicU64::new(0));
    let shrinks = Arc::new(AtomicU64::new(0));
    let (sender, receiver) = mpsc::channel::<(Stats, Option<String>)>();
    let total_ops: u64 = jobs.iter().map(|(_, ops)| ops).sum();
    let started = Instant::now();
    let mut workers = Vec::new();
    for _ in 0..threads {
        let jobs = Arc::clone(&jobs);
        let next = Arc::clone(&next);
        let shrinks = Arc::clone(&shrinks);
        let sender = sender.clone();
        workers.push(std::thread::spawn(move || {
            loop {
                let index = next.fetch_add(1, Ordering::SeqCst) as usize;
                let Some(&(seed, ops)) = jobs.get(index) else {
                    break;
                };
                let run = generate_and_run(seed, ops, barriers);
                let report = run.failure.as_ref().map(|_| {
                    let allowed = (shrinks.fetch_add(1, Ordering::SeqCst) as usize) < shrink_limit;
                    report(seed, ops, barriers, &run, allowed)
                });
                if sender.send((run.stats, report)).is_err() {
                    break;
                }
            }
        }));
    }
    drop(sender);
    let mut stats = Stats::default();
    let mut failures = Vec::new();
    let mut seeds = 0u64;
    let mut last_progress = Instant::now();
    for (seed_stats, report) in receiver {
        seeds += 1;
        stats.add(&seed_stats);
        if let Some(report) = report {
            eprintln!("{report}");
            failures.push(report);
        }
        if last_progress.elapsed() >= PROGRESS_EVERY {
            last_progress = Instant::now();
            let elapsed = started.elapsed().as_secs_f64();
            eprintln!(
                "{label}: {seeds}/{} seeds, {} of {total_ops} operations, {:.0} operations/s, {} failures",
                jobs.len(),
                stats.ops,
                stats.ops as f64 / elapsed,
                failures.len()
            );
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
    (stats, failures)
}

fn summary(label: &str, seeds: usize, stats: &Stats, elapsed: Duration, failures: usize) {
    let seconds = elapsed.as_secs_f64();
    println!(
        "{label}: {seeds} seeds, {} operations in {seconds:.1} s ({:.0} operations/s), {failures} failing seeds",
        stats.ops,
        stats.ops as f64 / seconds
    );
    println!(
        "  write boundaries guarded {}, flushes {}, crashes {} (mounted {}), crash boundaries mounted {}, read errors {}, ENOSPC {}, remounts {}, state checks {} (durable mounts {}), failed operations {}, allocator layouts that differ with equal free sets {}",
        stats.writes,
        stats.flushes,
        stats.crashes,
        stats.crash_mounts,
        stats.boundaries,
        stats.read_errors,
        stats.enospc,
        stats.remounts,
        stats.checks,
        stats.durable_mounts,
        stats.failed_ops,
        stats.layout_differences
    );
}

fn usage() -> ! {
    eprintln!(
        "usage: redoxfs-sim run SEEDS OPS [--first SEED] [--threads N] [--shrink N] [--flush-on-unmount]\n       redoxfs-sim seed SEED OPS [--flush-on-unmount]\n       redoxfs-sim replay FILE\n       redoxfs-sim maps SEEDS OPS [--first SEED] [--threads N]\n       redoxfs-sim keyroll SEEDS OPS [--first SEED] [--threads N]\n       redoxfs-sim ringdrain [SEEDS]\n       redoxfs-sim readonly"
    );
    std::process::exit(2)
}

fn run_keyroll(first: u64, seeds: u64, ops: u64, threads: usize) -> usize {
    let started = Instant::now();
    let next = AtomicU64::new(first);
    let last = first + seeds;
    let totals = std::sync::Mutex::new((0u64, 0u64, 0u64, 0u64, Vec::new()));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let seed = next.fetch_add(1, Ordering::SeqCst);
                    if seed >= last {
                        break;
                    }
                    let result = guarded(|| {
                        keyroll::run_seed(seed, ops).map_err(|message| Failure { step: 0, message })
                    });
                    let mut totals = totals.lock().unwrap_or_else(|poison| poison.into_inner());
                    match result {
                        Ok(stats) => {
                            totals.0 += stats.ops;
                            totals.1 += stats.injected_errors;
                            totals.2 += stats.retries;
                            totals.3 += stats.exhausted;
                        }
                        Err(failure) => {
                            eprintln!("FAIL keyroll seed {seed}: {}", failure.message);
                            totals.4.push(seed);
                        }
                    }
                }
            });
        }
    });
    let (ops_done, injected_errors, retries, exhausted, failed) =
        totals.into_inner().unwrap_or_else(|poison| poison.into_inner());
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "keyroll: {seeds} seeds, {ops_done} rolls in {seconds:.1} s, {} failing seeds",
        failed.len()
    );
    println!(
        "  injected write errors {injected_errors}, retries {retries}, exhausted {exhausted}"
    );
    failed.len()
}

fn run_maps(first: u64, seeds: u64, ops: u64, threads: usize) -> usize {
    let started = Instant::now();
    let next = AtomicU64::new(first);
    let last = first + seeds;
    let totals = std::sync::Mutex::new((0u64, 0u64, 0u64, 0u64, 0u64, 0u64, Vec::new()));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let seed = next.fetch_add(1, Ordering::SeqCst);
                    if seed >= last {
                        break;
                    }
                    let result = guarded(|| {
                        maps::run_seed(seed, ops).map_err(|message| Failure { step: 0, message })
                    });
                    let mut totals = totals.lock().unwrap_or_else(|poison| poison.into_inner());
                    match result {
                        Ok(stats) => {
                            totals.0 += stats.ops;
                            totals.1 += stats.maps;
                            totals.2 += stats.mapped_writes;
                            totals.3 += stats.reads;
                            totals.4 += stats.node_reuses;
                            totals.5 += stats.failed_commits;
                        }
                        Err(failure) => {
                            eprintln!("FAIL maps {}", failure.message);
                            totals.6.push(seed);
                        }
                    }
                }
            });
        }
    });
    let (ops_done, mapped, mapped_writes, reads, reuses, failed_commits, failed) =
        totals.into_inner().unwrap_or_else(|poison| poison.into_inner());
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "maps: {seeds} seeds, {ops_done} operations in {seconds:.1} s ({:.0} operations/s), {} failing seeds",
        ops_done as f64 / seconds,
        failed.len()
    );
    println!(
        "  maps {mapped}, mapped writes {mapped_writes}, reads {reads}, node reuses {reuses}, failed commits {failed_commits}"
    );
    failed.len()
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|location| format!(" at {}:{}", location.file(), location.line()))
            .unwrap_or_default();
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        PANIC.with(|panic| *panic.borrow_mut() = Some(format!("{payload}{location}")));
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else { usage() };
    match command.as_str() {
        "run" => {
            let seeds: u64 = args.get(1).and_then(|arg| arg.parse().ok()).unwrap_or_else(|| usage());
            let ops: u64 = args.get(2).and_then(|arg| arg.parse().ok()).unwrap_or_else(|| usage());
            let mut first = 1u64;
            let mut threads = std::thread::available_parallelism().map_or(4, |count| count.get() / 2).max(1);
            let mut shrink_limit = 8usize;
            let mut barriers = true;
            let mut index = 3;
            while index < args.len() {
                if args[index] == "--flush-on-unmount" {
                    barriers = false;
                    index += 1;
                    continue;
                }
                let value = args.get(index + 1).and_then(|arg| arg.parse::<u64>().ok()).unwrap_or_else(|| usage());
                match args[index].as_str() {
                    "--first" => first = value,
                    "--threads" => threads = (value as usize).clamp(1, MAX_THREADS),
                    "--shrink" => shrink_limit = value as usize,
                    _ => usage(),
                }
                index += 2;
            }
            let regressions = read_regressions();
            let mut failed = 0;
            if !regressions.is_empty() {
                let started = Instant::now();
                let jobs: Vec<(u64, u64)> = regressions.iter().map(|(seed, ops, _)| (*seed, *ops)).collect();
                let (stats, failures) = run_many(jobs, true, threads, shrink_limit, "regressions");
                summary("regression seeds", regressions.len(), &stats, started.elapsed(), failures.len());
                failed += failures.len();
            }
            let started = Instant::now();
            let jobs: Vec<(u64, u64)> = (first..first + seeds).map(|seed| (seed, ops)).collect();
            let (stats, failures) = run_many(jobs, barriers, threads, shrink_limit, "seeds");
            summary("seeds", seeds as usize, &stats, started.elapsed(), failures.len());
            failed += failures.len();
            std::process::exit(if failed == 0 { 0 } else { 1 });
        }
        "seed" => {
            let seed: u64 = args.get(1).and_then(|arg| arg.parse().ok()).unwrap_or_else(|| usage());
            let ops: u64 = args.get(2).and_then(|arg| arg.parse().ok()).unwrap_or_else(|| usage());
            let barriers = match args.get(3).map(String::as_str) {
                None => true,
                Some("--flush-on-unmount") => false,
                Some(_) => usage(),
            };
            let started = Instant::now();
            let run = generate_and_run(seed, ops, barriers);
            summary("seed", 1, &run.stats, started.elapsed(), usize::from(run.failure.is_some()));
            if run.failure.is_some() {
                eprintln!("{}", report(seed, ops, barriers, &run, true));
                std::process::exit(1);
            }
        }
        "replay" => {
            let path = args.get(1).unwrap_or_else(|| usage());
            let text = std::fs::read_to_string(path).unwrap_or_else(|err| {
                eprintln!("{path}: {err}");
                std::process::exit(2)
            });
            let mut lines = text.lines();
            let (seed, barriers): (u64, bool) = lines
                .next()
                .and_then(|line| line.strip_prefix("seed "))
                .and_then(|rest| match rest.trim().split_once(' ') {
                    None => Some((rest.trim().parse().ok()?, true)),
                    Some((seed, "flush-on-unmount")) => Some((seed.parse().ok()?, false)),
                    Some(_) => None,
                })
                .unwrap_or_else(|| usage());
            let steps: Vec<Step> = lines
                .filter(|line| !line.trim().is_empty())
                .map(|line| {
                    parse_step(line).unwrap_or_else(|| {
                        eprintln!("cannot parse step: {line}");
                        std::process::exit(2)
                    })
                })
                .collect();
            match replay(seed, barriers, &steps) {
                Ok(stats) => {
                    println!("replay passed: {} operations", stats.ops);
                }
                Err(failure) => {
                    println!(
                        "replay failed at step {}: {}\n  {}",
                        failure.step,
                        failure.message,
                        steps.get(failure.step).map(|step| step.to_string()).unwrap_or_default()
                    );
                    std::process::exit(1);
                }
            }
        }
        "maps" => {
            let seeds: u64 = args.get(1).and_then(|arg| arg.parse().ok()).unwrap_or_else(|| usage());
            let ops: u64 = args.get(2).and_then(|arg| arg.parse().ok()).unwrap_or_else(|| usage());
            let mut first = 1u64;
            let mut threads = std::thread::available_parallelism().map_or(4, |count| count.get() / 2).max(1);
            let mut index = 3;
            while index < args.len() {
                let value = args.get(index + 1).and_then(|arg| arg.parse::<u64>().ok()).unwrap_or_else(|| usage());
                match args[index].as_str() {
                    "--first" => first = value,
                    "--threads" => threads = (value as usize).clamp(1, MAX_THREADS),
                    _ => usage(),
                }
                index += 2;
            }
            let failed = run_maps(first, seeds, ops, threads);
            std::process::exit(if failed == 0 { 0 } else { 1 });
        }
        "keyroll" => {
            let seeds: u64 = args.get(1).and_then(|arg| arg.parse().ok()).unwrap_or_else(|| usage());
            let ops: u64 = args.get(2).and_then(|arg| arg.parse().ok()).unwrap_or_else(|| usage());
            let mut first = 1u64;
            let mut threads = std::thread::available_parallelism().map_or(4, |count| count.get() / 2).max(1);
            let mut index = 3;
            while index < args.len() {
                let value = args.get(index + 1).and_then(|arg| arg.parse::<u64>().ok()).unwrap_or_else(|| usage());
                match args[index].as_str() {
                    "--first" => first = value,
                    "--threads" => threads = (value as usize).clamp(1, MAX_THREADS),
                    _ => usage(),
                }
                index += 2;
            }
            let failed = run_keyroll(first, seeds, ops, threads);
            std::process::exit(if failed == 0 { 0 } else { 1 });
        }
        "ringdrain" => {
            let seeds: u64 = args.get(1).and_then(|arg| arg.parse().ok()).unwrap_or(200);
            let failed = ring::run_ringdrain(seeds);
            std::process::exit(if failed == 0 { 0 } else { 1 });
        }
        "readonly" => {
            let failed = readonly::run_readonly();
            std::process::exit(if failed == 0 { 0 } else { 1 });
        }
        _ => usage(),
    }
}
