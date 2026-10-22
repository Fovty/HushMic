/* Optional AVX-VNNI translation unit, entered only after CPU detection. */
#include "int8_matrix.h"
#include <immintrin.h>
#include <math.h>
#include <string.h>
#define DPDF_INT8_VNNI 1
#define DPDF_QAFFINE_IMPL dpdf_qaffine_vnni
#define DPDF_QPAIR_IMPL dpdf_qaffine_pair_vnni
#include "int8_kernels.inc"
