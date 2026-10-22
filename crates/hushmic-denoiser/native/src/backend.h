#ifndef DPDFNET_BACKEND_H
#define DPDFNET_BACKEND_H
#include "dpdfnet.h"
typedef struct {
    dpdfnet_model_info info;
    int (*cpu_supported)(void);
    void *(*create)(const float *, size_t);
    int (*init_state)(float *);
    int (*process)(void *, const float *, const float *, float *, float *);
    size_t (*owned_bytes)(const void *);
    void (*destroy)(void *);
} dpdfnet_backend;
const dpdfnet_backend *dpdfnet_backend2(void);
const dpdfnet_backend *dpdfnet_backend8(void);
#endif
