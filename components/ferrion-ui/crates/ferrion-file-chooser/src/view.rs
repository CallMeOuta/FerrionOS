#![forbid(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rune::file_dialog::chooser::ChooserReply;
use rune::tok::{self, radius, space, type_};
use rune::{
    Color, Cx, El, FileDialogKind, Key, KeyEvent, Tx, col, el, row, spacer, text, text_input,
};

use crate::model::{ChooserModel, ListingState, VISIBLE_ROWS, display_name};

pub struct ChooserAppState {
    pub model: ChooserModel,
    pub emit: Arc<dyn Fn(ChooserReply) + Send + Sync>,
    pub emitted: Arc<AtomicBool>,
}

impl ChooserAppState {
    pub fn finish(&mut self, reply: ChooserReply, tx: &mut Tx) {
        if !self.emitted.swap(true, Ordering::AcqRel) {
            (self.emit)(reply);
        }
        tx.quit();
    }
}

pub fn chooser_view(s: &ChooserAppState, cx: &mut Cx<ChooserAppState>) -> El {
    let dark = cx.system_dark();
    let t = tok::theme(dark);
    let accent = tok::accent(0, dark);
    let hair = 1.0 / cx.env().scale.max(1.0);
    let m = &s.model;

    let header = row()
        .key("chooser-header")
        .w_full()
        .px(space::S3)
        .py(space::S3)
        .gap(space::S3)
        .center_y()
        .fill(t.card)
        .border(hair, t.separator)
        .child(
            text(&m.title)
                .key("chooser-title")
                .font(type_::TITLE)
                .color(t.text),
        )
        .child(spacer())
        .child(
            row()
                .key("btn-up")
                .role("button")
                .label("Parent folder")
                .px(space::S3)
                .py(space::S1)
                .radius(radius::MD)
                .fill(t.control_off)
                .child(text("Up").font(type_::LABEL).color(t.text))
                .on_press(|s: &mut ChooserAppState| s.model.navigate_up()),
        )
        .child(
            row()
                .key("btn-filter")
                .role("button")
                .label(format!("Filter: {}", m.active_filter_label()))
                .px(space::S3)
                .py(space::S1)
                .radius(radius::MD)
                .fill(t.control_off)
                .child(
                    text(m.active_filter_label())
                        .font(type_::LABEL)
                        .color(t.text_secondary),
                )
                .on_press(|s: &mut ChooserAppState| s.model.cycle_filter()),
        );

    let path_bar = row()
        .key("path-bar")
        .w_full()
        .px(space::S3)
        .py(space::S2)
        .fill(t.bg)
        .child(
            text(m.current_dir.display().to_string())
                .key("current-dir")
                .font(type_::LABEL)
                .color(t.text_secondary),
        );

    let mut list_box = col()
        .key("entry-list")
        .role("listbox")
        .label("Directory entries")
        .w_full()
        .grow(1.0)
        .px(space::S3)
        .py(space::S2)
        .gap(2.0);

    match &m.listing {
        ListingState::Error(err_msg) => {
            list_box = list_box.child(
                row()
                    .key("dir-error")
                    .role("status")
                    .label(err_msg.clone())
                    .w_full()
                    .px(space::S3)
                    .py(space::S3)
                    .radius(radius::MD)
                    .fill(Color {
                        r: 0.45,
                        g: 0.12,
                        b: 0.14,
                        a: 0.9,
                    })
                    .child(text(err_msg.clone()).font(type_::BODY).color(Color::WHITE)),
            );
        }
        ListingState::Ok { entries, .. } => {
            if entries.is_empty() {
                list_box = list_box.child(
                    row()
                        .key("dir-empty")
                        .role("status")
                        .label("No matching entries")
                        .w_full()
                        .px(space::S3)
                        .py(space::S3)
                        .child(
                            text("No matching entries")
                                .font(type_::BODY)
                                .color(t.text_secondary),
                        ),
                );
            } else {
                let start = m.scroll_offset.min(entries.len());
                let end = (start + VISIBLE_ROWS).min(entries.len());
                for (offset, entry) in entries[start..end].iter().enumerate() {
                    let idx = start + offset;
                    let selected = m.selected == Some(idx);
                    let prefix = if entry.is_dir { "[dir]" } else { "[file]" };
                    let row_label = format!("{prefix} {}", entry.name);
                    let row_bg = if selected {
                        accent.alpha(0.85)
                    } else {
                        Color::TRANSPARENT
                    };
                    let row_fg = if selected { Color::WHITE } else { t.text };
                    list_box = list_box.child(
                        row()
                            .key(format!("entry-{idx}"))
                            .role("option")
                            .label(row_label.clone())
                            .w_full()
                            .h(28.0)
                            .px(space::S3)
                            .radius(radius::SM)
                            .center_y()
                            .fill(row_bg)
                            .child(text(row_label).font(type_::BODY).color(row_fg))
                            .on_press(move |s: &mut ChooserAppState| s.model.select_index(idx))
                            .on_double_press_tx(move |s: &mut ChooserAppState, tx: &mut Tx| {
                                if let Some(reply) = s.model.activate_index(idx) {
                                    s.finish(reply, tx);
                                }
                            }),
                    );
                }
            }
            if let Some(msg) = m.truncated_message() {
                list_box = list_box.child(
                    row()
                        .key("truncated-row")
                        .role("status")
                        .label(msg.clone())
                        .w_full()
                        .px(space::S3)
                        .py(space::S2)
                        .radius(radius::SM)
                        .fill(t.card)
                        .border(hair, t.separator)
                        .child(text(msg).font(type_::LABEL).color(t.text_secondary)),
                );
            }
        }
    }

    let mut root = col()
        .key("chooser-root")
        .role("window")
        .label(m.title.clone())
        .w_full()
        .h_full()
        .fill(t.bg)
        .focusable(true)
        .on_key(handle_key)
        .child(header)
        .child(path_bar)
        .child(list_box);

    if m.kind == FileDialogKind::Save {
        let name_field = text_input(&m.save_name, type_::BODY.lh(24.0))
            .key("save-name")
            .label("File name")
            .single_line()
            .grow(1.0)
            .color(t.text)
            .placeholder("File name", t.text_secondary)
            .on_edit(|s: &mut ChooserAppState| &mut s.model.save_name)
            .on_input_key(handle_save_input_key)
            .on_submit(|s: &mut ChooserAppState, tx: &mut Tx| {
                if let Some(reply) = s.model.confirm_primary() {
                    s.finish(reply, tx);
                }
            });
        root = root.child(
            row()
                .key("save-bar")
                .w_full()
                .px(space::S3)
                .py(space::S2)
                .gap(space::S3)
                .center_y()
                .fill(t.card)
                .border(hair, t.separator)
                .child(text("Name:").font(type_::LABEL).color(t.text_secondary))
                .child(
                    row()
                        .key("save-input-box")
                        .grow(1.0)
                        .px(space::S2)
                        .py(space::S1)
                        .radius(radius::SM)
                        .fill(t.bg)
                        .border(hair, t.separator)
                        .child(name_field),
                ),
        );
    }

    if let Some(err) = &m.inline_error {
        root = root.child(
            row()
                .key("inline-error")
                .role("status")
                .label(err.clone())
                .w_full()
                .px(space::S3)
                .py(space::S2)
                .fill(Color {
                    r: 0.45,
                    g: 0.12,
                    b: 0.14,
                    a: 0.9,
                })
                .child(text(err.clone()).font(type_::LABEL).color(Color::WHITE)),
        );
    }

    if let Some(target) = &m.overwrite_confirm {
        let prompt = format!("Overwrite {}?", display_name(target));
        root = root.child(
            row()
                .key("overwrite-prompt")
                .role("alertdialog")
                .label(prompt.clone())
                .w_full()
                .px(space::S3)
                .py(space::S3)
                .gap(space::S3)
                .center_y()
                .fill(Color {
                    r: 0.42,
                    g: 0.28,
                    b: 0.08,
                    a: 0.95,
                })
                .child(text(prompt).font(type_::BODY).color(Color::WHITE))
                .child(spacer())
                .child(
                    row()
                        .key("overwrite-cancel")
                        .role("button")
                        .label("Cancel overwrite")
                        .px(space::S3)
                        .py(space::S1)
                        .radius(radius::MD)
                        .fill(t.control_off)
                        .child(text("Back").font(type_::LABEL).color(t.text))
                        .on_press(|s: &mut ChooserAppState| {
                            s.model.overwrite_confirm = None;
                        }),
                )
                .child(
                    row()
                        .key("overwrite-confirm")
                        .role("button")
                        .label("Confirm overwrite")
                        .px(space::S3)
                        .py(space::S1)
                        .radius(radius::MD)
                        .fill(accent)
                        .child(text("Overwrite").font(type_::LABEL).color(Color::WHITE))
                        .on_press_tx(|s: &mut ChooserAppState, tx: &mut Tx| {
                            if let Some(reply) = s.model.confirm_primary() {
                                s.finish(reply, tx);
                            }
                        }),
                ),
        );
    }

    let primary_label = if m.overwrite_confirm.is_some() {
        "Overwrite"
    } else {
        match m.kind {
            FileDialogKind::Open => "Open",
            FileDialogKind::Save => "Save",
            FileDialogKind::Folder => "Choose Folder",
        }
    };
    let can_confirm = m.can_confirm();
    let confirm_bg = if can_confirm { accent } else { t.control_off };
    let confirm_fg = if can_confirm {
        Color::WHITE
    } else {
        t.text_secondary
    };

    let footer = row()
        .key("chooser-footer")
        .w_full()
        .px(space::S3)
        .py(space::S3)
        .gap(space::S3)
        .center_y()
        .fill(t.card)
        .border(hair, t.separator)
        .child(el().w(1.0))
        .child(spacer())
        .child(
            row()
                .key("btn-cancel")
                .role("button")
                .label("Cancel")
                .px(space::S3)
                .py(space::S2)
                .radius(radius::MD)
                .fill(t.control_off)
                .child(text("Cancel").font(type_::BODY).color(t.text))
                .on_press_tx(|s: &mut ChooserAppState, tx: &mut Tx| {
                    if let Some(reply) = s.model.cancel_overwrite_or_dialog() {
                        s.finish(reply, tx);
                    }
                }),
        )
        .child(
            row()
                .key("btn-confirm")
                .role("button")
                .label(primary_label)
                .px(space::S3)
                .py(space::S2)
                .radius(radius::MD)
                .fill(confirm_bg)
                .child(text(primary_label).font(type_::BODY).color(confirm_fg))
                .on_press_tx(|s: &mut ChooserAppState, tx: &mut Tx| {
                    if let Some(reply) = s.model.confirm_primary() {
                        s.finish(reply, tx);
                    }
                }),
        );

    root.child(footer)
}

fn handle_key(s: &mut ChooserAppState, ev: &KeyEvent, tx: &mut Tx) -> bool {
    match ev.key {
        Key::Down => {
            s.model.move_selection(1);
            true
        }
        Key::Up => {
            s.model.move_selection(-1);
            true
        }
        Key::PageDown => {
            s.model.move_selection(16);
            true
        }
        Key::PageUp => {
            s.model.move_selection(-16);
            true
        }
        Key::Home => {
            s.model.select_index(0);
            true
        }
        Key::End => {
            if let ListingState::Ok { entries, .. } = &s.model.listing
                && !entries.is_empty()
            {
                s.model.select_index(entries.len() - 1);
            }
            true
        }
        Key::Backspace => {
            s.model.navigate_up();
            true
        }
        Key::Escape => {
            if let Some(reply) = s.model.cancel_overwrite_or_dialog() {
                s.finish(reply, tx);
            }
            true
        }
        Key::Enter => {
            if let Some(reply) = s.model.confirm_primary() {
                s.finish(reply, tx);
            }
            true
        }
        _ => false,
    }
}

fn handle_save_input_key(s: &mut ChooserAppState, ev: &KeyEvent, tx: &mut Tx) -> bool {
    match ev.key {
        Key::Down => {
            s.model.move_selection(1);
            true
        }
        Key::Up => {
            s.model.move_selection(-1);
            true
        }
        Key::Escape => {
            if let Some(reply) = s.model.cancel_overwrite_or_dialog() {
                s.finish(reply, tx);
            }
            true
        }
        _ => false,
    }
}
