#ifndef DPDF_DPRNN_H
#define DPDF_DPRNN_H
#include <stddef.h>
#if defined(_WIN32)
#define DPDF_API __declspec(dllexport)
#else
#define DPDF_API __attribute__((visibility("default")))
#endif
#ifdef __cplusplus
extern "C" {
#endif

/* Prototype ABI v1. Exactly H=64, F=40 or 48, FP32; not a general RNN API.
 * Weights: input-major matrices, GRU gate order r,z,n, reset after hidden affine.
 * create copies weights; process allocates nothing and keeps no mutable state.
 * State is caller-owned [F,64]. Different streams may share an immutable block.
 * The caller must initialize each stream's temporal state to zero.
 * Each process call uses approximately 134 KiB of stack scratch.
 * Finite model features/weights required. No fast-math or quantization.
 */
enum { DPDF_H = 64, DPDF_WEIGHT_FLOATS = 87552 };
enum { DPDF_AUTO = 0, DPDF_SCALAR = 1, DPDF_AVX2 = 2 };
typedef struct dpdf_block dpdf_block;
DPDF_API int dpdf_has_avx2(void);
/* Explicit opt-in experiment; AUTO always retains FP32 weights. */
DPDF_API int dpdf_has_fp16(void);
DPDF_API size_t dpdf_block_bytes(const dpdf_block *block);
DPDF_API dpdf_block *dpdf_create_fp16(int freq, const float *weights, size_t count,
                                     float intra_epsilon, float inter_epsilon);
DPDF_API dpdf_block *dpdf_create_int8(int freq, const float *weights, size_t count,
                                     float intra_epsilon, float inter_epsilon);
DPDF_API dpdf_block *dpdf_create(int freq, const float *weights, size_t count,
                               float intra_epsilon, float inter_epsilon, int tier);
DPDF_API void dpdf_destroy(dpdf_block *block);
DPDF_API const char *dpdf_tier(const dpdf_block *block);
/* x,y: contiguous channel-major [64,F]; state_in/out: frequency-major [F,64].
 * x==y and state_in==state_out are supported; other buffer overlap is invalid.
 * Returns 0 on success, -1 for null arguments. Sizes are fixed at create time.
 */
DPDF_API int dpdf_process(const dpdf_block *block, const float *x,
                          const float *state_in, float *y, float *state_out);
/* Internal-layout entry point used by generated chains. Frequency-major flags
 * avoid round-trip transposes between adjacent DPRNN blocks; flag 1 describes
 * x and flag 2 requests y. The public channel-major dpdf_process ABI is kept. */
enum { DPDF_INPUT_FREQ_MAJOR = 1, DPDF_OUTPUT_FREQ_MAJOR = 2 };
DPDF_API int dpdf_process_layout(const dpdf_block *block, const float *x,
                                 const float *state_in, float *y, float *state_out,
                                 unsigned layout_flags);
/* Direct activation probe for numerical validation of the SIMD approximation. */
DPDF_API int dpdf_test_gates(int tier, const float *input, float *sigmoid,
                             float *tanh_out, size_t count);
#ifdef __cplusplus
}
#endif
#endif
