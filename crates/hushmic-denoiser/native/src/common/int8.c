/* Final W7A8 packing and one-time dispatch for the quantized kernels. */
#include "internal.h"
#include <immintrin.h>
#include <math.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>

#include "int8_matrix.h"
void dpdf_qdestroy(dpdf_qmatrix *q) {
    if (q) { free(q->packed_allocation); free(q->scale); free(q->sum); free(q); }
}
dpdf_qmatrix *dpdf_qcreate(const float *w,int k,int n) {
    if (!w || k<=0 || k>512 || k%8 || n<=0 || n%8) return NULL;
    dpdf_qmatrix *q=calloc(1,sizeof(*q)); if (!q) return NULL;
    q->k=k; q->n=n;
    q->affine=dpdf_qaffine_avx2; q->pair=dpdf_qaffine_pair_avx2;
#ifdef DPDF_HAS_AVX_VNNI
    if (dpdf_has_avx_vnni()) {
        q->affine=dpdf_qaffine_vnni; q->pair=dpdf_qaffine_pair_vnni;
    }
#endif
    q->packed_allocation=malloc((size_t)k*n+63);
    if (q->packed_allocation) q->packed=(int8_t *)(((uintptr_t)q->packed_allocation+63)&~(uintptr_t)63);
    q->scale=malloc(n*sizeof(float)); q->sum=calloc(n,sizeof(int32_t));
    if (!q->packed || !q->scale || !q->sum) { dpdf_qdestroy(q); return NULL; }
    for (int c=0;c<n;++c) {
        float maximum=0;
        for (int j=0;j<k;++j) { if (!isfinite(w[j*n+c])) { dpdf_qdestroy(q); return NULL; } maximum=fmaxf(maximum,fabsf(w[j*n+c])); }
        float scale=maximum>0 ? maximum/63.0f : 1.0f; q->scale[c]=scale;
        for (int j=0;j<k;++j) {
            int v=(int)nearbyintf(w[j*n+c]/scale);
            if (v>63) v=63;
            if (v< -63) v= -63;
            /* Each vector contains four K entries for each of eight outputs. */
            q->packed[(c/8)*k*8+(j/4)*32+(c%8)*4+j%4]=(int8_t)v;
            q->sum[c]+=v;
        }
    }
    return q;
}
size_t dpdf_qbytes(const dpdf_qmatrix *q) {
    return q ? sizeof(*q)+(size_t)q->k*q->n+63+(size_t)q->n*8 : 0;
}

/* Select at matrix creation, never inside a dot-product loop. */
int dpdf_has_avx_vnni(void) {
#ifdef DPDF_HAS_AVX_VNNI
    __builtin_cpu_init();
    return !!__builtin_cpu_supports("avxvnni");
#else
    return 0;
#endif
}

void dpdf_qaffine(const dpdf_qmatrix *q,const float *x,const float *bias,float *y,int m) {
    q->affine(q,x,bias,y,m);
}
void dpdf_qaffine_pair(const dpdf_qmatrix *q0,const dpdf_qmatrix *q1,const float *x,
                       const float *b0,const float *b1,float *y0,float *y1,int m) {
    q0->pair(q0,q1,x,b0,b1,y0,y1,m);
}

#define DPDF_QAFFINE_IMPL dpdf_qaffine_avx2
#define DPDF_QPAIR_IMPL dpdf_qaffine_pair_avx2
#include "int8_kernels.inc"
