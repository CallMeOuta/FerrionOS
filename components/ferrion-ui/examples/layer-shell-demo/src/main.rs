#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use rune::tok::{self, type_};
use rune::{
    Align, App, AppHandle, AppSpec, Color, Cx, El, Key, KeyEvent, KeyPolicy, OverlayHandle,
    OverlayInput, OverlayLayer, OverlayOptions, OverlayPlacement, OverlaySpec, ScreenEdge,
    WindowOptions, col, el, row, spacer, text,
};

const TOP_BAR_HEIGHT: f32 = 44.0;
const TOP_BAR_EXCLUSIVE_ZONE: i32 = 44;
const LAUNCHER_SIZE: (f32, f32) = (440.0, 140.0);
const LAUNCHER_TOP_MARGIN: f32 = 88.0;
const MAX_CONTROL_COMMANDS: usize = 64;
const MAX_RECORDED_KEYS: usize = 64;

#[derive(Default)]
struct MainState {
    keys: String,
    bar_visible: bool,
    launcher_visible: bool,
}

#[derive(Default)]
struct BarState;

#[derive(Default)]
struct LauncherState {
    typed: String,
}

#[derive(Default)]
struct Counters {
    main_keys: AtomicU32,
    launcher_keys: AtomicU32,
    last_main: Mutex<String>,
    last_launcher: Mutex<String>,
}

fn key_label(ev: &KeyEvent) -> Option<String> {
    match &ev.key {
        Key::Text(s) if !s.is_empty() => Some(s.clone()),
        Key::Enter => Some("Enter".into()),
        Key::Escape => Some("Escape".into()),
        Key::Backspace => Some("Backspace".into()),
        Key::Tab => Some("Tab".into()),
        _ => None,
    }
}

fn push_bounded(buf: &mut String, piece: &str) {
    if buf.len() + piece.len() <= MAX_RECORDED_KEYS {
        buf.push_str(piece);
    }
}

fn main_view(s: &MainState, _: &mut Cx<MainState>) -> El {
    let t = tok::theme(true);
    let summary = format!(
        "bar={} launcher={} keys={}",
        s.bar_visible, s.launcher_visible, s.keys
    );
    col()
        .w_full()
        .h_full()
        .padding(24.0)
        .gap(12.0)
        .fill(t.bg)
        .child(
            text("FerrionOS Layer-Shell Demo")
                .font(type_::TITLE)
                .color(t.text),
        )
        .child(text(summary).font(type_::BODY).color(t.text_secondary))
}

fn bar_view(_: &BarState, _: &mut Cx<BarState>) -> El {
    let ink = Color {
        r: 0.96,
        g: 0.97,
        b: 0.99,
        a: 1.0,
    };
    let pill_bg = Color {
        r: 0.10,
        g: 0.14,
        b: 0.22,
        a: 0.65,
    };
    let pill_border = Color {
        r: 0.90,
        g: 0.94,
        b: 1.0,
        a: 0.35,
    };
    let status_bg = Color {
        r: 0.16,
        g: 0.20,
        b: 0.30,
        a: 0.55,
    };
    let badge_fill = Color {
        r: 0.22,
        g: 0.64,
        b: 0.42,
        a: 1.0,
    };
    let brand_pill = row()
        .key("brand-pill")
        .h(28.0)
        .px(12.0)
        .gap(8.0)
        .center_y()
        .radius(10.0)
        .fill(pill_bg)
        .border(1.0, pill_border)
        .child(text("FerrionOS").font(type_::BODY).color(ink));
    let badge = el().key("opaque-badge").size(28.0, 20.0).fill(badge_fill);
    let clock_pill = row()
        .key("clock-pill")
        .h(28.0)
        .px(12.0)
        .center_y()
        .radius(10.0)
        .fill(status_bg)
        .child(text("12:00").font(type_::BODY).color(ink));
    row()
        .key("top-bar")
        .w_full()
        .h_full()
        .px(16.0)
        .gap(12.0)
        .center_y()
        .child(brand_pill)
        .child(badge)
        .child(spacer())
        .child(clock_pill)
}

fn launcher_view(s: &LauncherState, _: &mut Cx<LauncherState>) -> El {
    let ink = Color {
        r: 0.96,
        g: 0.97,
        b: 0.99,
        a: 1.0,
    };
    let sub = Color {
        r: 0.78,
        g: 0.84,
        b: 0.94,
        a: 0.85,
    };
    let card_bg = Color {
        r: 0.08,
        g: 0.10,
        b: 0.15,
        a: 0.80,
    };
    let card_border = Color {
        r: 0.85,
        g: 0.90,
        b: 1.0,
        a: 0.30,
    };
    let accent = Color {
        r: 0.28,
        g: 0.52,
        b: 0.92,
        a: 1.0,
    };
    let query_line = if s.typed.is_empty() {
        "Type to test exclusive keyboard focus".to_owned()
    } else {
        format!("Typed: {}", s.typed)
    };
    let card = col()
        .key("launcher-card")
        .w_full()
        .h_full()
        .padding(16.0)
        .gap(10.0)
        .radius(16.0)
        .fill(card_bg)
        .border(1.0, card_border)
        .child(text("Launcher").font(type_::TITLE).color(ink))
        .child(el().key("launcher-accent").size(120.0, 18.0).fill(accent))
        .child(text(query_line).font(type_::BODY).color(sub));
    col().w_full().h_full().child(card)
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

fn spawn_control_loop(
    path: String,
    app: AppHandle<MainState>,
    bar: OverlayHandle<BarState>,
    launcher: OverlayHandle<LauncherState>,
    counters: Arc<Counters>,
) {
    std::thread::spawn(move || {
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(err) => {
                eprintln!("layer-shell-demo: open control {path}: {err}");
                return;
            }
        };
        let reader = BufReader::new(file);
        for line in reader.lines().take(MAX_CONTROL_COMMANDS) {
            let Ok(cmd) = line else {
                break;
            };
            let cmd = cmd.trim();
            match cmd {
                "" => {}
                "show-bar" => {
                    bar.show();
                    app.update(|s, _| s.bar_visible = true);
                    print_flushed("ACK:show-bar");
                }
                "hide-bar" => {
                    bar.hide();
                    app.update(|s, _| s.bar_visible = false);
                    print_flushed("ACK:hide-bar");
                }
                "show-launcher" => {
                    launcher.show();
                    app.update(|s, _| s.launcher_visible = true);
                    print_flushed("ACK:show-launcher");
                }
                "hide-launcher" => {
                    launcher.hide();
                    app.update(|s, _| s.launcher_visible = false);
                    print_flushed("ACK:hide-launcher");
                }
                "clear-launcher" => {
                    launcher.update(|s, _| s.typed.clear());
                    print_flushed("ACK:clear-launcher");
                }
                "close-main" => {
                    app.close();
                    print_flushed("ACK:close-main");
                }
                "status" => {
                    let bar_avail = bar.availability();
                    let bar_fail = bar.failure().unwrap_or_default();
                    let bar_surf = bar.surface_description().unwrap_or_default();
                    let ln_avail = launcher.availability();
                    let ln_fail = launcher.failure().unwrap_or_default();
                    let ln_surf = launcher.surface_description().unwrap_or_default();
                    let mk = counters.main_keys.load(Ordering::Acquire);
                    let lk = counters.launcher_keys.load(Ordering::Acquire);
                    let lm = counters
                        .last_main
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    let ll = counters
                        .last_launcher
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    print_flushed(&format!(
                        "STATUS:bar_avail={bar_avail:?}|bar_fail={bar_fail}|bar_surf={bar_surf}|launcher_avail={ln_avail:?}|launcher_fail={ln_fail}|launcher_surf={ln_surf}|main_keys={mk}|main_text={lm}|launcher_keys={lk}|launcher_text={ll}"
                    ));
                }
                "quit" => {
                    print_flushed("ACK:quit");
                    app.quit();
                    break;
                }
                other => {
                    eprintln!("layer-shell-demo: unknown command: {other}");
                }
            }
        }
    });
}

fn main() {
    match arg_value("--role=").as_deref() {
        Some("bar") => {
            App::new(AppSpec::new(
                "Bar",
                (1280.0, TOP_BAR_HEIGHT),
                BarState::default,
                bar_view,
            ))
            .window_options(WindowOptions {
                transparent: true,
                borderless: true,
                ..Default::default()
            })
            .run();
            return;
        }
        Some("launcher") => {
            App::new(AppSpec::new(
                "Launcher",
                LAUNCHER_SIZE,
                LauncherState::default,
                launcher_view,
            ))
            .window_options(WindowOptions {
                transparent: true,
                borderless: true,
                ..Default::default()
            })
            .run();
            return;
        }
        Some(other) => {
            eprintln!("layer-shell-demo: unknown --role={other}");
            std::process::exit(2);
        }
        None => {}
    }

    let counters = Arc::new(Counters::default());
    static COUNTERS: std::sync::OnceLock<Arc<Counters>> = std::sync::OnceLock::new();
    if COUNTERS.set(counters.clone()).is_err() {
        return;
    }

    let mut app = App::new(AppSpec::<MainState> {
        on_key: Some(|s: &mut MainState, ev, _| {
            let Some(label) = key_label(ev) else {
                return false;
            };
            push_bounded(&mut s.keys, &label);
            if let Some(c) = COUNTERS.get() {
                c.main_keys.fetch_add(1, Ordering::AcqRel);
                let mut guard = c.last_main.lock().unwrap_or_else(|e| e.into_inner());
                push_bounded(&mut guard, &label);
            }
            print_flushed(&format!("MAIN_KEY:{label}"));
            true
        }),
        ..AppSpec::new(
            "Layer Shell Demo",
            (800.0, 500.0),
            MainState::default,
            main_view,
        )
    })
    .keep_running();

    let bar_spec = OverlaySpec::new("ferrion-top-bar", (0.0, TOP_BAR_HEIGHT), bar_view)
        .options(OverlayOptions {
            placement: Some(OverlayPlacement::Edge {
                edge: ScreenEdge::Top,
                margin: 0.0,
                align: Align::Center,
                inset: 0.0,
            }),
            keys: KeyPolicy::None,
            input: OverlayInput::None,
            ..Default::default()
        })
        .layer(OverlayLayer::Top)
        .exclusive_zone(TOP_BAR_EXCLUSIVE_ZONE);
    let bar_handle = app.overlay(bar_spec, BarState);

    let launcher_spec = OverlaySpec::new("ferrion-launcher", LAUNCHER_SIZE, launcher_view)
        .options(OverlayOptions {
            placement: Some(OverlayPlacement::Edge {
                edge: ScreenEdge::Top,
                margin: LAUNCHER_TOP_MARGIN,
                align: Align::Center,
                inset: 0.0,
            }),
            keys: KeyPolicy::Exclusive,
            input: OverlayInput::All,
            ..Default::default()
        })
        .layer(OverlayLayer::Overlay)
        .on_key(|s, ev, _| {
            let Some(label) = key_label(ev) else {
                return false;
            };
            push_bounded(&mut s.typed, &label);
            if let Some(c) = COUNTERS.get() {
                c.launcher_keys.fetch_add(1, Ordering::AcqRel);
                let mut guard = c.last_launcher.lock().unwrap_or_else(|e| e.into_inner());
                push_bounded(&mut guard, &label);
            }
            print_flushed(&format!("LAUNCHER_KEY:{label}"));
            true
        });
    let launcher_handle = app.overlay(launcher_spec, LauncherState::default());

    let app_handle = app.handle();
    if let Some(path) = arg_value("--control=") {
        spawn_control_loop(path, app_handle, bar_handle, launcher_handle, counters);
    } else {
        bar_handle.show();
    }

    app.run();
}
