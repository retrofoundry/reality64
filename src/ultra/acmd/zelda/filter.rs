use super::super::mem::Memory;
use super::super::vector::{sat, signed};
use super::{Context, HalfwordOrder, SCRATCH};

impl<O: HalfwordOrder> Context<O> {
    pub(super) fn zelda_filter(
        &mut self,
        ram: &mut (impl Memory + ?Sized),
        flags: u8,
        buffer: u32,
        word: u64,
    ) -> Result<(), String> {
        self.scalar[13] = SCRATCH;
        self.vectors[0] = [0; 8];
        self.store_quad(0, SCRATCH);
        self.store_quad(0, SCRATCH + 16);
        let state = ram.resolve_zelda(&self.dmem, word)?;
        if flags >= 2 {
            self.scalar[15] = buffer;
            self.scalar[12] = u32::from(flags) - 1;
            self.store_quad(0, SCRATCH + 32);
            self.store_quad(0, SCRATCH + 64);
            return self.dma(ram, SCRATCH + 48, state, 15, false);
        }
        if flags == 0 {
            self.dma(ram, SCRATCH, state, 31, false)?;
        }
        self.load_quad(24, SCRATCH + 48);
        self.load_quad(25, SCRATCH + 16);
        self.vectors[14] = std::array::from_fn(|i| {
            sat((signed(self.vectors[24][i]) + signed(self.vectors[25][i]) + 1) >> 1)
        });
        self.vectors[15][0] = 16384;
        self.store_quad(14, SCRATCH + 48);
        self.store_quad(14, SCRATCH + 16);
        self.load_quad(24, SCRATCH + 48);
        self.load(28, 0, SCRATCH + 40, 8);
        self.load(28, 8, SCRATCH + 48, 8);
        self.load(20, 0, SCRATCH + 56, 8);
        self.load(20, 8, SCRATCH + 64, 8);
        for (shift, registers) in [
            (2, [31, 27, 17, 21]),
            (4, [30, 26, 18, 22]),
            (6, [29, 25, 19, 23]),
        ] {
            for (i, reg) in registers.into_iter().enumerate() {
                let source = SCRATCH + 32 + shift + i as u32 * 8;
                self.load(reg, 0, source, 8);
                self.load(reg, 8, source + 8, 8);
            }
        }
        self.load_quad(15, SCRATCH);
        self.load_quad(16, buffer);
        let mut address = buffer;
        loop {
            self.load_quad(16, address);
            let registers = [23, 22, 21, 20, 19, 18, 17, 24, 25, 26, 27, 28, 29, 30, 31];
            self.vectors[14] = std::array::from_fn(|lane| {
                let sum: i64 = registers
                    .iter()
                    .enumerate()
                    .map(|(i, &reg)| {
                        let sample = if i < 7 {
                            self.vectors[15][i + 1]
                        } else {
                            self.vectors[16][i - 7]
                        };
                        signed(self.vectors[reg][lane]) * signed(sample)
                    })
                    .sum();
                sat((sum + 16384) >> 15)
            });
            self.scalar[15] = self.scalar[15].wrapping_sub(16);
            self.store_quad(14, address);
            address += 16;
            self.vectors[15] = self.vectors[16];
            self.vco = 0;
            if self.scalar[15] as i32 <= 0 {
                break;
            }
        }
        self.scalar[11] = address;
        self.scalar[12] = 16384;
        self.store_quad(16, SCRATCH);
        self.dma(ram, SCRATCH, state, 31, true)
    }
}
