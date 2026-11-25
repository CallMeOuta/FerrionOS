#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
mod epipe;

use rune::clipboard::{ClipboardContent, ClipboardImage, SecretCopy};
use rune::tok::{self, type_};
use rune::{App, AppHandle, AppSpec, Color, Cx, El, col, text};

const MAX_CONTROL_COMMANDS: usize = 64;
const MAX_CONTROL_POLLS: usize = 1200;
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Default)]
struct DemoState {
    last_text: String,
    last_html: String,
    last_png_len: usize,
    last_files: String,
    last_error: String,
    reads: u32,
    secret: Option<SecretCopy>,
}

fn make_test_png() -> ClipboardImage {
    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    write_chunk(&mut png, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    write_chunk(
        &mut png,
        b"IDAT",
        &[
            0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff,
        ],
    );
    write_chunk(&mut png, b"IEND", &[]);
    ClipboardImage::from_png(png).unwrap_or_else(|| std::process::abort())
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc: u32 = 0xffff_ffff;
    for &b in kind.iter().chain(data.iter()) {
        crc ^= u32::from(b);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xedb8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    out.extend_from_slice(&(!crc).to_be_bytes());
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

fn view(s: &DemoState, _: &mut Cx<DemoState>) -> El {
    let t = tok::theme(true);
    let secret_str = s
        .secret
        .as_ref()
        .map_or_else(|| "None".to_owned(), |sc| format!("{:?}", sc.state()));
    let summary = format!(
        "reads={} text={} html={} png={} files={} err={} secret={}",
        s.reads, s.last_text, s.last_html, s.last_png_len, s.last_files, s.last_error, secret_str
    );
    col()
        .w_full()
        .h_full()
        .padding(16.0)
        .gap(8.0)
        .fill(Color {
            r: 0.08,
            g: 0.10,
            b: 0.14,
            a: 1.0,
        })
        .child(
            text("FerrionOS Clipboard Demo")
                .font(type_::TITLE)
                .color(t.text),
        )
        .child(text(summary).font(type_::BODY).color(t.text_secondary))
}

fn handle_command(
    cmd: &str,
    handle: &AppHandle<DemoState>,
    last_secret: &Arc<Mutex<Option<SecretCopy>>>,
) -> bool {
    if cmd.is_empty() {
        return false;
    }
    if let Some(payload) = cmd.strip_prefix("copy-text ") {
        let text = payload.to_owned();
        handle.update(move |_, tx| tx.copy(text));
        print_flushed("ACK:copy-text");
    } else if let Some(rest) = cmd.strip_prefix("copy-repeat ") {
        let n = rest.parse::<usize>().unwrap_or(0).min(4 * 1024 * 1024);
        let text = "B".repeat(n);
        handle.update(move |_, tx| tx.copy(text));
        print_flushed(&format!("ACK:copy-repeat:{n}"));
    } else if let Some(rest) = cmd.strip_prefix("copy-html ") {
        let (plain, html) = rest.split_once('|').unwrap_or((rest, rest));
        let content = ClipboardContent::new(plain).with_html(html);
        handle.update(move |_, tx| tx.copy_rich(content));
        print_flushed("ACK:copy-html");
    } else if cmd == "copy-png" {
        let img = make_test_png();
        let len = img.png().len();
        let content = ClipboardContent::default().with_image(img);
        handle.update(move |_, tx| tx.copy_rich(content));
        print_flushed(&format!("ACK:copy-png:{len}"));
    } else if let Some(rest) = cmd.strip_prefix("copy-files ") {
        let files: Vec<PathBuf> = rest.split('|').map(PathBuf::from).collect();
        let content = ClipboardContent::default().with_files(files);
        handle.update(move |_, tx| tx.copy_rich(content));
        print_flushed("ACK:copy-files");
    } else if let Some(payload) = cmd.strip_prefix("copy-secret ") {
        let secret_text = payload.to_owned();
        let slot = last_secret.clone();
        handle.update(move |s, tx| {
            let sc = tx.copy_secret(secret_text, Duration::from_secs(30));
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(sc.clone());
            s.secret = Some(sc);
        });
        print_flushed("ACK:copy-secret");
    } else if cmd == "clear-secret" {
        handle.update(|_, tx| tx.clear_secret_clipboard());
        print_flushed("ACK:clear-secret");
    } else if cmd == "secret-state" {
        let state = last_secret
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or_else(|| "None".to_owned(), |sc| format!("{:?}", sc.state()));
        print_flushed(&format!("SECRET_STATE:{state}"));
    } else if cmd == "read" {
        handle.update(|_, tx| {
            tx.read_clipboard(|s: &mut DemoState, content, _| {
                s.reads = s.reads.saturating_add(1);
                s.last_text = content.text().to_owned();
                s.last_html = content.html().unwrap_or_default().to_owned();
                s.last_png_len = content.image().map_or(0, |i| i.png().len());
                s.last_files = content
                    .files()
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("|");
                s.last_error = content.error().unwrap_or_default().to_owned();
                let flat_text = s.last_text.replace('\r', "\\r").replace('\n', "\\n");
                let flat_html = s.last_html.replace('\r', "\\r").replace('\n', "\\n");
                print_flushed(&format!(
                    "READ:text={flat_text}|html={flat_html}|png_len={}|files={}|err={}",
                    s.last_png_len, s.last_files, s.last_error
                ));
            });
        });
        print_flushed("ACK:read");
    } else if cmd == "quit" {
        print_flushed("ACK:quit");
        handle.quit();
        return true;
    } else {
        eprintln!("clipboard-demo: unknown command: {cmd}");
    }
    false
}

fn spawn_control(
    path: String,
    handle: AppHandle<DemoState>,
    last_secret: Arc<Mutex<Option<SecretCopy>>>,
) {
    std::thread::spawn(move || {
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(err) => {
                eprintln!("clipboard-demo: open control {path}: {err}");
                return;
            }
        };
        let reader = BufReader::new(file);
        for line in reader.lines().take(MAX_CONTROL_COMMANDS) {
            let Ok(raw) = line else {
                break;
            };
            if handle_command(raw.trim(), &handle, &last_secret) {
                break;
            }
        }
    });
}

fn spawn_control_file(
    path: String,
    handle: AppHandle<DemoState>,
    last_secret: Arc<Mutex<Option<SecretCopy>>>,
) {
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
                            if handle_command(line.trim(), &handle, &last_secret) {
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
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    if std::env::args().skip(1).any(|a| a == "--epipe-reader") {
        if let Err(err) = epipe::run() {
            eprintln!("clipboard-demo: epipe-reader failed: {err}");
            std::process::exit(1);
        }
        return;
    }

    let title = arg_value("--title=").unwrap_or_else(|| "Clipboard Demo".to_owned());
    let title_ref: &'static str = Box::leak(title.into_boxed_str());
    let app = App::new(AppSpec::new(
        title_ref,
        (480.0, 240.0),
        DemoState::default,
        view,
    ))
    .on_ready(|_| print_flushed("CLIPBOARD_READY"));
    let handle = app.handle();
    let last_secret = Arc::new(Mutex::new(None));
    if let Some(path) = arg_value("--control-file=") {
        spawn_control_file(path, handle, last_secret);
    } else if let Some(path) = arg_value("--control=") {
        spawn_control(path, handle, last_secret);
    }
    app.run();
}
