/* Research-only Linux x86-64 SysV, AVX2/FMA, K=64.
 * Tile count1 or3; every output retains the original FP32 scale and FMA. */
.intel_syntax noprefix
.text
.p2align 5
.globl dpdf_qaffine64_fused_avx2
.hidden dpdf_qaffine64_fused_avx2
.type dpdf_qaffine64_fused_avx2,@function
dpdf_qaffine64_fused_avx2:
    .cfi_startproc
    vbroadcastss ymm15, xmm0
    mov eax, DWORD PTR [rsp+8]
    neg eax
    vmovd xmm14, eax
    vpbroadcastd ymm14, xmm14
    mov r10d, DWORD PTR [rsp+16]
    mov eax, 0x00010001
    vmovd xmm8, eax
    vpbroadcastd ymm8, xmm8
.p2align 5
.Ltile:
    vpmulld ymm0, ymm14, YMMWORD PTR [rdx+0]
    vpmulld ymm1, ymm14, YMMWORD PTR [rdx+32]
    vpmulld ymm2, ymm14, YMMWORD PTR [rdx+64]
    vpmulld ymm3, ymm14, YMMWORD PTR [rdx+96]
    vpmulld ymm4, ymm14, YMMWORD PTR [rdx+128]
    vpmulld ymm5, ymm14, YMMWORD PTR [rdx+160]
    vpmulld ymm6, ymm14, YMMWORD PTR [rdx+192]
    vpmulld ymm7, ymm14, YMMWORD PTR [rdx+224]
    vpbroadcastd ymm9, DWORD PTR [rdi+0]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+0]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+512]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1024]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1536]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2048]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2560]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3072]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3584]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+4]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+32]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+544]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1056]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1568]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2080]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2592]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3104]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3616]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+8]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+64]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+576]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1088]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1600]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2112]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2624]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3136]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3648]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+12]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+96]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+608]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1120]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1632]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2144]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2656]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3168]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3680]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+16]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+128]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+640]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1152]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1664]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2176]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2688]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3200]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3712]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+20]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+160]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+672]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1184]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1696]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2208]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2720]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3232]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3744]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+24]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+192]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+704]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1216]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1728]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2240]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2752]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3264]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3776]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+28]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+224]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+736]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1248]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1760]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2272]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2784]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3296]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3808]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+32]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+256]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+768]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1280]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1792]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2304]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2816]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3328]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3840]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+36]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+288]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+800]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1312]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1824]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2336]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2848]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3360]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3872]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+40]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+320]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+832]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1344]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1856]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2368]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2880]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3392]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3904]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+44]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+352]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+864]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1376]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1888]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2400]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2912]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3424]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3936]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+48]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+384]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+896]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1408]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1920]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2432]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2944]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3456]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+3968]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+52]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+416]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+928]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1440]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1952]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2464]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+2976]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3488]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+4000]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+56]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+448]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+960]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1472]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+1984]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2496]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+3008]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3520]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+4032]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vpbroadcastd ymm9, DWORD PTR [rdi+60]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+480]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+992]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+1504]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+2016]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+2528]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+3040]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+3552]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+4064]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    vcvtdq2ps ymm0, ymm0
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+0]
    vfmadd213ps ymm0, ymm9, YMMWORD PTR [r8+0]
    vmovups YMMWORD PTR [r9+0], ymm0
    vcvtdq2ps ymm1, ymm1
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+32]
    vfmadd213ps ymm1, ymm9, YMMWORD PTR [r8+32]
    vmovups YMMWORD PTR [r9+32], ymm1
    vcvtdq2ps ymm2, ymm2
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+64]
    vfmadd213ps ymm2, ymm9, YMMWORD PTR [r8+64]
    vmovups YMMWORD PTR [r9+64], ymm2
    vcvtdq2ps ymm3, ymm3
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+96]
    vfmadd213ps ymm3, ymm9, YMMWORD PTR [r8+96]
    vmovups YMMWORD PTR [r9+96], ymm3
    vcvtdq2ps ymm4, ymm4
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+128]
    vfmadd213ps ymm4, ymm9, YMMWORD PTR [r8+128]
    vmovups YMMWORD PTR [r9+128], ymm4
    vcvtdq2ps ymm5, ymm5
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+160]
    vfmadd213ps ymm5, ymm9, YMMWORD PTR [r8+160]
    vmovups YMMWORD PTR [r9+160], ymm5
    vcvtdq2ps ymm6, ymm6
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+192]
    vfmadd213ps ymm6, ymm9, YMMWORD PTR [r8+192]
    vmovups YMMWORD PTR [r9+192], ymm6
    vcvtdq2ps ymm7, ymm7
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+224]
    vfmadd213ps ymm7, ymm9, YMMWORD PTR [r8+224]
    vmovups YMMWORD PTR [r9+224], ymm7
    add rsi, 4096
    add rdx, 256
    add rcx, 256
    add r8, 256
    add r9, 256
    dec r10d
    jnz .Ltile
    vzeroupper
    ret
    .cfi_endproc
.size dpdf_qaffine64_fused_avx2,.-dpdf_qaffine64_fused_avx2
.section .note.GNU-stack,"",@progbits
