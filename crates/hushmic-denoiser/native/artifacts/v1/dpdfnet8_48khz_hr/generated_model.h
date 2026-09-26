#ifndef DPDF_GENERATED_dpdfnet8_48khz_hr_H
#define DPDF_GENERATED_dpdfnet8_48khz_hr_H
#include "dpdf_dprnn.h"
#include "native_api.h"
#ifdef __cplusplus
extern "C" {
#endif
/* Generated fixed-shape spectral model. No ONNX dependency. One mutable
 * context/arena per concurrent call; caller owns the model-sized stream state.
 * Caller verifies the exported weight SHA-256 before calling create.
 * Spec layout: [1,1,481,2], interleaved real/imag, exactly 962 float32 values.
 * All arrays must be finite. State starts with exported normalization seeds.
 * Spectrum and state may each be updated in place; other overlap is invalid.
 * No allocation during process. AUTO never selects reduced precision.
 */
typedef struct dpdfnet8_48khz_hr dpdfnet8_48khz_hr;
#ifndef DPDF_EXPERIMENTAL_TIERS_DEFINED
#define DPDF_EXPERIMENTAL_TIERS_DEFINED
enum { DPDF_EXPERIMENTAL_FP16 = 3 }; /* DPRNN matrices only; never selected by AUTO */
enum { DPDF_EXPERIMENTAL_INT8 = 4 }; /* W8A8 DPRNN matrices; FP32 state/norms */
#endif
DPDF_API const char *dpdfnet8_48khz_hr_weights_sha256(void);
DPDF_API size_t dpdfnet8_48khz_hr_weight_count(void);
DPDF_API size_t dpdfnet8_48khz_hr_arena_bytes(void);
DPDF_API size_t dpdfnet8_48khz_hr_state_size(void);
/* Initialize/reset exactly dpdfnet8_48khz_hr_state_size() floats. */
DPDF_API int dpdfnet8_48khz_hr_init_state(float *state);
DPDF_API dpdfnet8_48khz_hr *dpdfnet8_48khz_hr_create(const float *weights, size_t count, int tier);
/* Extended generated target only: precision 0/16/8, family mask:
 * 1=dense GRU/FC, 2=grouped FC, 4=1x1 CNN, 8=other CNN. */
DPDF_API dpdfnet8_48khz_hr *dpdfnet8_48khz_hr_create_config(const float *,size_t,int,int,unsigned);
DPDF_API size_t dpdfnet8_48khz_hr_owned_bytes(const dpdfnet8_48khz_hr *);
/* Extended generated target only. NULL for an unsupported ABI version. */
DPDF_API const dpdf_native_api_v1 *dpdfnet8_48khz_hr_get_api(uint32_t abi_version);
DPDF_API void dpdfnet8_48khz_hr_destroy(dpdfnet8_48khz_hr *model);
DPDF_API int dpdfnet8_48khz_hr_process(dpdfnet8_48khz_hr *model, const float *spec, const float *state_in,
                                 float *spec_out, float *state_out);
#ifdef __cplusplus
}
#endif
#endif
