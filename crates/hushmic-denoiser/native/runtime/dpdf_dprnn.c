#include "internal.h"
#include <math.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>

struct dpdf_block {
    int freq, tier;
    float eps[2];
    dpdf_affine_fn affine;
    dpdf_gates_fn gates;
    dpdf_norm_fn norm;
    void *allocation;
    uint16_t *w16;
#ifdef DPDF_X86_DISPATCH
    dpdf_qmatrix *q[8];
#endif
    void *weight_allocation;
    float *w;
    float params[1536]; /* Biases and normalization remain FP32 in every mode. */
};
static void norm_residual(float *,const float *,const float *,const float *,const float *,float,int);

int dpdf_has_avx2(void) {
#ifdef DPDF_X86_DISPATCH
    /* GCC/Clang builtins include OS XSAVE support in AVX feature detection. */
    __builtin_cpu_init();
    return !!(__builtin_cpu_supports("avx2") && __builtin_cpu_supports("fma"));
#else
    return 0;
#endif
}
int dpdf_has_fp16(void) {
#ifdef DPDF_X86_DISPATCH
    return dpdf_has_avx2() && __builtin_cpu_supports("f16c");
#else
    return 0;
#endif
}

dpdf_block *dpdf_create(int f, const float *w, size_t count, float e0, float e1, int tier) {
    if ((f != 40 && f != 48) || !w || count != DPDF_WEIGHT_FLOATS ||
        !isfinite(e0) || !isfinite(e1) || e0 <= 0 || e1 <= 0 || tier < 0 || tier > 2)
        return NULL;
    if (tier == DPDF_AUTO) tier = dpdf_has_avx2() ? DPDF_AVX2 : DPDF_SCALAR;
    if (tier == DPDF_AVX2 && !dpdf_has_avx2()) return NULL;
    for (size_t i = 0; i < count; ++i) if (!isfinite(w[i])) return NULL;
    void *allocation=malloc(sizeof(dpdf_block)+31);
    if (!allocation) return NULL;
    dpdf_block *b=(dpdf_block *)(((uintptr_t)allocation+31)&~(uintptr_t)31);
    b->allocation=allocation;
    b->weight_allocation=malloc(count*sizeof(float)+31);
    if (!b->weight_allocation) { free(allocation); return NULL; }
    b->w=(float *)(((uintptr_t)b->weight_allocation+31)&~(uintptr_t)31);
    b->w16=NULL;
#ifdef DPDF_X86_DISPATCH
    memset(b->q,0,sizeof(b->q));
#endif
    b->freq = f; b->tier = tier; b->eps[0] = e0; b->eps[1] = e1;
    b->affine = dpdf_affine_scalar; b->gates = dpdf_gates_scalar;
    b->norm = norm_residual;
#ifdef DPDF_X86_DISPATCH
    if (tier == DPDF_AVX2) { b->affine = dpdf_affine_avx2; b->gates = dpdf_gates_avx2; b->norm = dpdf_norm_residual_avx2; }
#endif
    memcpy(b->w, w, count * sizeof(float));
    memcpy(b->params,w+49152,768*sizeof(float));
    memcpy(b->params+768,w+58112,192*sizeof(float));
    memcpy(b->params+960,w+82880,384*sizeof(float));
    memcpy(b->params+1344,w+87360,192*sizeof(float));
    return b;
}

dpdf_block *dpdf_create_fp16(int f,const float *w,size_t count,float e0,float e1) {
#ifdef DPDF_X86_DISPATCH
    if (!dpdf_has_fp16()) return NULL;
    dpdf_block *b=dpdf_create(f,w,count,e0,e1,DPDF_AVX2);
    if (!b) return NULL;
    for (size_t i=0;i<count;++i) if (fabsf(w[i])>65504.0f) { dpdf_destroy(b); return NULL; }
    b->w16=malloc(count*sizeof(uint16_t));
    if (!b->w16) { dpdf_destroy(b); return NULL; }
    dpdf_pack_fp16(w,b->w16,count);
    free(b->weight_allocation); b->weight_allocation=NULL; b->w=NULL;
    return b;
#else
    (void)f; (void)w; (void)count; (void)e0; (void)e1;
    return NULL;
#endif
}

/* Matrix offsets in the public packed FP32 block layout. */
#ifdef DPDF_X86_DISPATCH
static const int matrix_offset[8]={0,12288,24576,36864,49920,58304,70592,83264};
#endif
dpdf_block *dpdf_create_int8(int f,const float *w,size_t count,float e0,float e1) {
#ifdef DPDF_X86_DISPATCH
    dpdf_block *b=dpdf_create(f,w,count,e0,e1,DPDF_AVX2);
    if (!b) return NULL;
    for (int i=0;i<8;++i) {
        int k=i==4?128:64,n=(i==4 || i==7)?64:192;
        b->q[i]=dpdf_qcreate(w+matrix_offset[i],k,n);
        if (!b->q[i]) { dpdf_destroy(b); return NULL; }
    }
    free(b->weight_allocation); b->weight_allocation=NULL; b->w=NULL;
    return b;
#else
    (void)f; (void)w; (void)count; (void)e0; (void)e1;
    return NULL;
#endif
}
void dpdf_destroy(dpdf_block *b) {
    if (!b) return;
#ifdef DPDF_X86_DISPATCH
    for (int i=0;i<8;++i) dpdf_qdestroy(b->q[i]);
#endif
    free(b->w16); free(b->weight_allocation); free(b->allocation);
}
size_t dpdf_block_bytes(const dpdf_block *b) {
    if (!b) return 0;
    size_t bytes=sizeof(*b)+31+(b->w16?DPDF_WEIGHT_FLOATS*sizeof(uint16_t):0);
    if (b->weight_allocation) bytes+=DPDF_WEIGHT_FLOATS*sizeof(float)+31;
#ifdef DPDF_X86_DISPATCH
    for (int i=0;i<8;++i) bytes+=dpdf_qbytes(b->q[i]);
#endif
    return bytes;
}
const char *dpdf_tier(const dpdf_block *b) {
#ifdef DPDF_X86_DISPATCH
    if (b && b->q[0]) return "experimental-int8-w8a8-fp32-state";
#endif
    return !b ? "invalid" : b->w16 ? "experimental-fp16-weights-fp32-compute" : b->tier == DPDF_AVX2 ? "avx2-fma-fp32" : "scalar-fp32";
}

static inline void affine(const dpdf_block *b,const float *x,int offset,const float *bias,
                           float *y,int m,int k,int n) {
#ifdef DPDF_X86_DISPATCH
    if (b->q[0]) {
        for (int i=0;i<8;++i) if (offset==matrix_offset[i]) { dpdf_qaffine(b->q[i],x,bias,y,m); return; }
    }
    if (b->w16) { dpdf_affine_fp16(x,b->w16+offset,bias,y,m,k,n); return; }
#endif
    b->affine(x,b->w+offset,bias,y,m,k,n);
}

void dpdf_affine_scalar(const float *x, const float *w, const float *bias,
                        float *y, int m, int k, int n) {
    for (int r = 0; r < m; ++r) {
        for (int c = 0; c < n; ++c) y[r*n+c] = bias[c];
        for (int j = 0; j < k; ++j)
            for (int c = 0; c < n; ++c) y[r*n+c] += x[r*k+j] * w[j*n+c];
    }
}

static float sigmoid(float x) { return 1.0f / (1.0f + expf(-x)); }
void dpdf_gates_scalar(const float *a, const float *b, const float *old, float *out, int rows) {
    for (int r = 0; r < rows; ++r)
        for (int c = 0; c < 64; ++c) {
            float reset = sigmoid(a[r*192+c] + b[r*192+c]);
            float update = sigmoid(a[r*192+64+c] + b[r*192+64+c]);
            float candidate = tanhf(a[r*192+128+c] + reset*b[r*192+128+c]);
            out[r*64+c] = candidate + update * (old[r*64+c] - candidate);
        }
}

/* Normalize over channels, then residual. No residual moved across LayerNorm. */
static void norm_residual(float *out, const float *p, const float *skip,
                           const float *scale, const float *bias, float eps, int f) {
    for (int r = 0; r < f; ++r) {
        double mean = 0, var = 0;
        for (int c = 0; c < 64; ++c) mean += p[r*64+c];
        mean /= 64;
        for (int c = 0; c < 64; ++c) { double d = p[r*64+c] - mean; var += d*d; }
        float inv = (float)(1.0 / sqrt(var / 64 + eps));
        for (int c = 0; c < 64; ++c)
            out[r*64+c] = ((p[r*64+c] - (float)mean) * inv) * scale[c] + bias[c] + skip[r*64+c];
    }
}

int dpdf_process_layout(const dpdf_block *b, const float *input, const float *state,
                        float *output, float *state_out, unsigned layout_flags) {
    if (!b || !input || !state || !output || !state_out || layout_flags > 3) return -1;
    const int f = b->freq;
    float x[48*64], bi[48*128], mid[48*64], a[48*192], hproj[48*192], p[48*64];
    float h[64], step[192];
    const float *bias=b->params,*fib=b->params+768,*nis=fib+64,*nib=nis+64;
    const float *tib=b->params+960,*thb=tib+192,*fob=b->params+1344,*nos=fob+64,*nob=nos+64;
    if (layout_flags & DPDF_INPUT_FREQ_MAJOR)
        memcpy(x,input,(size_t)f*64*sizeof(float));
    else
        for (int r = 0; r < f; ++r)
            for (int c = 0; c < 64; ++c) x[r*64+c] = input[c*f+r];
    float *intra[2]={a,hproj};
#if defined(DPDF_X86_DISPATCH) && !defined(DPDF_DISABLE_QAFFINE_PAIR)
    if (b->q[0]) dpdf_qaffine_pair(b->q[0],b->q[1],x,bias,bias+384,a,hproj,f);
#endif
    for (int direction = 0; direction < 2; ++direction) {
        memset(h, 0, sizeof(h));
#if defined(DPDF_X86_DISPATCH) && !defined(DPDF_DISABLE_QAFFINE_PAIR)
        if (!b->q[0])
#endif
            affine(b, x, direction*64*192, bias+direction*384, intra[direction], f, 64, 192);
        for (int t = 0; t < f; ++t) {
            int r = direction == 0 ? t : f-1-t;
            affine(b, h, 24576+direction*64*192, bias+direction*384+192, step, 1, 64, 192);
            b->gates(intra[direction]+r*192, step, h, h, 1);
            memcpy(bi+r*128+direction*64, h, sizeof(h));
        }
    }
    affine(b, bi, 49920, fib, p, f, 128, 64);
    b->norm(mid, p, x, nis, nib, b->eps[0], f);
    affine(b, mid, 58304, tib, a, f, 64, 192);
    affine(b, state, 70592, thb, hproj, f, 64, 192);
    b->gates(a, hproj, state, state_out, f);
    affine(b, state_out, 83264, fob, p, f, 64, 64);
    b->norm(x, p, mid, nos, nob, b->eps[1], f);
    if (layout_flags & DPDF_OUTPUT_FREQ_MAJOR)
        memcpy(output,x,(size_t)f*64*sizeof(float));
    else
        for (int r = 0; r < f; ++r)
            for (int c = 0; c < 64; ++c) output[c*f+r] = x[r*64+c];
    return 0;
}

int dpdf_process(const dpdf_block *b, const float *input, const float *state,
                 float *output, float *state_out) {
    return dpdf_process_layout(b,input,state,output,state_out,0);
}

int dpdf_test_gates(int tier, const float *x, float *s, float *t, size_t count) {
    if (!x || !s || !t || tier < 0 || tier > 2) return -1;
    if (tier == DPDF_AUTO) tier = dpdf_has_avx2() ? DPDF_AVX2 : DPDF_SCALAR;
#ifdef DPDF_X86_DISPATCH
    if (tier == DPDF_AVX2) {
        if (!dpdf_has_avx2()) return -1;
        dpdf_activations_avx2(x, s, t, count); return 0;
    }
#endif
    if (tier != DPDF_SCALAR) return -1;
    for (size_t i = 0; i < count; ++i) { s[i] = sigmoid(x[i]); t[i] = tanhf(x[i]); }
    return 0;
}
