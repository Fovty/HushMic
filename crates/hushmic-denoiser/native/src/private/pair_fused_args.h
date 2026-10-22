#ifndef DPDF_PAIR_FUSED_ARGS_H
#define DPDF_PAIR_FUSED_ARGS_H
#include <stddef.h>
#include <stdint.h>

/* Private Linux x86-64 SysV leaf contract, exactly four K64 activation rows.
 * Weight tiles each contain 64*8 packed bytes; each metadata vector has8 lanes.
 * Each output row writes8 floats, with stride_bytes bytes between rows.
 * All inputs/metadata remain read-only; normal native callers own each output.
 */
typedef struct {
    const int8_t *activation;
    const int8_t *packed0,*packed1;
    const int32_t *sum0,*sum1;
    const float *scale0,*scale1;
    const float *bias0,*bias1;
    float *output0,*output1;
    size_t stride_bytes;
    float activation_scale[4];
    int zero_point[4];
} dpdf_pair_fused_args;

_Static_assert(sizeof(dpdf_pair_fused_args)==128,"Fused-pair SysV argument size");
_Static_assert(offsetof(dpdf_pair_fused_args,activation)==0,"Fused-pair activation offset");
_Static_assert(offsetof(dpdf_pair_fused_args,packed0)==8,"Fused-pair packed0 offset");
_Static_assert(offsetof(dpdf_pair_fused_args,packed1)==16,"Fused-pair packed1 offset");
_Static_assert(offsetof(dpdf_pair_fused_args,sum0)==24,"Fused-pair sum0 offset");
_Static_assert(offsetof(dpdf_pair_fused_args,sum1)==32,"Fused-pair sum1 offset");
_Static_assert(offsetof(dpdf_pair_fused_args,scale0)==40,"Fused-pair scale0 offset");
_Static_assert(offsetof(dpdf_pair_fused_args,scale1)==48,"Fused-pair scale1 offset");
_Static_assert(offsetof(dpdf_pair_fused_args,bias0)==56,"Fused-pair bias0 offset");
_Static_assert(offsetof(dpdf_pair_fused_args,bias1)==64,"Fused-pair bias1 offset");
_Static_assert(offsetof(dpdf_pair_fused_args,output0)==72,"Fused-pair output0 offset");
_Static_assert(offsetof(dpdf_pair_fused_args,output1)==80,"Fused-pair output1 offset");
_Static_assert(offsetof(dpdf_pair_fused_args,stride_bytes)==88,"Fused-pair stride offset");
_Static_assert(offsetof(dpdf_pair_fused_args,activation_scale)==96,"Fused-pair scale offset");
_Static_assert(offsetof(dpdf_pair_fused_args,zero_point)==112,"Fused-pair zero-point offset");
void dpdf_qaffine4pair64_fused_avx2(const dpdf_pair_fused_args *);
#endif
