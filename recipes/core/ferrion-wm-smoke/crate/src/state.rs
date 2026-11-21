// Adapted from Smithay smallvil at 13488e2fc469841763495f89db832429534d82f5.
// Copyright (c) 2017 Victor Berger and Victoria Brekenfeld. MIT; see LICENSE.
use std::{sync::Arc, time::Instant};
use smithay::{
    backend::{input::{AbsolutePositionEvent, Event, InputBackend, InputEvent, KeyboardKeyEvent}, renderer::utils::on_commit_buffer_handler},
    desktop::{Space, Window, WindowSurfaceType},
    input::{keyboard::FilterResult, pointer::MotionEvent, Seat, SeatHandler, SeatState},
    reexports::wayland_server::{backend::{ClientData, ClientId, DisconnectReason}, protocol::{wl_buffer, wl_surface::WlSurface}, Client, DisplayHandle},
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::{buffer::BufferHandler, compositor::{CompositorClientState, CompositorHandler, CompositorState, with_states}, output::{OutputHandler, OutputManagerState}, shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState, XdgToplevelSurfaceData}, shm::{ShmHandler, ShmState}},
    delegate_compositor, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell,
};

#[derive(Default)]
pub struct ClientState { pub compositor: CompositorClientState }
impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

pub struct State {
    pub start: Instant,
    pub dh: DisplayHandle,
    pub space: Space<Window>,
    pub commits: Vec<(&'static str, u64, Instant)>,
    commit_counts: [u64; 2],
    compositor: CompositorState,
    shell: XdgShellState,
    shm: ShmState,
    _outputs: OutputManagerState,
    seats: SeatState<Self>,
    seat: Seat<Self>,
}
impl State {
    pub fn new(dh: DisplayHandle) -> Self {
        let compositor = CompositorState::new::<Self>(&dh);
        let shell = XdgShellState::new::<Self>(&dh);
        let shm = ShmState::new::<Self>(&dh, vec![]);
        let outputs = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let mut seats = SeatState::new();
        let mut seat = seats.new_wl_seat(&dh, "ferrion-inputd");
        seat.add_keyboard(Default::default(), 200, 25).expect("keyboard keymap");
        seat.add_pointer();
        Self { start: Instant::now(), dh, space: Space::default(), commits: Vec::new(), commit_counts: [0; 2], compositor, shell, shm, _outputs: outputs, seats, seat }
    }
    pub fn accept(&mut self, stream: std::os::unix::net::UnixStream) -> std::io::Result<()> {
        self.dh.insert_client(stream, Arc::new(ClientState::default()))?;
        Ok(())
    }
    pub fn input<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event } => {
                self.seat.get_keyboard().unwrap().input::<(), _>(self, event.key_code(), event.state(), SERIAL_COUNTER.next_serial(), event.time_msec(), |_, _, _| FilterResult::Forward);
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let output = self.space.outputs().next().unwrap();
                let geometry = self.space.output_geometry(output).unwrap();
                let pos = event.position_transformed(geometry.size);
                let focus = self.surface_under(pos);
                let pointer = self.seat.get_pointer().unwrap();
                pointer.motion(self, focus, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time: event.time_msec() });
                pointer.frame(self);
            }
            _ => {}
        }
    }
    fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space.element_under(pos).and_then(|(window, location)| window.surface_under(pos - location.to_f64(), WindowSurfaceType::ALL).map(|(surface, p)| (surface, (p + location).to_f64())))
    }
}
impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState { &mut self.compositor }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState { &client.get_data::<ClientState>().unwrap().compositor }
    fn commit(&mut self, surface: &WlSurface) {
        let received = Instant::now();
        on_commit_buffer_handler::<Self>(surface);
        if let Some(window) = self.space.elements().find(|w| w.toplevel().unwrap().wl_surface() == surface).cloned() {
            window.on_commit();
            let top = window.toplevel().unwrap();
            if !top.is_initial_configure_sent() { top.send_configure(); }
            let mapped = with_states(surface, |states| states.data_map.get::<smithay::backend::renderer::utils::RendererSurfaceStateUserData>().is_some_and(|data| data.lock().unwrap().buffer().is_some()));
            if mapped {
                let slow = with_states(surface, |states| states.data_map.get::<XdgToplevelSurfaceData>().unwrap().lock().unwrap().app_id.as_deref() == Some("ferrion-wayland-probe-slow"));
                let index = usize::from(slow);
                self.commit_counts[index] += 1;
                self.commits.push((if slow { "slow" } else { "normal" }, self.commit_counts[index], received));
            }
        }
    }
}
impl BufferHandler for State { fn buffer_destroyed(&mut self, _: &wl_buffer::WlBuffer) {} }
impl ShmHandler for State { fn shm_state(&self) -> &ShmState { &self.shm } }
impl OutputHandler for State {}
impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<Self> { &mut self.seats }
    fn cursor_image(&mut self, _: &Seat<Self>, _: smithay::input::pointer::CursorImageStatus) {}
}
impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState { &mut self.shell }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface);
        self.space.map_element(window, (80, 100), false);
    }
    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        let slow = with_states(surface.wl_surface(), |states| states.data_map.get::<XdgToplevelSurfaceData>().unwrap().lock().unwrap().app_id.as_deref() == Some("ferrion-wayland-probe-slow"));
        let window = self.space.elements().find(|w| w.toplevel().unwrap() == &surface).cloned();
        if let Some(window) = window {
            self.space.map_element(window, if slow { (520, 100) } else { (80, 100) }, false);
        }
        if !slow {
            self.seat.get_keyboard().unwrap().set_focus(self, Some(surface.wl_surface().clone()), SERIAL_COUNTER.next_serial());
        }
    }
    fn new_popup(&mut self, surface: PopupSurface, _: PositionerState) { surface.send_popup_done(); }
    fn reposition_request(&mut self, surface: PopupSurface, _: PositionerState, token: u32) { surface.send_repositioned(token); }
    fn grab(&mut self, surface: PopupSurface, _: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat, _: smithay::utils::Serial) { surface.send_popup_done(); }
}
delegate_compositor!(State);
delegate_shm!(State);
delegate_output!(State);
delegate_seat!(State);
delegate_xdg_shell!(State);
