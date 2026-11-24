#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader, Write};

use rune::tok::{self, type_};
use rune::{
    App, AppHandle, AppSpec, Color, Cx, El, PopupAnchor, PopupItem, PopupMenu, TextEdit, Tx, col,
    popup_menu_supported, row, text,
};

const MAX_CONTROL_COMMANDS: usize = 64;

struct DemoState {
    field: TextEdit,
    picks: u32,
    dismisses: u32,
    last_picked: String,
    window_blurs: u32,
    window_focused: bool,
    menu_open: bool,
}

impl Default for DemoState {
    fn default() -> Self {
        Self {
            field: TextEdit::new("keep-me"),
            picks: 0,
            dismisses: 0,
            last_picked: String::new(),
            window_blurs: 0,
            window_focused: true,
            menu_open: false,
        }
    }
}

fn demo_menu() -> PopupMenu {
    let sub = PopupMenu::new()
        .item(PopupItem::new("sub-one", "Sub One"))
        .item(PopupItem::new("sub-two", "Sub Two"));
    PopupMenu::new()
        .item(PopupItem::new("alpha", "Alpha").checked(true))
        .item(PopupItem::new("beta", "Beta"))
        .separator()
        .item(PopupItem::new("gamma", "Gamma"))
        .submenu("More", sub)
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

fn open_menu_at(s: &mut DemoState, tx: &mut Tx, anchor: impl Into<PopupAnchor>) {
    s.menu_open = true;
    tx.popup_menu(demo_menu(), anchor, |s: &mut DemoState, picked, tx: &mut Tx| {
        s.menu_open = false;
        let focused = tx.focused().unwrap_or("");
        match picked {
            Some(id) => {
                s.picks = s.picks.saturating_add(1);
                s.last_picked = id.to_owned();
                print_flushed(&format!(
                    "POPUP_RESULT:picked={id}|picks={}|dismisses={}|field={}|focused={focused}|blurs={}",
                    s.picks, s.dismisses, s.field.text, s.window_blurs
                ));
            }
            None => {
                s.dismisses = s.dismisses.saturating_add(1);
                s.last_picked.clear();
                print_flushed(&format!(
                    "POPUP_RESULT:picked=NONE|picks={}|dismisses={}|field={}|focused={focused}|blurs={}",
                    s.picks, s.dismisses, s.field.text, s.window_blurs
                ));
            }
        }
    });
}

fn view(s: &DemoState, _: &mut Cx<DemoState>) -> El {
    let t = tok::theme(true);
    let accent = tok::accent(0, true);
    let status = format!(
        "picks={} dismisses={} last={} blurs={}",
        s.picks, s.dismisses, s.last_picked, s.window_blurs
    );
    col()
        .w_full()
        .h_full()
        .padding(12.0)
        .gap(10.0)
        .fill(Color {
            r: 0.08,
            g: 0.10,
            b: 0.14,
            a: 1.0,
        })
        .on_window_focus(|s: &mut DemoState, focused, _| {
            s.window_focused = focused;
            if !focused {
                s.window_blurs = s.window_blurs.saturating_add(1);
            }
        })
        .on_secondary_press(|s: &mut DemoState, press, tx: &mut Tx| {
            open_menu_at(s, tx, press);
        })
        .child(
            row()
                .w_full()
                .gap(8.0)
                .items_center()
                .child(
                    text("Popup Demo")
                        .font(type_::TITLE)
                        .color(t.text)
                        .grow(1.0),
                )
                .child(
                    row()
                        .key("menu-trigger")
                        .px(10.0)
                        .py(6.0)
                        .radius(6.0)
                        .fill(accent)
                        .on_press_tx(|s: &mut DemoState, tx: &mut Tx| {
                            open_menu_at(s, tx, "menu-trigger");
                        })
                        .child(text("Menu").font(type_::BODY).color(Color::WHITE)),
                ),
        )
        .child(
            rune::text_input(&s.field, type_::BODY)
                .single_line()
                .key("editor")
                .label("Editor")
                .w_full()
                .h(32.0)
                .px(8.0)
                .py(6.0)
                .radius(6.0)
                .fill(t.card)
                .border(1.0, t.card_border)
                .color(t.text)
                .on_edit(|s: &mut DemoState| &mut s.field),
        )
        .child(text(status).font(type_::LABEL).color(t.text_secondary))
}

fn spawn_control(path: String, handle: AppHandle<DemoState>) {
    std::thread::spawn(move || {
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(err) => {
                eprintln!("popup-menu-demo: open control {path}: {err}");
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
            if cmd == "supported" {
                print_flushed(&format!("POPUP_SUPPORTED={}", popup_menu_supported()));
            } else if cmd == "focus-field" {
                handle.update(|_, tx| tx.focus("editor", true));
                print_flushed("ACK:focus-field");
            } else if cmd == "open-edge" {
                handle.update(|s, tx| open_menu_at(s, tx, (320.0_f32, 40.0_f32)));
                print_flushed("ACK:open-edge");
            } else if cmd == "open-anchor" {
                handle.update(|s, tx| open_menu_at(s, tx, "menu-trigger"));
                print_flushed("ACK:open-anchor");
            } else if cmd == "status" {
                handle.update(|s, tx| {
                    let focused = tx.focused().unwrap_or("");
                    print_flushed(&format!(
                        "STATUS:supported={}|picks={}|dismisses={}|last={}|field={}|focused={focused}|blurs={}|wfocus={}|open={}",
                        popup_menu_supported(),
                        s.picks,
                        s.dismisses,
                        s.last_picked,
                        s.field.text,
                        s.window_blurs,
                        s.window_focused,
                        s.menu_open
                    ));
                });
            } else if cmd == "quit" {
                print_flushed("ACK:quit");
                handle.quit();
                break;
            } else {
                eprintln!("popup-menu-demo: unknown command: {cmd}");
            }
        }
    });
}

fn main() {
    let title = arg_value("--title=").unwrap_or_else(|| "Popup Menu Demo".to_owned());
    let title_ref: &'static str = Box::leak(title.into_boxed_str());
    let app = App::new(AppSpec::new(
        title_ref,
        (320.0, 240.0),
        DemoState::default,
        view,
    ));
    let handle = app.handle();
    if let Some(path) = arg_value("--control=") {
        spawn_control(path, handle);
    }
    app.run();
}
