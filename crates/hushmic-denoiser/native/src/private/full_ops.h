#ifndef DPDF_FULL_OPS_H
#define DPDF_FULL_OPS_H
#include "internal.h"
typedef void (*dpdf_axpy_fn)(float *, const float *, float, int);
typedef void (*dpdf_transpose_fn)(const float *, float *, int, int);
void dpdf_transpose_scalar(const float *, float *, int, int);
void dpdf_axpy_scalar(float *, const float *, float, int);
void dpdf_generated_gru256_fused(const float *,const float *,const float *,float *);
#ifdef DPDF_X86_DISPATCH
void dpdf_generated_gru256_exact_avx2(const float *,const float *,const float *,float *);
void dpdf_axpy_avx2(float *, const float *, float, int);
void dpdf_transpose_avx2(const float *, float *, int, int);
int dpdf_conv_row_pair_avx2(const float *,const float *,const float *,float *,
                      int,int,int,int,int,int,int,int,int,int,int,int,int);
int dpdf_depthwise_stride_avx2(const float *, const float *, const float *, float *,
                      int, int, int, int, int, int, int, int, int, int, int, int, int);
int dpdf_conv_row_avx2(const float *, const float *, const float *, float *,
                      int, int, int, int, int, int, int, int, int, int, int, int, int);
#endif
void dpdf_conv(dpdf_axpy_fn axpy, const float *x, const float *w, const float *bias,
               float *y, int ci, int hi, int wi, int co, int ho, int wo,
               int kh, int kw, int sh, int sw, int ph, int pw, int group);
#endif
