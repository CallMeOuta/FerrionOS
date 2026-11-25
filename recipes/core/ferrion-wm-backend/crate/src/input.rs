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

#[cfg_attr(not(target_os = "redox"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutputRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[cfg_attr(not(target_os = "redox"), allow(dead_code))]
impl OutputRect {
    pub(crate) fn contains(self, x: i32, y: i32) -> bool {
        if self.width == 0 || self.height == 0 {
            return false;
        }
        let max_x = self.x.saturating_add(self.width as i32);
        let max_y = self.y.saturating_add(self.height as i32);
        x >= self.x && x < max_x && y >= self.y && y < max_y
    }

    fn clamp_point(self, x: i32, y: i32) -> (i32, i32) {
        let max_x = self.x.saturating_add(self.width.saturating_sub(1) as i32);
        let max_y = self.y.saturating_add(self.height.saturating_sub(1) as i32);
        (x.clamp(self.x, max_x), y.clamp(self.y, max_y))
    }
}

#[cfg_attr(not(target_os = "redox"), allow(dead_code))]
fn clamp_to_union(x: i32, y: i32, bounds: (u32, u32), rects: &[OutputRect]) -> (i32, i32) {
    for &rect in rects {
        if rect.contains(x, y) {
            return (x, y);
        }
    }
    let mut best: Option<((i32, i32), i128)> = None;
    for &rect in rects {
        if rect.width == 0 || rect.height == 0 {
            continue;
        }
        let (cx, cy) = rect.clamp_point(x, y);
        let dx = i128::from(x) - i128::from(cx);
        let dy = i128::from(y) - i128::from(cy);
        let dist2 = dx * dx + dy * dy;
        if best.is_none_or(|(_, best_dist2)| dist2 < best_dist2) {
            best = Some(((cx, cy), dist2));
        }
    }
    match best {
        Some((point, _)) => point,
        None => (
            x.clamp(0, bounds.0.saturating_sub(1) as i32),
            y.clamp(0, bounds.1.saturating_sub(1) as i32),
        ),
    }
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
        Self {
            keys: [false; 256],
            count: 0,
            buttons: 0,
            x: 0,
            y: 0,
            last_time: 0,
        }
    }

    pub(crate) fn convert(
        &mut self,
        code: i64,
        a: i64,
        b: i64,
        time_us: u64,
        dimensions: (u32, u32),
        rects: &[OutputRect],
    ) -> [Option<Input>; 3] {
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
                        if pressed {
                            self.count += 1;
                        } else {
                            self.count -= 1;
                        }
                        result[0] = Some(Input::Key(KeyEvent {
                            time_us,
                            code,
                            pressed,
                            count: self.count,
                        }));
                    }
                }
            }
            2 | 11 if width > 0 && height > 0 => {
                if code == 2 {
                    self.x = ((a.clamp(0, 65535) * i64::from(width)) / 65536) as i32;
                    self.y = ((b.clamp(0, 65535) * i64::from(height)) / 65536) as i32;
                } else {
                    self.x = self.x.saturating_add(a as i32);
                    self.y = self.y.saturating_add(b as i32);
                }
                let (clamped_x, clamped_y) = clamp_to_union(self.x, self.y, dimensions, rects);
                self.x = clamped_x;
                self.y = clamped_y;
                result[0] = Some(Input::Motion(MotionEvent {
                    time_us,
                    x: self.x,
                    y: self.y,
                    width,
                    height,
                }));
            }
            3 => {
                let buttons = a as u8 & 7;
                let changed = self.buttons ^ buttons;
                for (index, button_code) in [0x110, 0x112, 0x111].into_iter().enumerate() {
                    let mask = 1 << index;
                    if changed & mask != 0 {
                        result[index] = Some(Input::Button(ButtonEvent {
                            time_us,
                            code: button_code,
                            pressed: buttons & mask != 0,
                        }));
                    }
                }
                self.buttons = buttons;
            }
            4 => {
                result[0] = Some(Input::Axis(AxisEvent {
                    time_us,
                    horizontal: a as i32,
                    vertical: (b as i32).saturating_neg(),
                }))
            }
            _ => {}
        }
        result
    }
}

fn evdev_code(code: u8) -> Option<u32> {
    Some(match code {
        0x37 => 99,
        0x47 => 102,
        0x48 => 103,
        0x49 => 104,
        0x4B => 105,
        0x4D => 106,
        0x4F => 107,
        0x50 => 108,
        0x51 => 109,
        0x52 => 110,
        0x53 => 111,
        0x5B => 125,
        0x5C => 126,
        0x5D => 127,
        0x5E => 116,
        0x5F => 142,
        0x64 => 97,
        0x70 => 82,
        0x71 => 79,
        0x72 => 80,
        0x73 => 81,
        0x74 => 75,
        0x75 => 76,
        0x76 => 77,
        0x77 => 71,
        0x78 => 72,
        0x79 => 73,
        0x7A => 83,
        0x7B => 74,
        0x7C => 78,
        0x7D => 55,
        0x7E => 98,
        0x7F => 96,
        0xA0 => 113,
        0xAE => 114,
        0xB0 => 115,
        0xA2 => 164,
        0xA4 => 166,
        0x99 => 163,
        0x90 => 165,
        0x01..=0x46 | 0x57 | 0x58 => u32::from(code),
        _ => return None,
    })
}
