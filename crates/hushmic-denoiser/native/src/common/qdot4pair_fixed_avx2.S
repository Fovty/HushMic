/* Research-only Linux x86-64 SysV, AVX2, unsigned A8 / signed W7.
 * RDI four activation rows; RSI/RDX two packed eight-column weight tiles;
 * RCX output: tile0 rows0..3, then tile1 rows0..3. No stack or callee saves. */
.intel_syntax noprefix
.text
.p2align 5
.globl dpdf_qdot4pair64_avx2
.hidden dpdf_qdot4pair64_avx2
.type dpdf_qdot4pair64_avx2,@function
dpdf_qdot4pair64_avx2:
    .cfi_startproc
    mov eax, 0x00010001
    vmovd xmm8, eax
    vpbroadcastd ymm8, xmm8
    vpxor ymm0, ymm0, ymm0
    vpxor ymm1, ymm1, ymm1
    vpxor ymm2, ymm2, ymm2
    vpxor ymm3, ymm3, ymm3
    vpxor ymm4, ymm4, ymm4
    vpxor ymm5, ymm5, ymm5
    vpxor ymm6, ymm6, ymm6
    vpxor ymm7, ymm7, ymm7
    vmovdqu ymm9, YMMWORD PTR [rsi+0]
    vmovdqu ymm10, YMMWORD PTR [rdx+0]
    vpbroadcastd ymm12, DWORD PTR [rdi+0]
    vpbroadcastd ymm13, DWORD PTR [rdi+64]
    vpbroadcastd ymm14, DWORD PTR [rdi+128]
    vpbroadcastd ymm15, DWORD PTR [rdi+192]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+32]
    vmovdqu ymm10, YMMWORD PTR [rdx+32]
    vpbroadcastd ymm12, DWORD PTR [rdi+4]
    vpbroadcastd ymm13, DWORD PTR [rdi+68]
    vpbroadcastd ymm14, DWORD PTR [rdi+132]
    vpbroadcastd ymm15, DWORD PTR [rdi+196]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+64]
    vmovdqu ymm10, YMMWORD PTR [rdx+64]
    vpbroadcastd ymm12, DWORD PTR [rdi+8]
    vpbroadcastd ymm13, DWORD PTR [rdi+72]
    vpbroadcastd ymm14, DWORD PTR [rdi+136]
    vpbroadcastd ymm15, DWORD PTR [rdi+200]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+96]
    vmovdqu ymm10, YMMWORD PTR [rdx+96]
    vpbroadcastd ymm12, DWORD PTR [rdi+12]
    vpbroadcastd ymm13, DWORD PTR [rdi+76]
    vpbroadcastd ymm14, DWORD PTR [rdi+140]
    vpbroadcastd ymm15, DWORD PTR [rdi+204]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+128]
    vmovdqu ymm10, YMMWORD PTR [rdx+128]
    vpbroadcastd ymm12, DWORD PTR [rdi+16]
    vpbroadcastd ymm13, DWORD PTR [rdi+80]
    vpbroadcastd ymm14, DWORD PTR [rdi+144]
    vpbroadcastd ymm15, DWORD PTR [rdi+208]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+160]
    vmovdqu ymm10, YMMWORD PTR [rdx+160]
    vpbroadcastd ymm12, DWORD PTR [rdi+20]
    vpbroadcastd ymm13, DWORD PTR [rdi+84]
    vpbroadcastd ymm14, DWORD PTR [rdi+148]
    vpbroadcastd ymm15, DWORD PTR [rdi+212]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+192]
    vmovdqu ymm10, YMMWORD PTR [rdx+192]
    vpbroadcastd ymm12, DWORD PTR [rdi+24]
    vpbroadcastd ymm13, DWORD PTR [rdi+88]
    vpbroadcastd ymm14, DWORD PTR [rdi+152]
    vpbroadcastd ymm15, DWORD PTR [rdi+216]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+224]
    vmovdqu ymm10, YMMWORD PTR [rdx+224]
    vpbroadcastd ymm12, DWORD PTR [rdi+28]
    vpbroadcastd ymm13, DWORD PTR [rdi+92]
    vpbroadcastd ymm14, DWORD PTR [rdi+156]
    vpbroadcastd ymm15, DWORD PTR [rdi+220]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+256]
    vmovdqu ymm10, YMMWORD PTR [rdx+256]
    vpbroadcastd ymm12, DWORD PTR [rdi+32]
    vpbroadcastd ymm13, DWORD PTR [rdi+96]
    vpbroadcastd ymm14, DWORD PTR [rdi+160]
    vpbroadcastd ymm15, DWORD PTR [rdi+224]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+288]
    vmovdqu ymm10, YMMWORD PTR [rdx+288]
    vpbroadcastd ymm12, DWORD PTR [rdi+36]
    vpbroadcastd ymm13, DWORD PTR [rdi+100]
    vpbroadcastd ymm14, DWORD PTR [rdi+164]
    vpbroadcastd ymm15, DWORD PTR [rdi+228]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+320]
    vmovdqu ymm10, YMMWORD PTR [rdx+320]
    vpbroadcastd ymm12, DWORD PTR [rdi+40]
    vpbroadcastd ymm13, DWORD PTR [rdi+104]
    vpbroadcastd ymm14, DWORD PTR [rdi+168]
    vpbroadcastd ymm15, DWORD PTR [rdi+232]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+352]
    vmovdqu ymm10, YMMWORD PTR [rdx+352]
    vpbroadcastd ymm12, DWORD PTR [rdi+44]
    vpbroadcastd ymm13, DWORD PTR [rdi+108]
    vpbroadcastd ymm14, DWORD PTR [rdi+172]
    vpbroadcastd ymm15, DWORD PTR [rdi+236]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+384]
    vmovdqu ymm10, YMMWORD PTR [rdx+384]
    vpbroadcastd ymm12, DWORD PTR [rdi+48]
    vpbroadcastd ymm13, DWORD PTR [rdi+112]
    vpbroadcastd ymm14, DWORD PTR [rdi+176]
    vpbroadcastd ymm15, DWORD PTR [rdi+240]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+416]
    vmovdqu ymm10, YMMWORD PTR [rdx+416]
    vpbroadcastd ymm12, DWORD PTR [rdi+52]
    vpbroadcastd ymm13, DWORD PTR [rdi+116]
    vpbroadcastd ymm14, DWORD PTR [rdi+180]
    vpbroadcastd ymm15, DWORD PTR [rdi+244]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+448]
    vmovdqu ymm10, YMMWORD PTR [rdx+448]
    vpbroadcastd ymm12, DWORD PTR [rdi+56]
    vpbroadcastd ymm13, DWORD PTR [rdi+120]
    vpbroadcastd ymm14, DWORD PTR [rdi+184]
    vpbroadcastd ymm15, DWORD PTR [rdi+248]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu ymm9, YMMWORD PTR [rsi+480]
    vmovdqu ymm10, YMMWORD PTR [rdx+480]
    vpbroadcastd ymm12, DWORD PTR [rdi+60]
    vpbroadcastd ymm13, DWORD PTR [rdi+124]
    vpbroadcastd ymm14, DWORD PTR [rdi+188]
    vpbroadcastd ymm15, DWORD PTR [rdi+252]
    vpmaddubsw ymm11, ymm12, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm0, ymm0, ymm11
    vpmaddubsw ymm11, ymm13, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddubsw ymm11, ymm14, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm2, ymm2, ymm11
    vpmaddubsw ymm11, ymm15, ymm9
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm3, ymm3, ymm11
    vpmaddubsw ymm11, ymm12, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm4, ymm4, ymm11
    vpmaddubsw ymm11, ymm13, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddubsw ymm11, ymm14, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm6, ymm6, ymm11
    vpmaddubsw ymm11, ymm15, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm7, ymm7, ymm11
    vmovdqu YMMWORD PTR [rcx+0], ymm0
    vmovdqu YMMWORD PTR [rcx+32], ymm1
    vmovdqu YMMWORD PTR [rcx+64], ymm2
    vmovdqu YMMWORD PTR [rcx+96], ymm3
    vmovdqu YMMWORD PTR [rcx+128], ymm4
    vmovdqu YMMWORD PTR [rcx+160], ymm5
    vmovdqu YMMWORD PTR [rcx+192], ymm6
    vmovdqu YMMWORD PTR [rcx+224], ymm7
    vzeroupper
    ret
    .cfi_endproc
.size dpdf_qdot4pair64_avx2,.-dpdf_qdot4pair64_avx2
.section .note.GNU-stack,"",@progbits
