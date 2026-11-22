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

#[cfg(test)]
mod tests {
    use super::KeyFrames;

    #[test]
    fn keys_received_in_one_dispatch_remain_separate_frames() {
        let mut keys = KeyFrames::default();
        assert_eq!(keys.advance_frame(), 0);
        assert_eq!(keys.press(), 1);
        assert_eq!(keys.press(), 2);
        assert_eq!(keys.press(), 3);
        assert_eq!(keys.advance_frame(), 1);
        assert_eq!(keys.advance_frame(), 2);
        assert_eq!(keys.advance_frame(), 3);
        assert_eq!(keys.advance_frame(), 3);
        assert_eq!(keys.press(), 4);
        assert_eq!(keys.advance_frame(), 4);
    }
}
