use std::cell::RefCell;
use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use wayland_protocols::xdg::shell::client::xdg_positioner::{Anchor, Gravity};
use winit::platform::wayland::WindowExtWayland;
use winit::window::Window;
use xkbcommon::xkb::keysyms;

use crate::popup_menu::{Session, Show};
use crate::render::Gpu;
use crate::runtime::{Runtime, Waker};

mod dispatch;
pub(crate) mod menu_view;
mod present;
mod protocol;

use dispatch::ProtoEvent;
use menu_view::{MenuAction, MenuViewState, build_menu_runtime};
use present::LevelPresenter;
use protocol::{PopupConn, PopupParent, PopupPlacement, PopupSurfaceHandles};

pub(crate) const MAX_POPUP_DIM: u32 = 4096;
pub(crate) const MAX_POPUP_ITEMS: usize = 256;
pub(crate) const MAX_SUBMENU_DEPTH: usize = 4;
pub(crate) const MAX_CONFIGURE_WAIT: Duration = Duration::from_secs(2);
pub(crate) const MAX_POPUP_EVENTS_PER_TURN: usize = 256;

const BTN_LEFT: u32 = 0x110;

static XDG_WM_BASE_AVAILABLE: AtomicBool = AtomicBool::new(false);
static DARK_SCHEME: AtomicBool = AtomicBool::new(true);
static NEXT_TRACKING_ID: AtomicU64 = AtomicU64::new(1);
static LOGGED_ERROR_BITS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static ACTIVE: RefCell<Option<ActivePopup>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PopupWaylandError {
    NoDisplay,
    MissingGlobal(&'static str),
    MissingParentSurface,
    NoInputSerial,
    ConfigureTimeout,
    DismissedBeforeConfigure,
    EmptyMenu,
    TooManyItems,
    SubmenuDepthExceeded,
    SurfaceCreate(String),
    Present(String),
}

impl PopupWaylandError {
    fn log_bit(&self) -> u64 {
        match self {
            Self::NoDisplay => 1 << 0,
            Self::MissingGlobal(_) => 1 << 1,
            Self::MissingParentSurface => 1 << 2,
            Self::NoInputSerial => 1 << 3,
            Self::ConfigureTimeout => 1 << 4,
            Self::DismissedBeforeConfigure => 1 << 5,
            Self::EmptyMenu => 1 << 6,
            Self::TooManyItems => 1 << 7,
            Self::SubmenuDepthExceeded => 1 << 8,
            Self::SurfaceCreate(_) => 1 << 9,
            Self::Present(_) => 1 << 10,
        }
    }

    fn log_once(&self) {
        let bit = self.log_bit();
        if LOGGED_ERROR_BITS.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
            eprintln!("rune: popup_menu: {self}");
        }
    }
}

impl fmt::Display for PopupWaylandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDisplay => f.write_str("no Wayland display attached to this process"),
            Self::MissingGlobal(name) => write!(f, "Wayland compositor does not advertise {name}"),
            Self::MissingParentSurface => f.write_str("parent window has no Wayland xdg_surface"),
            Self::NoInputSerial => {
                f.write_str("Wayland xdg_popup grab requires a non-zero input serial")
            }
            Self::ConfigureTimeout => write!(
                f,
                "Wayland xdg_popup configure timed out after {} ms",
                MAX_CONFIGURE_WAIT.as_millis()
            ),
            Self::DismissedBeforeConfigure => {
                f.write_str("Wayland compositor dismissed xdg_popup before configure")
            }
            Self::EmptyMenu => f.write_str("Wayland pop-up menu has no entries"),
            Self::TooManyItems => {
                write!(f, "Wayland pop-up menu exceeded {MAX_POPUP_ITEMS} entries")
            }
            Self::SubmenuDepthExceeded => {
                write!(f, "Wayland submenu depth exceeded {MAX_SUBMENU_DEPTH}")
            }
            Self::SurfaceCreate(reason) => write!(f, "Wayland xdg_popup surface failed: {reason}"),
            Self::Present(reason) => write!(f, "Wayland xdg_popup present failed: {reason}"),
        }
    }
}

pub(crate) fn attach_display(display: NonNull<c_void>) {
    let available = protocol::probe_xdg_wm_base(display);
    XDG_WM_BASE_AVAILABLE.store(available, Ordering::Release);
}

pub(crate) fn detach_display() {
    XDG_WM_BASE_AVAILABLE.store(false, Ordering::Release);
    ACTIVE.with(|slot| {
        if let Some(mut active) = slot.borrow_mut().take() {
            active.close_all(None);
        }
    });
}

pub(super) fn supported() -> bool {
    XDG_WM_BASE_AVAILABLE.load(Ordering::Acquire)
}

pub(super) type Tracking = u64;

pub(super) fn scheme(dark: bool) {
    DARK_SCHEME.store(dark, Ordering::Release);
}

pub(super) fn start(_: Waker) {}

struct PopupLevel {
    handles: PopupSurfaceHandles,
    presenter: LevelPresenter,
    rt: Runtime<MenuViewState>,
    scale: i32,
    dirty: bool,
    open_submenu_row: Option<usize>,
}

impl PopupLevel {
    fn destroy(mut self, conn: &mut PopupConn) {
        self.presenter.clear_slots();
        drop(self.presenter);
        self.handles.destroy();
        conn.flush();
    }
}

struct ActivePopup {
    id: Tracking,
    conn: PopupConn,
    levels: Vec<PopupLevel>,
    session: Rc<Session>,
    wake: Option<Waker>,
    shared_gpu: Option<Arc<Gpu>>,
    dark: bool,
    scale: i32,
    grab_serial: u32,
    pointer_surface: Option<u32>,
    pointer_pos: (f32, f32),
}

impl ActivePopup {
    fn close_from(&mut self, keep_len: usize) {
        while self.levels.len() > keep_len {
            if let Some(level) = self.levels.pop() {
                level.destroy(&mut self.conn);
            }
        }
        if let Some(top) = self.levels.last_mut() {
            top.open_submenu_row = None;
        }
    }

    fn close_all(&mut self, picked: Option<String>) {
        self.close_from(0);
        let resolved = self.session.resolve(picked, None);
        if resolved && let Some(wake) = &self.wake {
            wake();
        }
    }

    fn open_submenu(
        &mut self,
        parent_idx: usize,
        row_index: usize,
        anchor_rect: [i32; 4],
        menu: crate::popup_menu::PopupMenu,
    ) {
        let next_depth = parent_idx + 1;
        if next_depth >= MAX_SUBMENU_DEPTH {
            PopupWaylandError::SubmenuDepthExceeded.log_once();
            return;
        }
        self.close_from(next_depth);
        let state = match MenuViewState::from_menu(&menu, self.dark, 0.0, next_depth) {
            Ok(s) => s,
            Err(err) => {
                err.log_once();
                return;
            }
        };
        let size = state.logical_size();
        let parent_xdg = self.levels[parent_idx].handles.xdg_surface.clone();
        let placement = PopupPlacement {
            size,
            anchor_rect,
            anchor: Anchor::TopRight,
            gravity: Gravity::BottomRight,
        };
        let handles = match self.conn.create_popup(
            PopupParent::PopupXdg(&parent_xdg),
            placement,
            self.grab_serial,
            self.scale,
        ) {
            Ok(h) => h,
            Err(err) => {
                err.log_once();
                return;
            }
        };
        let mut rt = build_menu_runtime(state, self.scale as f32);
        let mut presenter = match LevelPresenter::new(
            &self.conn,
            &handles,
            &mut rt,
            self.scale,
            self.shared_gpu.as_ref(),
        ) {
            Ok(p) => p,
            Err(err) => {
                err.log_once();
                handles.destroy();
                self.conn.flush();
                return;
            }
        };
        if let Err(err) = presenter.paint(&mut self.conn, &handles, &mut rt, self.scale) {
            err.log_once();
            presenter.clear_slots();
            drop(presenter);
            handles.destroy();
            self.conn.flush();
            return;
        }
        self.levels[parent_idx].open_submenu_row = Some(row_index);
        self.levels.push(PopupLevel {
            handles,
            presenter,
            rt,
            scale: self.scale,
            dirty: false,
            open_submenu_row: None,
        });
    }

    fn step(&mut self) -> bool {
        self.conn.dispatch_once();
        let released: Vec<u64> = self.conn.state.released_buffers.drain(..).collect();
        if !released.is_empty() {
            for level in &mut self.levels {
                level.presenter.mark_released(&released);
            }
        }
        let events: Vec<ProtoEvent> = self.conn.state.events.drain(..).collect();
        for ev in events {
            if self.levels.is_empty() {
                return false;
            }
            match ev {
                ProtoEvent::PopupDone { level_id } => {
                    if let Some(idx) = self
                        .levels
                        .iter()
                        .position(|l| l.handles.level_id == level_id)
                    {
                        if idx == 0 {
                            self.close_all(None);
                            return false;
                        }
                        self.close_from(idx);
                    }
                }
                ProtoEvent::PopupConfigured {
                    level_id,
                    rect: (_, _, w, h),
                } => {
                    if let Some(level) = self
                        .levels
                        .iter_mut()
                        .find(|l| l.handles.level_id == level_id)
                        && w > 0
                        && h > 0
                    {
                        level.handles.configured_size = (w, h);
                        level.dirty = true;
                    }
                }
                ProtoEvent::SurfaceScale { level_id, scale } => {
                    if let Some(level) = self
                        .levels
                        .iter_mut()
                        .find(|l| l.handles.level_id == level_id)
                        && scale > 0
                        && level.scale != scale
                    {
                        level.scale = scale;
                        level.dirty = true;
                    }
                }
                ProtoEvent::Key {
                    keysym,
                    pressed: true,
                    serial,
                } => {
                    if serial != 0 {
                        self.grab_serial = serial;
                    }
                    if !self.handle_key(keysym) {
                        return false;
                    }
                }
                ProtoEvent::Key { .. } => {}
                ProtoEvent::PointerEnter { surface_id, x, y } => {
                    self.pointer_surface = Some(surface_id);
                    self.pointer_pos = (x as f32, y as f32);
                    self.update_pointer_hover();
                }
                ProtoEvent::PointerLeave { surface_id } => {
                    if self.pointer_surface == Some(surface_id) {
                        self.pointer_surface = None;
                    }
                }
                ProtoEvent::PointerMotion { x, y } => {
                    self.pointer_pos = (x as f32, y as f32);
                    self.update_pointer_hover();
                }
                ProtoEvent::PointerButton {
                    button,
                    pressed: true,
                    serial,
                } => {
                    if serial != 0 {
                        self.grab_serial = serial;
                    }
                    if button == BTN_LEFT && !self.handle_click() {
                        return false;
                    }
                }
                ProtoEvent::PointerButton { .. } => {}
            }
        }
        for level in &mut self.levels {
            if level.dirty {
                if let Err(err) = level.presenter.paint(
                    &mut self.conn,
                    &level.handles,
                    &mut level.rt,
                    level.scale,
                ) {
                    err.log_once();
                }
                level.dirty = false;
            }
        }
        !self.levels.is_empty()
    }

    fn handle_key(&mut self, keysym: u32) -> bool {
        let Some(top_idx) = self.levels.len().checked_sub(1) else {
            return false;
        };
        match keysym {
            keysyms::KEY_Escape => {
                self.close_all(None);
                return false;
            }
            keysyms::KEY_Up | keysyms::KEY_KP_Up => {
                let changed = self.levels[top_idx].rt.state.step_highlight(-1);
                if changed {
                    self.levels[top_idx].dirty = true;
                }
            }
            keysyms::KEY_Down | keysyms::KEY_KP_Down => {
                let changed = self.levels[top_idx].rt.state.step_highlight(1);
                if changed {
                    self.levels[top_idx].dirty = true;
                }
            }
            keysyms::KEY_Home | keysyms::KEY_KP_Home => {
                let changed = self.levels[top_idx].rt.state.jump_highlight(true);
                if changed {
                    self.levels[top_idx].dirty = true;
                }
            }
            keysyms::KEY_End | keysyms::KEY_KP_End => {
                let changed = self.levels[top_idx].rt.state.jump_highlight(false);
                if changed {
                    self.levels[top_idx].dirty = true;
                }
            }
            keysyms::KEY_Left | keysyms::KEY_KP_Left if top_idx > 0 => {
                self.close_from(top_idx);
            }
            keysyms::KEY_Right | keysyms::KEY_KP_Right => {
                if let Some(MenuAction::OpenSubmenu {
                    row_index,
                    anchor_rect,
                    menu,
                }) = self.levels[top_idx].rt.state.activate_highlighted()
                {
                    self.open_submenu(top_idx, row_index, anchor_rect, menu);
                } else if matches!(
                    self.levels[top_idx].rt.state.activate_highlighted(),
                    Some(MenuAction::SubmenuDepthError)
                ) {
                    PopupWaylandError::SubmenuDepthExceeded.log_once();
                }
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_space => {
                match self.levels[top_idx].rt.state.activate_highlighted() {
                    Some(MenuAction::Pick(id)) => {
                        self.close_all(Some(id));
                        return false;
                    }
                    Some(MenuAction::OpenSubmenu {
                        row_index,
                        anchor_rect,
                        menu,
                    }) => {
                        self.open_submenu(top_idx, row_index, anchor_rect, menu);
                    }
                    Some(MenuAction::SubmenuDepthError) => {
                        PopupWaylandError::SubmenuDepthExceeded.log_once();
                    }
                    None => {}
                }
            }
            _ => {}
        }
        true
    }

    fn update_pointer_hover(&mut self) {
        let Some(surf_id) = self.pointer_surface else {
            return;
        };
        let Some(idx) = self
            .levels
            .iter()
            .position(|l| l.handles.surface_protocol_id() == surf_id)
        else {
            return;
        };
        let (x, y) = self.pointer_pos;
        let changed = self.levels[idx].rt.state.set_hover(x, y);
        if changed {
            self.levels[idx].dirty = true;
        }
    }

    fn handle_click(&mut self) -> bool {
        let Some(surf_id) = self.pointer_surface else {
            self.close_all(None);
            return false;
        };
        let Some(idx) = self
            .levels
            .iter()
            .position(|l| l.handles.surface_protocol_id() == surf_id)
        else {
            self.close_all(None);
            return false;
        };
        let (x, y) = self.pointer_pos;
        let Some(row_idx) = self.levels[idx].rt.state.hit_row(x, y) else {
            return true;
        };
        match self.levels[idx].rt.state.activate_row(row_idx) {
            Some(MenuAction::Pick(id)) => {
                self.close_all(Some(id));
                false
            }
            Some(MenuAction::OpenSubmenu {
                row_index,
                anchor_rect,
                menu,
            }) => {
                self.open_submenu(idx, row_index, anchor_rect, menu);
                true
            }
            Some(MenuAction::SubmenuDepthError) => {
                PopupWaylandError::SubmenuDepthExceeded.log_once();
                true
            }
            None => true,
        }
    }
}

fn open_popup_internal(
    parent: PopupParent<'_>,
    offset: (i32, i32),
    scale: i32,
    show: Show,
    session: Rc<Session>,
    wake: Option<Waker>,
    shared_gpu: Option<Arc<Gpu>>,
) -> Result<Tracking, PopupWaylandError> {
    let display =
        crate::clipboard::wayland::attached_display().ok_or(PopupWaylandError::NoDisplay)?;
    let serial =
        crate::clipboard::wayland::latest_input_serial().ok_or(PopupWaylandError::NoInputSerial)?;
    let dark = DARK_SCHEME.load(Ordering::Acquire);
    let state = MenuViewState::from_menu(&show.menu, dark, show.min_width, 0)?;
    let size = state.logical_size();
    let (anchor_rect, anchor, gravity) = match show.element {
        Some([x, y, w, h]) => {
            let rect = [
                x.round() as i32 + offset.0,
                y.round() as i32 + offset.1,
                w.round().max(1.0) as i32,
                h.round().max(1.0) as i32,
            ];
            if show.positioned.is_some() {
                (rect, Anchor::TopLeft, Gravity::BottomRight)
            } else {
                (rect, Anchor::BottomLeft, Gravity::BottomRight)
            }
        }
        None => {
            let rect = [
                show.at.0.round() as i32 + offset.0,
                show.at.1.round() as i32 + offset.1,
                1,
                1,
            ];
            (rect, Anchor::TopLeft, Gravity::BottomRight)
        }
    };

    let mut conn = PopupConn::new(display, wake.clone())?;
    let handles = conn.create_popup(
        parent,
        PopupPlacement {
            size,
            anchor_rect,
            anchor,
            gravity,
        },
        serial,
        scale,
    )?;
    let mut rt = build_menu_runtime(state, scale as f32);
    let mut presenter =
        match LevelPresenter::new(&conn, &handles, &mut rt, scale, shared_gpu.as_ref()) {
            Ok(p) => p,
            Err(err) => {
                handles.destroy();
                conn.flush();
                return Err(err);
            }
        };
    if let Err(err) = presenter.paint(&mut conn, &handles, &mut rt, scale) {
        presenter.clear_slots();
        drop(presenter);
        handles.destroy();
        conn.flush();
        return Err(err);
    }

    if !session.begin() {
        presenter.clear_slots();
        drop(presenter);
        handles.destroy();
        conn.flush();
        return Err(PopupWaylandError::DismissedBeforeConfigure);
    }
    let id = NEXT_TRACKING_ID.fetch_add(1, Ordering::Relaxed);
    session.tracking(Some(id));

    let active = ActivePopup {
        id,
        conn,
        levels: vec![PopupLevel {
            handles,
            presenter,
            rt,
            scale,
            dirty: false,
            open_submenu_row: None,
        }],
        session,
        wake,
        shared_gpu,
        dark,
        scale,
        grab_serial: serial,
        pointer_surface: None,
        pointer_pos: (0.0, 0.0),
    };
    ACTIVE.with(|slot| {
        if let Some(mut prev) = slot.borrow_mut().replace(active) {
            prev.close_all(None);
        }
    });
    Ok(id)
}

pub(super) fn show_with_gpu(
    window: &Arc<Window>,
    show: Show,
    session: Rc<Session>,
    wake: Option<Waker>,
    shared_gpu: Option<Arc<Gpu>>,
) -> bool {
    let Some(xdg_ptr) = window.xdg_surface() else {
        PopupWaylandError::MissingParentSurface.log_once();
        return false;
    };
    let offset = window.window_geometry_offset();
    let scale = window.scale_factor().round().max(1.0) as i32;
    match open_popup_internal(
        PopupParent::WindowXdg(xdg_ptr),
        offset,
        scale,
        show,
        session,
        wake,
        shared_gpu,
    ) {
        Ok(_) => true,
        Err(err) => {
            err.log_once();
            false
        }
    }
}

pub(crate) fn show_on_layer(
    layer_ptr: NonNull<c_void>,
    scale: i32,
    show: Show,
    session: Rc<Session>,
    wake: Option<Waker>,
    shared_gpu: Option<Arc<Gpu>>,
) -> bool {
    match open_popup_internal(
        PopupParent::Layer(layer_ptr),
        (0, 0),
        scale.max(1),
        show,
        session,
        wake,
        shared_gpu,
    ) {
        Ok(_) => true,
        Err(err) => {
            err.log_once();
            false
        }
    }
}

pub(super) fn stop(id: Tracking) {
    ACTIVE.with(|slot| {
        let matches = slot.borrow().as_ref().is_some_and(|a| a.id == id);
        if matches && let Some(mut active) = slot.borrow_mut().take() {
            active.close_from(0);
        }
    });
}

pub(super) fn step_active() {
    ACTIVE.with(|slot| {
        let mut taken = slot.borrow_mut().take();
        if let Some(mut active) = taken.take()
            && active.step()
        {
            *slot.borrow_mut() = Some(active);
        }
    });
}
