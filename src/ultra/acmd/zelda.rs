use super::mem::Memory;
use super::vector::{low, mix, sat, signed};
use super::{Context, HalfwordOrder, CHUNK_REMAINING, TASK_DATA_SIZE, TASK_DESCRIPTOR};

mod adpcm;
mod envelope;
mod filter;
mod resample;

const BUFFERS: u32 = 0x2e0;
const LOOP: u32 = 0x2e8;
const BOOK: u32 = 0x330;
const SCRATCH: u32 = 0xfb0;

impl<O: HalfwordOrder> Context<O> {
    pub(super) fn zelda_start(&mut self) -> [u16; 26] {
        self.scalar[10] = TASK_DESCRIPTOR;
        std::array::from_fn(|i| self.dmem.h16(0x10 + i as u32 * 2))
    }

    pub(super) fn zelda_command(
        &mut self,
        ram: &mut (impl Memory + ?Sized),
        w0: u32,
        word: u64,
        dispatch: &[u16; 26],
    ) -> Result<(), String> {
        let w1 = word as u32;
        let n = w0 & 65535;
        let q = (w0 >> 16) & 255;
        let a = w1 >> 16;
        let b = w1 & 65535;
        let op = (w0 >> 24) & 127;
        if dispatch
            .get(op as usize)
            .is_some_and(|&entry| self.dmem.h16(0x10 + op * 2) != entry)
        {
            return Err(format!("overwritten Zelda audio dispatch entry {op:#x}"));
        }
        match op {
            0 => {
                let address = ram.resolve_zelda(&self.dmem, word)?;
                let length = (q * 16).wrapping_sub(1);
                self.dma(ram, SCRATCH, address, length, false)?;
                self.dmem
                    .set_word(SCRATCH + 4, self.dmem.word(SCRATCH + 4).wrapping_add(1));
                self.dmem.set_word(SCRATCH + 8, self.scalar[TASK_DATA_SIZE]);
                self.dmem
                    .set_word(SCRATCH + 12, self.scalar[CHUNK_REMAINING]);
                self.dmem.set_h16(SCRATCH, n as u16);
                self.dma(ram, SCRATCH, address, length, true)?;
            }
            1 => self.zelda_adpcm(ram, q as u8, word)?,
            2 => {
                if b != 0 {
                    self.vectors[0] = [0; 8];
                    for offset in (0..b).step_by(16) {
                        self.store_raw(0, 0, n + offset, 8);
                        self.store_raw(0, 0, n + offset + 8, 8);
                    }
                }
            }
            3 => {
                self.load_quad(31, 0);
                let mut source = a;
                let mut dest = b;
                self.load_quad(29, source);
                self.load_quad(28, source + 16);
                self.load_quad(27, dest);
                self.load_quad(26, dest + 16);
                for _ in (0..n.max(1)).step_by(32) {
                    for i in 0..8 {
                        let product = i64::from(self.vectors[28][i]) * signed(self.vectors[29][i]);
                        self.vectors[27][i] = product as u16;
                        self.vectors[26][i] = low(product * 2);
                    }
                    self.store_quad(27, dest + 16);
                    self.store_quad(26, dest);
                    source += 32;
                    dest += 32;
                    self.load_quad(29, source);
                    self.load_quad(28, source + 16);
                }
            }
            4 => self.zelda_add_mixer(q * 16, a, b),
            5 => self.zelda_resample(ram, q as u8, n as u16, word)?,
            6 => {
                self.scalar[15] = self.dmem.h16(BUFFERS + 2) as i16 as u32;
                self.scalar[13] = self.dmem.h16(BUFFERS + 4) as i16 as u32;
                self.scalar[12] = n << 2;
                self.scalar[10] = (u32::from(self.dmem.h16(BUFFERS)) << 16) | b;
                self.zelda_zoh_samples();
                self.scalar[13] = self.scalar[13].wrapping_sub(8);
                self.zelda_zoh_store();
            }
            7 => self.zelda_filter(ram, q as u8, n, word)?,
            8 => {
                self.dmem.set_h16(BUFFERS, n as u16);
                self.dmem.set_h16(BUFFERS + 2, a as u16);
                self.dmem.set_h16(BUFFERS + 4, b as u16);
            }
            9 => {
                self.scalar[13] = n;
                self.scalar[14] = a;
                self.scalar[15] = q;
                for reg in 1..=8 {
                    self.load_quad(reg, n + (reg as u32 - 1) * 16);
                }
                loop {
                    self.scalar[15] = self.scalar[15].wrapping_sub(1);
                    for reg in 1..=8 {
                        self.store_quad(reg, self.scalar[14] + (reg as u32 - 1) * 16);
                    }
                    self.scalar[14] += 128;
                    if self.scalar[15] as i32 <= 0 {
                        break;
                    }
                }
            }
            10 => {
                for offset in (0..b).step_by(16) {
                    self.load(1, 0, n + offset, 8);
                    self.load(2, 0, n + offset + 8, 8);
                    self.store(1, 0, a + offset, 8);
                    self.store(2, 0, a + offset + 8, 8);
                }
            }
            11 | 20 | 21 => {
                let address = ram.resolve_zelda(&self.dmem, word)?;
                let count = if op == 11 { n } else { q * 16 };
                self.dma(
                    ram,
                    if op == 11 { BOOK } else { n },
                    address,
                    count.wrapping_sub(1),
                    op == 21,
                )?;
            }
            12 => {
                self.load_quad(31, 0);
                self.vectors[30][0] = n as u16;
                let mut source = a;
                let mut dest = b;
                self.load_quad(27, dest);
                self.load_quad(29, source);
                self.load_quad(26, dest + 16);
                self.load_quad(28, source + 16);
                for _ in (0..(q * 16).max(1)).step_by(32) {
                    for i in 0..8 {
                        self.vectors[27][i] = mix(
                            self.vectors[27][i],
                            self.vectors[29][i],
                            n as u16,
                            self.vectors[31][6],
                        );
                        self.vectors[26][i] = mix(
                            self.vectors[26][i],
                            self.vectors[28][i],
                            n as u16,
                            self.vectors[31][6],
                        );
                    }
                    source += 32;
                    self.load_quad(29, source);
                    self.store_quad(27, dest);
                    self.load_quad(27, dest + 32);
                    self.store_quad(26, dest + 16);
                    self.load_quad(28, source + 16);
                    dest += 32;
                    self.load_quad(26, dest + 16);
                }
            }
            13 => {
                for offset in (0..(q * 16).max(8)).step_by(8) {
                    self.load(1, 0, a + offset, 8);
                    self.load(2, 0, b + offset, 8);
                    for i in 0..4 {
                        self.dmem
                            .set_h16(n + offset * 2 + i * 4, self.vectors[1][i as usize]);
                        self.dmem
                            .set_h16(n + offset * 2 + i * 4 + 2, self.vectors[2][i as usize]);
                    }
                }
            }
            14 => {
                self.scalar[12] = n;
                self.scalar[13] = a;
                self.scalar[15] = q >> 4;
                self.vectors[3][0] = (q >> 4) as u16;
                self.vectors[3][1] = ((q & 15) << 12) as u16;
                loop {
                    self.load_quad(1, self.scalar[13]);
                    self.load_quad(2, self.scalar[13] + 16);
                    for (input, output) in [(1, 4), (2, 5)] {
                        self.vectors[output] =
                            self.vectors[input].map(|v| sat((signed(v) * i64::from(q)) >> 4));
                    }
                    self.store_quad(4, self.scalar[13]);
                    self.store_quad(5, self.scalar[13] + 16);
                    self.scalar[12] = self.scalar[12].wrapping_sub(32);
                    self.scalar[13] += 32;
                    if self.scalar[12] as i32 <= 0 {
                        break;
                    }
                }
                self.vectors[0] = [0; 8];
            }
            15 => {
                let address = ram.resolve_zelda(&self.dmem, word)?;
                self.dmem.set_word(LOOP, address);
            }
            16 => {
                self.scalar[15] = q;
                self.scalar[13] = n;
                self.scalar[14] = a;
                loop {
                    self.scalar[15] = self.scalar[15].wrapping_sub(1);
                    self.scalar[12] = b;
                    loop {
                        self.load_quad(1, self.scalar[13]);
                        self.load_quad(2, self.scalar[13] + 16);
                        self.scalar[12] = self.scalar[12].wrapping_sub(32);
                        self.scalar[13] += 32;
                        self.store_quad(1, self.scalar[14]);
                        self.store_quad(2, self.scalar[14] + 16);
                        self.scalar[14] += 32;
                        if self.scalar[12] as i32 <= 0 {
                            break;
                        }
                    }
                    if self.scalar[15] as i32 <= 0 {
                        break;
                    }
                }
            }
            17 => {
                self.scalar[12] = n;
                self.scalar[13] = a;
                self.scalar[14] = b;
                loop {
                    for i in 0..4 {
                        self.load(i + 1, 0, self.scalar[13] + i as u32 * 8, 2);
                        self.load(i + 1, 2, self.scalar[13] + i as u32 * 8 + 4, 2);
                    }
                    self.scalar[13] += 32;
                    self.scalar[12] = self.scalar[12].wrapping_sub(8);
                    for i in 0..4 {
                        self.store(i + 1, 0, self.scalar[14] + i as u32 * 4, 4);
                    }
                    self.scalar[14] += 16;
                    if self.scalar[12] as i32 <= 0 {
                        break;
                    }
                }
            }
            18 => {
                self.vectors[1] = [0; 8];
                self.scalar[11] = n;
                self.scalar[12] = (q << 8) + n;
                self.vectors[1][4] = (q << 8) as u16;
                self.vectors[1][5] = self.scalar[12] as u16;
                self.scalar[21] = a;
                self.scalar[22] = b;
            }
            19 => self.zelda_envelope(w0, w1),
            22 => {
                self.vectors[1][0] = a as u16;
                self.vectors[1][1] = a.wrapping_add(self.scalar[21]) as u16;
                self.vectors[1][2] = b as u16;
                self.scalar[12] = b.wrapping_add(self.scalar[22]);
                self.vectors[1][3] = self.scalar[12] as u16;
            }
            23 => self.zelda_s8(ram, q as u8, word)?,
            25 => self.zelda_zoh_store(),
            _ => return Err(format!("unsupported Zelda audio command {op:#x}")),
        }
        Ok(())
    }

    fn zelda_zoh_samples(&mut self) {
        for i in 0..4 {
            let source = (self.scalar[10] >> 16) & 0xfffe;
            self.load(1, i * 2, source, 2);
            self.scalar[10] = self.scalar[10].wrapping_add(self.scalar[12]);
        }
    }

    fn zelda_zoh_store(&mut self) {
        loop {
            self.store(1, 0, self.scalar[15], 8);
            self.scalar[15] = self.scalar[15].wrapping_add(8);
            if self.scalar[13] as i32 <= 0 {
                break;
            }
            self.zelda_zoh_samples();
            self.scalar[13] = self.scalar[13].wrapping_sub(8);
        }
    }

    fn zelda_add_mixer(&mut self, count: u32, source: u32, dest: u32) {
        let mut carry = 0;
        for i in 0..8 {
            let sum = u32::from(self.vectors[31][i]) * 2;
            self.vectors[31][i] = sum as u16;
            carry |= ((sum >> 16) as u16) << i;
        }
        self.vco = carry;
        self.load_quad(27, dest);
        for offset in (0..count.max(1)).step_by(64) {
            for (reg, block) in [(29, 0), (28, 16), (23, 32), (22, 48)] {
                self.load_quad(reg, source + offset + block);
            }
            for (reg, block) in [(26, 16), (25, 32), (24, 48)] {
                self.load_quad(reg, dest + offset + block);
            }
            for (output, input) in [(27, 29), (26, 28), (25, 23), (24, 22)] {
                for i in 0..8 {
                    self.vectors[output][i] = sat(signed(self.vectors[output][i])
                        + signed(self.vectors[input][i])
                        + i64::from((self.vco >> i) & 1));
                }
                self.vco = 0;
            }
            for (reg, block) in [(27, 0), (26, 16), (25, 32), (24, 48)] {
                self.store_quad(reg, dest + offset + block);
            }
            self.load_quad(27, dest + offset + 64);
        }
    }
}
