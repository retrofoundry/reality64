# Native interpolation sidecars

`include/reality64/interpolation.h` exposes annotation ABI 2. Call
`R64InterpolationRegister(R64_INTERPOLATION_ABI_VERSION)` once at startup. It returns
zero on success or -1 with an error diagnostic on an ABI mismatch. Check that result.

This C sidecar version is independent of ultragfx's Rust annotation ABI 3. The adapter
uses invocation scopes without composed views; no C layout or caller changes are needed.

Initialize `R64TaskAnnotations` with `R64_TASK_ANNOTATIONS_INIT`, then set `source`,
`epoch`, `producer_sample`, `draws` and `draw_count`. Attach it to the graphics
`OSTask*` with `R64InterpolationAttach(task, &annotations)` before
`osSpTaskStartGo`. Attachment validates and copies the complete sidecar, including
matrix bindings, palette parts and billboard recipes. Guest storage may be reused
as soon as attachment returns. The task consumes its attachment once; attaching
again before consumption fails. Tasks without an attachment retain the empty
annotations used by the opt-in path.

Sizes and counts use `uintptr_t`, including on ports whose local `size_t` remains
32-bit. Versions, enum tags and flags use `uint32_t`; identities and timestamps use
`uint64_t`. The header supplies size fields for every variable-length element
layout. The caller must provide a readable version/flags/struct_size prefix and
readable allocations for every nonempty array. Validation rejects mismatched
sizes, null or misaligned arrays, unknown tags, nonfinite recipes and excessive
counts. Limits are 16,384 scopes and 65,536 total bindings and palette parts per
task, separately. Invalid input returns -1 with a diagnostic.

A scope selects the native address of a call or branch command, not its target.
`has_occurrence = 0` requires that command to execute exactly once. Set it to 1 to
select the zero-based `occurrence`. Static descendants inherit the scope.
`R64DrawIdentity` supplies the cross-source identity. Command and matrix addresses
are task-local operands, never persistent object IDs.

Policies map directly to Interpolate, StaticWorld, HoldModel, BillboardHold,
ScreenSpace and Hold. Bind allocated View/Projection operands, or an opaque
ViewProjection operand. The engine resolves each binding at actual matrix reads,
including late rewrites. Palette parts map consecutive 64-byte slots to semantic
parts. Billboard recipes carry anchor, scale, spin in radians, row-vector basis
and CPU or segment-1 provenance.

World geometry marked Hold makes an ABI 2 source ineligible. It repeats the whole
source endpoint; it does not freeze just that geometry while the camera moves.
ScreenSpace is for UI. World pose holds require HoldModel or BillboardHold.

`producer_sample` is an opaque producer timestamp. `epoch` is the producer's
continuity generation. `events` and `R64_INTERPOLATION_FRAMEBUFFER_DEPENDENT` are
producer diagnostics reserved by the ABI. The producer must change epoch across a
dependency interval and on its exit; the flag itself is not an engine admission
reason. Engine framebuffer admission still runs independently. Task cookies and
monotonic swap timestamps remain host-owned. Sidecar conversion does not change
consume, guest write-back, completion, SP and DP ordering.

The default mode is Original. A port can call `R64SetRefreshRate(mode, hz)` at any
point, including from a settings menu. Modes are `R64_REFRESH_ORIGINAL`,
`R64_REFRESH_DISPLAY` and `R64_REFRESH_MANUAL`. Pass zero Hz for Original/Display;
Manual accepts positive finite Hz. Invalid input returns -1 with a diagnostic and
preserves the previous setting. Ports own their defaults and persistence.

`REALITY64_REFRESH_RATE=original|display|manual:<hz>` overrides the port setting
for development. Invalid values fail at startup, including during capture. Capture
forces Original. The former `REALITY64_FRAME_INTERPOLATION` variable is obsolete.

Settings take effect at the next VI swap. `R64InterpolationActive()` reports the
value latched at the last swap, so ports can avoid building sidecars in Original.
The first swap after enabling interpolation presents a plain image; compatible
retained tasks from later swaps establish a pair. Registration and annotation ABI 2
are unchanged. Only registered ports retain tasks when interpolation is active.

Ultragfx owns the native surface and its present thread. Original acknowledges
each presentation before returning. Display and Manual accept immutable scanouts
asynchronously, keeping the newest sources. Missing or stale annotations break the
pair and still present the real framebuffer. Phases never alter canonical output
or guest write-back. The latest task before the swap is selected explicitly; a
no-swap task creates no source.

Reality64 supplies the current monitor's nominal millihertz at startup and on
moves/resizes. Unknown rates use 60 Hz. Manual retains its requested rate and clamps
the effective rate to the display. The scheduler paces before surface acquisition,
delays interpolation by the larger of the last two source intervals plus the
preparation lead, and rebases on rate changes. It retains the preceding pair until
the delayed sample reaches the latest pair. Its phase time is an estimate, not a
physical scanout timestamp or a VRR guarantee.

Until presenter B merges, this branch patches ultragfx to
`/Users/ci/hub/wt/fast3d-rs-ultragfx-refresh-rate/ultragfx`. Replace that local patch
with the merged revision before sharing the branch.
