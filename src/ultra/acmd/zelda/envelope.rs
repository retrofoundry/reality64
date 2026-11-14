use super::super::vector::{sat, signed};
use super::{Context, HalfwordOrder};

impl<O: HalfwordOrder> Context<O> {
    pub(super) fn zelda_envelope(&mut self, w0: u32, w1: u32) {
        self.vectors[4] = [0; 8];
        self.vectors[0] = [0; 8];
        self.load_quad(3, 0);
        for (register, lane) in [(21, 0), (22, 2), (11, 4)] {
            self.scalar[register] = self.scalar[register].wrapping_mul(2);
            self.vectors[4][lane] = self.scalar[register] as u16;
            self.vectors[4][lane + 1] = self.scalar[register] as u16;
        }
        let mut source = (w0 >> 12) & 0xff0;
        let mut left = (w1 >> 20) & 0xff0;
        let mut right = (w1 >> 12) & 0xff0;
        let mut wet_left = (w1 >> 4) & 0xff0;
        let mut wet_right = (w1 << 4) & 0xff0;
        self.vectors[2][0] = ((w0 >> 1) & 1).wrapping_neg() as u16;
        self.vectors[2][1] = (w0 & 1).wrapping_neg() as u16;
        self.vectors[2][2] = ((w0 & 8) >> 1).wrapping_neg() as u16;
        self.vectors[2][3] = ((w0 & 4) >> 1).wrapping_neg() as u16;
        self.scalar[12] = w0 >> 8;
        self.scalar[10] = w0 & 16;
        self.vco = 0;
        self.load_quad(8, source);
        for _ in (0..((w0 >> 8) & 255).max(1)).step_by(16) {
            self.load_quad(15, source + 16);
            source += 32;
            for (output, input, gain) in [(9, 8, 0), (10, 8, 2), (16, 15, 1), (17, 15, 3)] {
                self.vectors[output] = self.vectors[input]
                    .map(|v| ((signed(v) * i64::from(self.vectors[1][gain])) >> 16) as u16);
            }
            for (reg, address) in [
                (11, left),
                (12, right),
                (18, left + 16),
                (19, right + 16),
                (13, wet_left),
                (14, wet_right),
                (20, wet_left + 16),
                (21, wet_right + 16),
            ] {
                self.load_quad(reg, address);
            }
            for (contribution, dry, xor, gain) in [
                (9, 11, 0, 4),
                (10, 12, 1, 4),
                (16, 18, 0, 5),
                (17, 19, 1, 5),
            ] {
                for i in 0..8 {
                    let value = self.vectors[contribution][i] ^ self.vectors[2][xor];
                    self.vectors[dry][i] = sat(signed(self.vectors[dry][i]) + signed(value));
                    self.vectors[contribution][i] =
                        ((signed(value) * i64::from(self.vectors[1][gain])) >> 16) as u16
                            ^ self.vectors[2][xor + 2];
                }
            }
            self.store_quad(11, left);
            self.store_quad(12, right);
            let swap = usize::from(w0 & 16 != 0);
            for (output, input) in [
                (13, 9 + swap),
                (14, 10 - swap),
                (20, 16 + swap),
                (21, 17 - swap),
            ] {
                self.vectors[output] = std::array::from_fn(|i| {
                    sat(signed(self.vectors[output][i]) + signed(self.vectors[input][i]))
                });
            }
            self.store_quad(18, left + 16);
            self.store_quad(19, right + 16);
            left += 32;
            self.store_quad(13, wet_left);
            self.store_quad(14, wet_right);
            right += 32;
            self.load_quad(8, source);
            self.store_quad(20, wet_left + 16);
            wet_left += 32;
            for i in 0..8 {
                self.vectors[1][i] = self.vectors[1][i].wrapping_add(self.vectors[4][i]);
            }
            self.store_quad(21, wet_right + 16);
            wet_right += 32;
        }
        self.scalar[15] = right;
        self.vco = 0;
        self.vectors[0] = [0; 8];
    }
}
