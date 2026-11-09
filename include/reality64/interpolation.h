#ifndef REALITY64_INTERPOLATION_H
#define REALITY64_INTERPOLATION_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define R64_INTERPOLATION_ABI_VERSION 2
#define R64_INTERPOLATION_MAX_DRAWS 16384
#define R64_INTERPOLATION_MAX_PARTS 65536
#define R64_INTERPOLATION_MAX_BINDINGS 65536
#define R64_INTERPOLATION_FRAMEBUFFER_DEPENDENT 1

enum {
    R64_MATRIX_VIEW,
    R64_MATRIX_PROJECTION,
    R64_MATRIX_VIEW_PROJECTION
};
enum {
    R64_TRANSFORM_INTERPOLATE,
    R64_TRANSFORM_STATIC_WORLD,
    R64_TRANSFORM_HOLD_MODEL,
    R64_TRANSFORM_BILLBOARD_HOLD,
    R64_TRANSFORM_SCREEN_SPACE,
    R64_TRANSFORM_HOLD
};
enum { R64_BILLBOARD_CPU, R64_BILLBOARD_SEGMENT_1 };

typedef struct {
    uint64_t namespace_id, instance, generation, part;
    uint32_t subdraw, pass;
} R64DrawIdentity;

typedef struct {
    uintptr_t address;
    uint32_t occurrence, has_occurrence;
} R64InvocationSelector;

typedef struct {
    uintptr_t operand;
    uint32_t role;
} R64MatrixBinding;

typedef struct {
    uintptr_t base;
    const uint64_t* parts;
    uintptr_t count;
} R64MatrixPalette;

typedef struct {
    float anchor[3], scale[3], spin, basis[3][3];
    uint32_t provenance;
} R64BillboardRecipe;

typedef struct {
    R64InvocationSelector scope;
    R64DrawIdentity identity;
    uint32_t policy;
    const R64MatrixBinding* matrices;
    uintptr_t matrix_count;
    const R64MatrixPalette* palette;
    const R64BillboardRecipe* billboard;
} R64DrawAnnotation;

typedef struct {
    uint32_t version, flags;
    uintptr_t struct_size, draw_size, matrix_size, palette_size, billboard_size;
    uint64_t source, epoch, producer_sample, events;
    const R64DrawAnnotation* draws;
    uintptr_t draw_count;
} R64TaskAnnotations;

#define R64_TASK_ANNOTATIONS_INIT { \
    .version = R64_INTERPOLATION_ABI_VERSION, \
    .struct_size = sizeof(R64TaskAnnotations), .draw_size = sizeof(R64DrawAnnotation), \
    .matrix_size = sizeof(R64MatrixBinding), .palette_size = sizeof(R64MatrixPalette), \
    .billboard_size = sizeof(R64BillboardRecipe) \
}

enum { R64_REFRESH_ORIGINAL, R64_REFRESH_DISPLAY, R64_REFRESH_MANUAL };

/* May be called before startup or during gameplay. Latches at the next VI swap.
 * Original/Display require hz=0. Manual requires positive finite Hz and is clamped
 * to the monitor rate. Returns 0 on success, -1 with a diagnostic on invalid input. */
int32_t R64SetRefreshRate(uint32_t mode, double hz);
/* Last swap's effective interpolation mode; ports may skip annotations when zero. */
int32_t R64InterpolationActive(void);

/* Returns 0 on success, -1 with a diagnostic on invalid ABI or input. */
int32_t R64InterpolationRegister(uint32_t version);
/* The input must contain at least the version/flags/struct_size prefix; nonempty
 * arrays must point to readable allocations of the declared lengths.
 * task is an OSTask*. Attach once before osSpTaskStartGo. Copies all sidecar data;
 * the guest may release it after this returns. A pending attachment is consumed once. */
int32_t R64InterpolationAttach(const void* task, const R64TaskAnnotations* annotations);

#ifdef __cplusplus
}
#endif
#endif
