#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Duration;

use rune::tok::{self, type_};
use rune::{
    App, AppHandle, AppSpec, Color, Cx, El, FileDialogOptions, FileDialogResult, Key, KeyEvent, Tx,
    WindowOptions, WindowSpec, col, text,
};

const MAX_CONTROL_COMMANDS: usize = 64;
const MAX_CONTROL_POLLS: usize = 2400;
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Default)]
struct WinState {
    name: &'static str,
    fixture_dir: Option<PathBuf>,
    last_answer: String,
    ok_count: u32,
    cancel_count: u32,
    err_count: u32,
    space_presses: u32,
}

fn print_flushed(line: &str) {
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{line}").is_ok() {
        drop(out.flush());
    }
}

fn arg_value(prefix: &str) -> Option<String> {
    std::env::args()
        .skip(1)
        .find_map(|a| a.strip_prefix(prefix).map(str::to_owned))
}

fn has_flag(flag: &str) -> bool {
    std::env::args().skip(1).any(|a| a == flag)
}

fn on_demo_key(s: &mut WinState, ev: &KeyEvent, tx: &mut Tx) -> bool {
    let Key::Text(ch) = &ev.key else {
        return false;
    };
    if ch == " " {
        s.space_presses = s.space_presses.saturating_add(1);
        print_flushed(&format!("DEMO_KEY:{}:count={}", s.name, s.space_presses));
        return true;
    }
    let Some(dir) = s.fixture_dir.clone() else {
        return false;
    };
    match ch.as_str() {
        "o" => {
            let opts = FileDialogOptions {
                title: "Open Test".to_owned(),
                directory: Some(dir),
                ..Default::default()
            }
            .filter("Filtered", &["json"]);
            tx.open_file(opts, |st: &mut WinState, res, _| {
                record_answer(st, "open", res);
            });
            print_flushed(&format!("ACK:open:{}", s.name));
            true
        }
        "f" => {
            let opts = FileDialogOptions {
                title: "Folder Test".to_owned(),
                directory: Some(dir),
                ..Default::default()
            };
            tx.open_folder(opts, |st: &mut WinState, res, _| {
                record_answer(st, "folder", res);
            });
            print_flushed(&format!("ACK:folder:{}", s.name));
            true
        }
        "s" => {
            let opts = FileDialogOptions {
                title: "Save Test".to_owned(),
                directory: Some(dir),
                file_name: Some("fresh.txt".to_owned()),
                ..Default::default()
            };
            tx.save_file(opts, |st: &mut WinState, res, _| {
                record_answer(st, "save", res);
            });
            print_flushed(&format!("ACK:save:{}", s.name));
            true
        }
        "w" => {
            let opts = FileDialogOptions {
                title: "Save Test".to_owned(),
                directory: Some(dir),
                file_name: Some("existing.txt".to_owned()),
                ..Default::default()
            };
            tx.save_file(opts, |st: &mut WinState, res, _| {
                record_answer(st, "save", res);
            });
            print_flushed(&format!("ACK:save:{}", s.name));
            true
        }
        "u" => {
            let opts = FileDialogOptions {
                title: "Open Test".to_owned(),
                directory: Some(dir),
                ..Default::default()
            };
            tx.open_file(opts, |st: &mut WinState, res, _| {
                record_answer(st, "open", res);
            });
            print_flushed(&format!("ACK:open:{}", s.name));
            true
        }
        _ => false,
    }
}

fn win_view(s: &WinState, _: &mut Cx<WinState>) -> El {
    let t = tok::theme(true);
    let summary = format!(
        "win={} ok={} cancel={} err={} keys={} last={}",
        s.name, s.ok_count, s.cancel_count, s.err_count, s.space_presses, s.last_answer
    );
    col()
        .key("demo-root")
        .role("window")
        .label(format!("Chooser Demo ({})", s.name))
        .w_full()
        .h_full()
        .padding(16.0)
        .gap(8.0)
        .focusable(true)
        .on_key(on_demo_key)
        .fill(Color {
            r: 0.08,
            g: 0.10,
            b: 0.14,
            a: 1.0,
        })
        .child(
            text(format!("Chooser Demo ({})", s.name))
                .font(type_::TITLE)
                .color(t.text),
        )
        .child(text(summary).font(type_::BODY).color(t.text_secondary))
}

fn record_answer(s: &mut WinState, kind: &'static str, result: FileDialogResult) {
    match result {
        Ok(Some(path)) => {
            s.ok_count = s.ok_count.saturating_add(1);
            let p = path.display().to_string();
            s.last_answer = format!("ok:{p}");
            print_flushed(&format!("ANSWER:{}:{kind}:ok:{p}", s.name));
        }
        Ok(None) => {
            s.cancel_count = s.cancel_count.saturating_add(1);
            s.last_answer = "cancel".to_owned();
            print_flushed(&format!("ANSWER:{}:{kind}:cancel", s.name));
        }
        Err(err) => {
            s.err_count = s.err_count.saturating_add(1);
            s.last_answer = format!("err:{}", err.0);
            print_flushed(&format!("ANSWER:{}:{kind}:err:{}", s.name, err.0));
        }
    }
}

fn handle_command(cmd: &str, main_h: &AppHandle<WinState>, aux_h: &AppHandle<WinState>) -> bool {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return false;
    }
    match parts[0] {
        "open" if parts.len() >= 3 => {
            let target = if parts[1] == "aux" { aux_h } else { main_h };
            let mut opts = FileDialogOptions {
                title: "Open Test".to_owned(),
                directory: Some(PathBuf::from(parts[2])),
                ..Default::default()
            };
            if let Some(ext) = parts.get(3) {
                opts = opts.filter("Filtered", &[ext]);
            }
            target.update(move |_, tx| {
                tx.open_file(opts, |s: &mut WinState, res, _| {
                    record_answer(s, "open", res);
                });
            });
            print_flushed(&format!("ACK:open:{}", parts[1]));
        }
        "open-twice" if parts.len() >= 3 => {
            let target = if parts[1] == "aux" { aux_h } else { main_h };
            let opts1 = FileDialogOptions {
                title: "Open First".to_owned(),
                directory: Some(PathBuf::from(parts[2])),
                ..Default::default()
            };
            let opts2 = FileDialogOptions {
                title: "Open Second".to_owned(),
                directory: Some(PathBuf::from(parts[2])),
                ..Default::default()
            };
            target.update(move |_, tx| {
                tx.open_file(opts1, |s: &mut WinState, res, _| {
                    record_answer(s, "open1", res);
                });
                tx.open_file(opts2, |s: &mut WinState, res, _| {
                    record_answer(s, "open2", res);
                });
            });
            print_flushed(&format!("ACK:open-twice:{}", parts[1]));
        }
        "save" if parts.len() >= 4 => {
            let target = if parts[1] == "aux" { aux_h } else { main_h };
            let opts = FileDialogOptions {
                title: "Save Test".to_owned(),
                directory: Some(PathBuf::from(parts[2])),
                file_name: Some(parts[3].to_owned()),
                ..Default::default()
            };
            target.update(move |_, tx| {
                tx.save_file(opts, |s: &mut WinState, res, _| {
                    record_answer(s, "save", res);
                });
            });
            print_flushed(&format!("ACK:save:{}", parts[1]));
        }
        "folder" if parts.len() >= 3 => {
            let target = if parts[1] == "aux" { aux_h } else { main_h };
            let opts = FileDialogOptions {
                title: "Folder Test".to_owned(),
                directory: Some(PathBuf::from(parts[2])),
                ..Default::default()
            };
            target.update(move |_, tx| {
                tx.open_folder(opts, |s: &mut WinState, res, _| {
                    record_answer(s, "folder", res);
                });
            });
            print_flushed(&format!("ACK:folder:{}", parts[1]));
        }
        "counts" if parts.len() >= 2 => {
            let target = if parts[1] == "aux" { aux_h } else { main_h };
            target.update(|s, _| {
                print_flushed(&format!(
                    "COUNTS:{}:ok={}|cancel={}|err={}",
                    s.name, s.ok_count, s.cancel_count, s.err_count
                ));
            });
        }
        "close-aux" => {
            aux_h.close();
            print_flushed("ACK:close-aux");
        }
        "open-aux" => {
            aux_h.open();
            print_flushed("ACK:open-aux");
        }
        "quit" => {
            print_flushed("ACK:quit");
            main_h.quit();
            return true;
        }
        other => {
            eprintln!("chooser-demo: unknown command: {other}");
        }
    }
    false
}

fn spawn_control_file(path: String, main_h: AppHandle<WinState>, aux_h: AppHandle<WinState>) {
    std::thread::spawn(move || {
        let mut offset = 0u64;
        let mut handled = 0usize;
        for _ in 0..MAX_CONTROL_POLLS {
            if handled >= MAX_CONTROL_COMMANDS {
                break;
            }
            if let Ok(mut file) = std::fs::File::open(&path)
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
                            if handle_command(line.trim(), &main_h, &aux_h) {
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
    rune::tooling::set_app_id("org.ferrionos.ChooserDemo");
    print_flushed(&format!("DEMO_PID={}", std::process::id()));
    let single_window = has_flag("--single-window");
    let fixture_dir = arg_value("--fixture-dir=").map(PathBuf::from);
    let main_fixture = fixture_dir.clone();

    let app = App::new(AppSpec::new(
        "Chooser Demo Main",
        (360.0, 200.0),
        WinState::default,
        win_view,
    ))
    .init(move || WinState {
        name: "main",
        fixture_dir: main_fixture,
        ..Default::default()
    })
    .on_focus(|s, focused, tx| {
        if focused {
            tx.focus("demo-root", false);
        }
        print_flushed(&format!("DEMO_FOCUS:{}:focused={focused}", s.name));
    });

    let main_h = app.handle();
    let main_ready = main_h.clone();
    let mut app = app.on_ready(move |_| {
        main_ready.update(|_, tx| tx.focus("demo-root", false));
        print_flushed("CHOOSER_DEMO_READY");
    });

    let aux_h = if single_window {
        main_h.clone()
    } else {
        let aux_app = App::new(AppSpec::new(
            "Chooser Demo Aux",
            (360.0, 200.0),
            || WinState {
                name: "aux",
                ..Default::default()
            },
            win_view,
        ))
        .on_ready(|_| print_flushed("CHOOSER_AUX_READY"));
        let (h, _) = app.window(WindowSpec::from_app("aux", aux_app).options(WindowOptions {
            visible: true,
            ..Default::default()
        }));
        h
    };

    if let Some(path) = arg_value("--control-file=") {
        spawn_control_file(path, main_h, aux_h);
    }

    app.run();
}
