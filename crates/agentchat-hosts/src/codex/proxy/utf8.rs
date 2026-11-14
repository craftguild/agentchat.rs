//! Constant-space validation before trusting text-frame lifecycle metadata.
#[derive(Default)]
pub(super) struct Utf8 {
    remaining: u8,
    lower: u8,
    upper: u8,
}
impl Utf8 {
    pub(super) fn push(&mut self, byte: u8) -> bool {
        if self.remaining != 0 {
            if byte < self.lower || byte > self.upper {
                return false;
            }
            self.remaining -= 1;
            self.lower = 0x80;
            self.upper = 0xbf;
            return true;
        }
        let (remaining, lower, upper) = match byte {
            0..=0x7f => return true,
            0xc2..=0xdf => (1, 0x80, 0xbf),
            0xe0 => (2, 0xa0, 0xbf),
            0xe1..=0xec | 0xee..=0xef => (2, 0x80, 0xbf),
            0xed => (2, 0x80, 0x9f),
            0xf0 => (3, 0x90, 0xbf),
            0xf1..=0xf3 => (3, 0x80, 0xbf),
            0xf4 => (3, 0x80, 0x8f),
            _ => return false,
        };
        self.remaining = remaining;
        self.lower = lower;
        self.upper = upper;
        true
    }
    pub(super) fn complete(&self) -> bool {
        self.remaining == 0
    }
}
