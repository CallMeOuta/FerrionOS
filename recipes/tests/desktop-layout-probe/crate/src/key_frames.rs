// SPDX-License-Identifier: MIT
#[derive(Default)]
pub struct KeyFrames {
    received: u64,
    drawn: u64,
}

impl KeyFrames {
    pub fn press(&mut self) -> u64 {
        self.received += 1;
        self.received
    }

    pub fn advance_frame(&mut self) -> u64 {
        self.drawn = self.drawn.saturating_add(1).min(self.received);
        self.drawn
    }
}
