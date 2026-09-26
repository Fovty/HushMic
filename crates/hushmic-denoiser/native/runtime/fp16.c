/* Experimental FP16 weights, FP32 activations/accumulators. F16C only here. */
#include "internal.h"
#include <immintrin.h>

void dpdf_pack_fp16(const float *w, uint16_t *out, size_t count) {
    size_t i=0;
    for (; i+7<count; i+=8)
        _mm_storeu_si128((__m128i *)(out+i),_mm256_cvtps_ph(_mm256_loadu_ps(w+i),_MM_FROUND_TO_NEAREST_INT|_MM_FROUND_NO_EXC));
    for (; i<count; ++i) out[i]=(uint16_t)_cvtss_sh(w[i],_MM_FROUND_TO_NEAREST_INT|_MM_FROUND_NO_EXC);
}
static inline __m256 load8(const uint16_t *p) {
    return _mm256_cvtph_ps(_mm_loadu_si128((const __m128i *)p));
}
/* Same accumulation order as FP32; supported DPRNN widths 64 and 192. */
void dpdf_affine_fp16(const float *x,const uint16_t *w,const float *bias,
                      float *y,int m,int k,int n) {
    int r=0;
    for (;r+3<m;r+=4) for (int c=0;c<n;c+=16) {
        __m256 a0=_mm256_loadu_ps(bias+c),a1=_mm256_loadu_ps(bias+c+8);
        __m256 b0=a0,b1=a1,c0=a0,c1=a1,d0=a0,d1=a1;
        for (int j=0;j<k;++j) {
            __m256 w0=load8(w+j*n+c),w1=load8(w+j*n+c+8);
            __m256 v=_mm256_set1_ps(x[r*k+j]);
            a0=_mm256_fmadd_ps(v,w0,a0); a1=_mm256_fmadd_ps(v,w1,a1);
            v=_mm256_set1_ps(x[(r+1)*k+j]);
            b0=_mm256_fmadd_ps(v,w0,b0); b1=_mm256_fmadd_ps(v,w1,b1);
            v=_mm256_set1_ps(x[(r+2)*k+j]);
            c0=_mm256_fmadd_ps(v,w0,c0); c1=_mm256_fmadd_ps(v,w1,c1);
            v=_mm256_set1_ps(x[(r+3)*k+j]);
            d0=_mm256_fmadd_ps(v,w0,d0); d1=_mm256_fmadd_ps(v,w1,d1);
        }
        _mm256_storeu_ps(y+r*n+c,a0); _mm256_storeu_ps(y+r*n+c+8,a1);
        _mm256_storeu_ps(y+(r+1)*n+c,b0); _mm256_storeu_ps(y+(r+1)*n+c+8,b1);
        _mm256_storeu_ps(y+(r+2)*n+c,c0); _mm256_storeu_ps(y+(r+2)*n+c+8,c1);
        _mm256_storeu_ps(y+(r+3)*n+c,d0); _mm256_storeu_ps(y+(r+3)*n+c+8,d1);
    }
    for (;r<m;++r) for (int c=0;c<n;c+=32) {
        __m256 a=_mm256_loadu_ps(bias+c),b=_mm256_loadu_ps(bias+c+8);
        __m256 d=_mm256_loadu_ps(bias+c+16),e=_mm256_loadu_ps(bias+c+24);
        for (int j=0;j<k;++j) {
            __m256 v=_mm256_set1_ps(x[r*k+j]);
            a=_mm256_fmadd_ps(v,load8(w+j*n+c),a);
            b=_mm256_fmadd_ps(v,load8(w+j*n+c+8),b);
            d=_mm256_fmadd_ps(v,load8(w+j*n+c+16),d);
            e=_mm256_fmadd_ps(v,load8(w+j*n+c+24),e);
        }
        _mm256_storeu_ps(y+r*n+c,a); _mm256_storeu_ps(y+r*n+c+8,b);
        _mm256_storeu_ps(y+r*n+c+16,d); _mm256_storeu_ps(y+r*n+c+24,e);
    }
}
