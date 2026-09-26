/* Original AVX2 W8A8 experiment. Weights: per-output symmetric [-127,127].
 * Activations: per-row asymmetric 254-step range, centered at -127.
 * No static calibration, saturating pair sums, or quantized recurrent state.
 */
#include "internal.h"
#include <immintrin.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>

struct dpdf_qmatrix {
    int k,n;
    int8_t *packed;
    float *scale;
    int32_t *sum;
};
void dpdf_qdestroy(dpdf_qmatrix *q) {
    if (q) { free(q->packed); free(q->scale); free(q->sum); free(q); }
}
dpdf_qmatrix *dpdf_qcreate(const float *w,int k,int n) {
    if (!w || k<=0 || k>512 || k%8 || n<=0 || n%8) return NULL;
    dpdf_qmatrix *q=calloc(1,sizeof(*q)); if (!q) return NULL;
    q->k=k; q->n=n;
    q->packed=malloc((size_t)k*n); q->scale=malloc(n*sizeof(float)); q->sum=calloc(n,sizeof(int32_t));
    if (!q->packed || !q->scale || !q->sum) { dpdf_qdestroy(q); return NULL; }
    for (int c=0;c<n;++c) {
        float maximum=0;
        for (int j=0;j<k;++j) { if (!isfinite(w[j*n+c])) { dpdf_qdestroy(q); return NULL; } maximum=fmaxf(maximum,fabsf(w[j*n+c])); }
        float scale=maximum>0 ? maximum/127.0f : 1.0f; q->scale[c]=scale;
        for (int j=0;j<k;++j) {
            int v=(int)nearbyintf(w[j*n+c]/scale);
            if (v>127) v=127;
            if (v< -127) v= -127;
            /* Each vector contains four K entries for each of eight outputs. */
            q->packed[(c/8)*k*8+(j/4)*32+(c%8)*4+j%4]=(int8_t)v;
            q->sum[c]+=v;
        }
    }
    return q;
}
size_t dpdf_qbytes(const dpdf_qmatrix *q) {
    return q ? sizeof(*q)+(size_t)q->k*q->n+(size_t)q->n*8 : 0;
}

static void quantize(const float *x,int8_t *out,int k,float *scale,int *zp) {
    __m256 lo=_mm256_setzero_ps(),hi=lo;
    for (int j=0;j<k;j+=8) { __m256 v=_mm256_loadu_ps(x+j); lo=_mm256_min_ps(lo,v); hi=_mm256_max_ps(hi,v); }
    /* Reduce in registers. The scalar reference starts at zero and ignores
     * unordered comparisons; a zero second operand preserves that behavior. */
    lo=_mm256_min_ps(lo,_mm256_setzero_ps());
    hi=_mm256_max_ps(hi,_mm256_setzero_ps());
    __m128 low4=_mm_min_ps(_mm256_castps256_ps128(lo),_mm256_extractf128_ps(lo,1));
    __m128 high4=_mm_max_ps(_mm256_castps256_ps128(hi),_mm256_extractf128_ps(hi,1));
    low4=_mm_min_ps(low4,_mm_movehl_ps(low4,low4));
    high4=_mm_max_ps(high4,_mm_movehl_ps(high4,high4));
    float low=_mm_cvtss_f32(_mm_min_ss(low4,_mm_shuffle_ps(low4,low4,0x55)));
    float high=_mm_cvtss_f32(_mm_max_ss(high4,_mm_shuffle_ps(high4,high4,0x55)));
    if (low==high) { memset(out,0,k); *scale=1; *zp=127; return; }
    *scale=(high-low)/254.0f;
    *zp=(int)nearbyintf(-low / *scale);
    if (*zp<0) *zp=0;
    if (*zp>254) *zp=254;
    __m256 inv=_mm256_set1_ps(1.0f / *scale),z=_mm256_set1_ps((float)*zp);
    for (int j=0;j<k;j+=8) {
        __m256 v=_mm256_add_ps(_mm256_mul_ps(_mm256_loadu_ps(x+j),inv),z);
        __m256i q=_mm256_cvtps_epi32(v);
        q=_mm256_min_epi32(_mm256_set1_epi32(254),_mm256_max_epi32(_mm256_setzero_si256(),q));
        q=_mm256_sub_epi32(q,_mm256_set1_epi32(127));
        __m128i p=_mm_packs_epi32(_mm256_castsi256_si128(q),_mm256_extracti128_si256(q,1));
        p=_mm_packs_epi16(p,p); _mm_storel_epi64((__m128i *)(out+j),p);
    }
}

#ifndef DPDF_DISABLE_QAFFINE_ROW
/* Keep integer reduction separate from quantization/dequantization: inlining
 * their live temporaries caused GCC 12 to spill two accumulators every K step.
 * Short weight lifetimes let all eight accumulators stay in registers. The
 * noinline boundary is intentional; this still uses only the existing AVX2 ISA.
 * Inputs/weights exclude -128, so pair sums fit i16 (2*127*127=32258).
 * K<=512 also keeps the full dot product safely within i32. */
static __attribute__((noinline)) void qdot64(const int8_t *activation,
        const int8_t *packed,int k,int32_t *out) {
    const __m256i ones=_mm256_set1_epi16(1);
    __m256i sum0=_mm256_setzero_si256(),sum1=sum0,sum2=sum0,sum3=sum0;
    __m256i sum4=sum0,sum5=sum0,sum6=sum0,sum7=sum0;
    for (int j=0;j<k;j+=4) {
        int32_t bytes; memcpy(&bytes,activation+j,4);
        __m256i a=_mm256_set1_epi32(bytes),absolute=_mm256_abs_epi8(a);
        const int8_t *base=packed+j*8;
        __m256i w0=_mm256_loadu_si256((const __m256i *)(base));
        sum0=_mm256_add_epi32(sum0,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w0,a)),ones));
        __m256i w1=_mm256_loadu_si256((const __m256i *)(base+k*8));
        sum1=_mm256_add_epi32(sum1,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w1,a)),ones));
        __m256i w2=_mm256_loadu_si256((const __m256i *)(base+2*k*8));
        sum2=_mm256_add_epi32(sum2,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w2,a)),ones));
        __m256i w3=_mm256_loadu_si256((const __m256i *)(base+3*k*8));
        sum3=_mm256_add_epi32(sum3,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w3,a)),ones));
        __m256i w4=_mm256_loadu_si256((const __m256i *)(base+4*k*8));
        sum4=_mm256_add_epi32(sum4,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w4,a)),ones));
        __m256i w5=_mm256_loadu_si256((const __m256i *)(base+5*k*8));
        sum5=_mm256_add_epi32(sum5,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w5,a)),ones));
        __m256i w6=_mm256_loadu_si256((const __m256i *)(base+6*k*8));
        sum6=_mm256_add_epi32(sum6,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w6,a)),ones));
        __m256i w7=_mm256_loadu_si256((const __m256i *)(base+7*k*8));
        sum7=_mm256_add_epi32(sum7,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w7,a)),ones));
    }
    _mm256_storeu_si256((__m256i *)(out+0),sum0);
    _mm256_storeu_si256((__m256i *)(out+8),sum1);
    _mm256_storeu_si256((__m256i *)(out+16),sum2);
    _mm256_storeu_si256((__m256i *)(out+24),sum3);
    _mm256_storeu_si256((__m256i *)(out+32),sum4);
    _mm256_storeu_si256((__m256i *)(out+40),sum5);
    _mm256_storeu_si256((__m256i *)(out+48),sum6);
    _mm256_storeu_si256((__m256i *)(out+56),sum7);
}

static void qaffine_row(const dpdf_qmatrix *q,const float *x,const float *bias,float *y) {
    int8_t activation[512]; float activation_scale; int zp;
    const int k=q->k,n=q->n;
    quantize(x,activation,k,&activation_scale,&zp);
    const __m256i ones=_mm256_set1_epi16(1);
    int c=0;
    for (;c+63<n;c+=64) {
        int32_t integer_sums[64];
        qdot64(activation,q->packed+c*k,k,integer_sums);
        __m256i sums[8];
        for (int t=0;t<8;++t) sums[t]=_mm256_loadu_si256((const __m256i *)(integer_sums+t*8));
        for (int t=0;t<8;++t) {
            int column=c+t*8;
            __m256i correction=_mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(q->sum+column)),_mm256_set1_epi32(127-zp));
            __m256 sum=_mm256_cvtepi32_ps(_mm256_add_epi32(sums[t],correction));
            __m256 scale=_mm256_mul_ps(_mm256_set1_ps(activation_scale),_mm256_loadu_ps(q->scale+column));
            _mm256_storeu_ps(y+column,_mm256_fmadd_ps(sum,scale,_mm256_loadu_ps(bias+column)));
        }
    }
    for (;c<n;c+=8) {
        __m256i sum=_mm256_setzero_si256();
        for (int j=0;j<k;j+=4) {
            int32_t bytes; memcpy(&bytes,activation+j,4);
            __m256i a=_mm256_set1_epi32(bytes);
            __m256i w=_mm256_loadu_si256((const __m256i *)(q->packed+c*k+j*8));
            __m256i pairs=_mm256_maddubs_epi16(_mm256_abs_epi8(a),_mm256_sign_epi8(w,a));
            sum=_mm256_add_epi32(sum,_mm256_madd_epi16(pairs,ones));
        }
        sum=_mm256_add_epi32(sum,_mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(q->sum+c)),_mm256_set1_epi32(127-zp)));
        __m256 scale=_mm256_mul_ps(_mm256_set1_ps(activation_scale),_mm256_loadu_ps(q->scale+c));
        _mm256_storeu_ps(y+c,_mm256_fmadd_ps(_mm256_cvtepi32_ps(sum),scale,_mm256_loadu_ps(bias+c)));
    }
}
#endif

/* Four rows and two independent 8-column tiles. Keeping the integer loop in
 * its own function leaves room for eight accumulators and shared weights;
 * scale/correction temporaries are only live after the reduction returns. */
static __attribute__((noinline)) void qdot4pair(const int8_t *activation,
        const int8_t *packed0,const int8_t *packed1,int k,__m256i *out) {
    const __m256i ones=_mm256_set1_epi16(1);
    __m256i a0=_mm256_setzero_si256(),a1=a0,a2=a0,a3=a0;
    __m256i b0=a0,b1=a0,b2=a0,b3=a0;
    for (int j=0;j<k;j+=4) {
        __m256i w0=_mm256_loadu_si256((const __m256i *)(packed0+j*8));
        __m256i w1=_mm256_loadu_si256((const __m256i *)(packed1+j*8));
#define DPDF_DOT_ROW(row,sa,sb) do { \
        int32_t bytes; memcpy(&bytes,activation+(row)*k+j,4); \
        __m256i v=_mm256_set1_epi32(bytes),absolute=_mm256_abs_epi8(v); \
        sa=_mm256_add_epi32(sa,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w0,v)),ones)); \
        sb=_mm256_add_epi32(sb,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w1,v)),ones)); \
    } while (0)
        DPDF_DOT_ROW(0,a0,b0); DPDF_DOT_ROW(1,a1,b1);
        DPDF_DOT_ROW(2,a2,b2); DPDF_DOT_ROW(3,a3,b3);
#undef DPDF_DOT_ROW
    }
    out[0]=a0;out[1]=a1;out[2]=a2;out[3]=a3;
    out[4]=b0;out[5]=b1;out[6]=b2;out[7]=b3;
}

static void qaffine_batch_tiled(const dpdf_qmatrix *q,const float *x,const float *bias,float *y,int m) {
    int8_t activation[48*512]; float scales[48]; int zp[48];
    const int k=q->k,n=q->n;
    for (int r=0;r<m;++r) quantize(x+r*k,activation+r*k,k,scales+r,zp+r);
    for (int r=0;r<m;r+=4) for (int c=0;c<n;c+=16) {
        __m256i sums[8];
        qdot4pair(activation+r*k,q->packed+c*k,q->packed+(c+8)*k,k,sums);
        for (int t=0;t<4;++t) for (int tile=0;tile<2;++tile) {
            int col=c+tile*8;
            __m256i correction=_mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(q->sum+col)),_mm256_set1_epi32(127-zp[r+t]));
            __m256 scale=_mm256_mul_ps(_mm256_set1_ps(scales[r+t]),_mm256_loadu_ps(q->scale+col));
            __m256 result=_mm256_fmadd_ps(_mm256_cvtepi32_ps(_mm256_add_epi32(sums[t+tile*4],correction)),scale,_mm256_loadu_ps(bias+col));
            _mm256_storeu_ps(y+(r+t)*n+col,result);
        }
    }
}

static void qaffine_batch(const dpdf_qmatrix *q,const float *x,const float *bias,float *y,int m) {
    if (m%4==0 && q->n%16==0) { qaffine_batch_tiled(q,x,bias,y,m); return; }
    /* Callers split larger batches into at most 48 rows. */
    int8_t activation[48*512]; float scales[48]; int zp[48];
    const int k=q->k,n=q->n;
    for (int r=0;r<m;++r) quantize(x+r*k,activation+r*k,k,scales+r,zp+r);
    const __m256i ones=_mm256_set1_epi16(1);
    int r=0;
    for (;r+3<m;r+=4) for (int c=0;c<n;c+=8) {
        __m256i s0=_mm256_setzero_si256(),s1=s0,s2=s0,s3=s0;
        for (int j=0;j<k;j+=4) {
            int32_t v0,v1,v2,v3;
            memcpy(&v0,activation+r*k+j,4); memcpy(&v1,activation+(r+1)*k+j,4);
            memcpy(&v2,activation+(r+2)*k+j,4); memcpy(&v3,activation+(r+3)*k+j,4);
            __m256i a0=_mm256_set1_epi32(v0),a1=_mm256_set1_epi32(v1);
            __m256i a2=_mm256_set1_epi32(v2),a3=_mm256_set1_epi32(v3);
            __m256i w=_mm256_loadu_si256((const __m256i *)(q->packed+c*k+j*8));
            s0=_mm256_add_epi32(s0,_mm256_madd_epi16(_mm256_maddubs_epi16(_mm256_abs_epi8(a0),_mm256_sign_epi8(w,a0)),ones));
            s1=_mm256_add_epi32(s1,_mm256_madd_epi16(_mm256_maddubs_epi16(_mm256_abs_epi8(a1),_mm256_sign_epi8(w,a1)),ones));
            s2=_mm256_add_epi32(s2,_mm256_madd_epi16(_mm256_maddubs_epi16(_mm256_abs_epi8(a2),_mm256_sign_epi8(w,a2)),ones));
            s3=_mm256_add_epi32(s3,_mm256_madd_epi16(_mm256_maddubs_epi16(_mm256_abs_epi8(a3),_mm256_sign_epi8(w,a3)),ones));
        }
        __m256i sums[4]={s0,s1,s2,s3};
        for (int t=0;t<4;++t) {
            __m256i correction=_mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(q->sum+c)),_mm256_set1_epi32(127-zp[r+t]));
            __m256 scale=_mm256_mul_ps(_mm256_set1_ps(scales[r+t]),_mm256_loadu_ps(q->scale+c));
            __m256 result=_mm256_fmadd_ps(_mm256_cvtepi32_ps(_mm256_add_epi32(sums[t],correction)),scale,_mm256_loadu_ps(bias+c));
            _mm256_storeu_ps(y+(r+t)*n+c,result);
        }
    }
    for (;r<m;++r) {
      int c=0;
      /* Recurrent GRU projections arrive here one row at a time. Walk K once
       * for eight output vectors so each activation load/broadcast is reused.
       * Each accumulator retains the original increasing-K sum order. */
      for (;c+63<n;c+=64) {
        __m256i sum0=_mm256_setzero_si256(),sum1=sum0,sum2=sum0,sum3=sum0;
        __m256i sum4=sum0,sum5=sum0,sum6=sum0,sum7=sum0;
        for (int j=0;j<k;j+=4) {
            int32_t bytes; memcpy(&bytes,activation+r*k+j,4);
            __m256i a=_mm256_set1_epi32(bytes);
            __m256i absolute=_mm256_abs_epi8(a);
            const int8_t *base=q->packed+(c/8)*k*8+j*8;
            __m256i w0=_mm256_loadu_si256((const __m256i *)(base));
            __m256i w1=_mm256_loadu_si256((const __m256i *)(base+k*8));
            __m256i w2=_mm256_loadu_si256((const __m256i *)(base+2*k*8));
            __m256i w3=_mm256_loadu_si256((const __m256i *)(base+3*k*8));
            __m256i w4=_mm256_loadu_si256((const __m256i *)(base+4*k*8));
            __m256i w5=_mm256_loadu_si256((const __m256i *)(base+5*k*8));
            __m256i w6=_mm256_loadu_si256((const __m256i *)(base+6*k*8));
            __m256i w7=_mm256_loadu_si256((const __m256i *)(base+7*k*8));
            /* |a|<=127, |w|<=127 => pair sums <=32258, no i16 saturation.
             * Neither operand contains -128, so sign transfer is exact. */
            sum0=_mm256_add_epi32(sum0,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w0,a)),ones));
            sum1=_mm256_add_epi32(sum1,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w1,a)),ones));
            sum2=_mm256_add_epi32(sum2,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w2,a)),ones));
            sum3=_mm256_add_epi32(sum3,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w3,a)),ones));
            sum4=_mm256_add_epi32(sum4,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w4,a)),ones));
            sum5=_mm256_add_epi32(sum5,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w5,a)),ones));
            sum6=_mm256_add_epi32(sum6,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w6,a)),ones));
            sum7=_mm256_add_epi32(sum7,_mm256_madd_epi16(_mm256_maddubs_epi16(absolute,_mm256_sign_epi8(w7,a)),ones));
        }
        __m256i sums[8]={sum0,sum1,sum2,sum3,sum4,sum5,sum6,sum7};
        for (int t=0;t<8;++t) {
            int column=c+t*8;
            __m256i correction=_mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(q->sum+column)),_mm256_set1_epi32(127-zp[r]));
            __m256 sum=_mm256_cvtepi32_ps(_mm256_add_epi32(sums[t],correction));
            __m256 scale=_mm256_mul_ps(_mm256_set1_ps(scales[r]),_mm256_loadu_ps(q->scale+column));
            _mm256_storeu_ps(y+r*n+column,_mm256_fmadd_ps(sum,scale,_mm256_loadu_ps(bias+column)));
        }
      }
      for (;c<n;c+=8) {
        __m256i sum=_mm256_setzero_si256();
        for (int j=0;j<k;j+=4) {
            int32_t bytes; memcpy(&bytes,activation+r*k+j,4);
            __m256i a=_mm256_set1_epi32(bytes);
            __m256i w=_mm256_loadu_si256((const __m256i *)(q->packed+c*k+j*8));
            __m256i pairs=_mm256_maddubs_epi16(_mm256_abs_epi8(a),_mm256_sign_epi8(w,a));
            sum=_mm256_add_epi32(sum,_mm256_madd_epi16(pairs,ones));
        }
        __m256i correction=_mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(q->sum+c)),_mm256_set1_epi32(127-zp[r]));
        sum=_mm256_add_epi32(sum,correction);
        __m256 scale=_mm256_mul_ps(_mm256_set1_ps(scales[r]),_mm256_loadu_ps(q->scale+c));
        __m256 result=_mm256_fmadd_ps(_mm256_cvtepi32_ps(sum),scale,_mm256_loadu_ps(bias+c));
        _mm256_storeu_ps(y+r*n+c,result);
      }
    }
}

void dpdf_qaffine(const dpdf_qmatrix *q,const float *x,const float *bias,float *y,int m) {
#ifndef DPDF_DISABLE_QAFFINE_ROW
    if (m==1) qaffine_row(q,x,bias,y);
    else qaffine_batch(q,x,bias,y,m);
#else
    qaffine_batch(q,x,bias,y,m);
#endif
}

/* The bidirectional intra-frequency GRU applies two matrices to exactly the
 * same rows. Quantize once and walk both packed matrices together, retaining
 * the original increasing-K accumulation order for every output. */
void dpdf_qaffine_pair(const dpdf_qmatrix *q0,const dpdf_qmatrix *q1,const float *x,
                       const float *bias0,const float *bias1,float *y0,float *y1,int m) {
    if (q0->k!=q1->k || q0->n!=q1->n || m%4) {
        dpdf_qaffine(q0,x,bias0,y0,m); dpdf_qaffine(q1,x,bias1,y1,m); return;
    }
    int8_t activation[48*512]; float scales[48]; int zp[48];
    const int k=q0->k,n=q0->n;
    for (int r=0;r<m;++r) quantize(x+r*k,activation+r*k,k,scales+r,zp+r);
    for (int r=0;r<m;r+=4) for (int c=0;c<n;c+=8) {
        __m256i sums[8];
        qdot4pair(activation+r*k,q0->packed+c*k,q1->packed+c*k,k,sums);
        const __m256i weight_sum0=_mm256_loadu_si256((const __m256i *)(q0->sum+c));
        const __m256i weight_sum1=_mm256_loadu_si256((const __m256i *)(q1->sum+c));
        const __m256 weight_scale0=_mm256_loadu_ps(q0->scale+c);
        const __m256 weight_scale1=_mm256_loadu_ps(q1->scale+c);
        for (int t=0;t<4;++t) {
            __m256i zero=_mm256_set1_epi32(127-zp[r+t]);
            __m256 scale=_mm256_set1_ps(scales[r+t]);
            __m256i corrected=_mm256_add_epi32(sums[t],_mm256_mullo_epi32(weight_sum0,zero));
            _mm256_storeu_ps(y0+(r+t)*n+c,_mm256_fmadd_ps(_mm256_cvtepi32_ps(corrected),
                             _mm256_mul_ps(scale,weight_scale0),_mm256_loadu_ps(bias0+c)));
            corrected=_mm256_add_epi32(sums[t+4],_mm256_mullo_epi32(weight_sum1,zero));
            _mm256_storeu_ps(y1+(r+t)*n+c,_mm256_fmadd_ps(_mm256_cvtepi32_ps(corrected),
                             _mm256_mul_ps(scale,weight_scale1),_mm256_loadu_ps(bias1+c)));
        }
    }
}
