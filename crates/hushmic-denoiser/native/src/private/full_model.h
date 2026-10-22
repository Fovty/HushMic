#ifndef DPDF_FULL_MODEL_H
#define DPDF_FULL_MODEL_H
#include "dpdf_dprnn.h"
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
typedef struct dpdf_model dpdf_model;
#ifndef DPDF_EXPERIMENTAL_TIERS_DEFINED
#define DPDF_EXPERIMENTAL_TIERS_DEFINED
enum { DPDF_EXPERIMENTAL_FP16 = 3 }; /* DPRNN matrices only; never selected by AUTO */
enum { DPDF_EXPERIMENTAL_INT8 = 4 }; /* W8A8 DPRNN matrices; FP32 state/norms */
#endif
DPDF_API const char *dpdf_model_weights_sha256(void);
DPDF_API size_t dpdf_model_weight_count(void);
DPDF_API size_t dpdf_model_arena_bytes(void);
DPDF_API size_t dpdf_model_state_size(void);
/* Initialize/reset exactly dpdf_model_state_size() floats. */
DPDF_API int dpdf_model_init_state(float *state);
DPDF_API dpdf_model *dpdf_model_create(const float *weights, size_t count, int tier);
/* Extended generated target only: precision 0/16/8, family mask:
 * 1=dense GRU/FC, 2=grouped FC, 4=1x1 CNN, 8=other CNN. */
DPDF_API dpdf_model *dpdf_model_create_config(const float *,size_t,int,int,unsigned);
DPDF_API size_t dpdf_model_owned_bytes(const dpdf_model *);
DPDF_API void dpdf_model_destroy(dpdf_model *model);
DPDF_API int dpdf_model_process(dpdf_model *model, const float *spec, const float *state_in,
                                 float *spec_out, float *state_out);
#ifdef __cplusplus
}
#endif
#endif
