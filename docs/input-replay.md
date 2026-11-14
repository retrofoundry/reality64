# Input replay

`REALITY64_INPUT_RECORD=/path/route.input` records controller changes at
`osViSwapBuffer`. `REALITY64_INPUT_REPLAY=/path/route.input` replays them.
Both can be set to extend a route with live input after its end.

The version 1 format remains unchanged:

```text
reality64-input 1 1
0 0000 0 0
10 8000 0 0
12 0000 0 0
end 60
```

The header's last number is the connected-controller bit mask. Each change is
`swap buttons-hex stick-x stick-y`; swaps start at zero. `end` is inclusive and
otherwise defaults to the last change. Input before the first change is neutral.
Existing route files need no conversion.

During replay, VI advances only when no positive-priority guest thread is ready.
Each step advances one NTSC/MPAL or PAL retrace, services guest timers, then posts
VI with the registered divisor. The scheduler retains the run token while
injecting events and drains runnable guest work before advancing again. Input
still changes at swaps, so the retrace that samples a change follows guest
progress. PI DMA takes no virtual time: the oldest pending transfer completes at
each guest message send or receive and before each step, one transfer at a time.
This stands in for the PI interrupt, so it arrives early when a message call
follows the request sooner than the cart would deliver, and late when a guest runs
a long stretch without one. The audio interface follows the same clock: AI length,
status and queue-full results come from accepted buffers playing at the source rate
in virtual time, with live playback's queue cap and 80 ms prebuffer, and nothing
reaches the output device.

`osGetTime` and `osGetCount` read this virtual clock; time does not advance while
guest code or a synchronous graphics task is running. Timers fire at the first
idle retrace at or after their deadline, before that retrace's VI message.
Playback runs as fast as guest work and rendering allow. It is not a measurement
of ordinary play's wall-clock pacing. Busy waits for time without blocking the
guest scheduler cannot progress in this mode. VI must be registered before a
guest blocks waiting for a timer.

After the inclusive end swap, the next swap returns to live input and starts the
ordinary VI and timer clocks. Guest time stays continuous and pending timers
retain their remaining duration. Recording alone and play without a valid replay
keep their ordinary timing.

This controls guest VI, timer and input ordering. It is not a general emulator
snapshot: start from the same executable, assets and save. Other host-driven
services must be controlled separately if a guest uses them. Native OoT's audio
driver is stubbed; its graphics and DMA completions execute under the guest run
token. The parity harness also fixes the game's RNG seed.
