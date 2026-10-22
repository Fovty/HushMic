#ifndef DPDFNET_H
#define DPDFNET_H
#include <stddef.h>
#include <stdint.h>
#if defined(__GNUC__)
#define DPDFNET_API __attribute__((visibility("default")))
#else
#define DPDFNET_API
#endif
#ifdef __cplusplus
extern "C" {
#endif
#define DPDFNET_ABI_VERSION 1
typedef enum {
    DPDFNET_MODEL_2 = 2,
    DPDFNET_MODEL_8 = 8
} dpdfnet_model_id;
typedef struct dpdfnet_model dpdfnet_model;
typedef struct {
    uint32_t abi_version;
    const char *name;
    const char *weights_sha256;
    uint32_t sample_rate;
    uint32_t hop_samples;
    size_t spectrum_floats;
    size_t state_floats;
    size_t weight_floats;
} dpdfnet_model_info;

/* Metadata has static lifetime. Unknown IDs return NULL. */
DPDFNET_API const dpdfnet_model_info *dpdfnet_get_model_info(dpdfnet_model_id id);
DPDFNET_API int dpdfnet_cpu_supported(void); /* AVX2 and FMA, including OS support. */
/* Weights are copied. Validate their SHA-256 against metadata before creation.
 * Only the optimized W7A8 configuration is exposed. NULL means failure. */
DPDFNET_API dpdfnet_model *dpdfnet_create(dpdfnet_model_id id,
                                         const float *weights, size_t count);
/* Reset state using model normalization seeds, not all-zero initialization.
 * Return 0 on success, -1 for invalid ID or NULL state. */
DPDFNET_API int dpdfnet_init_state(dpdfnet_model_id id, float *state);
/* One 10 ms spectral hop: 962 floats [481,2], interleaved real/imag.
 * State length comes from metadata. Caller supplies finite, correctly sized
 * buffers. Spectrum and state may each be updated in place; other overlaps
 * are invalid. No allocation. One context per simultaneous call.
 * Return 0 on success, -1 for NULL arguments. */
DPDFNET_API int dpdfnet_process(dpdfnet_model *model, const float *spectrum,
                               const float *state, float *output, float *next_state);
DPDFNET_API size_t dpdfnet_owned_bytes(const dpdfnet_model *model);
DPDFNET_API void dpdfnet_destroy(dpdfnet_model *model); /* NULL is allowed. */
#ifdef __cplusplus
}
#endif
#endif
