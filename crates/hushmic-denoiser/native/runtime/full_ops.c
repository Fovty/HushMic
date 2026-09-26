#include "full_ops.h"

/* Disjoint row-major input/output, including arbitrary edge dimensions. */
void dpdf_transpose_scalar(const float *x,float *y,int rows,int cols) {
    for (int r=0;r<rows;r+=8) for (int c=0;c<cols;c+=8)
        for (int i=r;i<r+8 && i<rows;++i) for (int j=c;j<c+8 && j<cols;++j)
            y[j*rows+i]=x[i*cols+j];
}

void dpdf_axpy_scalar(float *y, const float *x, float a, int n) {
    for (int i=0; i<n; ++i) y[i] += a*x[i];
}

/* NCHW, batch=1, unit dilation. Accumulate contiguous spatial rows so the
 * dense 1x1 and 1x3 paths vectorize across frequency without transposing. */
void dpdf_conv(dpdf_axpy_fn axpy, const float *x, const float *w, const float *bias,
               float *y, int ci, int hi, int wi, int co, int ho, int wo,
               int kh, int kw, int sh, int sw, int ph, int pw, int group) {
#ifdef DPDF_X86_DISPATCH
    if (axpy==dpdf_axpy_avx2 && dpdf_conv_row_avx2(x,w,bias,y,ci,hi,wi,co,ho,wo,
                                               kh,kw,sh,sw,ph,pw,group)) return;
#endif
    int cig=ci/group, cog=co/group;
    for (int oc=0; oc<co; ++oc) {
        float *out=y+oc*ho*wo;
        for (int i=0; i<ho*wo; ++i) out[i]=bias ? bias[oc] : 0;
        for (int ic=0; ic<cig; ++ic) for (int ky=0; ky<kh; ++ky) for (int kx=0; kx<kw; ++kx) {
            float weight=w[((oc*cig+ic)*kh+ky)*kw+kx];
            for (int oh=0; oh<ho; ++oh) {
                int ih=oh*sh+ky-ph;
                if (ih<0 || ih>=hi) continue;
                const float *in=x+((oc/cog*cig+ic)*hi+ih)*wi;
                int begin=0, end=wo;
                while (begin<end && begin*sw+kx-pw<0) ++begin;
                while (end>begin && (end-1)*sw+kx-pw>=wi) --end;
                if (sw==1) axpy(out+oh*wo+begin,in+begin+kx-pw,weight,end-begin);
                else for (int ow=begin; ow<end; ++ow) out[oh*wo+ow]+=weight*in[ow*sw+kx-pw];
            }
        }
    }
}
