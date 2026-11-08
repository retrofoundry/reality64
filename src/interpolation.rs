use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::{align_of, size_of};
use std::sync::{Mutex, OnceLock};
use ultragfx::annotation::*;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct R64DrawIdentity {
    pub namespace_id: u64,
    pub instance: u64,
    pub generation: u64,
    pub part: u64,
    pub subdraw: u32,
    pub pass: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct R64InvocationSelector {
    pub address: usize,
    pub occurrence: u32,
    pub has_occurrence: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct R64MatrixBinding {
    pub operand: usize,
    pub role: u32,
}

#[repr(C)]
pub struct R64MatrixPalette {
    pub base: usize,
    pub parts: *const u64,
    pub count: usize,
}

#[repr(C)]
pub struct R64BillboardRecipe {
    pub anchor: [f32; 3],
    pub scale: [f32; 3],
    pub spin: f32,
    pub basis: [[f32; 3]; 3],
    pub provenance: u32,
}

#[repr(C)]
pub struct R64DrawAnnotation {
    pub scope: R64InvocationSelector,
    pub identity: R64DrawIdentity,
    pub policy: u32,
    pub matrices: *const R64MatrixBinding,
    pub matrix_count: usize,
    pub palette: *const R64MatrixPalette,
    pub billboard: *const R64BillboardRecipe,
}

#[repr(C)]
pub struct R64TaskAnnotations {
    pub version: u32,
    pub flags: u32,
    pub struct_size: usize,
    pub draw_size: usize,
    pub matrix_size: usize,
    pub palette_size: usize,
    pub billboard_size: usize,
    pub source: u64,
    pub epoch: u64,
    pub producer_sample: u64,
    pub events: u64,
    pub draws: *const R64DrawAnnotation,
    pub draw_count: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Sidecar {
    pub annotations: TaskAnnotations,
    pub source: u64,
    pub epoch: u64,
    pub producer_sample: u64,
}

#[derive(Default)]
struct Registry {
    registered: bool,
    refresh_rate: ultragfx::RefreshRate,
    tasks: HashMap<usize, Sidecar>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

fn version(version: u32) -> anyhow::Result<()> {
    anyhow::ensure!(
        version == ANNOTATION_ABI_VERSION,
        "annotation ABI {version}, expected {ANNOTATION_ABI_VERSION}"
    );
    Ok(())
}

#[no_mangle]
pub extern "C" fn R64InterpolationRegister(requested: u32) -> i32 {
    match version(requested) {
        Ok(()) => {
            registry().lock().unwrap().registered = true;
            0
        }
        Err(error) => {
            log::error!("R64InterpolationRegister: {error}");
            -1
        }
    }
}

unsafe fn array<'a, T>(ptr: *const T, count: usize, max: usize) -> anyhow::Result<&'a [T]> {
    anyhow::ensure!(count <= max, "sidecar count {count} exceeds {max}");
    if count == 0 {
        return Ok(&[]);
    }
    anyhow::ensure!(
        !ptr.is_null() && (ptr as usize).is_multiple_of(align_of::<T>()),
        "null or misaligned sidecar array"
    );
    anyhow::ensure!(
        (ptr as usize).checked_add(count * size_of::<T>()).is_some(),
        "sidecar address overflow"
    );
    Ok(std::slice::from_raw_parts(ptr, count))
}

unsafe fn convert(input: *const R64TaskAnnotations) -> anyhow::Result<Sidecar> {
    version(array(input.cast::<u32>(), 1, 1)?[0])?;
    #[repr(C)]
    struct Prefix {
        version: u32,
        flags: u32,
        struct_size: usize,
    }
    let prefix = &array(input.cast::<Prefix>(), 1, 1)?[0];
    anyhow::ensure!(
        prefix.struct_size == size_of::<R64TaskAnnotations>(),
        "sidecar structure size {}, expected {}",
        prefix.struct_size,
        size_of::<R64TaskAnnotations>()
    );
    let input = &array(input, 1, 1)?[0];
    anyhow::ensure!(
        input.draw_size == size_of::<R64DrawAnnotation>()
            && input.matrix_size == size_of::<R64MatrixBinding>()
            && input.palette_size == size_of::<R64MatrixPalette>()
            && input.billboard_size == size_of::<R64BillboardRecipe>(),
        "sidecar structure size mismatch"
    );
    anyhow::ensure!(input.flags & !1 == 0, "unknown sidecar flags");
    let mut sidecar = Sidecar {
        source: input.source,
        epoch: input.epoch,
        producer_sample: input.producer_sample,
        ..Default::default()
    };
    let mut total_parts = 0;
    let mut total_matrices = 0;
    for draw in array(input.draws, input.draw_count, 16384)? {
        anyhow::ensure!(
            draw.scope.address != 0 && draw.scope.has_occurrence <= 1,
            "invalid invocation selector"
        );
        let policy = match draw.policy {
            0 => TransformPolicy::Interpolate,
            1 => TransformPolicy::StaticWorld,
            2 => TransformPolicy::HoldModel,
            3 => {
                let recipe = &array(draw.billboard, 1, 1)?[0];
                anyhow::ensure!(
                    recipe
                        .anchor
                        .iter()
                        .chain(&recipe.scale)
                        .chain(std::iter::once(&recipe.spin))
                        .chain(recipe.basis.iter().flatten())
                        .all(|v| v.is_finite()),
                    "non-finite billboard recipe"
                );
                TransformPolicy::BillboardHold(BillboardRecipe {
                    anchor: recipe.anchor,
                    scale: recipe.scale,
                    spin: recipe.spin,
                    basis: recipe.basis,
                    provenance: match recipe.provenance {
                        0 => BillboardProvenance::Cpu,
                        1 => BillboardProvenance::Segment1,
                        _ => anyhow::bail!("unknown billboard provenance"),
                    },
                })
            }
            4 => TransformPolicy::ScreenSpace,
            5 => TransformPolicy::Hold,
            _ => anyhow::bail!("unknown transform policy"),
        };
        let mut matrices = Vec::new();
        anyhow::ensure!(
            draw.matrix_count <= 65536 - total_matrices,
            "too many matrix bindings"
        );
        total_matrices += draw.matrix_count;
        for binding in array(draw.matrices, draw.matrix_count, 65536)? {
            matrices.push(MatrixBinding {
                operand: binding.operand as u64,
                role: match binding.role {
                    0 => MatrixRole::View,
                    1 => MatrixRole::Projection,
                    2 => MatrixRole::ViewProjection,
                    _ => anyhow::bail!("unknown matrix role"),
                },
            });
        }
        let palette = if draw.palette.is_null() {
            None
        } else {
            let palette = &array(draw.palette, 1, 1)?[0];
            anyhow::ensure!(
                palette.count <= 65536 - total_parts,
                "invalid palette count"
            );
            total_parts += palette.count;
            Some(MatrixPalette {
                base: palette.base as u64,
                parts: array(palette.parts, palette.count, 65536)?.to_vec(),
            })
        };
        sidecar.annotations.draws.push(DrawAnnotation {
            scope: InvocationSelector {
                address: draw.scope.address as u64,
                occurrence: (draw.scope.has_occurrence == 1).then_some(draw.scope.occurrence),
            },
            identity: DrawIdentity {
                namespace: draw.identity.namespace_id,
                instance: draw.identity.instance,
                generation: draw.identity.generation,
                part: draw.identity.part,
                subdraw: draw.identity.subdraw,
                pass: draw.identity.pass,
            },
            policy,
            matrices,
            palette,
        });
    }
    Ok(sidecar)
}

#[no_mangle]
pub extern "C" fn R64InterpolationAttach(
    task: *const c_void,
    input: *const R64TaskAnnotations,
) -> i32 {
    let result = (|| {
        let mut registry = registry().lock().unwrap();
        anyhow::ensure!(
            registry.registered,
            "register the annotation ABI before attaching a task"
        );
        anyhow::ensure!(!task.is_null(), "null OSTask");
        anyhow::ensure!(
            !registry.tasks.contains_key(&(task as usize)),
            "OSTask already has an unconsumed sidecar"
        );
        anyhow::ensure!(registry.tasks.len() < 64, "too many unconsumed sidecars");
        let sidecar = unsafe { convert(input)? };
        registry.tasks.insert(task as usize, sidecar);
        Ok::<_, anyhow::Error>(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            log::error!("R64InterpolationAttach: {error}");
            -1
        }
    }
}

pub(crate) fn take(task: usize) -> Option<Sidecar> {
    registry().lock().unwrap().tasks.remove(&task)
}

pub(crate) fn port_settings() -> (bool, ultragfx::RefreshRate) {
    let registry = registry().lock().unwrap();
    (registry.registered, registry.refresh_rate)
}

pub(crate) static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn refresh_rate(mode: u32, hz: f64) -> anyhow::Result<ultragfx::RefreshRate> {
    let rate = match mode {
        0 if hz == 0.0 => ultragfx::RefreshRate::Original,
        1 if hz == 0.0 => ultragfx::RefreshRate::Display,
        2 => ultragfx::RefreshRate::Manual(hz),
        _ => anyhow::bail!(
            "expected Original (0) or Display (1) with hz=0, or Manual (2) with positive finite Hz"
        ),
    };
    rate.validate()
        .map_err(|error| anyhow::anyhow!("invalid refresh rate {hz}: {error:?}"))?;
    Ok(rate)
}

#[no_mangle]
pub extern "C" fn R64SetRefreshRate(mode: u32, hz: f64) -> i32 {
    match refresh_rate(mode, hz) {
        Ok(rate) => {
            registry().lock().unwrap().refresh_rate = rate;
            0
        }
        Err(error) => {
            log::error!("R64SetRefreshRate: {error}");
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn R64InterpolationActive() -> i32 {
    i32::from(ACTIVE.load(std::sync::atomic::Ordering::Acquire))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> R64TaskAnnotations {
        R64TaskAnnotations {
            version: ANNOTATION_ABI_VERSION,
            flags: 0,
            struct_size: size_of::<R64TaskAnnotations>(),
            draw_size: size_of::<R64DrawAnnotation>(),
            matrix_size: size_of::<R64MatrixBinding>(),
            palette_size: size_of::<R64MatrixPalette>(),
            billboard_size: size_of::<R64BillboardRecipe>(),
            source: 7,
            epoch: 11,
            producer_sample: 19,
            events: 0,
            draws: std::ptr::null(),
            draw_count: 0,
        }
    }

    #[test]
    fn rejects_version_sizes_counts_and_null_arrays() {
        let mut input = empty();
        input.version += 1;
        assert!(unsafe { convert(&input) }.is_err());
        assert!(version(input.version).is_err());
        for field in 0..5 {
            input = empty();
            let size = match field {
                0 => &mut input.struct_size,
                1 => &mut input.draw_size,
                2 => &mut input.matrix_size,
                3 => &mut input.palette_size,
                _ => &mut input.billboard_size,
            };
            *size -= 1;
            assert!(unsafe { convert(&input) }.is_err());
        }
        input = empty();
        input.draw_count = usize::MAX;
        assert!(unsafe { convert(&input) }.is_err());
        input.draw_count = 1;
        assert!(unsafe { convert(&input) }.is_err());
        #[repr(C)]
        struct Short {
            version: u32,
            flags: u32,
            size: usize,
        }
        let short = Short {
            version: ANNOTATION_ABI_VERSION,
            flags: 0,
            size: size_of::<Short>(),
        };
        assert!(unsafe { convert((&short as *const Short).cast()) }.is_err());
    }

    #[test]
    fn empty_sidecar_preserves_task_metadata() {
        let result = unsafe { convert(&empty()) }.unwrap();
        assert!(result.annotations.draws.is_empty());
        assert_eq!(
            (result.source, result.epoch, result.producer_sample),
            (7, 11, 19)
        );
    }

    #[test]
    fn scope_round_trip_copies_native_operands_and_palette() {
        let mut matrices = [R64MatrixBinding {
            operand: 0x123456789,
            role: 0,
        }];
        let mut parts = [17, 10];
        let palette = R64MatrixPalette {
            base: 0xd000000,
            parts: parts.as_ptr(),
            count: 2,
        };
        let mut draw = R64DrawAnnotation {
            scope: R64InvocationSelector {
                address: 0x234567890,
                occurrence: 3,
                has_occurrence: 1,
            },
            identity: R64DrawIdentity {
                namespace_id: 3,
                instance: 9,
                generation: 12,
                part: 17,
                subdraw: 2,
                pass: 1,
            },
            policy: 2,
            matrices: matrices.as_ptr(),
            matrix_count: 1,
            palette: &palette,
            billboard: std::ptr::null(),
        };
        let mut input = empty();
        input.draws = &draw;
        input.draw_count = 1;
        let result = unsafe { convert(&input) }.unwrap();
        matrices[0].operand = 0;
        parts[0] = 0;
        let result = &result.annotations.draws[0];
        assert_eq!(
            result.scope,
            InvocationSelector {
                address: 0x234567890,
                occurrence: Some(3)
            }
        );
        assert_eq!(
            result.identity,
            DrawIdentity {
                namespace: 3,
                instance: 9,
                generation: 12,
                part: 17,
                subdraw: 2,
                pass: 1
            }
        );
        assert_eq!(result.policy, TransformPolicy::HoldModel);
        assert_eq!(result.matrices[0].operand, 0x123456789);
        assert_eq!(result.palette.as_ref().unwrap().parts, [17, 10]);
        assert_eq!(matrices[0].operand, 0);
        assert_eq!(parts[0], 0);
        draw.matrix_count = usize::MAX;
        input.draws = &draw;
        assert!(unsafe { convert(&input) }.is_err());
        draw.matrix_count = 1;
        draw.scope.has_occurrence = 2;
        input.draws = &draw;
        assert!(unsafe { convert(&input) }.is_err());
        draw.scope.has_occurrence = 1;
        draw.policy = 99;
        input.draws = &draw;
        assert!(unsafe { convert(&input) }.is_err());
    }

    #[test]
    fn attachment_is_owned_and_consumed_once() {
        assert_eq!(R64InterpolationRegister(ANNOTATION_ABI_VERSION + 1), -1);
        assert_eq!(R64InterpolationRegister(ANNOTATION_ABI_VERSION), 0);
        let task = 0x1_3456_7890usize as *const c_void;
        let mut input = empty();
        assert_eq!(R64InterpolationAttach(task, &input), 0);
        input.source = 99;
        assert_eq!(R64InterpolationAttach(task, &input), -1);
        assert_eq!(take(task as usize).unwrap().source, 7);
        assert!(take(task as usize).is_none());
        assert_eq!(R64InterpolationAttach(task, &input), 0);
        assert_eq!(take(task as usize).unwrap().source, 99);
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    #[test]
    fn rejected_port_setting_preserves_the_last_valid_rate() {
        let previous = port_settings().1;
        assert_eq!(R64SetRefreshRate(2, 59.94), 0);
        assert_eq!(port_settings().1, ultragfx::RefreshRate::Manual(59.94));
        assert_eq!(R64SetRefreshRate(2, f64::NAN), -1);
        assert_eq!(port_settings().1, ultragfx::RefreshRate::Manual(59.94));
        registry().lock().unwrap().refresh_rate = previous;
    }

    #[test]
    fn port_modes_validate_without_a_renderer() {
        assert_eq!(
            refresh_rate(0, 0.0).unwrap(),
            ultragfx::RefreshRate::Original
        );
        assert_eq!(
            refresh_rate(1, 0.0).unwrap(),
            ultragfx::RefreshRate::Display
        );
        assert_eq!(
            refresh_rate(2, 59.94).unwrap(),
            ultragfx::RefreshRate::Manual(59.94)
        );
        for (mode, hz) in [
            (3, 0.0),
            (2, 0.0),
            (2, -1.0),
            (2, f64::NAN),
            (2, f64::INFINITY),
            (1, 60.0),
        ] {
            assert!(refresh_rate(mode, hz).is_err());
        }
    }
}
