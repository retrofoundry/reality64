# Audio microcode

reality64 runs a game's audio command lists itself instead of emulating the RSP. Each handler is
written for one microcode revision. The console-format path reproduces its output samples,
state in memory and DMEM contents. The native path uses host-order samples and full host pointers.

| Selector | Microcode | Data section |
| --- | --- | --- |
| `R64_RSP_AUDIO_ACMD` (5) | classic `aspMain`, Super Mario 64 (US) | 704 bytes, SHA-256 `79bb2a3e8555019bc4b229f0c218f1e329ca5711bfa55c7c7f992facf2e12c83` |
| `R64_RSP_AUDIO_ZELDA` (9) | Zelda `aspMain`, Ocarina of Time (NTSC 1.0) | 736 bytes, SHA-256 `7a2773900752702bd70e073813f6e57601a9c1c741b07057799bd882eaa10e27` |

## What the port provides

Register the microcode's text address with `R64RspRegister`, as for graphics.

Link the real `aspMain` data section and point `OSTask.t.ucode_data` at it, with
`ucode_data_size` set to 704 for classic or 735 for Zelda. Zelda's size field is one less
than its raw extent; the pointer must still cover all 736 bytes. The handler reads its tables
from there, the way the RSP loads them at the start of each task, so reality64 itself contains
no microcode. Generate the data at build time from the game's own sources or ROM; don't commit
it. A missing or different data section rejects the task with a message naming the revision
reality64 expected.

Commands are reality64's native `Acmd` packets: two 64-bit words, with full host pointers wherever
the console command carries an RDRAM address.

Keep data in the representation the game's CPU code already uses: PCM, codebooks, loop history
and voice state as host-order 16-bit values, compressed ADPCM bytes as they come from the ROM.

Keep every audio buffer at the same alignment modulo 8 it had on the console. Like the RSP's
DMA, the handler rounds addresses down to a multiple of 8 and lengths up, so allocations must
also cover those rounded extents.

Tasks run synchronously on the calling guest thread and keep their state between tasks. A loop
or state buffer the game hands to one task must stay alive while later tasks can use it.
Native classic and Zelda tasks have separate persistent contexts. Neither shares physical
RSP state with the runtime's graphics or JPEG handlers.

Zelda envelope commands must be consecutive: ENVSETUP1 → ENVSETUP2 → ENVMIXER, as emitted by
the synthesis builder. Interleaving other commands can change scalar registers, vector state
and subsequent DMEM output. Self-modifying dispatch is unsupported: a command
whose dispatch target was overwritten earlier in the task returns an error instead of
executing the original hard-coded handler.

A task that fails stops at the failing command and is still completed, so the game keeps running;
its error is logged. Output the task wrote before that command stays in place. One such failure is
a DMA whose length counts down below zero: the register value then means 256 rows of 4 KB with a
4095-byte skip, which the console carries out and the native path refuses.

## How it was checked

The handlers were checked against the microcode itself, running on a real N64 and in an
interpreter that matches the console byte for byte. Synthetic tasks cover every command's
boundaries, and consumer fixtures reproduce command sequences emitted by the games' builders.
Console checks compare all captured regions and all 4096 DMEM bytes. Native checks normalize
typed halfwords and retained pointers, excluding console transport and spans with incompatible
raw/halfword interpretations. They are not full-capture byte-equality checks.

Native output can differ from the console in two places: odd-address operations on halfword data,
and the refused underflowing DMA above. The port must audit the first: Zelda's comb-filter builder
can form odd addresses, though the saved consumer fixtures do not establish an executed gameplay
case. Logical byte windows through the microcode's halfword tables are handled separately,
including small ADPCM's odd windows.
