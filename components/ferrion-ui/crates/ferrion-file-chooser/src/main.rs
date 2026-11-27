#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rune::file_dialog::chooser::{
    ChooserReply, ChooserRequest, MAX_IPC_BYTES, decode_request, encode_reply,
};
use rune::{App, AppSpec, FileDialogKind, FileDialogOptions};

mod model;
mod view;

use model::ChooserModel;
use view::{ChooserAppState, chooser_view};

fn arg_value(prefix: &str) -> Option<String> {
    std::env::args()
        .skip(1)
        .find_map(|a| a.strip_prefix(prefix).map(str::to_owned))
}

fn has_flag(flag: &str) -> bool {
    std::env::args().skip(1).any(|a| a == flag)
}

const MAX_EOF_WAIT_POLLS: usize = 50;
const EOF_WAIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

fn read_bounded_stdin() -> Result<Vec<u8>, String> {
    let mut stdin = std::io::stdin().lock();
    let mut out = Vec::new();
    let mut byte = [0u8; 1];
    let limit = MAX_IPC_BYTES + 1;
    while out.len() < limit {
        match stdin.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                out.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(format!("stdin read failed: {err}")),
        }
    }
    Ok(out)
}

fn spawn_parent_eof_watcher(handle: rune::AppHandle<ChooserAppState>, emitted: Arc<AtomicBool>) {
    if let Err(err) = std::thread::Builder::new()
        .name("chooser-parent-watch".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 64];
            let mut drained = 0usize;
            while drained < MAX_IPC_BYTES {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => drained = drained.saturating_add(n),
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            if !emitted.swap(true, Ordering::AcqRel) {
                handle.quit();
                for _ in 0..MAX_EOF_WAIT_POLLS {
                    std::thread::sleep(EOF_WAIT_INTERVAL);
                }
                std::process::exit(0);
            }
        })
    {
        eprintln!("ferrion-file-chooser: parent-eof watcher spawn failed: {err}");
    }
}

fn write_reply_stdout(reply: &ChooserReply) {
    let bytes = match encode_reply(reply) {
        Ok(b) => b,
        Err(err) => encode_reply(&ChooserReply::Error(err.0)).unwrap_or_default(),
    };
    let mut out = std::io::stdout().lock();
    if out.write_all(&bytes).is_ok() {
        drop(out.flush());
    }
}

#[cfg(feature = "fault-hooks")]
fn maybe_run_fault(fault: &str, req: &ChooserRequest) -> bool {
    match fault {
        "garbage" => {
            let mut out = std::io::stdout().lock();
            drop(out.write_all(b"not-valid-json\n"));
            drop(out.flush());
            true
        }
        "oversized" => {
            let mut out = std::io::stdout().lock();
            let mut payload = vec![b'A'; MAX_IPC_BYTES + 2048];
            payload.push(b'\n');
            drop(out.write_all(&payload));
            drop(out.flush());
            true
        }
        "bad-version" => {
            let mut out = std::io::stdout().lock();
            drop(out.write_all(b"{\"version\":999,\"status\":\"cancel\"}\n"));
            drop(out.flush());
            true
        }
        "relative-path" => {
            let mut out = std::io::stdout().lock();
            drop(
                out.write_all(
                    b"{\"version\":1,\"status\":\"ok\",\"mode\":\"read\",\"path\":\"relative/file.txt\",\"overwrite_confirmed\":false}\n",
                ),
            );
            drop(out.flush());
            true
        }
        "wrong-mode" => {
            let dir = req
                .options
                .directory
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "/tmp".to_owned());
            let line = serde_json::json!({
                "version": 1,
                "status": "ok",
                "mode": "dir",
                "path": dir,
                "overwrite_confirmed": false,
            });
            let mut out = std::io::stdout().lock();
            drop(writeln!(out, "{line}"));
            drop(out.flush());
            true
        }
        "unconfirmed-overwrite" => {
            let dir = req
                .options
                .directory
                .clone()
                .unwrap_or_else(|| PathBuf::from("/tmp"));
            let name = req
                .options
                .file_name
                .clone()
                .unwrap_or_else(|| "existing.txt".to_owned());
            let target = dir.join(name);
            let line = serde_json::json!({
                "version": 1,
                "status": "ok",
                "mode": "write",
                "path": target.display().to_string(),
                "overwrite_confirmed": false,
            });
            let mut out = std::io::stdout().lock();
            drop(writeln!(out, "{line}"));
            drop(out.flush());
            true
        }
        "hang" => {
            for _ in 0..600 {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            true
        }
        _ => false,
    }
}

fn load_request(ipc_mode: bool) -> Result<ChooserRequest, String> {
    if ipc_mode {
        let raw = read_bounded_stdin()?;
        return decode_request(&raw).map_err(|e| e.0);
    }
    if let Some(path) = arg_value("--request-file=") {
        let raw = std::fs::read(&path).map_err(|e| format!("read {path}: {e}"))?;
        return decode_request(&raw).map_err(|e| e.0);
    }
    let dir = arg_value("--dir=").map(PathBuf::from);
    let kind = match arg_value("--kind=").as_deref() {
        Some("save") => FileDialogKind::Save,
        Some("folder") => FileDialogKind::Folder,
        _ => FileDialogKind::Open,
    };
    let title = arg_value("--title=").unwrap_or_default();
    let file_name = arg_value("--file-name=");
    Ok(ChooserRequest {
        version: rune::file_dialog::chooser::PROTOCOL_VERSION,
        kind,
        options: FileDialogOptions {
            title,
            directory: dir,
            file_name,
            filters: Vec::new(),
        },
    })
}

fn main() {
    let ipc_mode = has_flag("--ipc");
    let req = match load_request(ipc_mode) {
        Ok(r) => r,
        Err(err) => {
            if ipc_mode {
                write_reply_stdout(&ChooserReply::Error(err));
            } else {
                eprintln!("ferrion-file-chooser: {err}");
            }
            std::process::exit(1);
        }
    };

    #[cfg(feature = "fault-hooks")]
    if let Some(fault) = arg_value("--fault=")
        && maybe_run_fault(&fault, &req)
    {
        return;
    }

    let emitted = Arc::new(AtomicBool::new(false));
    let emitted_close = Arc::clone(&emitted);
    let emitted_post = Arc::clone(&emitted);
    let emit: Arc<dyn Fn(ChooserReply) + Send + Sync> = Arc::new(move |reply| {
        if ipc_mode {
            write_reply_stdout(&reply);
        }
    });
    let emit_init = Arc::clone(&emit);
    let emit_post = Arc::clone(&emit);

    let title_owned = if req.options.title.is_empty() {
        "Choose File".to_owned()
    } else {
        req.options.title.clone()
    };
    let title_ref: &'static str = Box::leak(title_owned.into_boxed_str());
    let initial_kind = req.kind;
    let init_req = std::sync::Mutex::new(Some(req));

    rune::tooling::set_app_id("org.ferrionos.FileChooser");
    let app = App::new(AppSpec::new(
        title_ref,
        (640.0, 440.0),
        || unreachable!(),
        chooser_view,
    ))
    .init(move || {
        let req = init_req
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or(ChooserRequest {
                version: rune::file_dialog::chooser::PROTOCOL_VERSION,
                kind: FileDialogKind::Open,
                options: FileDialogOptions::default(),
            });
        ChooserAppState {
            model: ChooserModel::from_request(req),
            emit: Arc::clone(&emit_init),
            emitted: Arc::clone(&emitted_close),
        }
    });
    let app_handle = app.handle();
    if ipc_mode {
        spawn_parent_eof_watcher(app_handle.clone(), Arc::clone(&emitted));
    }
    let app = app.on_ready(move |_| {
        if initial_kind != FileDialogKind::Save {
            app_handle.update(|_, tx| tx.focus("chooser-root", false));
        }
    });

    app.run();

    if !emitted_post.swap(true, Ordering::AcqRel) {
        emit_post(ChooserReply::Cancel);
    }
}
