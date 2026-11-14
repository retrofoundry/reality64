use super::dmem::{Dmem, HalfwordOrder, Ne};
use super::mem::{Dma, Memory};
use super::{Context, Microcode, CHUNK_REMAINING, SEGMENT_TABLE, TASK_DATA_SIZE};
use crate::ultra::rcp::OSTaskT;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::c_void;

const DATA_SIZE: usize = 704;
const DATA_SHA256: &str = "79bb2a3e8555019bc4b229f0c218f1e329ca5711bfa55c7c7f992facf2e12c83";
const REVISION: &str = "classic US aspMain (sm64 US)";

#[derive(Default)]
struct Tokens {
    by_address: HashMap<usize, u32>,
    addresses: Vec<usize>,
}

impl Tokens {
    fn insert(&mut self, address: usize) -> Result<u32, String> {
        if let Some(&token) = self.by_address.get(&address) {
            return Ok(token);
        }
        let index = u32::try_from(self.addresses.len() + 1)
            .ok()
            .filter(|&index| index <= u32::MAX >> 3)
            .ok_or("native audio address tokens exhausted")?;
        let token = (index << 3) | (address as u32 & 7);
        self.addresses.push(address);
        self.by_address.insert(address, token);
        Ok(token)
    }

    fn address(&self, token: u32) -> Result<usize, String> {
        let address = (token >> 3)
            .checked_sub(1)
            .and_then(|index| self.addresses.get(index as usize))
            .filter(|&&address| address & 7 == token as usize & 7);
        address
            .copied()
            .ok_or_else(|| format!("unmapped native audio address token {token:#010x}"))
    }
}

#[derive(Default)]
struct HostMemory {
    tokens: Tokens,
}

impl Memory for HostMemory {
    fn resolve<O: HalfwordOrder>(&mut self, _dmem: &Dmem<O>, word: u64) -> Result<u32, String> {
        self.tokens
            .insert(usize::try_from(word).map_err(|_| "native audio address exceeds host width")?)
    }

    fn dma<O: HalfwordOrder>(
        &mut self,
        dmem: &mut Dmem<O>,
        address: u32,
        dram: u32,
        value: u32,
        write: bool,
    ) -> Result<(), String> {
        if value == u32::MAX {
            return Err("native audio DMA length underflow".into());
        }
        let dma = Dma::decode(value);
        let start = self.tokens.address(dram)? & !7;
        let extent = dma.rows * dma.length + (dma.rows - 1) * dma.skip;
        if start == 0 || start.checked_add(extent).is_none() {
            return Err("invalid native audio DMA address".into());
        }
        // RSP DMA truncates both starting addresses to multiples of eight.
        let mut address = address & 0xff8;
        for row in 0..dma.rows {
            let pointer = (start + row * (dma.length + dma.skip)) as *mut u8;
            for i in 0..dma.length {
                let offset = address + i as u32;
                unsafe {
                    if write {
                        pointer.add(i).write(dmem.raw(offset));
                    } else {
                        dmem.set_raw(offset, pointer.add(i).read());
                    }
                }
            }
            address = (address + dma.length as u32) & 4095;
        }
        Ok(())
    }
}

/// Audio-local state and retained host addresses survive successive native tasks.
#[derive(Default)]
pub struct NativeContext {
    pub state: Context<Ne>,
    memory: HostMemory,
    microcode: Microcode,
}

impl NativeContext {
    pub(in crate::ultra) fn new(microcode: Microcode) -> Self {
        Self {
            microcode,
            ..Self::default()
        }
    }

    #[allow(dead_code, reason = "Used by the offline native oracle bridge.")]
    pub fn retained_address(&self, token: u32) -> Result<usize, String> {
        self.memory.tokens.address(token)
    }

    /// Execute a native audio task synchronously. On error the task stops at the failing command.
    ///
    /// # Safety
    /// `task` must point to a live host OSTask. Its data payload must be readable for
    /// the supported revision's full data extent (736 bytes for Zelda, whose task
    /// size field is 735), and its list for `data_size / 16` native packets.
    /// Every command address must support the microcode's rounded DMA reads/writes,
    /// including address truncation, rows and skips, without aliasing this context.
    /// Retained addresses must stay live while later tasks can use their DMEM tokens.
    /// Guest memory must not be accessed concurrently during execution.
    pub unsafe fn task(&mut self, task: *const c_void) -> Result<(), String> {
        if task.is_null() {
            return Err("null native audio task".into());
        }
        if std::mem::size_of::<usize>() != 8 {
            return Err("native audio requires 16-byte packets with 64-bit words".into());
        }
        let task = unsafe { (task as *const OSTaskT).read_unaligned() };
        let (revision, extent, task_size, hash) = match self.microcode {
            Microcode::Classic => (REVISION, DATA_SIZE, DATA_SIZE, DATA_SHA256),
            Microcode::Zelda => (
                "Zelda aspMain (NTSC 1.0)",
                736,
                735,
                "7a2773900752702bd70e073813f6e57601a9c1c741b07057799bd882eaa10e27",
            ),
        };
        if task.ucode_data_size as usize != task_size || task.ucode_data.is_null() {
            return Err(format!(
                "expected {revision} data: {extent} raw bytes, task size {task_size}"
            ));
        }
        let data = unsafe { std::slice::from_raw_parts(task.ucode_data.cast::<u8>(), extent) };
        if format!("{:x}", Sha256::digest(data)) != hash {
            return Err(format!("expected {revision} data: SHA-256 {hash}"));
        }
        self.load_data(data);
        unsafe { self.commands(task.data_ptr.cast(), task.data_size) }
    }

    fn load_data(&mut self, data: &[u8]) {
        for (i, pair) in data.as_chunks::<2>().0.iter().enumerate() {
            self.state
                .dmem
                .set_h16(i as u32 * 2, u16::from_be_bytes(*pair));
        }
    }

    unsafe fn commands(&mut self, packets: *const [usize; 2], size: u32) -> Result<(), String> {
        let count = size / 16;
        if count != 0 && packets.is_null() {
            return Err("null native audio command list".into());
        }
        if self.microcode == Microcode::Classic {
            self.state.dmem.set_word(SEGMENT_TABLE, 0);
        }
        let dispatch = if self.microcode == Microcode::Zelda {
            self.state.zelda_start()
        } else {
            [0; 26]
        };
        for i in 0..count as usize {
            let [w0, w1] = unsafe { packets.add(i).read_unaligned() };
            match self.microcode {
                Microcode::Classic => self.state.command(&mut self.memory, w0 as u32, w1 as u64)?,
                Microcode::Zelda => {
                    self.state.scalar[TASK_DATA_SIZE] = (count - i as u32 - 1) * 8;
                    self.state.scalar[CHUNK_REMAINING] =
                        (count - i as u32).min(8 - i as u32 % 8) * 8 - 8;
                    self.state
                        .zelda_command(&mut self.memory, w0 as u32, w1 as u64, &dispatch)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
