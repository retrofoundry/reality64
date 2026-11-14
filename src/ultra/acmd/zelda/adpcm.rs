use super::super::mem::Memory;
use super::super::vector::{low, sat, signed};
use super::{Context, HalfwordOrder, BOOK, BUFFERS, LOOP};

impl<O: HalfwordOrder> Context<O> {
    fn zelda_adpcm_header(&mut self, source: u32, small: bool) -> (u32, i32) {
        let header = self.dmem.raw(source);
        let book = BOOK + u32::from(header & 15) * 32;
        let shift = if small { 14 } else { 12 } - i32::from(header >> 4);
        self.vectors[22][0] = if shift == 0 {
            65535
        } else {
            (32768u32 >> ((shift - 1) as u32 & 31)) as u16
        };
        self.vectors[4] = [0; 8];
        self.vectors[6] = [0; 8];
        for i in 0..8 {
            self.vectors[3][i] = self.vectors[25][i] & self.vectors[1][0];
            self.vectors[5][i] = self.vectors[25][i] & self.vectors[1][1];
            if !small {
                self.vectors[4][i] = self.vectors[24][i] & self.vectors[1][1];
                self.vectors[5][i] = self.vectors[25][i] & self.vectors[1][2];
                self.vectors[6][i] = self.vectors[24][i] & self.vectors[1][3];
            }
        }
        (book, shift)
    }

    fn zelda_table_load(&mut self, reg: usize, element: usize, address: u32) {
        for i in 0..8 {
            self.set_vbyte(reg, element + i, self.dmem.table_byte(address + i as u32));
        }
    }

    pub(super) fn zelda_adpcm(
        &mut self,
        ram: &mut (impl Memory + ?Sized),
        flags: u8,
        word: u64,
    ) -> Result<(), String> {
        self.load_quad(31, 0);
        for reg in [27, 25, 24, 13, 14, 15, 16, 17, 18, 19] {
            self.vectors[reg] = [0; 8];
        }
        let mut source = u32::from(self.dmem.h16(BUFFERS));
        let mut dest = u32::from(self.dmem.h16(BUFFERS + 2));
        let mut count = i32::from(self.dmem.h16(BUFFERS + 4));
        let state = ram.resolve_zelda(&self.dmem, word)?;
        self.store_quad(27, dest);
        self.store_quad(27, dest + 16);
        let small = flags & 4 != 0;
        let stride = if small { 5 } else { 9 };
        self.scalar[10] = stride;
        self.scalar[15] = BOOK;
        if small {
            self.zelda_table_load(25, 0, 0x50);
            self.zelda_table_load(23, 0, 0x58);
            self.zelda_table_load(25, 8, 0x4f);
            self.zelda_table_load(23, 8, 0x59);
        } else {
            self.load(25, 0, 0x40, 8);
            self.load(24, 8, 0x40, 8);
            self.load(23, 0, 0x48, 8);
            self.load(23, 8, 0x48, 8);
        }
        if flags & 1 == 0 {
            let history = if flags & 2 != 0 {
                self.dmem.word(LOOP)
            } else {
                state
            };
            self.dma(ram, dest, history, 31, false)?;
        }
        self.load_quad(27, dest + 16);
        dest += 32;
        self.load_raw(1, 0, source + 1, 8);
        if count != 0 {
            let (book, mut shift) = self.zelda_adpcm_header(source, small);
            self.adpcm_book(book);
            loop {
                for (output, a, b) in [(30, 3, 4), (29, 5, 6)] {
                    for i in 0..8 {
                        let total = i64::from(self.vectors[a][i]) * signed(self.vectors[23][i])
                            + i64::from(self.vectors[b][i]) * signed(self.vectors[23][i]);
                        let residual = low(total);
                        self.vectors[output][i] = if shift > 0 {
                            ((signed(residual) * i64::from(self.vectors[22][0])) >> 16) as u16
                        } else {
                            residual
                        };
                    }
                }
                source += stride;
                self.load_raw(1, 0, source + 1, 8);
                let (next_book, next_shift) = self.zelda_adpcm_header(source, small);
                let scale = signed(self.vectors[31][4]);
                let first = self.adpcm_group(self.vectors[30], self.vectors[27]);
                self.vectors[28] = first.map(|v| sat((i64::from(v) * scale) >> 16));
                let second = self.adpcm_group(self.vectors[29], self.vectors[28]);
                self.vectors[26] = second.map(|v| v as u16);
                self.vectors[2] = second.map(|v| (i64::from(v as u16) * scale) as u16);
                self.vectors[27] = second.map(|v| sat((i64::from(v) * scale) >> 16));
                self.adpcm_book(next_book);
                self.scalar[13] = next_book;
                self.scalar[12] = next_book.wrapping_sub(14);
                self.store(28, 0, dest, 16);
                self.store(27, 0, dest + 16, 16);
                dest += 32;
                count -= 32;
                shift = next_shift;
                if count <= 0 {
                    break;
                }
            }
        }
        self.dma(ram, dest - 32, state, 31, true)
    }

    pub(super) fn zelda_s8(
        &mut self,
        ram: &mut (impl Memory + ?Sized),
        flags: u8,
        word: u64,
    ) -> Result<(), String> {
        self.scalar[13] = u32::from(self.dmem.h16(BUFFERS));
        let mut dest = u32::from(self.dmem.h16(BUFFERS + 2));
        self.scalar[12] = u32::from(self.dmem.h16(BUFFERS + 4));
        let state = ram.resolve_zelda(&self.dmem, word)?;
        self.vectors[2] = [0; 8];
        self.vectors[3] = [0; 8];
        self.store_quad(2, dest);
        self.store_quad(3, dest + 16);
        if flags & 1 == 0 {
            let history = if flags & 2 != 0 {
                self.dmem.word(LOOP)
            } else {
                state
            };
            self.dma(ram, dest, history, 31, false)?;
        }
        dest += 32;
        if self.scalar[12] != 0 {
            self.zelda_packed_load(2, self.scalar[13]);
            self.zelda_packed_load(3, self.scalar[13] + 8);
            self.scalar[13] += 16;
            loop {
                self.scalar[12] = self.scalar[12].wrapping_sub(32);
                self.zelda_packed_load(4, self.scalar[13]);
                self.store_quad(2, dest);
                self.zelda_packed_load(5, self.scalar[13] + 8);
                self.store_quad(3, dest + 16);
                dest += 32;
                if self.scalar[12] as i32 <= 0 {
                    break;
                }
                self.scalar[12] = self.scalar[12].wrapping_sub(32);
                self.zelda_packed_load(2, self.scalar[13] + 16);
                self.store_quad(4, dest);
                self.zelda_packed_load(3, self.scalar[13] + 24);
                self.store_quad(5, dest + 16);
                self.scalar[13] += 32;
                dest += 32;
                if self.scalar[12] as i32 <= 0 {
                    break;
                }
            }
        }
        self.dma(ram, dest - 32, state, 31, true)
    }

    fn zelda_packed_load(&mut self, reg: usize, address: u32) {
        self.vectors[reg] = std::array::from_fn(|i| {
            u16::from(
                self.dmem
                    .raw((address & !7) + (((address & 7) + i as u32) & 15)),
            ) << 8
        });
    }
}
