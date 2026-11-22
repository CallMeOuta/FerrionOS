use std::cell::RefCell;

#[derive(Clone)]
pub struct Rng {
    state: [u64; 4],
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Rng {
    pub fn new(seed: u64) -> Rng {
        let mut s = seed;
        let state = [
            splitmix(&mut s),
            splitmix(&mut s),
            splitmix(&mut s),
            splitmix(&mut s),
        ];
        assert!(state.iter().any(|&word| word != 0));
        Rng { state }
    }

    pub fn next_u64(&mut self) -> u64 {
        let result = self.state[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.state[1] << 17;
        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];
        self.state[2] ^= t;
        self.state[3] = self.state[3].rotate_left(45);
        result
    }

    pub fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0);
        ((u128::from(self.next_u64()) * u128::from(bound)) >> 64) as u64
    }

    pub fn range(&mut self, low: u64, high: u64) -> u64 {
        assert!(low <= high);
        low + self.below(high - low + 1)
    }

    pub fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        assert!(numerator <= denominator && denominator > 0);
        self.below(denominator) < numerator
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        assert!(!items.is_empty());
        &items[self.below(items.len() as u64) as usize]
    }

    pub fn fill(&mut self, bytes: &mut [u8]) {
        for chunk in bytes.chunks_mut(8) {
            let word = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }
}

thread_local! {
    static ENTROPY: RefCell<Rng> = RefCell::new(Rng::new(0));
}

pub fn seed_entropy(seed: u64) {
    ENTROPY.with(|entropy| *entropy.borrow_mut() = Rng::new(seed ^ 0x5eed_e17a_0f5a_17e5));
}

#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    let bytes = unsafe { std::slice::from_raw_parts_mut(dest, len) };
    ENTROPY.with(|entropy| entropy.borrow_mut().fill(bytes));
    Ok(())
}
