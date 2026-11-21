// SPDX-License-Identifier: MIT
// Wire codes and scan-code mapping follow orbclient/src/event.rs at
// b0ee62d1eaadb68c89a6f1e227b5c3987f09d780. See LICENSE for its notice.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub time_us: u64,
    /// Linux input code, without the XKB offset of eight.
    pub code: u32,
    pub pressed: bool,
    pub count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotionEvent {
    pub time_us: u64,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonEvent {
    pub time_us: u64,
    pub code: u32,
    pub pressed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisEvent {
    pub time_us: u64,
    /// Wheel detents, positive right/down in Wayland coordinates.
    pub horizontal: i32,
    pub vertical: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Key(KeyEvent),
    Motion(MotionEvent),
    Button(ButtonEvent),
    Axis(AxisEvent),
}

/// Allocated once per input reader. One wire event produces at most three
/// button transitions; no allocation or key-count scan occurs on motion.
#[cfg_attr(not(target_os = "redox"), allow(dead_code))]
pub(crate) struct InputState {
    keys: [bool; 256],
    count: u32,
    buttons: u8,
    x: i32,
    y: i32,
    last_time: u64,
}

#[cfg_attr(not(target_os = "redox"), allow(dead_code))]
impl InputState {
    pub(crate) fn new() -> Self {
        Self { keys: [false; 256], count: 0, buttons: 0, x: 0, y: 0, last_time: 0 }
    }

    pub(crate) fn convert(&mut self, code: i64, a: i64, b: i64, time_us: u64, dimensions: (u32, u32)) -> [Option<Input>; 3] {
        // Events in the same read may legitimately share a timestamp. Never
        // manufacture extra elapsed time to force strict rather than monotonic order.
        let time_us = time_us.max(self.last_time);
        self.last_time = time_us;
        let (width, height) = dimensions;
        let mut result = [None; 3];
        match code {
            1 => {
                let scancode = b as u8;
                if let Some(code) = evdev_code(scancode) {
                    let pressed = b & 0x100 != 0;
                    let old = &mut self.keys[usize::from(scancode)];
                    if *old != pressed {
                        *old = pressed;
                        if pressed { self.count += 1; } else { self.count -= 1; }
                        result[0] = Some(Input::Key(KeyEvent { time_us, code, pressed, count: self.count }));
                    }
                }
            }
            2 | 11 if width > 0 && height > 0 => {
                if code == 2 {
                    // Inputd's absolute wire range is 0..=65535, not pixels.
                    self.x = ((a.clamp(0, 65535) * i64::from(width)) / 65536) as i32;
                    self.y = ((b.clamp(0, 65535) * i64::from(height)) / 65536) as i32;
                } else {
                    self.x = self.x.saturating_add(a as i32);
                    self.y = self.y.saturating_add(b as i32);
                }
                self.x = self.x.clamp(0, width.saturating_sub(1) as i32);
                self.y = self.y.clamp(0, height.saturating_sub(1) as i32);
                result[0] = Some(Input::Motion(MotionEvent { time_us, x: self.x, y: self.y, width, height }));
            }
            3 => {
                let buttons = a as u8 & 7;
                let changed = self.buttons ^ buttons;
                for (index, button_code) in [0x110, 0x112, 0x111].into_iter().enumerate() {
                    let mask = 1 << index;
                    if changed & mask != 0 {
                        result[index] = Some(Input::Button(ButtonEvent { time_us, code: button_code, pressed: buttons & mask != 0 }));
                    }
                }
                self.buttons = buttons;
            }
            4 => result[0] = Some(Input::Axis(AxisEvent {
                time_us,
                horizontal: a as i32,
                vertical: (b as i32).saturating_neg(),
            })),
            _ => {}
        }
        result
    }
}

// Orbclient separates navigation keys from its private keypad range. Passing
// those scan codes straight to XKB makes arrows appear as keypad digits.
fn evdev_code(code: u8) -> Option<u32> {
    Some(match code {
        0x37 => 99,
        0x47 => 102, 0x48 => 103, 0x49 => 104,
        0x4B => 105, 0x4D => 106,
        0x4F => 107, 0x50 => 108, 0x51 => 109,
        0x52 => 110, 0x53 => 111,
        0x5B => 125, 0x5C => 126, 0x5D => 127,
        0x5E => 116, 0x5F => 142,
        // The pinned wire protocol aliases right-control and AltGr at 0x64;
        // it cannot distinguish them. Preserve its named right-control code.
        0x64 => 97,
        0x70 => 82, 0x71 => 79, 0x72 => 80, 0x73 => 81,
        0x74 => 75, 0x75 => 76, 0x76 => 77,
        0x77 => 71, 0x78 => 72, 0x79 => 73,
        0x7A => 83, 0x7B => 74, 0x7C => 78,
        0x7D => 55, 0x7E => 98, 0x7F => 96,
        0xA0 => 113, 0xAE => 114, 0xB0 => 115,
        0xA2 => 164, 0xA4 => 166, 0x99 => 163, 0x90 => 165,
        0x01..=0x46 | 0x57 | 0x58 => u32::from(code),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Contract: inputd absolute range reaches each display edge and relative
    // motion cannot escape it. A raw-pixel interpretation fails these fixtures.
    #[test]
    fn absolute_and_relative_share_clamped_pixel_coordinates() {
        let mut state = InputState::new();
        assert_eq!(state.convert(2, 32768, 65535, 50, (800, 600))[0], Some(Input::Motion(MotionEvent {
            time_us: 50, x: 400, y: 599, width: 800, height: 600,
        })));
        assert_eq!(state.convert(11, -900, 900, 49, (800, 600))[0], Some(Input::Motion(MotionEvent {
            time_us: 50, x: 0, y: 599, width: 800, height: 600,
        })));
    }

    // Contract: state snapshots become only changed button transitions, in
    // standard Linux button numbering. Replaying a snapshot must emit nothing.
    #[test]
    fn button_snapshots_emit_edges_not_repeated_presses() {
        let mut state = InputState::new();
        assert_eq!(state.convert(3, 5, 0, 10, (800, 600)), [
            Some(Input::Button(ButtonEvent { time_us: 10, code: 0x110, pressed: true })), None,
            Some(Input::Button(ButtonEvent { time_us: 10, code: 0x111, pressed: true })),
        ]);
        assert_eq!(state.convert(3, 5, 0, 11, (800, 600)), [None; 3]);
        assert_eq!(state.convert(3, 4, 0, 12, (800, 600))[0], Some(Input::Button(ButtonEvent {
            time_us: 12, code: 0x110, pressed: false,
        })));
    }

    // Contract: arrows are not keypad keys; repeated physical presses do not
    // inflate Smithay's held-key count. This is the sole wire-conversion owner.
    #[test]
    fn scan_codes_and_held_counts_match_keyboard_contract() {
        let mut state = InputState::new();
        for (index, (scan, expected)) in [(0x1E, 30), (0x48, 103), (0x78, 72), (0x5B, 125)].into_iter().enumerate() {
            let time_us = 20 + index as u64 * 3;
            assert_eq!(state.convert(1, 0, scan | 0x100, time_us, (800, 600))[0], Some(Input::Key(KeyEvent {
                time_us, code: expected, pressed: true, count: 1,
            })));
            assert_eq!(state.convert(1, 0, scan | 0x100, time_us + 1, (800, 600)), [None; 3]);
            assert_eq!(state.convert(1, 0, scan, time_us + 2, (800, 600))[0], Some(Input::Key(KeyEvent {
                time_us: time_us + 2, code: expected, pressed: false, count: 0,
            })));
        }
    }
}
