use std::path::Path;

use crate::frame::host_step::draw_ui;
use crate::popup_menu::{Entry, PopupMenu};
use crate::render::surface_alpha::AlphaSurface;
use crate::render::{Gpu, OFFSCREEN_FORMAT, Offscreen, Renderer, write_png};
use crate::tok::{self, type_};
use crate::{Color, Cx, El, Env, Runtime, col, el, row, text};

use super::{MAX_POPUP_DIM, MAX_POPUP_ITEMS, MAX_SUBMENU_DEPTH, PopupWaylandError};

const MENU_PAD_X: f32 = 4.0;
const MENU_PAD_Y: f32 = 4.0;
const ITEM_HEIGHT: f32 = 28.0;
const SEPARATOR_HEIGHT: f32 = 9.0;
const MENU_MIN_WIDTH: f32 = 160.0;
const MENU_MAX_WIDTH: f32 = 360.0;
const MENU_RADIUS: f32 = 8.0;
const CHECK_COL_WIDTH: f32 = 18.0;
const CHAR_ESTIMATE_PX: f32 = 7.5;
const ROW_EXTRA_PX: f32 = 52.0;

#[derive(Clone)]
pub(super) enum MenuRowKind {
    Item {
        id: String,
        title: String,
        shortcut: Option<String>,
        checked: bool,
        enabled: bool,
    },
    Separator,
    Submenu {
        title: String,
        menu: PopupMenu,
        enabled: bool,
    },
}

impl MenuRowKind {
    pub(super) fn is_selectable(&self) -> bool {
        match self {
            MenuRowKind::Item { enabled, .. } => *enabled,
            MenuRowKind::Submenu { enabled, .. } => *enabled,
            MenuRowKind::Separator => false,
        }
    }

    fn height(&self) -> f32 {
        match self {
            MenuRowKind::Separator => SEPARATOR_HEIGHT,
            MenuRowKind::Item { .. } | MenuRowKind::Submenu { .. } => ITEM_HEIGHT,
        }
    }

    fn estimated_width(&self) -> f32 {
        match self {
            MenuRowKind::Separator => 0.0,
            MenuRowKind::Item {
                title, shortcut, ..
            } => {
                let sc = shortcut.as_deref().map_or(0.0, |s| {
                    (s.chars().count() as f32) * CHAR_ESTIMATE_PX + 16.0
                });
                (title.chars().count() as f32) * CHAR_ESTIMATE_PX + sc + ROW_EXTRA_PX
            }
            MenuRowKind::Submenu { title, .. } => {
                (title.chars().count() as f32) * CHAR_ESTIMATE_PX + 20.0 + ROW_EXTRA_PX
            }
        }
    }
}

pub(super) enum MenuAction {
    Pick(String),
    OpenSubmenu {
        row_index: usize,
        anchor_rect: [i32; 4],
        menu: PopupMenu,
    },
    SubmenuDepthError,
}

#[derive(Clone)]
pub(super) struct MenuViewState {
    pub(super) rows: Vec<MenuRowKind>,
    pub(super) row_rects: Vec<(f32, f32)>,
    pub(super) highlighted: Option<usize>,
    pub(super) dark: bool,
    pub(super) width: f32,
    pub(super) height: f32,
    pub(super) depth: usize,
}

impl MenuViewState {
    pub(super) fn from_menu(
        menu: &PopupMenu,
        dark: bool,
        min_width: f32,
        depth: usize,
    ) -> Result<Self, PopupWaylandError> {
        if menu.entries.is_empty() {
            return Err(PopupWaylandError::EmptyMenu);
        }
        if menu.entries.len() > MAX_POPUP_ITEMS {
            return Err(PopupWaylandError::TooManyItems);
        }
        let mut rows = Vec::with_capacity(menu.entries.len());
        let mut row_rects = Vec::with_capacity(menu.entries.len());
        let mut y = MENU_PAD_Y;
        let mut width = min_width.max(MENU_MIN_WIDTH);
        for entry in &menu.entries {
            let row_kind = match entry {
                Entry::Item(item) => MenuRowKind::Item {
                    id: item.id.clone(),
                    title: item.title.clone(),
                    shortcut: item.key_equivalent.as_ref().map(ToString::to_string),
                    checked: item.checked,
                    enabled: item.enabled,
                },
                Entry::Separator => MenuRowKind::Separator,
                Entry::Submenu(title, sub, enabled) => MenuRowKind::Submenu {
                    title: title.clone(),
                    menu: sub.clone(),
                    enabled: *enabled && depth + 1 < MAX_SUBMENU_DEPTH,
                },
            };
            width = width.max(row_kind.estimated_width());
            let h = row_kind.height();
            row_rects.push((y, y + h));
            y += h;
            rows.push(row_kind);
        }
        let width = width
            .ceil()
            .clamp(MENU_MIN_WIDTH, MENU_MAX_WIDTH.min(MAX_POPUP_DIM as f32));
        let height = (y + MENU_PAD_Y).ceil().clamp(1.0, MAX_POPUP_DIM as f32);
        Ok(Self {
            rows,
            row_rects,
            highlighted: None,
            dark,
            width,
            height,
            depth,
        })
    }

    pub(super) fn logical_size(&self) -> (i32, i32) {
        (
            self.width.round().max(1.0) as i32,
            self.height.round().max(1.0) as i32,
        )
    }

    pub(super) fn step_highlight(&mut self, dir: i32) -> bool {
        let len = self.rows.len();
        if len == 0 {
            return false;
        }
        let start = match self.highlighted {
            Some(idx) if dir > 0 => (idx + 1) % len,
            Some(idx) => (idx + len - 1) % len,
            None if dir > 0 => 0,
            None => len - 1,
        };
        for offset in 0..len {
            let idx = if dir > 0 {
                (start + offset) % len
            } else {
                (start + len - offset) % len
            };
            if self.rows[idx].is_selectable() {
                let changed = self.highlighted != Some(idx);
                self.highlighted = Some(idx);
                return changed;
            }
        }
        false
    }

    pub(super) fn jump_highlight(&mut self, first: bool) -> bool {
        let found = if first {
            self.rows.iter().position(MenuRowKind::is_selectable)
        } else {
            self.rows.iter().rposition(MenuRowKind::is_selectable)
        };
        let changed = self.highlighted != found;
        self.highlighted = found;
        changed
    }

    pub(super) fn hit_row(&self, x: f32, y: f32) -> Option<usize> {
        if x < 0.0 || x > self.width || y < 0.0 || y > self.height {
            return None;
        }
        self.row_rects
            .iter()
            .position(|&(y0, y1)| y >= y0 && y < y1)
    }

    pub(super) fn set_hover(&mut self, x: f32, y: f32) -> bool {
        let next = self
            .hit_row(x, y)
            .filter(|&idx| self.rows[idx].is_selectable());
        let changed = self.highlighted != next;
        self.highlighted = next;
        changed
    }

    pub(super) fn activate_row(&self, idx: usize) -> Option<MenuAction> {
        match self.rows.get(idx)? {
            MenuRowKind::Item {
                id, enabled: true, ..
            } => Some(MenuAction::Pick(id.clone())),
            MenuRowKind::Submenu {
                menu,
                enabled: true,
                ..
            } => {
                if self.depth + 1 >= MAX_SUBMENU_DEPTH {
                    return Some(MenuAction::SubmenuDepthError);
                }
                let (y0, y1) = self.row_rects[idx];
                Some(MenuAction::OpenSubmenu {
                    row_index: idx,
                    anchor_rect: [
                        0,
                        y0.round() as i32,
                        self.width.round().max(1.0) as i32,
                        (y1 - y0).round().max(1.0) as i32,
                    ],
                    menu: menu.clone(),
                })
            }
            _ => None,
        }
    }

    pub(super) fn activate_highlighted(&self) -> Option<MenuAction> {
        let idx = self.highlighted?;
        self.activate_row(idx)
    }
}

pub(super) fn build_menu_runtime(state: MenuViewState, scale: f32) -> Runtime<MenuViewState> {
    let viewport = (state.width, state.height);
    let env = Env {
        system_dark: state.dark,
        scale: scale.max(1.0),
    };
    let mut rt = Runtime::new(state, build_menu_el, viewport, env);
    rt.core_mut().transparent = true;
    rt.set_srgb_blend(true);
    rt
}

fn build_menu_el(s: &MenuViewState, _: &mut Cx<MenuViewState>) -> El {
    let t = tok::theme(s.dark);
    let accent = tok::accent(0, s.dark);
    let mut root = col()
        .w(s.width)
        .h(s.height)
        .px(MENU_PAD_X)
        .py(MENU_PAD_Y)
        .radius(MENU_RADIUS)
        .fill(t.card)
        .border(1.0, t.card_border);

    for (idx, item) in s.rows.iter().enumerate() {
        let active = s.highlighted == Some(idx);
        let child = match item {
            MenuRowKind::Separator => col()
                .w_full()
                .h(SEPARATOR_HEIGHT)
                .justify_center()
                .px(8.0)
                .child(el().w_full().h(1.0).fill(t.separator)),
            MenuRowKind::Item {
                title,
                shortcut,
                checked,
                enabled,
                ..
            } => {
                let bg = if active && *enabled {
                    accent
                } else {
                    Color::TRANSPARENT
                };
                let fg = if active && *enabled {
                    Color::WHITE
                } else if *enabled {
                    t.text
                } else {
                    t.text_secondary
                };
                let mut r = row()
                    .w_full()
                    .h(ITEM_HEIGHT)
                    .px(8.0)
                    .radius(5.0)
                    .items_center()
                    .fill(bg)
                    .child(
                        row().w(CHECK_COL_WIDTH).h_full().items_center().child(
                            text(if *checked { "✓" } else { "" })
                                .font(type_::BODY)
                                .color(fg),
                        ),
                    )
                    .child(text(title.clone()).font(type_::BODY).color(fg).grow(1.0));
                if let Some(sc) = shortcut {
                    let sc_fg = if active && *enabled {
                        Color::WHITE
                    } else {
                        t.text_secondary
                    };
                    r = r.child(text(sc.clone()).font(type_::LABEL).color(sc_fg));
                }
                r
            }
            MenuRowKind::Submenu { title, enabled, .. } => {
                let bg = if active && *enabled {
                    accent
                } else {
                    Color::TRANSPARENT
                };
                let fg = if active && *enabled {
                    Color::WHITE
                } else if *enabled {
                    t.text
                } else {
                    t.text_secondary
                };
                row()
                    .w_full()
                    .h(ITEM_HEIGHT)
                    .px(8.0)
                    .radius(5.0)
                    .items_center()
                    .fill(bg)
                    .child(el().w(CHECK_COL_WIDTH).h_full())
                    .child(text(title.clone()).font(type_::BODY).color(fg).grow(1.0))
                    .child(text("›").font(type_::BODY).color(fg))
            }
        };
        root = root.child(child);
    }
    root
}

pub(crate) fn snap_popup_offscreen(
    gpu: &Gpu,
    menu: &PopupMenu,
    dark: bool,
    min_width: f32,
    scale: f32,
    path: &Path,
) -> Result<(), String> {
    let state = MenuViewState::from_menu(menu, dark, min_width, 0).map_err(|e| e.to_string())?;
    let (lw, lh) = state.logical_size();
    let scale = scale.max(1.0);
    let wp = ((lw as f32) * scale).round().max(1.0) as u32;
    let hp = ((lh as f32) * scale).round().max(1.0) as u32;
    let mut rt = build_menu_runtime(state, scale);
    let srgb_blend = rt.core().srgb_blend();
    let target_format = if srgb_blend {
        OFFSCREEN_FORMAT.remove_srgb_suffix()
    } else {
        OFFSCREEN_FORMAT
    };
    let mut renderer = Renderer::new(&gpu.device, target_format);
    renderer.set_srgb_blend(srgb_blend);
    renderer.attach_text(&mut rt.core_mut().text.atlas);
    rt.frame(0.0);
    let target = Offscreen::with_format(&gpu.device, wp, hp, target_format);
    let alpha = AlphaSurface::new(&gpu.device, target_format, (wp, hp));
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("popup-snap"),
        });
    let clear = rt.core().clear_color();
    draw_ui(
        &mut renderer,
        gpu,
        &mut encoder,
        &alpha.view,
        rt.core_mut(),
        (wp, hp),
        clear,
    );
    alpha.draw(&mut encoder, &target.view);
    target.copy(&mut encoder);
    let _ = renderer.submit(&gpu.queue, encoder);
    let px = target.read(&gpu.device);
    write_png(path, wp, hp, &px).map_err(|e| format!("{}: {e}", path.display()))
}
