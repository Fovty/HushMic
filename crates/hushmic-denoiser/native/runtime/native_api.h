#ifndef DPDF_NATIVE_API_H
#define DPDF_NATIVE_API_H
#include <stddef.h>
#include <stdint.h>
#include "dpdf_dprnn.h"
#ifdef __cplusplus
extern "C" {
#endif

#define DPDF_NATIVE_ABI_VERSION 1u
/* Preset values and v1 table layout are frozen. New incompatible interfaces
 * use a new version. FP32 uses automatic SIMD dispatch, with scalar fallback.
 * INT8_SELECTIVE is the measured DPRNN + dense/grouped FC + 1x1 CNN W8A8
 * configuration. Other CNN, gates, normalization and state remain FP32.
 * It requires AVX2/FMA; unsupported requests fail rather than change precision.
 */
enum {
    DPDF_PRESET_FP32 = 0,
    DPDF_PRESET_INT8_SELECTIVE = 1
};
typedef struct dpdf_native_model dpdf_native_model;

/* Obtain this immutable table from <symbol_prefix>_get_api(1).
 * An unsupported ABI version returns NULL. The table lives for the lifetime
 * of the library. Use only this table's functions with its model instances.
 * Sizes below are float counts, except struct_size and owned_bytes.
 * Weights are little-endian IEEE754 float32 on disk; verify SHA-256 and size
 * before decoding/loading. create copies all retained weights, so the input
 * blob can be released afterwards. create returns NULL for invalid arguments,
 * unavailable presets, nonfinite weights or allocation failure.
 * Call init_state before the first hop and on every stream reset.
 * Each process call consumes spectrum_size floats; state buffers hold
 * state_size floats. Spectrum/state can each be in-place; no other overlap.
 * Inputs must be finite. process allocates nothing and returns 0 on success,
 * -1 for NULL arguments. One mutable context per simultaneous process call;
 * never use a context concurrently. Different streams need independent state.
 * destroy(NULL) is safe. This is a spectral API: STFT/iSTFT remain in the host.
 */
typedef struct dpdf_native_api_v1 {
    uint32_t abi_version;
    uint32_t struct_size;
    const char *model_name;
    const char *weights_sha256;
    uint32_t sample_rate;
    uint32_t hop_size;
    size_t spectrum_size;
    size_t state_size;
    size_t weight_count;
    int (*preset_supported)(uint32_t preset);
    dpdf_native_model *(*create)(const float *weights, size_t count, uint32_t preset);
    int (*init_state)(float *state);
    int (*process)(dpdf_native_model *, const float *spec, const float *state_in,
                   float *spec_out, float *state_out);
    void (*destroy)(dpdf_native_model *);
    size_t (*owned_bytes)(const dpdf_native_model *);
} dpdf_native_api_v1;

#ifdef __cplusplus
}
#endif
#endif
