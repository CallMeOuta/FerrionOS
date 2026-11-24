#![forbid(unsafe_code)]

use std::time::Duration;

use rune::tok::{self, type_};
use rune::{App, AppSpec, Color, Cx, El, GpuBackend, Tx, col, el, text};

#[derive(Default)]
struct State {
    refused: bool,
    ticks: u32,
}

const MAX_TICKS: u32 = 8;

fn view(s: &State, _: &mut Cx<State>) -> El {
    let t = tok::theme(true);
    let badge = el().key("badge").size(160.0, 64.0).radius(8.0).fill(Color {
        r: 0.18,
        g: 0.55,
        b: 0.32,
        a: 1.0,
    });
    let badge = if s.refused { badge.blur(8.0) } else { badge };
    let label = text(if s.refused {
        "Refused frame"
    } else {
        "Initial frame"
    })
    .font(type_::TITLE)
    .color(t.text);
    col()
        .w_full()
        .h_full()
        .center()
        .gap(16.0)
        .fill(t.bg)
        .child(label)
        .child(badge)
}

fn on_tick(s: &mut State, _dt: f64, tx: &mut Tx) -> bool {
    if !s.refused {
        return false;
    }
    s.ticks = s.ticks.saturating_add(1);
    if s.ticks < MAX_TICKS {
        tx.wake_after(Duration::from_millis(50));
    }
    true
}

fn main() {
    let app = App::new(AppSpec::<State> {
        scenes: &[("refused", |s| s.refused = true)],
        poll: Some(on_tick),
        ..AppSpec::new("CPU Refusal", (320.0, 200.0), State::default, view)
    })
    .gpu_backend(GpuBackend::Cpu);
    let handle = app.handle();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        handle.update(|s, tx| {
            s.refused = true;
            tx.wake_after(Duration::from_millis(50));
        });
    });
    app.run();
}
