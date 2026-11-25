#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use rune::tok::type_;
use rune::{
    App, AppHandle, AppSpec, Color, Cx, DragImage, DragItem, DragOperation, DragOutcome, El, Tx,
    col, row, text,
};

const MAX_CONTROL_COMMANDS: usize = 64;
const MAX_TEXT_KB: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DemoMode {
    Source,
    Target,
    Both,
}

struct DemoState {
    mode: DemoMode,
    file: PathBuf,
    payload_text: String,
    within_app: bool,
    close_on_start: bool,
    drag_starts: u32,
    drag_ends: u32,
    last_outcome: String,
    drop_count: u32,
    reject_count: u32,
    hover_left: bool,
    hover_right: bool,
    last_drop_zone: String,
    last_drop_paths: String,
    last_drop_point: String,
}

impl DemoState {
    fn from_args() -> Self {
        let mode = match arg_value("--mode=").as_deref() {
            Some("source") => DemoMode::Source,
            Some("target") => DemoMode::Target,
            _ => DemoMode::Both,
        };
        let file = PathBuf::from(
            arg_value("--file=").unwrap_or_else(|| "/tmp/rune-dnd-sample.txt".to_owned()),
        );
        let text_kb = arg_value("--text-kb=")
            .and_then(|v| v.parse::<usize>().ok())
            .map(|n| n.min(MAX_TEXT_KB))
            .unwrap_or(0);
        let payload_text = if text_kb > 0 {
            "A".repeat(text_kb.saturating_mul(1024))
        } else {
            arg_value("--text=").unwrap_or_else(|| "rune-dnd-payload".to_owned())
        };
        let within_app = has_flag("--within-app");
        let close_on_start = has_flag("--close-on-start");
        Self {
            mode,
            file,
            payload_text,
            within_app,
            close_on_start,
            drag_starts: 0,
            drag_ends: 0,
            last_outcome: String::new(),
            drop_count: 0,
            reject_count: 0,
            hover_left: false,
            hover_right: false,
            last_drop_zone: String::new(),
            last_drop_paths: String::new(),
            last_drop_point: String::new(),
        }
    }
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

fn solid_rgba(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut buf = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        buf.extend_from_slice(&rgba);
    }
    buf
}

fn make_drag_icon() -> Option<DragImage> {
    let pixels = solid_rgba(24, 24, [0x22, 0x99, 0xEE, 0xFF]);
    DragImage::rgba(24, 24, &pixels).map(|img| img.size(24.0, 24.0))
}

fn format_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn format_point(tx: &Tx) -> String {
    match tx.drop_point() {
        Some(pt) => format!(
            "local=({:.1},{:.1})|win=({:.1},{:.1})",
            pt.x, pt.y, pt.window_x, pt.window_y
        ),
        None => "none".to_owned(),
    }
}

fn drag_tile(
    key: &'static str,
    label: &'static str,
    height: f32,
    fill: Color,
    kind: &'static str,
    build: impl Fn(&DemoState) -> Option<DragItem> + 'static,
) -> El {
    row()
        .key(key)
        .w_full()
        .h(height)
        .items_center()
        .justify_center()
        .fill(fill)
        .drag_out(build)
        .on_drag_start(move |s: &mut DemoState, tx: &mut Tx| {
            s.drag_starts = s.drag_starts.saturating_add(1);
            print_flushed(&format!("DRAG_START:{kind}|starts={}", s.drag_starts));
            if s.close_on_start {
                tx.close_window();
            }
        })
        .on_drag_end(move |s: &mut DemoState, outcome: DragOutcome, _| {
            s.drag_ends = s.drag_ends.saturating_add(1);
            s.last_outcome = format!("{outcome:?}");
            print_flushed(&format!(
                "DRAG_END:{kind}|outcome={outcome:?}|ends={}",
                s.drag_ends
            ));
        })
        .child(text(label).font(type_::BODY).color(Color::WHITE))
}

fn drop_zones(height: f32, hover_left: bool, hover_right: bool) -> El {
    let left_fill = if hover_left {
        Color {
            r: 0.16,
            g: 0.45,
            b: 0.28,
            a: 1.0,
        }
    } else {
        Color {
            r: 0.10,
            g: 0.22,
            b: 0.16,
            a: 1.0,
        }
    };
    let right_fill = if hover_right {
        Color {
            r: 0.20,
            g: 0.34,
            b: 0.56,
            a: 1.0,
        }
    } else {
        Color {
            r: 0.10,
            g: 0.16,
            b: 0.28,
            a: 1.0,
        }
    };
    row()
        .w_full()
        .h(height)
        .child(
            row()
                .key("left-zone")
                .w(160.0)
                .h(height)
                .items_center()
                .justify_center()
                .fill(left_fill)
                .accepts_extensions(&["txt"])
                .on_drop_hover(|s: &mut DemoState, over, tx| {
                    s.hover_left = over;
                    print_flushed(&format!("HOVER:left:{over}|{}", format_point(tx)));
                })
                .on_drop_move(|_: &mut DemoState, pt, _| {
                    print_flushed(&format!(
                        "MOVE:left|local=({:.1},{:.1})|win=({:.1},{:.1})",
                        pt.x, pt.y, pt.window_x, pt.window_y
                    ));
                })
                .on_drop_files(|s: &mut DemoState, files, tx| {
                    s.drop_count = s.drop_count.saturating_add(1);
                    s.last_drop_zone = "left".to_owned();
                    s.last_drop_paths = format_paths(files);
                    s.last_drop_point = format_point(tx);
                    print_flushed(&format!(
                        "DROP:left|count={}|paths={}|{}",
                        s.drop_count, s.last_drop_paths, s.last_drop_point
                    ));
                })
                .on_drop_rejected(|s: &mut DemoState, files, tx| {
                    s.reject_count = s.reject_count.saturating_add(1);
                    print_flushed(&format!(
                        "REJECT:left|count={}|paths={}|{}",
                        s.reject_count,
                        format_paths(files),
                        format_point(tx)
                    ));
                })
                .child(text("Left (.txt)").font(type_::BODY).color(Color::WHITE)),
        )
        .child(
            row()
                .key("right-zone")
                .w(160.0)
                .h(height)
                .items_center()
                .justify_center()
                .fill(right_fill)
                .on_drop_hover(|s: &mut DemoState, over, tx| {
                    s.hover_right = over;
                    print_flushed(&format!("HOVER:right:{over}|{}", format_point(tx)));
                })
                .on_drop_move(|_: &mut DemoState, pt, _| {
                    print_flushed(&format!(
                        "MOVE:right|local=({:.1},{:.1})|win=({:.1},{:.1})",
                        pt.x, pt.y, pt.window_x, pt.window_y
                    ));
                })
                .on_drop_files(|s: &mut DemoState, files, tx| {
                    s.drop_count = s.drop_count.saturating_add(1);
                    s.last_drop_zone = "right".to_owned();
                    s.last_drop_paths = format_paths(files);
                    s.last_drop_point = format_point(tx);
                    print_flushed(&format!(
                        "DROP:right|count={}|paths={}|{}",
                        s.drop_count, s.last_drop_paths, s.last_drop_point
                    ));
                })
                .child(text("Right (any)").font(type_::BODY).color(Color::WHITE)),
        )
}

fn build_copy_item(s: &DemoState) -> Option<DragItem> {
    let png_pixels = solid_rgba(4, 4, [0xEE, 0x33, 0x44, 0xFF]);
    let mut item = DragItem::new()
        .file(&s.file)
        .text(&s.payload_text)
        .rgba(4, 4, &png_pixels);
    if let Some(icon) = make_drag_icon() {
        item = item.image(icon);
    }
    if s.within_app {
        item = item.within_app();
    }
    Some(item)
}

fn build_move_item(s: &DemoState) -> Option<DragItem> {
    let mut item = DragItem::new().file(&s.file).operation(DragOperation::Move);
    if let Some(icon) = make_drag_icon() {
        item = item.image(icon);
    }
    Some(item)
}

fn build_link_item(s: &DemoState) -> Option<DragItem> {
    Some(DragItem::new().file(&s.file).operation(DragOperation::Link))
}

fn view(s: &DemoState, _: &mut Cx<DemoState>) -> El {
    let root = col().w_full().h_full().fill(Color {
        r: 0.08,
        g: 0.10,
        b: 0.14,
        a: 1.0,
    });
    match s.mode {
        DemoMode::Source => root
            .child(drag_tile(
                "copy-tile",
                "Drag Copy",
                80.0,
                Color {
                    r: 0.18,
                    g: 0.32,
                    b: 0.52,
                    a: 1.0,
                },
                "copy",
                build_copy_item,
            ))
            .child(drag_tile(
                "move-tile",
                "Drag Move",
                80.0,
                Color {
                    r: 0.44,
                    g: 0.28,
                    b: 0.16,
                    a: 1.0,
                },
                "move",
                build_move_item,
            ))
            .child(drag_tile(
                "link-tile",
                "Drag Link",
                80.0,
                Color {
                    r: 0.42,
                    g: 0.16,
                    b: 0.24,
                    a: 1.0,
                },
                "link",
                build_link_item,
            )),
        DemoMode::Target => root.child(drop_zones(240.0, s.hover_left, s.hover_right)),
        DemoMode::Both => root
            .child(drag_tile(
                "copy-tile",
                "Drag Copy",
                120.0,
                Color {
                    r: 0.18,
                    g: 0.32,
                    b: 0.52,
                    a: 1.0,
                },
                "copy",
                build_copy_item,
            ))
            .child(drop_zones(120.0, s.hover_left, s.hover_right)),
    }
}

fn spawn_control(path: String, handle: AppHandle<DemoState>) {
    std::thread::spawn(move || {
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(err) => {
                eprintln!("dnd-demo: open control {path}: {err}");
                return;
            }
        };
        let reader = BufReader::new(file);
        for line in reader.lines().take(MAX_CONTROL_COMMANDS) {
            let Ok(raw) = line else {
                break;
            };
            let cmd = raw.trim();
            if cmd.is_empty() {
                continue;
            }
            if cmd == "status" {
                handle.update(|s, _| {
                    print_flushed(&format!(
                        "STATUS:starts={}|ends={}|outcome={}|drops={}|rejects={}|zone={}|paths={}|pt={}",
                        s.drag_starts,
                        s.drag_ends,
                        s.last_outcome,
                        s.drop_count,
                        s.reject_count,
                        s.last_drop_zone,
                        s.last_drop_paths,
                        s.last_drop_point
                    ));
                });
            } else if cmd == "close" {
                print_flushed("ACK:close");
                handle.update(|_, tx| tx.close_window());
            } else if cmd == "quit" {
                print_flushed("ACK:quit");
                handle.quit();
                break;
            } else {
                eprintln!("dnd-demo: unknown command: {cmd}");
            }
        }
    });
}

fn main() {
    let title = arg_value("--title=").unwrap_or_else(|| "DnD Demo".to_owned());
    let title_ref: &'static str = Box::leak(title.into_boxed_str());
    let app = App::new(AppSpec::new(
        title_ref,
        (320.0, 240.0),
        DemoState::from_args,
        view,
    ));
    let handle = app.handle();
    if let Some(path) = arg_value("--control=") {
        spawn_control(path, handle);
    }
    app.run();
}
