use super::super::mem::Memory;
use super::super::vector::{fractional, low, sat, signed};
use super::{Context, HalfwordOrder, BUFFERS, SCRATCH};

impl<O: HalfwordOrder> Context<O> {
    pub(super) fn zelda_resample(
        &mut self,
        ram: &mut (impl Memory + ?Sized),
        flags: u8,
        pitch: u16,
        word: u64,
    ) -> Result<(), String> {
        let mut source = self.dmem.h16(BUFFERS) as i16 as u32;
        let mut dest = self.dmem.h16(BUFFERS + 2) as i16 as u32;
        let mut count = self.dmem.h16(BUFFERS + 4) as i16 as i32;
        let state = ram.resolve_zelda(&self.dmem, word)?;
        if flags & 1 != 0 {
            self.dmem.set_h16(SCRATCH + 8, 0);
            self.vectors[16] = [0; 8];
            self.store(16, 0, SCRATCH, 8);
        } else {
            self.dma(ram, SCRATCH, state, 31, false)?;
        }
        self.load(16, 0, SCRATCH, 8);
        if flags & 2 != 0 {
            source = source.wrapping_sub(4);
            self.store(16, 0, source, 2);
            self.store(16, 4, source.wrapping_add(2), 2);
        } else if flags & 4 != 0 {
            source = source.wrapping_sub(16);
            for i in 0..8 {
                self.store(16, (i / 2) * 2, source.wrapping_add(i as u32 * 2), 2);
            }
        } else {
            source = source.wrapping_sub(8);
            self.store(16, 0, source, 8);
        }
        self.load(23, 14, SCRATCH + 8, 2);
        self.load(16, 0, source, 8);
        self.vectors[18][2] = source as u16;
        self.vectors[18][3] = 0xe0;
        self.vectors[18][4] = pitch;
        self.vectors[18][5] = 64;
        self.load_quad(31, 0x70);
        self.load_quad(25, 0x60);
        for i in 0..8 {
            let value = sat(signed(self.vectors[25][i])
                - signed(self.vectors[31][i])
                - i64::from((self.vco >> i) & 1));
            self.vectors[25][i] = sat(signed(value) - signed(self.vectors[31][i]));
        }
        self.vco = 0;
        for (i, reg) in [30, 29, 28, 27, 26, 24].into_iter().enumerate() {
            self.load_quad(reg, 0x80 + i as u32 * 16);
        }
        self.vectors[22] = [0; 8];
        self.resample_addresses();
        self.load_quad(25, 0x60);
        self.resample_scratch(SCRATCH);
        self.store(23, 7, SCRATCH + 8, 2);
        loop {
            for i in 0..4 {
                let samples = 16 - 2 * i;
                self.load(samples, 0, self.scalar[17 - i], 8);
                self.load(samples, 8, self.scalar[13 - i], 8);
                self.load(samples - 1, 0, self.scalar[9 - i], 8);
                self.load(samples - 1, 8, self.scalar[5 - i], 8);
            }
            self.resample_addresses();
            for i in 0..4 {
                let products: [u16; 8] = std::array::from_fn(|j| {
                    fractional(self.vectors[16 - i * 2][j], self.vectors[15 - i * 2][j])
                });
                let pairs: [u16; 8] =
                    std::array::from_fn(|j| sat(signed(products[j]) + signed(products[j | 1])));
                self.vectors[8 - i] =
                    std::array::from_fn(|j| sat(signed(pairs[j]) + signed(pairs[(j & 4) + 2])));
            }
            self.resample_scratch(SCRATCH);
            self.vectors[4] = std::array::from_fn(|i| {
                low((0..4)
                    .map(|j| {
                        i64::from(self.vectors[29 - j][i]) * signed(self.vectors[8 - j][i & 4])
                    })
                    .sum())
            });
            self.store_quad(4, dest);
            count -= 16;
            if count <= 0 {
                break;
            }
            dest = dest.wrapping_add(16);
        }
        self.store(23, 0, SCRATCH + 8, 2);
        self.load(16, 0, self.scalar[17], 8);
        self.store(16, 0, SCRATCH, 8);
        self.dma(ram, SCRATCH, state, 31, true)
    }
}
