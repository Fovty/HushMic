/* Shared AVX2/VNNI kernels. Instantiated in two translation units so the
 * AVX2 path never contains instructions unavailable on an older CPU.
 * Quantization is unchanged: u8 activations <=254 and signed weights <=63.
 * Pair products fit i16, and K<=512 bounds the complete sum within i32. */
#ifdef DPDF_INT8_VNNI
#define DPDF_DOT4(sum,a,w) _mm256_dpbusd_avx_epi32(sum,a,w)
#include "int8_vnni_aliases.h"
#else
#define DPDF_DOT4(sum,a,w) _mm256_add_epi32(sum,_mm256_madd_epi16( \
    _mm256_maddubs_epi16(a,w),_mm256_set1_epi16(1)))
#endif

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
    if (low==high) { memset(out,0,k); *scale=1; *zp=0; return; }
    *scale=(high-low)/254.0f;
    *zp=(int)nearbyintf(-low / *scale);
    if (*zp<0) *zp=0;
    if (*zp>254) *zp=254;
    __m256 inv=_mm256_set1_ps(1.0f / *scale),z=_mm256_set1_ps((float)*zp);
    int j=0;
    for (;j+31<k;j+=32) {
        __m256i q[4];
        for (int t=0;t<4;++t) {
            __m256 v=_mm256_add_ps(_mm256_mul_ps(_mm256_loadu_ps(x+j+t*8),inv),z);
            q[t]=_mm256_cvtps_epi32(v);
        }
        __m256i p=_mm256_packus_epi16(_mm256_packs_epi32(q[0],q[1]),_mm256_packs_epi32(q[2],q[3]));
        /* The two packs saturate to [0,255]; one byte min gives [0,254]. */
        p=_mm256_min_epu8(p,_mm256_set1_epi8((char)254));
        /* AVX2 packs within 128-bit lanes: reorder four-byte groups. */
        p=_mm256_permutevar8x32_epi32(p,_mm256_setr_epi32(0,4,1,5,2,6,3,7));
        _mm256_storeu_si256((__m256i *)(out+j),p);
    }
    for (;j<k;j+=8) {
        __m256 v=_mm256_add_ps(_mm256_mul_ps(_mm256_loadu_ps(x+j),inv),z);
        __m256i q=_mm256_cvtps_epi32(v);
        __m128i p=_mm_packs_epi32(_mm256_castsi256_si128(q),_mm256_extracti128_si256(q,1));
        p=_mm_packus_epi16(p,p);
        p=_mm_min_epu8(p,_mm_set1_epi8((char)254));
        _mm_storel_epi64((__m128i *)(out+j),p);
    }
}

static inline __attribute__((always_inline,unused)) void quantize64(
        const float *x,int8_t *out,float *scale,int *zp) {
    const int k=64;
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
    if (low==high) { memset(out,0,k); *scale=1; *zp=0; return; }
    *scale=(high-low)/254.0f;
    *zp=(int)nearbyintf(-low / *scale);
    if (*zp<0) *zp=0;
    if (*zp>254) *zp=254;
    __m256 inv=_mm256_set1_ps(1.0f / *scale),z=_mm256_set1_ps((float)*zp);
    int j=0;
    for (;j+31<k;j+=32) {
        __m256i q[4];
        for (int t=0;t<4;++t) {
            __m256 v=_mm256_add_ps(_mm256_mul_ps(_mm256_loadu_ps(x+j+t*8),inv),z);
            q[t]=_mm256_cvtps_epi32(v);
        }
        __m256i p=_mm256_packus_epi16(_mm256_packs_epi32(q[0],q[1]),_mm256_packs_epi32(q[2],q[3]));
        /* The two packs saturate to [0,255]; one byte min gives [0,254]. */
        p=_mm256_min_epu8(p,_mm256_set1_epi8((char)254));
        /* AVX2 packs within 128-bit lanes: reorder four-byte groups. */
        p=_mm256_permutevar8x32_epi32(p,_mm256_setr_epi32(0,4,1,5,2,6,3,7));
        _mm256_storeu_si256((__m256i *)(out+j),p);
    }
    for (;j<k;j+=8) {
        __m256 v=_mm256_add_ps(_mm256_mul_ps(_mm256_loadu_ps(x+j),inv),z);
        __m256i q=_mm256_cvtps_epi32(v);
        __m128i p=_mm_packs_epi32(_mm256_castsi256_si128(q),_mm256_extracti128_si256(q,1));
        p=_mm_packus_epi16(p,p);
        p=_mm_min_epu8(p,_mm_set1_epi8((char)254));
        _mm_storel_epi64((__m128i *)(out+j),p);
    }
}

#ifndef DPDF_DISABLE_QAFFINE_ROW
#ifdef DPDF_QASM64
void dpdf_qdot64_fixed_avx2(const int8_t *,const int8_t *,int32_t *);
#if DPDF_MODEL_SIZE == 2
#ifdef DPDF_QFUSED256
void dpdf_qaffine256_fused_avx2(const int8_t *,const int8_t *,const int32_t *,
        const float *,const float *,float *,float,int,int);
#endif
#else
#endif
#ifdef DPDF_QFUSED64
void dpdf_qaffine64_fused_avx2(const int8_t *,const int8_t *,const int32_t *,
        const float *,const float *,float *,float,int,int);
#endif
#endif
/* Keep integer reduction separate from quantization/dequantization: inlining
 * their live temporaries caused GCC 12 to spill two accumulators every K step.
 * Short weight lifetimes let all eight accumulators stay in registers. The
 * noinline boundary is intentional; both instruction variants retain the same reduction order.
 * Activations are [0,254], weights are [-63,63]: pair sums fit i16 (32004).
 * K<=512 also keeps the full dot product safely within i32. */
static __attribute__((noinline)) void qdot64(const int8_t *activation,
        const int8_t *packed,int k,int32_t *out) {
    __m256i sum0=_mm256_setzero_si256(),sum1=sum0,sum2=sum0,sum3=sum0;
    __m256i sum4=sum0,sum5=sum0,sum6=sum0,sum7=sum0;
    for (int j=0;j<k;j+=4) {
        int32_t bytes; memcpy(&bytes,activation+j,4);
        __m256i a=_mm256_set1_epi32(bytes),absolute=a;
        const int8_t *base=packed+j*8;
        __m256i w0=_mm256_loadu_si256((const __m256i *)(base));
        sum0=DPDF_DOT4(sum0,absolute,w0);
        __m256i w1=_mm256_loadu_si256((const __m256i *)(base+k*8));
        sum1=DPDF_DOT4(sum1,absolute,w1);
        __m256i w2=_mm256_loadu_si256((const __m256i *)(base+2*k*8));
        sum2=DPDF_DOT4(sum2,absolute,w2);
        __m256i w3=_mm256_loadu_si256((const __m256i *)(base+3*k*8));
        sum3=DPDF_DOT4(sum3,absolute,w3);
        __m256i w4=_mm256_loadu_si256((const __m256i *)(base+4*k*8));
        sum4=DPDF_DOT4(sum4,absolute,w4);
        __m256i w5=_mm256_loadu_si256((const __m256i *)(base+5*k*8));
        sum5=DPDF_DOT4(sum5,absolute,w5);
        __m256i w6=_mm256_loadu_si256((const __m256i *)(base+6*k*8));
        sum6=DPDF_DOT4(sum6,absolute,w6);
        __m256i w7=_mm256_loadu_si256((const __m256i *)(base+7*k*8));
        sum7=DPDF_DOT4(sum7,absolute,w7);
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
#if DPDF_MODEL_SIZE == 2
    quantize(x,activation,k,&activation_scale,&zp);
#ifdef DPDF_QFUSED256
    if (k==256 && n==768) {
        dpdf_qaffine256_fused_avx2(activation,q->packed,q->sum,q->scale,bias,y,
                                 activation_scale,zp,12);
        return;
    }
#endif
#else
    if (k==64) quantize64(x,activation,&activation_scale,&zp);
    else quantize(x,activation,k,&activation_scale,&zp);
#endif
#ifdef DPDF_QFUSED64
    if (k==64 && n==192) {
        dpdf_qaffine64_fused_avx2(activation,q->packed,q->sum,q->scale,bias,y,
                                activation_scale,zp,3);
        return;
    }
#endif
    int c=0;
    for (;c+63<n;c+=64) {
#ifdef DPDF_QFUSED64
        if (k==64) {
            dpdf_qaffine64_fused_avx2(activation,q->packed+c*k,q->sum+c,q->scale+c,
                                    bias+c,y+c,activation_scale,zp,1);
            continue;
        }
#endif
        int32_t integer_sums[64];
#ifdef DPDF_QASM64
        if (k==64) dpdf_qdot64_fixed_avx2(activation,q->packed+c*k,integer_sums);
        else
#endif
            qdot64(activation,q->packed+c*k,k,integer_sums);
        __m256i sums[8];
        for (int t=0;t<8;++t) sums[t]=_mm256_loadu_si256((const __m256i *)(integer_sums+t*8));
        for (int t=0;t<8;++t) {
            int column=c+t*8;
            __m256i correction=_mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(q->sum+column)),_mm256_set1_epi32(-zp));
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
            sum=DPDF_DOT4(sum,a,w);
        }
        sum=_mm256_add_epi32(sum,_mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(q->sum+c)),_mm256_set1_epi32(-zp)));
        __m256 scale=_mm256_mul_ps(_mm256_set1_ps(activation_scale),_mm256_loadu_ps(q->scale+c));
        _mm256_storeu_ps(y+c,_mm256_fmadd_ps(_mm256_cvtepi32_ps(sum),scale,_mm256_loadu_ps(bias+c)));
    }
}
#endif

/* Four rows and two independent 8-column tiles. Keeping the integer loop in
 * its own function leaves room for eight accumulators and shared weights;
 * scale/correction temporaries are only live after the reduction returns. */
static __attribute__((noinline)) void qdot4pair_generic(const int8_t *activation,
        const int8_t *packed0,const int8_t *packed1,int k,__m256i *out) {
    __m256i a0=_mm256_setzero_si256(),a1=a0,a2=a0,a3=a0;
    __m256i b0=a0,b1=a0,b2=a0,b3=a0;
    for (int j=0;j<k;j+=4) {
        __m256i w0=_mm256_loadu_si256((const __m256i *)(packed0+j*8));
        __m256i w1=_mm256_loadu_si256((const __m256i *)(packed1+j*8));
#define DPDF_DOT_ROW(row,sa,sb) do { \
        int32_t bytes; memcpy(&bytes,activation+(row)*k+j,4); \
        __m256i v=_mm256_set1_epi32(bytes),absolute=v; \
        sa=DPDF_DOT4(sa,absolute,w0); \
        sb=DPDF_DOT4(sb,absolute,w1); \
    } while (0)
        DPDF_DOT_ROW(0,a0,b0); DPDF_DOT_ROW(1,a1,b1);
        DPDF_DOT_ROW(2,a2,b2); DPDF_DOT_ROW(3,a3,b3);
#undef DPDF_DOT_ROW
    }
    out[0]=a0;out[1]=a1;out[2]=a2;out[3]=a3;
    out[4]=b0;out[5]=b1;out[6]=b2;out[7]=b3;
}

#ifdef DPDF_QPAIR_FIXED
void dpdf_qdot4pair64_avx2(const int8_t *,const int8_t *,const int8_t *,__m256i *);
#endif
static __attribute__((noinline)) void qdot4pair(const int8_t *activation,
        const int8_t *packed0,const int8_t *packed1,int k,__m256i *out) {
#ifdef DPDF_QPAIR_FIXED
    if (k==64) { dpdf_qdot4pair64_avx2(activation,packed0,packed1,out); return; }
#endif
    qdot4pair_generic(activation,packed0,packed1,k,out);
}

#if DPDF_MODEL_SIZE == 2

#ifdef DPDF_QPAIR_FUSED64
#include "pair_fused_args.h"
static inline __attribute__((always_inline)) void qpair_fused4(
        const dpdf_qmatrix *q0,const dpdf_qmatrix *q1,const int8_t *activation,
        const float *scales,const int *zp,const float *bias0,const float *bias1,
        float *y0,float *y1,int c0,int c1) {
    const dpdf_pair_fused_args args={
        activation,q0->packed+c0*64,q1->packed+c1*64,q0->sum+c0,q1->sum+c1,
        q0->scale+c0,q1->scale+c1,bias0+c0,bias1+c1,y0+c0,y1+c1,
        (size_t)q0->n*sizeof(float),
        {scales[0],scales[1],scales[2],scales[3]},
        {zp[0],zp[1],zp[2],zp[3]}
    };
    dpdf_qaffine4pair64_fused_avx2(&args);
}
#endif

#else
#endif
static void qaffine_batch_tiled(const dpdf_qmatrix *q,const float *x,const float *bias,float *y,int m) {
    int8_t activation[48*512]; float scales[48]; int zp[48];
    const int k=q->k,n=q->n;
#if DPDF_MODEL_SIZE == 2
    if (k==64) {
        for (int r=0;r<m;++r) quantize64(x+r*64,activation+r*64,scales+r,zp+r);
    } else {
        for (int r=0;r<m;++r) quantize(x+r*k,activation+r*k,k,scales+r,zp+r);
    }
#ifdef DPDF_QPAIR_FUSED64
    if (k==64) {
        for (int r=0;r<m;r+=4) for (int c=0;c<n;c+=16)
            qpair_fused4(q,q,activation+r*64,scales+r,zp+r,bias,bias,
                         y+r*n,y+r*n,c,c+8);
        return;
    }
#endif
#else
    for (int r=0;r<m;++r) quantize(x+r*k,activation+r*k,k,scales+r,zp+r);
#endif
    for (int r=0;r<m;r+=4) for (int c=0;c<n;c+=16) {
        __m256i sums[8];
        qdot4pair(activation+r*k,q->packed+c*k,q->packed+(c+8)*k,k,sums);
        for (int t=0;t<4;++t) for (int tile=0;tile<2;++tile) {
            int col=c+tile*8;
            __m256i correction=_mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(q->sum+col)),_mm256_set1_epi32(-zp[r+t]));
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
#if DPDF_MODEL_SIZE == 2
    if (k==64) {
        for (int r=0;r<m;++r) quantize64(x+r*64,activation+r*64,scales+r,zp+r);
    } else {
        for (int r=0;r<m;++r) quantize(x+r*k,activation+r*k,k,scales+r,zp+r);
    }
#else
    for (int r=0;r<m;++r) quantize(x+r*k,activation+r*k,k,scales+r,zp+r);
#endif
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
            s0=DPDF_DOT4(s0,a0,w);
            s1=DPDF_DOT4(s1,a1,w);
            s2=DPDF_DOT4(s2,a2,w);
            s3=DPDF_DOT4(s3,a3,w);
        }
        __m256i sums[4]={s0,s1,s2,s3};
        for (int t=0;t<4;++t) {
            __m256i correction=_mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(q->sum+c)),_mm256_set1_epi32(-zp[r+t]));
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
            __m256i absolute=a;
            const int8_t *base=q->packed+(c/8)*k*8+j*8;
            __m256i w0=_mm256_loadu_si256((const __m256i *)(base));
            __m256i w1=_mm256_loadu_si256((const __m256i *)(base+k*8));
            __m256i w2=_mm256_loadu_si256((const __m256i *)(base+2*k*8));
            __m256i w3=_mm256_loadu_si256((const __m256i *)(base+3*k*8));
            __m256i w4=_mm256_loadu_si256((const __m256i *)(base+4*k*8));
            __m256i w5=_mm256_loadu_si256((const __m256i *)(base+5*k*8));
            __m256i w6=_mm256_loadu_si256((const __m256i *)(base+6*k*8));
            __m256i w7=_mm256_loadu_si256((const __m256i *)(base+7*k*8));
            /* 2*254*63=32004: AVX2 never saturates, so VNNI is exact. */
            sum0=DPDF_DOT4(sum0,absolute,w0);
            sum1=DPDF_DOT4(sum1,absolute,w1);
            sum2=DPDF_DOT4(sum2,absolute,w2);
            sum3=DPDF_DOT4(sum3,absolute,w3);
            sum4=DPDF_DOT4(sum4,absolute,w4);
            sum5=DPDF_DOT4(sum5,absolute,w5);
            sum6=DPDF_DOT4(sum6,absolute,w6);
            sum7=DPDF_DOT4(sum7,absolute,w7);
        }
        __m256i sums[8]={sum0,sum1,sum2,sum3,sum4,sum5,sum6,sum7};
        for (int t=0;t<8;++t) {
            int column=c+t*8;
            __m256i correction=_mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(q->sum+column)),_mm256_set1_epi32(-zp[r]));
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
            sum=DPDF_DOT4(sum,a,w);
        }
        __m256i correction=_mm256_mullo_epi32(_mm256_loadu_si256((const __m256i *)(q->sum+c)),_mm256_set1_epi32(-zp[r]));
        sum=_mm256_add_epi32(sum,correction);
        __m256 scale=_mm256_mul_ps(_mm256_set1_ps(scales[r]),_mm256_loadu_ps(q->scale+c));
        __m256 result=_mm256_fmadd_ps(_mm256_cvtepi32_ps(sum),scale,_mm256_loadu_ps(bias+c));
        _mm256_storeu_ps(y+r*n+c,result);
      }
    }
}

void DPDF_QAFFINE_IMPL(const dpdf_qmatrix *q,const float *x,const float *bias,float *y,int m) {
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
void DPDF_QPAIR_IMPL(const dpdf_qmatrix *q0,const dpdf_qmatrix *q1,const float *x,
                       const float *bias0,const float *bias1,float *y0,float *y1,int m) {
    if (q0->k!=q1->k || q0->n!=q1->n || m%4) {
        DPDF_QAFFINE_IMPL(q0,x,bias0,y0,m); DPDF_QAFFINE_IMPL(q1,x,bias1,y1,m); return;
    }
    int8_t activation[48*512]; float scales[48]; int zp[48];
    const int k=q0->k,n=q0->n;
#if DPDF_MODEL_SIZE == 2
    if (k==64) {
        for (int r=0;r<m;++r) quantize64(x+r*64,activation+r*64,scales+r,zp+r);
    } else {
        for (int r=0;r<m;++r) quantize(x+r*k,activation+r*k,k,scales+r,zp+r);
    }
#ifdef DPDF_QPAIR_FUSED64
    if (k==64) {
        for (int r=0;r<m;r+=4) for (int c=0;c<n;c+=8)
            qpair_fused4(q0,q1,activation+r*64,scales+r,zp+r,bias0,bias1,
                         y0+r*n,y1+r*n,c,c);
        return;
    }
#endif
#else
    for (int r=0;r<m;++r) quantize(x+r*k,activation+r*k,k,scales+r,zp+r);
#endif
    for (int r=0;r<m;r+=4) for (int c=0;c<n;c+=8) {
        __m256i sums[8];
        qdot4pair(activation+r*k,q0->packed+c*k,q1->packed+c*k,k,sums);
        const __m256i weight_sum0=_mm256_loadu_si256((const __m256i *)(q0->sum+c));
        const __m256i weight_sum1=_mm256_loadu_si256((const __m256i *)(q1->sum+c));
        const __m256 weight_scale0=_mm256_loadu_ps(q0->scale+c);
        const __m256 weight_scale1=_mm256_loadu_ps(q1->scale+c);
        for (int t=0;t<4;++t) {
            __m256i zero=_mm256_set1_epi32(-zp[r+t]);
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

#undef DPDF_DOT4
