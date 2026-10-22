#include "internal.h"
#include <immintrin.h>
#include <math.h>

/* Four independent rows occupy four double lanes. Every lane retains the
 * scalar channel order and double precision; no horizontal reassociation or
 * FMA is used. DPRNN frequency counts (40/48) are divisible by four. */
static void dpdf_norm_residual_four_avx2(float *out,const float *p,const float *skip,
        const float *scale,const float *bias,float eps,int rows) {
    for (int r=0;r<rows;r+=4) {
        __m256d mean=_mm256_setzero_pd(),var=mean;
        const float *x=p+r*64;
        for (int c=0;c<64;c+=4) {
            __m128 v0=_mm_loadu_ps(x+c),v1=_mm_loadu_ps(x+64+c);
            __m128 v2=_mm_loadu_ps(x+128+c),v3=_mm_loadu_ps(x+192+c);
            _MM_TRANSPOSE4_PS(v0,v1,v2,v3);
            /* Add channels in their original increasing order in each lane. */
            mean=_mm256_add_pd(mean,_mm256_cvtps_pd(v0));
            mean=_mm256_add_pd(mean,_mm256_cvtps_pd(v1));
            mean=_mm256_add_pd(mean,_mm256_cvtps_pd(v2));
            mean=_mm256_add_pd(mean,_mm256_cvtps_pd(v3));
        }
        mean=_mm256_mul_pd(mean,_mm256_set1_pd(1.0/64));
        for (int c=0;c<64;c+=4) {
            __m128 v0=_mm_loadu_ps(x+c),v1=_mm_loadu_ps(x+64+c);
            __m128 v2=_mm_loadu_ps(x+128+c),v3=_mm_loadu_ps(x+192+c);
            _MM_TRANSPOSE4_PS(v0,v1,v2,v3);
            __m256d d=_mm256_sub_pd(_mm256_cvtps_pd(v0),mean);
            var=_mm256_add_pd(var,_mm256_mul_pd(d,d));
            d=_mm256_sub_pd(_mm256_cvtps_pd(v1),mean);
            var=_mm256_add_pd(var,_mm256_mul_pd(d,d));
            d=_mm256_sub_pd(_mm256_cvtps_pd(v2),mean);
            var=_mm256_add_pd(var,_mm256_mul_pd(d,d));
            d=_mm256_sub_pd(_mm256_cvtps_pd(v3),mean);
            var=_mm256_add_pd(var,_mm256_mul_pd(d,d));
        }
        __m256d inv=_mm256_div_pd(_mm256_set1_pd(1),_mm256_sqrt_pd(
            _mm256_add_pd(_mm256_mul_pd(var,_mm256_set1_pd(1.0/64)),_mm256_set1_pd(eps))));
        float means[4],invs[4];
        _mm_storeu_ps(means,_mm256_cvtpd_ps(mean)); _mm_storeu_ps(invs,_mm256_cvtpd_ps(inv));
        for (int t=0;t<4;++t) for (int c=0;c<64;c+=8) {
            int offset=(r+t)*64+c;
            __m256 value=_mm256_sub_ps(_mm256_loadu_ps(p+offset),_mm256_set1_ps(means[t]));
            value=_mm256_mul_ps(value,_mm256_set1_ps(invs[t]));
            value=_mm256_mul_ps(value,_mm256_loadu_ps(scale+c));
            value=_mm256_add_ps(value,_mm256_loadu_ps(bias+c));
            value=_mm256_add_ps(value,_mm256_loadu_ps(skip+offset));
            _mm256_storeu_ps(out+offset,value);
        }
    }
}

void dpdf_norm_residual_avx2(float *out,const float *p,const float *skip,
        const float *scale,const float *bias,float eps,int rows) {
    int r=0;
    for (;r+7<rows;r+=8) {
        __m256d mean0=_mm256_setzero_pd(),mean1=mean0;
        __m256d var0=mean0,var1=mean0;
        const float *x=p+r*64;
        for (int c=0;c<64;c+=4) {
            __m128 v0=_mm_loadu_ps(x+c),v1=_mm_loadu_ps(x+64+c);
            __m128 v2=_mm_loadu_ps(x+128+c),v3=_mm_loadu_ps(x+192+c);
            __m128 w0=_mm_loadu_ps(x+256+c),w1=_mm_loadu_ps(x+320+c);
            __m128 w2=_mm_loadu_ps(x+384+c),w3=_mm_loadu_ps(x+448+c);
            _MM_TRANSPOSE4_PS(v0,v1,v2,v3);
            _MM_TRANSPOSE4_PS(w0,w1,w2,w3);
            mean0=_mm256_add_pd(mean0,_mm256_cvtps_pd(v0));
            mean1=_mm256_add_pd(mean1,_mm256_cvtps_pd(w0));
            mean0=_mm256_add_pd(mean0,_mm256_cvtps_pd(v1));
            mean1=_mm256_add_pd(mean1,_mm256_cvtps_pd(w1));
            mean0=_mm256_add_pd(mean0,_mm256_cvtps_pd(v2));
            mean1=_mm256_add_pd(mean1,_mm256_cvtps_pd(w2));
            mean0=_mm256_add_pd(mean0,_mm256_cvtps_pd(v3));
            mean1=_mm256_add_pd(mean1,_mm256_cvtps_pd(w3));
        }
        mean0=_mm256_mul_pd(mean0,_mm256_set1_pd(1.0/64));
        mean1=_mm256_mul_pd(mean1,_mm256_set1_pd(1.0/64));
        for (int c=0;c<64;c+=4) {
            __m128 v0=_mm_loadu_ps(x+c),v1=_mm_loadu_ps(x+64+c);
            __m128 v2=_mm_loadu_ps(x+128+c),v3=_mm_loadu_ps(x+192+c);
            __m128 w0=_mm_loadu_ps(x+256+c),w1=_mm_loadu_ps(x+320+c);
            __m128 w2=_mm_loadu_ps(x+384+c),w3=_mm_loadu_ps(x+448+c);
            _MM_TRANSPOSE4_PS(v0,v1,v2,v3);
            _MM_TRANSPOSE4_PS(w0,w1,w2,w3);
            __m256d d0=_mm256_sub_pd(_mm256_cvtps_pd(v0),mean0);
            __m256d e0=_mm256_sub_pd(_mm256_cvtps_pd(w0),mean1);
            var0=_mm256_add_pd(var0,_mm256_mul_pd(d0,d0));
            var1=_mm256_add_pd(var1,_mm256_mul_pd(e0,e0));
            __m256d d1=_mm256_sub_pd(_mm256_cvtps_pd(v1),mean0);
            __m256d e1=_mm256_sub_pd(_mm256_cvtps_pd(w1),mean1);
            var0=_mm256_add_pd(var0,_mm256_mul_pd(d1,d1));
            var1=_mm256_add_pd(var1,_mm256_mul_pd(e1,e1));
            __m256d d2=_mm256_sub_pd(_mm256_cvtps_pd(v2),mean0);
            __m256d e2=_mm256_sub_pd(_mm256_cvtps_pd(w2),mean1);
            var0=_mm256_add_pd(var0,_mm256_mul_pd(d2,d2));
            var1=_mm256_add_pd(var1,_mm256_mul_pd(e2,e2));
            __m256d d3=_mm256_sub_pd(_mm256_cvtps_pd(v3),mean0);
            __m256d e3=_mm256_sub_pd(_mm256_cvtps_pd(w3),mean1);
            var0=_mm256_add_pd(var0,_mm256_mul_pd(d3,d3));
            var1=_mm256_add_pd(var1,_mm256_mul_pd(e3,e3));
        }
        __m256d inv0=_mm256_div_pd(_mm256_set1_pd(1),_mm256_sqrt_pd(
            _mm256_add_pd(_mm256_mul_pd(var0,_mm256_set1_pd(1.0/64)),_mm256_set1_pd(eps))));
        __m256d inv1=_mm256_div_pd(_mm256_set1_pd(1),_mm256_sqrt_pd(
            _mm256_add_pd(_mm256_mul_pd(var1,_mm256_set1_pd(1.0/64)),_mm256_set1_pd(eps))));
        float means[8],invs[8];
        _mm_storeu_ps(means,_mm256_cvtpd_ps(mean0));
        _mm_storeu_ps(means+4,_mm256_cvtpd_ps(mean1));
        _mm_storeu_ps(invs,_mm256_cvtpd_ps(inv0));
        _mm_storeu_ps(invs+4,_mm256_cvtpd_ps(inv1));
        for (int t=0;t<8;++t) for (int c=0;c<64;c+=8) {
            int offset=(r+t)*64+c;
            __m256 value=_mm256_sub_ps(_mm256_loadu_ps(p+offset),_mm256_set1_ps(means[t]));
            value=_mm256_mul_ps(value,_mm256_set1_ps(invs[t]));
            value=_mm256_mul_ps(value,_mm256_loadu_ps(scale+c));
            value=_mm256_add_ps(value,_mm256_loadu_ps(bias+c));
            value=_mm256_add_ps(value,_mm256_loadu_ps(skip+offset));
            _mm256_storeu_ps(out+offset,value);
        }
    }
    if (r<rows) dpdf_norm_residual_four_avx2(out+r*64,p+r*64,skip+r*64,
                                          scale,bias,eps,rows-r);
}

/* Exact 8x8 transpose: the pointwise CNN layout conversions move bits only. */
void dpdf_transpose_avx2(const float *x,float *y,int rows,int cols) {
    int r=0;
    for (;r+7<rows;r+=8) {
        int c=0;
        for (;c+7<cols;c+=8) {
            __m256 a0=_mm256_loadu_ps(x+(r+0)*cols+c),a1=_mm256_loadu_ps(x+(r+1)*cols+c);
            __m256 a2=_mm256_loadu_ps(x+(r+2)*cols+c),a3=_mm256_loadu_ps(x+(r+3)*cols+c);
            __m256 a4=_mm256_loadu_ps(x+(r+4)*cols+c),a5=_mm256_loadu_ps(x+(r+5)*cols+c);
            __m256 a6=_mm256_loadu_ps(x+(r+6)*cols+c),a7=_mm256_loadu_ps(x+(r+7)*cols+c);
            __m256 b0=_mm256_unpacklo_ps(a0,a1),b1=_mm256_unpackhi_ps(a0,a1);
            __m256 b2=_mm256_unpacklo_ps(a2,a3),b3=_mm256_unpackhi_ps(a2,a3);
            __m256 b4=_mm256_unpacklo_ps(a4,a5),b5=_mm256_unpackhi_ps(a4,a5);
            __m256 b6=_mm256_unpacklo_ps(a6,a7),b7=_mm256_unpackhi_ps(a6,a7);
            a0=_mm256_shuffle_ps(b0,b2,0x44); a1=_mm256_shuffle_ps(b0,b2,0xee);
            a2=_mm256_shuffle_ps(b1,b3,0x44); a3=_mm256_shuffle_ps(b1,b3,0xee);
            a4=_mm256_shuffle_ps(b4,b6,0x44); a5=_mm256_shuffle_ps(b4,b6,0xee);
            a6=_mm256_shuffle_ps(b5,b7,0x44); a7=_mm256_shuffle_ps(b5,b7,0xee);
            _mm256_storeu_ps(y+(c+0)*rows+r,_mm256_permute2f128_ps(a0,a4,0x20));
            _mm256_storeu_ps(y+(c+1)*rows+r,_mm256_permute2f128_ps(a1,a5,0x20));
            _mm256_storeu_ps(y+(c+2)*rows+r,_mm256_permute2f128_ps(a2,a6,0x20));
            _mm256_storeu_ps(y+(c+3)*rows+r,_mm256_permute2f128_ps(a3,a7,0x20));
            _mm256_storeu_ps(y+(c+4)*rows+r,_mm256_permute2f128_ps(a0,a4,0x31));
            _mm256_storeu_ps(y+(c+5)*rows+r,_mm256_permute2f128_ps(a1,a5,0x31));
            _mm256_storeu_ps(y+(c+6)*rows+r,_mm256_permute2f128_ps(a2,a6,0x31));
            _mm256_storeu_ps(y+(c+7)*rows+r,_mm256_permute2f128_ps(a3,a7,0x31));
        }
        for (;c<cols;++c) for (int i=0;i<8;++i) y[c*rows+r+i]=x[(r+i)*cols+c];
    }
    for (;r<rows;++r) for (int c=0;c<cols;++c) y[c*rows+r]=x[r*cols+c];
}

void dpdf_axpy_avx2(float *y, const float *x, float a, int n) {
    __m256 v=_mm256_set1_ps(a);
    int i=0;
    for (; i+7<n; i+=8)
        _mm256_storeu_ps(y+i,_mm256_fmadd_ps(v,_mm256_loadu_ps(x+i),_mm256_loadu_ps(y+i)));
    for (; i<n; ++i) y[i]+=a*x[i];
}

/* Keep output tiles in registers through the ordered convolution reduction.
 * Only the single-output-row, stride-one case is handled here. The original
 * axpy uses FMA for full eight-element groups and separate multiply/add in
 * its tails. Their intersection is safe for vector FMA; fringe outputs retain
 * that per-tap distinction, including skipped padding. */
int dpdf_conv_row_avx2(const float *x,const float *w,const float *bias,float *y,
        int ci,int hi,int wi,int co,int ho,int wo,int kh,int kw,
        int sh,int sw,int ph,int pw,int group) {
    if (ho!=1 || hi!=kh || sh!=1 || sw!=1 || ph!=0 || wi<8) return 0;
    /* Fringe setup dominates the shortest padded depthwise reductions. */
    if (ci==group && kh==1 && kw>1 && pw>0 && wo<64) return 0;
    int first=0,limit=wo;
    for (int kx=0;kx<kw;++kx) {
        int begin=pw-kx; if (begin<0) begin=0;
        int end=wi+pw-kx; if (end>wo) end=wo;
        if (end<=begin) return 0;
        if (begin>first) first=begin;
        int fused_end=begin+(end-begin)/8*8;
        if (fused_end<limit) limit=fused_end;
    }
    first=(first+7)/8*8;
    int last=limit/8*8;
    if (last-first<8) return 0;
    int cig=ci/group,cog=co/group;
    for (int oc=0;oc<co;++oc) {
        const float *input=x+(oc/cog)*cig*hi*wi;
        const float *weight=w+oc*cig*kh*kw;
        float *out=y+oc*wo;
        float initial=bias?bias[oc]:0;
        int ow=first;
        for (;ow+63<last;ow+=64) {
            __m256 a0=_mm256_set1_ps(initial),a1=a0,a2=a0,a3=a0,a4=a0,a5=a0,a6=a0,a7=a0;
            for (int ic=0;ic<cig;++ic) for (int ky=0;ky<kh;++ky) for (int kx=0;kx<kw;++kx) {
                __m256 v=_mm256_set1_ps(weight[(ic*kh+ky)*kw+kx]);
                const float *p=input+(ic*hi+ky)*wi+ow+kx-pw;
                a0=_mm256_fmadd_ps(v,_mm256_loadu_ps(p),a0);
                a1=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+8),a1);
                a2=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+16),a2);
                a3=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+24),a3);
                a4=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+32),a4);
                a5=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+40),a5);
                a6=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+48),a6);
                a7=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+56),a7);
            }
            _mm256_storeu_ps(out+ow,a0);_mm256_storeu_ps(out+ow+8,a1);
            _mm256_storeu_ps(out+ow+16,a2);_mm256_storeu_ps(out+ow+24,a3);
            _mm256_storeu_ps(out+ow+32,a4);_mm256_storeu_ps(out+ow+40,a5);
            _mm256_storeu_ps(out+ow+48,a6);_mm256_storeu_ps(out+ow+56,a7);
        }
        for (;ow<last;ow+=8) {
            __m256 sum=_mm256_set1_ps(initial);
            for (int ic=0;ic<cig;++ic) for (int ky=0;ky<kh;++ky) for (int kx=0;kx<kw;++kx)
                sum=_mm256_fmadd_ps(_mm256_set1_ps(weight[(ic*kh+ky)*kw+kx]),
                     _mm256_loadu_ps(input+(ic*hi+ky)*wi+ow+kx-pw),sum);
            _mm256_storeu_ps(out+ow,sum);
        }
        for (int fringe=0;fringe<first+wo-last;fringe+=8) {
            int col=fringe<first?fringe:last+fringe-first;
            __m256 sum=_mm256_set1_ps(initial);
            __m256i indices=_mm256_add_epi32(_mm256_set1_epi32(col),_mm256_setr_epi32(0,1,2,3,4,5,6,7));
            for (int ic=0;ic<cig;++ic) for (int ky=0;ky<kh;++ky) for (int kx=0;kx<kw;++kx) {
                int ix=col+kx-pw,base=ix;
                if (base<0) base=0;
                if (base>wi-8) base=wi-8;
                int begin=pw-kx; if (begin<0) begin=0;
                int end=wi+pw-kx; if (end>wo) end=wo;
                __m256 values=_mm256_loadu_ps(input+(ic*hi+ky)*wi+base);
                __m256i lanes=_mm256_add_epi32(_mm256_set1_epi32(ix-base),_mm256_setr_epi32(0,1,2,3,4,5,6,7));
                values=_mm256_permutevar8x32_ps(values,lanes);
                __m256 v=_mm256_set1_ps(weight[(ic*kh+ky)*kw+kx]);
                __m256 fused=_mm256_fmadd_ps(v,values,sum);
                __m256 separate=_mm256_add_ps(sum,_mm256_mul_ps(v,values));
                __m256 use_fma=_mm256_castsi256_ps(_mm256_cmpgt_epi32(_mm256_set1_epi32(begin+(end-begin)/8*8),indices));
                __m256 result=_mm256_blendv_ps(separate,fused,use_fma);
                __m256i valid=_mm256_and_si256(_mm256_cmpgt_epi32(indices,_mm256_set1_epi32(begin-1)),
                                             _mm256_cmpgt_epi32(_mm256_set1_epi32(end),indices));
                sum=_mm256_blendv_ps(sum,result,_mm256_castsi256_ps(valid));
            }
            _mm256_maskstore_ps(out+col,_mm256_cmpgt_epi32(_mm256_set1_epi32(wo),indices),sum);
        }
    }
    return 1;
}


/* Exact specialization for the model's depthwise 1x3 stride-two/three
 * downsamplers. The original sw!=1 path uses separate binary32 multiply/add,
 * so these vectors deliberately do not fuse. Padding and odd-width tails
 * use the original ordered scalar terms; vector loads stay inside input. */
int dpdf_depthwise_stride_avx2(const float *x,const float *w,const float *bias,float *y,
        int ci,int hi,int wi,int co,int ho,int wo,int kh,int kw,
        int sh,int sw,int ph,int pw,int group) {
    if (ci<=0 || group<=0 || hi!=1 || ho!=1 || kh!=1 || kw!=3 || sh!=1 || ph!=0 || pw!=1 ||
        group!=ci || co%group || (sw!=2 && sw!=3) || wi<8 ||
        wo!=(wi+2*pw-kw)/sw+1) return 0;
    const __m256i offsets=_mm256_setr_epi32(0,sw,2*sw,3*sw,4*sw,5*sw,6*sw,7*sw);
    int last=(wi+pw-kw)/sw+1;
    if (last>wo) last=wo;
    for (int oc=0;oc<co;++oc) {
        const float *in=x+(oc/(co/group))*wi;
        const float *weight=w+oc*3;
        float *out=y+oc*wo;
        float initial=bias?bias[oc]:0;
        int ow=0;
        while (ow<wo) {
            if (ow>=1 && ow+7<last) {
                __m256 sum=_mm256_set1_ps(initial);
                for (int kx=0;kx<3;++kx) {
                    const float *p=in+ow*sw+kx-pw;
                    __m256 values;
                    if (sw==2 && ow*sw+kx-pw+15<wi) {
                        __m128 lo=_mm_shuffle_ps(_mm_loadu_ps(p),_mm_loadu_ps(p+4),
                                                  _MM_SHUFFLE(2,0,2,0));
                        __m128 hi=_mm_shuffle_ps(_mm_loadu_ps(p+8),_mm_loadu_ps(p+12),
                                                  _MM_SHUFFLE(2,0,2,0));
                        values=_mm256_insertf128_ps(_mm256_castps128_ps256(lo),hi,1);
                    } else values=_mm256_i32gather_ps(p,offsets,4);
                    sum=_mm256_add_ps(sum,_mm256_mul_ps(_mm256_set1_ps(weight[kx]),values));
                }
                _mm256_storeu_ps(out+ow,sum);
                ow+=8;
            } else {
                float sum=initial;
                for (int kx=0;kx<3;++kx) {
                    int ix=ow*sw+kx-pw;
                    if (ix>=0 && ix<wi) sum+=weight[kx]*in[ix];
                }
                out[ow++]=sum;
            }
        }
    }
    return 1;
}

/* Input-major, output-contiguous weights. Four rows share each loaded tile.
 * Handles arbitrary widths, including the grouped projections. */
void dpdf_affine_avx2(const float *x, const float *w, const float *bias,
                      float *y, int m, int k, int n) {
    int r = 0;
    for (; n%16==0 && r+3 < m; r += 4) {
        for (int c = 0; c < n; c += 16) {
            __m256 a0 = _mm256_loadu_ps(bias+c), a1 = _mm256_loadu_ps(bias+c+8);
            __m256 b0=a0, b1=a1, c0=a0, c1=a1, d0=a0, d1=a1;
            for (int j = 0; j < k; ++j) {
                __m256 w0 = _mm256_loadu_ps(w+j*n+c), w1 = _mm256_loadu_ps(w+j*n+c+8);
                __m256 v = _mm256_set1_ps(x[r*k+j]);
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
    }
    for (; r < m; ++r) {
        int c=0;
        for (; c+63<n; c+=64) {
            __m256 a0=_mm256_loadu_ps(bias+c),a1=_mm256_loadu_ps(bias+c+8);
            __m256 a2=_mm256_loadu_ps(bias+c+16),a3=_mm256_loadu_ps(bias+c+24);
            __m256 a4=_mm256_loadu_ps(bias+c+32),a5=_mm256_loadu_ps(bias+c+40);
            __m256 a6=_mm256_loadu_ps(bias+c+48),a7=_mm256_loadu_ps(bias+c+56);
            for (int j=0;j<k;++j) {
                __m256 v=_mm256_set1_ps(x[r*k+j]);
                const float *p=w+j*n+c;
                a0=_mm256_fmadd_ps(v,_mm256_loadu_ps(p),a0);
                a1=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+8),a1);
                a2=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+16),a2);
                a3=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+24),a3);
                a4=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+32),a4);
                a5=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+40),a5);
                a6=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+48),a6);
                a7=_mm256_fmadd_ps(v,_mm256_loadu_ps(p+56),a7);
            }
            _mm256_storeu_ps(y+r*n+c,a0); _mm256_storeu_ps(y+r*n+c+8,a1);
            _mm256_storeu_ps(y+r*n+c+16,a2); _mm256_storeu_ps(y+r*n+c+24,a3);
            _mm256_storeu_ps(y+r*n+c+32,a4); _mm256_storeu_ps(y+r*n+c+40,a5);
            _mm256_storeu_ps(y+r*n+c+48,a6); _mm256_storeu_ps(y+r*n+c+56,a7);
        }
        for (; c+31 < n; c += 32) {
            __m256 a=_mm256_loadu_ps(bias+c), b=_mm256_loadu_ps(bias+c+8);
            __m256 d=_mm256_loadu_ps(bias+c+16), e=_mm256_loadu_ps(bias+c+24);
            for (int j = 0; j < k; ++j) {
                __m256 v=_mm256_set1_ps(x[r*k+j]);
                a=_mm256_fmadd_ps(v,_mm256_loadu_ps(w+j*n+c),a);
                b=_mm256_fmadd_ps(v,_mm256_loadu_ps(w+j*n+c+8),b);
                d=_mm256_fmadd_ps(v,_mm256_loadu_ps(w+j*n+c+16),d);
                e=_mm256_fmadd_ps(v,_mm256_loadu_ps(w+j*n+c+24),e);
            }
            _mm256_storeu_ps(y+r*n+c,a); _mm256_storeu_ps(y+r*n+c+8,b);
            _mm256_storeu_ps(y+r*n+c+16,d); _mm256_storeu_ps(y+r*n+c+24,e);
        }
        for (; c+7<n; c+=8) {
            __m256 a=_mm256_loadu_ps(bias+c);
            for (int j=0; j<k; ++j)
                a=_mm256_fmadd_ps(_mm256_set1_ps(x[r*k+j]),_mm256_loadu_ps(w+j*n+c),a);
            _mm256_storeu_ps(y+r*n+c,a);
        }
        for (; c<n; ++c) {
            float a=bias[c];
            for (int j=0; j<k; ++j) a+=x[r*k+j]*w[j*n+c];
            y[r*n+c]=a;
        }
    }
}

/* exp(x): range reduction x=n*ln(2)+r, |r|<=ln(2)/2, degree-5 Chebyshev interpolation (research)
 * evaluated by FMA. No external approximation code. Clamp affects only
 * saturated sigmoid/tanh tails. Activations are FP32 approximations, not
 * bit-identical libm functions; their error is explicitly tested. */
static inline __m256 exp8(__m256 x) {
    x=_mm256_max_ps(_mm256_set1_ps(-87.0f),_mm256_min_ps(_mm256_set1_ps(87.0f),x));
    __m256 nf=_mm256_round_ps(_mm256_mul_ps(x,_mm256_set1_ps(1.4426950408889634f)),
                             _MM_FROUND_TO_NEAREST_INT|_MM_FROUND_NO_EXC);
    __m256 r=_mm256_fnmadd_ps(nf,_mm256_set1_ps(0.693145751953125f),x);
    r=_mm256_fnmadd_ps(nf,_mm256_set1_ps(1.428606765330187e-6f),r);
    __m256 p=_mm256_set1_ps(0x1.123d820000000p-7f);
    p=_mm256_fmadd_ps(p,r,_mm256_set1_ps(0x1.5763620000000p-5f));
    p=_mm256_fmadd_ps(p,r,_mm256_set1_ps(0x1.55547c0000000p-3f));
    p=_mm256_fmadd_ps(p,r,_mm256_set1_ps(0x1.fffd0a0000000p-2f));
    p=_mm256_fmadd_ps(p,r,_mm256_set1_ps(0x1.0000000000000p+0f));
    p=_mm256_fmadd_ps(p,r,_mm256_set1_ps(0x1.0000020000000p+0f));
    __m256i e=_mm256_slli_epi32(_mm256_add_epi32(_mm256_cvttps_epi32(nf),_mm256_set1_epi32(127)),23);
    return _mm256_mul_ps(p,_mm256_castsi256_ps(e));
}
static inline __m256 exp8_nonpositive(__m256 x) {
    x=_mm256_max_ps(_mm256_set1_ps(-87.0f),x);
    __m256 nf=_mm256_round_ps(_mm256_mul_ps(x,_mm256_set1_ps(1.4426950408889634f)),
                             _MM_FROUND_TO_NEAREST_INT|_MM_FROUND_NO_EXC);
    __m256 r=_mm256_fnmadd_ps(nf,_mm256_set1_ps(0.693145751953125f),x);
    r=_mm256_fnmadd_ps(nf,_mm256_set1_ps(1.428606765330187e-6f),r);
    __m256 p=_mm256_set1_ps(0x1.123d820000000p-7f);
    p=_mm256_fmadd_ps(p,r,_mm256_set1_ps(0x1.5763620000000p-5f));
    p=_mm256_fmadd_ps(p,r,_mm256_set1_ps(0x1.55547c0000000p-3f));
    p=_mm256_fmadd_ps(p,r,_mm256_set1_ps(0x1.fffd0a0000000p-2f));
    p=_mm256_fmadd_ps(p,r,_mm256_set1_ps(0x1.0000000000000p+0f));
    p=_mm256_fmadd_ps(p,r,_mm256_set1_ps(0x1.0000020000000p+0f));
    __m256i e=_mm256_slli_epi32(_mm256_add_epi32(_mm256_cvttps_epi32(nf),_mm256_set1_epi32(127)),23);
    return _mm256_mul_ps(p,_mm256_castsi256_ps(e));
}
static inline __m256 sigmoid8(__m256 x) {
    return _mm256_div_ps(_mm256_set1_ps(1),_mm256_add_ps(_mm256_set1_ps(1),exp8(_mm256_sub_ps(_mm256_setzero_ps(),x))));
}
static inline __m256 tanh8(__m256 x) {
    __m256 sign=_mm256_and_ps(x,_mm256_set1_ps(-0.0f));
    __m256 absolute=_mm256_andnot_ps(_mm256_set1_ps(-0.0f),x);
    __m256 e=exp8_nonpositive(_mm256_mul_ps(_mm256_set1_ps(-2),absolute));
    return _mm256_xor_ps(sign,_mm256_div_ps(_mm256_sub_ps(_mm256_set1_ps(1),e),_mm256_add_ps(_mm256_set1_ps(1),e)));
}
void dpdf_gates_avx2(const float *a, const float *b, const float *old, float *out, int rows) {
    for (int r=0; r<rows; ++r) {
        float resets[64],updates[64];
        for (int c=0; c<64; c+=8) {
            _mm256_storeu_ps(resets+c,sigmoid8(_mm256_add_ps(_mm256_loadu_ps(a+r*192+c),_mm256_loadu_ps(b+r*192+c))));
            _mm256_storeu_ps(updates+c,sigmoid8(_mm256_add_ps(_mm256_loadu_ps(a+r*192+64+c),_mm256_loadu_ps(b+r*192+64+c))));
        }
        for (int c=0; c<64; c+=8) {
            __m256 reset=_mm256_loadu_ps(resets+c),update=_mm256_loadu_ps(updates+c);
            __m256 candidate=tanh8(_mm256_fmadd_ps(reset,_mm256_loadu_ps(b+r*192+128+c),_mm256_loadu_ps(a+r*192+128+c)));
            __m256 result=_mm256_fmadd_ps(update,_mm256_sub_ps(_mm256_loadu_ps(old+r*64+c),candidate),candidate);
            _mm256_storeu_ps(out+r*64+c,result);
        }
    }
}
void dpdf_activations_avx2(const float *x, float *s, float *t, size_t count) {
    size_t i=0;
    for (; i+7<count; i+=8) {
        __m256 v=_mm256_loadu_ps(x+i);
        _mm256_storeu_ps(s+i,sigmoid8(v)); _mm256_storeu_ps(t+i,tanh8(v));
    }
    for (; i<count; ++i) { s[i]=1.0f/(1.0f+expf(-x[i])); t[i]=tanhf(x[i]); }
}

/* Two outputs share input vectors; each output preserves the original
 * ic/ky reduction order and full-vector FMA versus scalar-tail distinction.
 * Used by shared DF convolutions: CI=32, KH=5, CO=5, W=96. */
int dpdf_conv_row_pair_avx2(const float *x,const float *w,const float *bias,float *y,
        int ci,int hi,int wi,int co,int ho,int wo,int kh,int kw,
        int sh,int sw,int ph,int pw,int group) {
    if (ci<=0 || co<=0 || group<=0 || ci%group || co%group || co/group<2 ||
        hi!=kh || ho!=1 || kw!=1 || sh!=1 || sw!=1 || ph || pw ||
        wi!=wo || wi<8 || kh<=0) return 0;
    int cig=ci/group,cog=co/group,k=cig*kh;
    for (int g=0;g<group;++g) {
        const float *input=x+g*cig*kh*wi;
        int oc=0;
        for (;oc+1<cog;oc+=2) {
            const float *w0=w+(g*cog+oc)*k,*w1=w0+k;
            float *out0=y+(g*cog+oc)*wo,*out1=out0+wo;
            float initial0=bias?bias[g*cog+oc]:0.0f;
            float initial1=bias?bias[g*cog+oc+1]:0.0f;
            int ow=0;
            for (;ow+31<wo;ow+=32) {
                __m256 a0=_mm256_set1_ps(initial0),a1=a0,a2=a0,a3=a0;
                __m256 b0=_mm256_set1_ps(initial1),b1=b0,b2=b0,b3=b0;
                for (int ic=0;ic<cig;++ic) for (int ky=0;ky<kh;++ky) {
                    int j=ic*kh+ky;
                    const float *p=input+j*wi+ow;
                    __m256 v0=_mm256_loadu_ps(p),v1=_mm256_loadu_ps(p+8);
                    __m256 v2=_mm256_loadu_ps(p+16),v3=_mm256_loadu_ps(p+24);
                    __m256 weight0=_mm256_set1_ps(w0[j]),weight1=_mm256_set1_ps(w1[j]);
                    a0=_mm256_fmadd_ps(weight0,v0,a0);a1=_mm256_fmadd_ps(weight0,v1,a1);
                    a2=_mm256_fmadd_ps(weight0,v2,a2);a3=_mm256_fmadd_ps(weight0,v3,a3);
                    b0=_mm256_fmadd_ps(weight1,v0,b0);b1=_mm256_fmadd_ps(weight1,v1,b1);
                    b2=_mm256_fmadd_ps(weight1,v2,b2);b3=_mm256_fmadd_ps(weight1,v3,b3);
                }
                _mm256_storeu_ps(out0+ow,a0);_mm256_storeu_ps(out0+ow+8,a1);
                _mm256_storeu_ps(out0+ow+16,a2);_mm256_storeu_ps(out0+ow+24,a3);
                _mm256_storeu_ps(out1+ow,b0);_mm256_storeu_ps(out1+ow+8,b1);
                _mm256_storeu_ps(out1+ow+16,b2);_mm256_storeu_ps(out1+ow+24,b3);
            }
            for (;ow+7<wo;ow+=8) {
                __m256 a=_mm256_set1_ps(initial0),b=_mm256_set1_ps(initial1);
                for (int ic=0;ic<cig;++ic) for (int ky=0;ky<kh;++ky) {
                    int j=ic*kh+ky;
                    __m256 value=_mm256_loadu_ps(input+j*wi+ow);
                    a=_mm256_fmadd_ps(_mm256_set1_ps(w0[j]),value,a);
                    b=_mm256_fmadd_ps(_mm256_set1_ps(w1[j]),value,b);
                }
                _mm256_storeu_ps(out0+ow,a);_mm256_storeu_ps(out1+ow,b);
            }
            for (;ow<wo;++ow) {
                float a=initial0,b=initial1;
                for (int ic=0;ic<cig;++ic) for (int ky=0;ky<kh;++ky) {
                    int j=ic*kh+ky;
                    float value=input[j*wi+ow];
                    a+=w0[j]*value;b+=w1[j]*value;
                }
                out0[ow]=a;out1[ow]=b;
            }
        }
        if (oc<cog) {
            /* Reuse the preserved odd-channel kernel for all frequency tails. */
            int handled=dpdf_conv_row_avx2(input,w+(g*cog+oc)*k,
                       bias?bias+g*cog+oc:NULL,y+(g*cog+oc)*wo,
                       cig,kh,wi,1,1,wo,kh,1,1,1,0,0,1);
            if (!handled) return 0; /* Shape guards above imply handled. */
        }
    }
    return 1;
}
#if DPDF_MODEL_SIZE == 2

/* exact surrounding arithmetic; nonlinear calls remain scalar libm.
 * Each eight-lane group consumes every A/B gate slice before output writes.
 * Separate add/mul/div/sub intrinsics preserve FP32 rounding and no FMA. */
void dpdf_generated_gru256_exact_avx2(const float *a,const float *b,
                                    const float *old,float *out) {
    const __m256 one=_mm256_set1_ps(1.0f);
    for (int i=0;i<256;i+=8) {
        float reset_sum[8],update_sum[8],reset_exp[8],update_exp[8];
        float candidate_sum[8],candidate[8];
        _mm256_storeu_ps(reset_sum,_mm256_add_ps(_mm256_loadu_ps(b+i),_mm256_loadu_ps(a+i)));
        _mm256_storeu_ps(update_sum,_mm256_add_ps(_mm256_loadu_ps(b+256+i),_mm256_loadu_ps(a+256+i)));
        for (int t=0;t<8;++t) {
            reset_exp[t]=expf(-reset_sum[t]);
            update_exp[t]=expf(-update_sum[t]);
        }
        __m256 reset=_mm256_div_ps(one,_mm256_add_ps(one,_mm256_loadu_ps(reset_exp)));
        __m256 update=_mm256_div_ps(one,_mm256_add_ps(one,_mm256_loadu_ps(update_exp)));
        __m256 product=_mm256_mul_ps(_mm256_loadu_ps(b+512+i),reset);
        _mm256_storeu_ps(candidate_sum,_mm256_add_ps(_mm256_loadu_ps(a+512+i),product));
        for (int t=0;t<8;++t) candidate[t]=tanhf(candidate_sum[t]);
        __m256 value=_mm256_loadu_ps(candidate);
        __m256 difference=_mm256_sub_ps(_mm256_loadu_ps(old+i),value);
        __m256 update_product=_mm256_mul_ps(difference,update);
        _mm256_storeu_ps(out+i,_mm256_add_ps(update_product,value));
    }
}
#else
#endif
