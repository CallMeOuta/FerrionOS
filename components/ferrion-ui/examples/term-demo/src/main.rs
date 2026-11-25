#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Duration;

use rune::{App, AppHandle, AppSpec, Cx, El, Key, KeyEvent, Tx, col};
use rune_term::{
    Palette, Pty, PtyConfig, Selection, TermMetrics, TermView, Terminal, encode_key, encode_paste,
    extract_selection, read_stdin_winsize, srgb_to_rune,
};
use serde_json::{Value, json};

const MAX_PTY_WAIT_POLLS: usize = 500;
const PTY_POLL_SLICE: Duration = Duration::from_millis(10);
const MAX_CONTROL_COMMANDS: usize = 64;
const MAX_CONTROL_POLLS: usize = 1200;
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(50);

fn arg_value(flag: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next();
        }
        if let Some(rest) = arg.strip_prefix(&format!("{flag}=")) {
            return Some(rest.to_owned());
        }
    }
    None
}

fn has_flag(flag: &str) -> bool {
    std::env::args()
        .skip(1)
        .any(|a| a == flag || a.starts_with(&format!("{flag}=")))
}

fn print_flushed(line: &str) {
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{line}").is_ok() {
        out.flush().ok();
    }
}

fn drain_pty_until(pty: &Pty, term: &mut Terminal, needle: &str, timeout: Duration) -> bool {
    let start = rune::time::now();
    for _ in 0..MAX_PTY_WAIT_POLLS {
        for chunk in pty.try_read_chunks(64) {
            term.feed(&chunk);
        }
        if term
            .screen()
            .text_lines()
            .iter()
            .any(|l| l.contains(needle))
        {
            return true;
        }
        if pty.is_eof() || start.elapsed() >= timeout {
            break;
        }
        std::thread::sleep(PTY_POLL_SLICE);
    }
    for chunk in pty.try_read_chunks(64) {
        term.feed(&chunk);
    }
    term.screen()
        .text_lines()
        .iter()
        .any(|l| l.contains(needle))
}

fn self_winsize_cmd() -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_else(|| "rune-term-demo".to_owned());
    format!("'{exe}' --print-winsize\n")
}

fn run_pty_check(shell: &str, out_path: &str) -> i32 {
    let mut term = Terminal::new(80, 24);
    let mut steps: Vec<Value> = Vec::new();
    let cfg = PtyConfig::shell(shell, 80, 24);
    let mut pty = match Pty::spawn(&cfg) {
        Ok(p) => p,
        Err(err) => {
            let report = json!({
                "ok": false,
                "error": format!("spawn: {err}"),
                "steps": steps,
            });
            write_json(out_path, &report);
            return 1;
        }
    };
    steps.push(json!({"step": "spawn", "ok": true, "pid": pty.pid()}));

    let mut all_ok = true;
    let mut record = |name: &str, ok: bool, detail: String| {
        if !ok {
            all_ok = false;
        }
        steps.push(json!({"step": name, "ok": ok, "detail": detail}));
    };

    if let Err(e) = pty.write_all(b"printf '%s%s\\n' RUNE_TERM_ ECHO_OK\n") {
        record("echo", false, e.to_string());
    } else {
        let found = drain_pty_until(&pty, &mut term, "RUNE_TERM_ECHO_OK", Duration::from_secs(4));
        record("echo", found, format!("found={found}"));
    }

    let ws_cmd = self_winsize_cmd();
    if let Err(e) = pty.write_all(ws_cmd.as_bytes()) {
        record("winsize_initial", false, e.to_string());
    } else {
        let found = drain_pty_until(&pty, &mut term, "24 80", Duration::from_secs(4));
        record(
            "winsize_initial",
            found,
            format!("expected=24 80 found={found}"),
        );
    }

    match pty.resize(60, 18) {
        Ok(()) => {
            term.resize(60, 18);
            if let Err(e) = pty.write_all(ws_cmd.as_bytes()) {
                record("winsize_resized", false, e.to_string());
            } else {
                let found = drain_pty_until(&pty, &mut term, "18 60", Duration::from_secs(4));
                record(
                    "winsize_resized",
                    found,
                    format!("expected=18 60 found={found}"),
                );
            }
        }
        Err(e) => record("winsize_resized", false, format!("resize failed: {e}")),
    }

    let _ = pty.write_all(b"exit 0\n").ok();
    let exit_code = match pty.wait_timeout(Duration::from_secs(4)) {
        Ok(Some(status)) => {
            let code = status.code().unwrap_or(-1);
            record("reap", status.success(), format!("status={code}"));
            code
        }
        Ok(None) => {
            record(
                "reap",
                false,
                "child did not exit before timeout".to_owned(),
            );
            -1
        }
        Err(e) => {
            record("reap", false, e.to_string());
            -1
        }
    };

    for chunk in pty.try_read_chunks(64) {
        term.feed(&chunk);
    }

    match pty.shutdown() {
        Ok(_) => record("shutdown", true, "joined".to_owned()),
        Err(e) => record("shutdown", false, e.to_string()),
    }

    let report = json!({
        "ok": all_ok,
        "exit_code": exit_code,
        "steps": steps,
        "screen": term.screen().to_snapshot_json(),
    });
    write_json(out_path, &report);
    if all_ok { 0 } else { 1 }
}

fn run_pty_signals_check(shell: &str, out_path: &str) -> i32 {
    let mut term = Terminal::new(80, 24);
    let mut steps: Vec<Value> = Vec::new();
    let cfg = PtyConfig::shell(shell, 80, 24);
    let mut pty = match Pty::spawn(&cfg) {
        Ok(p) => p,
        Err(err) => {
            let report = json!({
                "ok": false,
                "error": format!("spawn: {err}"),
                "steps": steps,
            });
            write_json(out_path, &report);
            return 1;
        }
    };
    steps.push(json!({"step": "spawn", "ok": true, "pid": pty.pid()}));

    let mut all_ok = true;
    let mut record = |name: &str, ok: bool, detail: String| {
        if !ok {
            all_ok = false;
        }
        steps.push(json!({"step": name, "ok": ok, "detail": detail}));
    };

    if let Err(e) = pty.write_all(b"printf '%s%s\\n' RUNE_TERM_ JOB_OK\n") {
        record("job_control", false, e.to_string());
    } else {
        let ready = drain_pty_until(&pty, &mut term, "RUNE_TERM_JOB_OK", Duration::from_secs(4));
        let no_jc = term
            .screen()
            .text_lines()
            .iter()
            .any(|l| l.contains("no job control"));
        record(
            "job_control",
            ready && !no_jc,
            format!("ready={ready} no_job_control={no_jc}"),
        );
    }

    if let Err(e) = pty.write_all(b"trap 'printf \"\\n%s%s\\n\" RUNE_TERM_ WINCH_OK' WINCH\n") {
        record("sigwinch", false, e.to_string());
    } else {
        std::thread::sleep(Duration::from_millis(150));
        match pty.resize(60, 18) {
            Ok(()) => {
                term.resize(60, 18);
                std::thread::sleep(Duration::from_millis(150));
                let _ = pty
                    .write_all(b"printf '%s%s\\n' RUNE_TERM_ POST_WINCH\n")
                    .ok();
                let winch_found = drain_pty_until(
                    &pty,
                    &mut term,
                    "RUNE_TERM_WINCH_OK",
                    Duration::from_secs(4),
                );
                record("sigwinch", winch_found, format!("found={winch_found}"));
            }
            Err(e) => record("sigwinch", false, format!("resize failed: {e}")),
        }
    }

    if let Err(e) = pty.write_all(b"sleep 30\n") {
        record("sigint", false, e.to_string());
    } else {
        std::thread::sleep(Duration::from_millis(250));
        for chunk in pty.try_read_chunks(64) {
            term.feed(&chunk);
        }
        if let Err(e) = pty.write_all(&[0x03]) {
            record("sigint", false, format!("ctrl-c write: {e}"));
        } else {
            std::thread::sleep(Duration::from_millis(200));
            if let Err(e) = pty.write_all(b"printf '%s%s\\n' RUNE_TERM_ AFTER_SIGINT\n") {
                record("sigint", false, format!("post-sigint write: {e}"));
            } else {
                let found = drain_pty_until(
                    &pty,
                    &mut term,
                    "RUNE_TERM_AFTER_SIGINT",
                    Duration::from_secs(4),
                );
                record("sigint", found, format!("found={found}"));
            }
        }
    }

    let _ = pty.write_all(b"exit 0\n").ok();
    let exit_code = match pty.wait_timeout(Duration::from_secs(4)) {
        Ok(Some(status)) => {
            let code = status.code().unwrap_or(-1);
            record("reap", status.success(), format!("status={code}"));
            code
        }
        Ok(None) => {
            record(
                "reap",
                false,
                "child did not exit before timeout".to_owned(),
            );
            -1
        }
        Err(e) => {
            record("reap", false, e.to_string());
            -1
        }
    };

    for chunk in pty.try_read_chunks(64) {
        term.feed(&chunk);
    }

    match pty.shutdown() {
        Ok(_) => record("shutdown", true, "joined".to_owned()),
        Err(e) => record("shutdown", false, e.to_string()),
    }

    let report = json!({
        "ok": all_ok,
        "exit_code": exit_code,
        "steps": steps,
        "screen": term.screen().to_snapshot_json(),
    });
    write_json(out_path, &report);
    if all_ok { 0 } else { 1 }
}

fn write_json(out_path: &str, value: &Value) {
    let serialized = serde_json::to_string_pretty(value).unwrap_or_else(|e| {
        eprintln!("term-demo: serialize error: {e}");
        std::process::exit(1);
    });
    if out_path == "-" {
        println!("{serialized}");
    } else if let Err(e) = fs::write(out_path, serialized) {
        eprintln!("term-demo: failed to write {out_path}: {e}");
        std::process::exit(1);
    }
}

struct DemoState {
    term: Terminal,
    view: RefCell<TermView>,
    selection: Option<Selection>,
    last_input: Vec<u8>,
}

impl DemoState {
    fn new(cols: u16, rows: u16, initial: &[u8]) -> Self {
        let mut term = Terminal::new(cols, rows);
        term.feed(initial);
        Self {
            term,
            view: RefCell::new(TermView::new(TermMetrics::default())),
            selection: None,
            last_input: Vec::new(),
        }
    }

    fn send_input(&mut self, bytes: &[u8]) {
        self.last_input.clear();
        self.last_input.extend_from_slice(bytes);
        self.term.feed(bytes);
    }
}

fn handle_key(s: &mut DemoState, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if ev.mods.shift && ev.is_command('c') {
        let sel = s.selection.unwrap_or(Selection {
            start: (0, 0),
            end: (
                s.term.screen().cols().saturating_sub(1),
                s.term.screen().rows().saturating_sub(1),
            ),
        });
        let text = extract_selection(s.term.screen(), sel);
        tx.copy(text);
        return true;
    }
    if ev.mods.shift && ev.is_command('v') {
        tx.read_clipboard(|st: &mut DemoState, content, _| {
            let bracketed = st.term.screen().modes().bracketed_paste;
            let bytes = encode_paste(content.text(), bracketed);
            st.send_input(&bytes);
        });
        return true;
    }
    if matches!(ev.key, Key::Escape) && s.selection.is_some() {
        s.selection = None;
        return true;
    }
    let app_cursor = s.term.screen().modes().app_cursor_keys;
    if let Some(bytes) = encode_key(ev, app_cursor) {
        s.send_input(&bytes);
        return true;
    }
    false
}

fn view(s: &DemoState, _: &mut Cx<DemoState>) -> El {
    let bg = srgb_to_rune(Palette::XTERM_DARK.default_bg);
    let grid_el = s
        .view
        .borrow_mut()
        .build(s.term.screen(), s.selection)
        .key("terminal-grid");
    col()
        .key("terminal-root")
        .w_full()
        .h_full()
        .focusable(true)
        .on_key(handle_key)
        .fill(bg)
        .child(grid_el)
}

fn spawn_control_file(path: String, handle: AppHandle<DemoState>) {
    std::thread::spawn(move || {
        let mut offset = 0u64;
        let mut handled = 0usize;
        for _ in 0..MAX_CONTROL_POLLS {
            if handled >= MAX_CONTROL_COMMANDS {
                break;
            }
            if let Ok(mut file) = fs::File::open(&path)
                && file.seek(SeekFrom::Start(offset)).is_ok()
            {
                let mut reader = BufReader::new(file);
                let mut line = String::new();
                while handled < MAX_CONTROL_COMMANDS {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(n) if line.ends_with('\n') => {
                            offset = offset.saturating_add(n as u64);
                            handled += 1;
                            let cmd = line.trim();
                            if cmd == "copy-row0" {
                                handle.update(|st, tx| {
                                    let sel = Selection {
                                        start: (0, 0),
                                        end: (st.term.screen().cols().saturating_sub(1), 0),
                                    };
                                    let text = extract_selection(st.term.screen(), sel);
                                    tx.copy(text.clone());
                                    print_flushed(&format!("COPIED:{text}"));
                                });
                            } else if cmd == "paste" {
                                handle.update(|_, tx| {
                                    tx.read_clipboard(|st: &mut DemoState, content, _| {
                                        let bracketed = st.term.screen().modes().bracketed_paste;
                                        let bytes = encode_paste(content.text(), bracketed);
                                        st.send_input(&bytes);
                                        let hex: String =
                                            bytes.iter().map(|b| format!("{b:02x}")).collect();
                                        print_flushed(&format!(
                                            "PASTED:bracketed={bracketed}:hex={hex}"
                                        ));
                                    });
                                });
                            } else if cmd == "enable-bp" {
                                handle.update(|st, _| {
                                    st.term.feed(b"\x1b[?2004h");
                                    print_flushed("ACK:enable-bp");
                                });
                            } else if cmd == "quit" {
                                print_flushed("ACK:quit");
                                handle.quit();
                                return;
                            }
                        }
                        _ => break,
                    }
                }
            }
            std::thread::sleep(CONTROL_POLL_INTERVAL);
        }
    });
}

fn main() {
    if has_flag("--print-winsize") {
        match read_stdin_winsize() {
            Ok((rows, cols)) => {
                println!("{rows} {cols}");
                return;
            }
            Err(err) => {
                eprintln!("term-demo: {err}");
                std::process::exit(1);
            }
        }
    }

    let cols = arg_value("--cols")
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(80);
    let rows = arg_value("--rows")
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(24);

    if let Some(out_path) = arg_value("--pty-check") {
        let shell = arg_value("--shell").unwrap_or_else(|| "/bin/sh".to_owned());
        let code = run_pty_check(&shell, &out_path);
        std::process::exit(code);
    }

    if let Some(out_path) = arg_value("--pty-signals-check") {
        let shell = arg_value("--shell").unwrap_or_else(|| "/bin/sh".to_owned());
        let code = run_pty_signals_check(&shell, &out_path);
        std::process::exit(code);
    }

    let initial_bytes = if let Some(feed_path) = arg_value("--feed") {
        fs::read(PathBuf::from(&feed_path)).unwrap_or_else(|e| {
            eprintln!("term-demo: failed to read {feed_path}: {e}");
            std::process::exit(1);
        })
    } else {
        b"\x1b[1;32mFerrionOS\x1b[0m Rune Terminal\r\nReady$ ".to_vec()
    };

    if let Some(dump_path) = arg_value("--dump-screen")
        && !has_flag("--snap")
        && !has_flag("--tree")
    {
        let mut term = Terminal::new(cols, rows);
        term.feed(&initial_bytes);
        write_json(&dump_path, &term.screen().to_snapshot_json());
        return;
    }

    let metrics = TermMetrics::default();
    let width = f32::from(cols) * metrics.cell_w;
    let height = f32::from(rows) * metrics.cell_h;
    let bytes_box: &'static [u8] = Box::leak(initial_bytes.into_boxed_slice());
    let init_fn: fn() -> DemoState = || DemoState::new(80, 24, b"");
    let app = App::new(AppSpec::new(
        "Rune Terminal",
        (width, height),
        init_fn,
        view,
    ))
    .init(move || DemoState::new(cols, rows, bytes_box))
    .on_ready(|_| print_flushed("TERM_READY"));
    let handle = app.handle();
    if let Some(path) = arg_value("--control-file") {
        spawn_control_file(path, handle);
    }
    app.run();
}
