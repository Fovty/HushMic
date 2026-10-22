#include "backend.h"
#include "full_model.h"
#ifndef DPDF_MODEL_SIZE
#error DPDF_MODEL_SIZE must identify the compiled graph.
#endif
static void *create(const float *weights, size_t count) {
    return dpdf_model_create_config(weights, count, DPDF_EXPERIMENTAL_INT8, 8, 7);
}
static int process(void *m, const float *x, const float *s, float *y, float *t) {
    return dpdf_model_process(m, x, s, y, t);
}
static size_t owned_bytes(const void *m) { return dpdf_model_owned_bytes(m); }
static void destroy(void *m) { dpdf_model_destroy(m); }
#if DPDF_MODEL_SIZE == 2
#define BACKEND_FUNCTION dpdfnet_backend2
#define MODEL_NAME "dpdfnet2_48khz_hr"
#define WEIGHT_HASH "cb7248b8fccbff7b32f254ec2ed0061e604514b2ccd98d997cd081a876424e7b"
#define WEIGHT_COUNT 2582444
#define STATE_COUNT 56436
#elif DPDF_MODEL_SIZE == 8
#define BACKEND_FUNCTION dpdfnet_backend8
#define MODEL_NAME "dpdfnet8_48khz_hr"
#define WEIGHT_HASH "5a5bb67a8619090c54dc2793536fe813fe38123b236d445f3433aafe3075529d"
#define WEIGHT_COUNT 3633068
#define STATE_COUNT 90228
#else
#error Unsupported model size.
#endif
const dpdfnet_backend *BACKEND_FUNCTION(void) {
    static const dpdfnet_backend backend = {
        {DPDFNET_ABI_VERSION, MODEL_NAME, WEIGHT_HASH, 48000, 480, 962, STATE_COUNT, WEIGHT_COUNT},
        dpdf_has_avx2, create, dpdf_model_init_state, process, owned_bytes, destroy
    };
    return &backend;
}
