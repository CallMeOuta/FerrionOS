#![forbid(unsafe_code)]

#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
use std::future::Future;
#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
use std::io::{Read, Write};
#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
use std::path::Path;
#[cfg(feature = "native-file-dialog")]
use std::path::PathBuf;
#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
use std::pin::Pin;
use std::process::Child;
#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
use std::task::{Context, Poll, Waker};
use std::time::Duration;

#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
use super::super::FileDialogResult;
#[cfg(feature = "native-file-dialog")]
use super::super::{FileDialogError, FileDialogKind, FileDialogOptions};
#[cfg(all(feature = "native-file-dialog", target_os = "redox"))]
use super::DEFAULT_CHOOSER_PATH;
#[cfg(all(
    feature = "native-file-dialog",
    any(feature = "tooling", feature = "hot", debug_assertions)
))]
use super::MAX_PATH_BYTES;
#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
use super::{ChooserRequest, MAX_IPC_BYTES, PROTOCOL_VERSION, encode_request, validate_reply};

pub const MAX_KILL_WAIT: Duration = Duration::from_secs(2);
const KILL_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_KILL_POLLS: usize = 200;
#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
const MAX_ACTIVE_CHOOSERS: usize = 16;

static ACTIVE_CHOOSERS: Mutex<Vec<Arc<ChooserProcess>>> = Mutex::new(Vec::new());

#[cfg(feature = "fault-hooks")]
static TEST_FAULT: Mutex<Option<String>> = Mutex::new(None);

#[cfg(feature = "fault-hooks")]
pub(crate) fn set_fault_for_test(fault: Option<String>) {
    *TEST_FAULT.lock().unwrap_or_else(|e| e.into_inner()) = fault;
}

#[cfg(all(
    feature = "fault-hooks",
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
fn active_fault() -> Option<String> {
    TEST_FAULT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(all(
    feature = "native-file-dialog",
    any(feature = "tooling", feature = "hot", debug_assertions)
))]
pub(in super::super) fn tooling_override_path() -> Result<Option<PathBuf>, FileDialogError> {
    let Some(val) = rune_core::env::var("FERRION_FILE_CHOOSER") else {
        return Ok(None);
    };
    if val.is_empty() {
        return Ok(None);
    }
    if val.len() > MAX_PATH_BYTES || val.contains('\0') {
        return Err(FileDialogError(
            "FERRION_FILE_CHOOSER override path is invalid".into(),
        ));
    }
    let path = PathBuf::from(&val);
    if !path.is_absolute() {
        return Err(FileDialogError(
            "FERRION_FILE_CHOOSER override path must be absolute".into(),
        ));
    }
    Ok(Some(path))
}

#[cfg(all(
    feature = "native-file-dialog",
    not(any(feature = "tooling", feature = "hot", debug_assertions))
))]
pub(in super::super) fn tooling_override_path() -> Result<Option<PathBuf>, FileDialogError> {
    Ok(None)
}

#[cfg(all(feature = "native-file-dialog", target_os = "redox"))]
pub(in super::super) async fn show(
    kind: FileDialogKind,
    options: FileDialogOptions,
) -> FileDialogResult {
    let path = tooling_override_path()?.unwrap_or_else(|| PathBuf::from(DEFAULT_CHOOSER_PATH));
    show_with_path(path, kind, options).await
}

#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
pub(in super::super) async fn show_with_path(
    chooser_path: PathBuf,
    kind: FileDialogKind,
    options: FileDialogOptions,
) -> FileDialogResult {
    if !chooser_path.is_absolute() {
        return Err(FileDialogError(
            "file chooser executable path must be absolute".into(),
        ));
    }
    let req = ChooserRequest {
        version: PROTOCOL_VERSION,
        kind,
        options,
    };
    let payload = encode_request(&req)?;
    let proc = ChooserProcess::spawn(&chooser_path, kind, payload)?;
    ChooserFuture { proc }.await
}

struct ChooserProcess {
    child: Mutex<Option<Child>>,
    #[cfg(all(
        feature = "native-file-dialog",
        any(target_os = "linux", target_os = "redox")
    ))]
    stdin: Mutex<Option<ChildStdin>>,
    cancelled: AtomicBool,
    #[cfg(all(
        feature = "native-file-dialog",
        any(target_os = "linux", target_os = "redox")
    ))]
    result: Mutex<Option<FileDialogResult>>,
    #[cfg(all(
        feature = "native-file-dialog",
        any(target_os = "linux", target_os = "redox")
    ))]
    waker: Mutex<Option<Waker>>,
}

impl ChooserProcess {
    #[cfg(all(
        feature = "native-file-dialog",
        any(target_os = "linux", target_os = "redox")
    ))]
    fn spawn(
        chooser_path: &Path,
        kind: FileDialogKind,
        payload: Vec<u8>,
    ) -> Result<Arc<Self>, FileDialogError> {
        assert!(chooser_path.is_absolute());
        let mut cmd = Command::new(chooser_path);
        cmd.arg("--ipc")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(feature = "fault-hooks")]
        if let Some(fault) = active_fault() {
            cmd.arg(format!("--fault={fault}"));
        }
        let mut child = cmd.spawn().map_err(|e| {
            FileDialogError(format!(
                "failed to launch file chooser {}: {e}",
                chooser_path.display()
            ))
        })?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| FileDialogError("file chooser stdin pipe unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| FileDialogError("file chooser stdout pipe unavailable".into()))?;
        if let Err(err) = stdin.write_all(&payload).and_then(|()| stdin.flush()) {
            drop(stdin);
            kill_and_reap_child(&mut child);
            return Err(FileDialogError(format!(
                "failed to write file chooser request: {err}"
            )));
        }
        let proc = Arc::new(Self {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(stdin)),
            cancelled: AtomicBool::new(false),
            result: Mutex::new(None),
            waker: Mutex::new(None),
        });
        register_active(&proc)?;
        let worker_proc = Arc::clone(&proc);
        if let Err(err) = std::thread::Builder::new()
            .name("rune-file-chooser".into())
            .spawn(move || read_reply_thread(worker_proc, stdout, kind))
        {
            proc.cancel_and_reap();
            return Err(FileDialogError(format!(
                "failed to start file chooser reader thread: {err}"
            )));
        }
        Ok(proc)
    }

    #[cfg(all(
        feature = "native-file-dialog",
        any(target_os = "linux", target_os = "redox")
    ))]
    fn finish_with(&self, res: FileDialogResult) {
        if self.cancelled.load(Ordering::Acquire) {
            return;
        }
        *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(res);
        if let Some(waker) = self.waker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            waker.wake();
        }
    }

    #[cfg(all(
        feature = "native-file-dialog",
        any(target_os = "linux", target_os = "redox")
    ))]
    fn reap_or_kill(&self) {
        drop(self.stdin.lock().unwrap_or_else(|e| e.into_inner()).take());
        let maybe_child = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut child) = maybe_child {
            wait_or_kill_child(&mut child);
        }
        unregister_active(self);
    }

    fn cancel_and_reap(&self) {
        self.cancelled.store(true, Ordering::Release);
        #[cfg(all(
            feature = "native-file-dialog",
            any(target_os = "linux", target_os = "redox")
        ))]
        drop(self.stdin.lock().unwrap_or_else(|e| e.into_inner()).take());
        let maybe_child = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut child) = maybe_child {
            kill_and_reap_child(&mut child);
        }
        unregister_active(self);
    }
}

#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
fn read_reply_thread(
    proc: Arc<ChooserProcess>,
    mut stdout: std::process::ChildStdout,
    kind: FileDialogKind,
) {
    let raw = read_bounded_line(&mut stdout, MAX_IPC_BYTES + 1);
    drop(stdout);
    proc.reap_or_kill();
    let outcome = match raw {
        Ok(bytes) => validate_reply(kind, &bytes),
        Err(err) => Err(FileDialogError(format!(
            "failed to read file chooser reply: {err}"
        ))),
    };
    proc.finish_with(outcome);
}

#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
fn read_bounded_line(reader: &mut dyn Read, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 512];
    while out.len() < limit {
        let remaining = limit - out.len();
        let take = chunk.len().min(remaining);
        match reader.read(&mut chunk[..take]) {
            Ok(0) => break,
            Ok(n) => {
                if let Some(pos) = chunk[..n].iter().position(|&b| b == b'\n') {
                    out.extend_from_slice(&chunk[..=pos]);
                    break;
                }
                out.extend_from_slice(&chunk[..n]);
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(out)
}

#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
fn wait_or_kill_child(child: &mut Child) {
    for _ in 0..MAX_KILL_POLLS {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => std::thread::sleep(KILL_POLL_INTERVAL),
        }
    }
    kill_and_reap_child(child);
}

fn kill_and_reap_child(child: &mut Child) {
    drop(child.kill());
    for _ in 0..MAX_KILL_POLLS {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => std::thread::sleep(KILL_POLL_INTERVAL),
        }
    }
}

#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
fn register_active(proc: &Arc<ChooserProcess>) -> Result<(), FileDialogError> {
    let mut list = ACTIVE_CHOOSERS.lock().unwrap_or_else(|e| e.into_inner());
    if list.len() >= MAX_ACTIVE_CHOOSERS {
        return Err(FileDialogError(format!(
            "too many active file choosers (limit {MAX_ACTIVE_CHOOSERS})"
        )));
    }
    list.push(Arc::clone(proc));
    Ok(())
}

fn unregister_active(target: &ChooserProcess) {
    let mut list = ACTIVE_CHOOSERS.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|entry| !std::ptr::eq(Arc::as_ptr(entry), target));
}

pub(crate) fn shutdown() {
    let active = {
        let mut list = ACTIVE_CHOOSERS.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *list)
    };
    for proc in active {
        proc.cancel_and_reap();
    }
}

#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
struct ChooserFuture {
    proc: Arc<ChooserProcess>,
}

#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
impl Future for ChooserFuture {
    type Output = FileDialogResult;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(res) = self
            .proc
            .result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            return Poll::Ready(res);
        }
        *self.proc.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(cx.waker().clone());
        if let Some(res) = self
            .proc
            .result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            return Poll::Ready(res);
        }
        Poll::Pending
    }
}

#[cfg(all(
    feature = "native-file-dialog",
    any(target_os = "linux", target_os = "redox")
))]
impl Drop for ChooserFuture {
    fn drop(&mut self) {
        self.proc.cancel_and_reap();
    }
}
