#include "extended_ops.h"
#include <stdlib.h>
#include <string.h>
#include <math.h>
struct dpdf_dense {
    int k,n,kp,np,precision;
    float *w,*bias,*x,*y;
    uint16_t *half;
    dpdf_affine_fn affine;
#ifdef DPDF_X86_DISPATCH
    dpdf_qmatrix *q;
#endif
};
void dpdf_dense_destroy(dpdf_dense *d) {
    if (!d) return;
    free(d->w); free(d->bias); free(d->x); free(d->y); free(d->half);
#ifdef DPDF_X86_DISPATCH
    dpdf_qdestroy(d->q);
#endif
    free(d);
}
dpdf_dense *dpdf_dense_create(const float *w,const float *bias,int k,int n,int precision,int tiled) {
    if (!w || k<=0 || k>512 || n<=0 || n>1024 || (precision!=0 && precision!=8 && precision!=16)) return NULL;
    if ((precision==8 && !dpdf_has_avx2()) || (precision==16 && !dpdf_has_fp16())) return NULL;
    if (tiled && (tiled!=64 || n%64)) return NULL;
    dpdf_dense *d=calloc(1,sizeof(*d)); if (!d) return NULL;
    d->k=k; d->n=n; d->kp=(k+7)/8*8; d->np=(n+31)/32*32; d->precision=precision;
    d->w=calloc((size_t)d->kp*d->np,sizeof(float));
    d->bias=calloc(d->np,sizeof(float));
    if (d->k!=d->kp) d->x=calloc((size_t)48*d->kp,sizeof(float));
    if (d->n!=d->np) d->y=malloc((size_t)48*d->np*sizeof(float));
    if (!d->w || !d->bias || (d->k!=d->kp && !d->x) || (d->n!=d->np && !d->y)) { dpdf_dense_destroy(d); return NULL; }
    for (int j=0;j<k;++j) for (int c=0;c<n;++c) {
        float v=w[tiled?(c/64)*k*64+j*64+c%64:j*n+c];
        if (!isfinite(v) || (precision==16 && fabsf(v)>65504)) { dpdf_dense_destroy(d); return NULL; }
        d->w[j*d->np+c]=v;
    }
    if (bias) memcpy(d->bias,bias,n*sizeof(float));
    d->affine=dpdf_affine_scalar;
#ifdef DPDF_X86_DISPATCH
    if (dpdf_has_avx2()) d->affine=dpdf_affine_avx2;
    if (precision==16) {
        d->half=malloc((size_t)d->kp*d->np*sizeof(uint16_t));
        if (!d->half) { dpdf_dense_destroy(d); return NULL; }
        dpdf_pack_fp16(d->w,d->half,(size_t)d->kp*d->np);
    } else if (precision==8) {
        d->q=dpdf_qcreate(d->w,d->kp,d->np);
        if (!d->q) { dpdf_dense_destroy(d); return NULL; }
    }
    if (precision) { free(d->w); d->w=NULL; }
#endif
    return d;
}
void dpdf_dense_run(dpdf_dense *d,const float *x,float *y,int rows) {
    for (int start=0;start<rows;start+=48) {
        int m=rows-start; if (m>48) m=48;
        const float *input=x+start*d->k;
        if (d->k!=d->kp) {
            for (int r=0;r<m;++r) memcpy(d->x+r*d->kp,input+r*d->k,d->k*sizeof(float));
            input=d->x;
        }
        float *output=d->np==d->n ? y+start*d->n : d->y;
#ifdef DPDF_X86_DISPATCH
        if (d->precision==16) dpdf_affine_fp16(input,d->half,d->bias,output,m,d->kp,d->np);
        else if (d->precision==8) dpdf_qaffine(d->q,input,d->bias,output,m);
        else
#endif
        d->affine(input,d->w,d->bias,output,m,d->kp,d->np);
        if (output==d->y) for (int r=0;r<m;++r) memcpy(y+(start+r)*d->n,output+r*d->np,d->n*sizeof(float));
    }
}
size_t dpdf_dense_bytes(const dpdf_dense *d) {
    if (!d) return 0;
    size_t b=sizeof(*d)+(size_t)d->np*4;
    if (d->x) b+=(size_t)48*d->kp*4;
    if (d->y) b+=(size_t)48*d->np*4;
    if (d->w) b+=(size_t)d->kp*d->np*4;
    if (d->half) b+=(size_t)d->kp*d->np*2;
#ifdef DPDF_X86_DISPATCH
    b+=dpdf_qbytes(d->q);
#endif
    return b;
}

struct dpdf_convop {
    int ci,hi,wi,co,ho,wo,kh,kw,sh,sw,ph,pw,group,precision;
    float *w,*bias,*patch,*out;
    dpdf_dense **dense;
    dpdf_axpy_fn axpy;
};
void dpdf_convop_destroy(dpdf_convop *c) {
    if (!c) return;
    if (c->dense) for (int g=0;g<c->group;++g) dpdf_dense_destroy(c->dense[g]);
    free(c->dense); free(c->w); free(c->bias); free(c->patch); free(c->out); free(c);
}
dpdf_convop *dpdf_convop_create(const float *w,const float *bias,int ci,int hi,int wi,int co,int ho,int wo,
 int kh,int kw,int sh,int sw,int ph,int pw,int group,int precision) {
    dpdf_convop *c=calloc(1,sizeof(*c)); if (!c) return NULL;
    c->ci=ci;c->hi=hi;c->wi=wi;c->co=co;c->ho=ho;c->wo=wo;c->kh=kh;c->kw=kw;
    c->sh=sh;c->sw=sw;c->ph=ph;c->pw=pw;c->group=group;c->precision=precision;
    c->axpy=dpdf_axpy_scalar;
#ifdef DPDF_X86_DISPATCH
    if (dpdf_has_avx2()) c->axpy=dpdf_axpy_avx2;
#endif
    int k=ci/group*kh*kw,n=co/group;
    if (!precision) {
        c->w=malloc((size_t)co*k*4);c->bias=calloc(co,4);
        if (!c->w || !c->bias) { dpdf_convop_destroy(c); return NULL; }
        memcpy(c->w,w,(size_t)co*k*4); if (bias) memcpy(c->bias,bias,co*4);
    } else {
        c->dense=calloc(group,sizeof(dpdf_dense *));c->patch=malloc((size_t)48*k*4);c->out=malloc((size_t)48*n*4);
        float *packed=malloc((size_t)k*n*4);
        if (!c->dense || !c->patch || !c->out || !packed) { free(packed);dpdf_convop_destroy(c);return NULL; }
        for (int g=0;g<group;++g) {
            for (int j=0;j<k;++j) for (int oc=0;oc<n;++oc) packed[j*n+oc]=w[(g*n+oc)*k+j];
            c->dense[g]=dpdf_dense_create(packed,bias?bias+g*n:NULL,k,n,precision,0);
            if (!c->dense[g]) { free(packed);dpdf_convop_destroy(c);return NULL; }
        }
        free(packed);
    }
    return c;
}
void dpdf_convop_run(dpdf_convop *c,const float *x,float *y) {
    if (!c->precision) {
        dpdf_conv(c->axpy,x,c->w,c->bias,y,c->ci,c->hi,c->wi,c->co,c->ho,c->wo,c->kh,c->kw,c->sh,c->sw,c->ph,c->pw,c->group);
        return;
    }
    int k=c->ci/c->group*c->kh*c->kw,n=c->co/c->group,spatial=c->ho*c->wo;
    for (int g=0;g<c->group;++g) for (int start=0;start<spatial;start+=48) {
        int count=spatial-start;if (count>48) count=48;
        for (int r=0;r<count;++r) {
            int oh=(start+r)/c->wo,ow=(start+r)%c->wo,j=0;
            for (int ic=0;ic<c->ci/c->group;++ic) for (int ky=0;ky<c->kh;++ky) for (int kx=0;kx<c->kw;++kx) {
                int ih=oh*c->sh+ky-c->ph,iw=ow*c->sw+kx-c->pw;
                c->patch[r*k+j++]=(ih<0 || ih>=c->hi || iw<0 || iw>=c->wi)?0:x[((g*(c->ci/c->group)+ic)*c->hi+ih)*c->wi+iw];
            }
        }
        dpdf_dense_run(c->dense[g],c->patch,c->out,count);
        for (int r=0;r<count;++r) for (int oc=0;oc<n;++oc) y[(g*n+oc)*spatial+start+r]=c->out[r*n+oc];
    }
}
size_t dpdf_convop_bytes(const dpdf_convop *c) {
    if (!c) return 0;
    size_t b=sizeof(*c),k=(size_t)c->ci/c->group*c->kh*c->kw,n=c->co/c->group;
    if (!c->precision) return b+c->co*k*4+c->co*4;
    b+=c->group*sizeof(dpdf_dense *)+48*(k+n)*4;
    for (int g=0;g<c->group;++g) b+=dpdf_dense_bytes(c->dense[g]);
    return b;
}
