#include "backend.h"
#include <stdlib.h>
struct dpdfnet_model {
    const dpdfnet_backend *backend;
    void *context;
};
static const dpdfnet_backend *select_backend(dpdfnet_model_id id) {
    switch (id) {
        case DPDFNET_MODEL_2: return dpdfnet_backend2();
        case DPDFNET_MODEL_8: return dpdfnet_backend8();
        default: return NULL;
    }
}
const dpdfnet_model_info *dpdfnet_get_model_info(dpdfnet_model_id id) {
    const dpdfnet_backend *b = select_backend(id);
    return b ? &b->info : NULL;
}
int dpdfnet_cpu_supported(void) { return dpdfnet_backend2()->cpu_supported(); }
dpdfnet_model *dpdfnet_create(dpdfnet_model_id id, const float *weights, size_t count) {
    const dpdfnet_backend *b = select_backend(id);
    if (!b || !weights || count != b->info.weight_floats || !b->cpu_supported()) return NULL;
    dpdfnet_model *m = malloc(sizeof(*m));
    if (!m) return NULL;
    m->backend = b;
    m->context = b->create(weights, count);
    if (!m->context) { free(m); return NULL; }
    return m;
}
int dpdfnet_init_state(dpdfnet_model_id id, float *state) {
    const dpdfnet_backend *b = select_backend(id);
    return b ? b->init_state(state) : -1;
}
int dpdfnet_process(dpdfnet_model *m, const float *spectrum, const float *state,
                    float *output, float *next_state) {
    if (!m || !spectrum || !state || !output || !next_state) return -1;
    return m->backend->process(m->context, spectrum, state, output, next_state);
}
size_t dpdfnet_owned_bytes(const dpdfnet_model *m) {
    return m ? sizeof(*m) + m->backend->owned_bytes(m->context) : 0;
}
void dpdfnet_destroy(dpdfnet_model *m) {
    if (!m) return;
    m->backend->destroy(m->context);
    free(m);
}
