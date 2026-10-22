/* Linux x86-64 SysV AVX2/FMA, four K64 rows and two eight-column tiles.
 * RDI points to the read-only 128-byte pair_fused_args contract.
 * Increasing-K sums and the separate FP32 scale product are unchanged.
 * Only caller-saved GPR/YMM registers; no stack writes or allocations. */
.intel_syntax noprefix
.text
.p2align 5
.globl dpdf_qaffine4pair64_fused_avx2
.hidden dpdf_qaffine4pair64_fused_avx2
.type dpdf_qaffine4pair64_fused_avx2,@function
dpdf_qaffine4pair64_fused_avx2:
    .cfi_startproc
    mov rcx, QWORD PTR [rdi+0]
    mov rsi, QWORD PTR [rdi+8]
    mov rdx, QWORD PTR [rdi+16]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+0]
    vpbroadcastd ymm13, DWORD PTR [rcx+64]
    vpbroadcastd ymm14, DWORD PTR [rcx+128]
    vpbroadcastd ymm15, DWORD PTR [rcx+192]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+4]
    vpbroadcastd ymm13, DWORD PTR [rcx+68]
    vpbroadcastd ymm14, DWORD PTR [rcx+132]
    vpbroadcastd ymm15, DWORD PTR [rcx+196]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+8]
    vpbroadcastd ymm13, DWORD PTR [rcx+72]
    vpbroadcastd ymm14, DWORD PTR [rcx+136]
    vpbroadcastd ymm15, DWORD PTR [rcx+200]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+12]
    vpbroadcastd ymm13, DWORD PTR [rcx+76]
    vpbroadcastd ymm14, DWORD PTR [rcx+140]
    vpbroadcastd ymm15, DWORD PTR [rcx+204]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+16]
    vpbroadcastd ymm13, DWORD PTR [rcx+80]
    vpbroadcastd ymm14, DWORD PTR [rcx+144]
    vpbroadcastd ymm15, DWORD PTR [rcx+208]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+20]
    vpbroadcastd ymm13, DWORD PTR [rcx+84]
    vpbroadcastd ymm14, DWORD PTR [rcx+148]
    vpbroadcastd ymm15, DWORD PTR [rcx+212]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+24]
    vpbroadcastd ymm13, DWORD PTR [rcx+88]
    vpbroadcastd ymm14, DWORD PTR [rcx+152]
    vpbroadcastd ymm15, DWORD PTR [rcx+216]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+28]
    vpbroadcastd ymm13, DWORD PTR [rcx+92]
    vpbroadcastd ymm14, DWORD PTR [rcx+156]
    vpbroadcastd ymm15, DWORD PTR [rcx+220]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+32]
    vpbroadcastd ymm13, DWORD PTR [rcx+96]
    vpbroadcastd ymm14, DWORD PTR [rcx+160]
    vpbroadcastd ymm15, DWORD PTR [rcx+224]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+36]
    vpbroadcastd ymm13, DWORD PTR [rcx+100]
    vpbroadcastd ymm14, DWORD PTR [rcx+164]
    vpbroadcastd ymm15, DWORD PTR [rcx+228]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+40]
    vpbroadcastd ymm13, DWORD PTR [rcx+104]
    vpbroadcastd ymm14, DWORD PTR [rcx+168]
    vpbroadcastd ymm15, DWORD PTR [rcx+232]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+44]
    vpbroadcastd ymm13, DWORD PTR [rcx+108]
    vpbroadcastd ymm14, DWORD PTR [rcx+172]
    vpbroadcastd ymm15, DWORD PTR [rcx+236]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+48]
    vpbroadcastd ymm13, DWORD PTR [rcx+112]
    vpbroadcastd ymm14, DWORD PTR [rcx+176]
    vpbroadcastd ymm15, DWORD PTR [rcx+240]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+52]
    vpbroadcastd ymm13, DWORD PTR [rcx+116]
    vpbroadcastd ymm14, DWORD PTR [rcx+180]
    vpbroadcastd ymm15, DWORD PTR [rcx+244]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+56]
    vpbroadcastd ymm13, DWORD PTR [rcx+120]
    vpbroadcastd ymm14, DWORD PTR [rcx+184]
    vpbroadcastd ymm15, DWORD PTR [rcx+248]
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
    vpbroadcastd ymm12, DWORD PTR [rcx+60]
    vpbroadcastd ymm13, DWORD PTR [rcx+124]
    vpbroadcastd ymm14, DWORD PTR [rcx+188]
    vpbroadcastd ymm15, DWORD PTR [rcx+252]
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
    mov rax, QWORD PTR [rdi+24]
    vmovdqu ymm8, YMMWORD PTR [rax]
    mov rax, QWORD PTR [rdi+32]
    vmovdqu ymm9, YMMWORD PTR [rax]
    mov rax, QWORD PTR [rdi+40]
    vmovdqu ymm10, YMMWORD PTR [rax]
    mov rax, QWORD PTR [rdi+48]
    vmovdqu ymm11, YMMWORD PTR [rax]
    mov rcx, QWORD PTR [rdi+56]
    mov rdx, QWORD PTR [rdi+64]
    mov r8, QWORD PTR [rdi+72]
    mov r9, QWORD PTR [rdi+80]
    mov r10, QWORD PTR [rdi+88]
    mov eax, DWORD PTR [rdi+112]
    neg eax
    vmovd xmm12, eax
    vpbroadcastd ymm12, xmm12
    vbroadcastss ymm14, DWORD PTR [rdi+96]
    vpmulld ymm13, ymm8, ymm12
    vpaddd ymm0, ymm0, ymm13
    vcvtdq2ps ymm0, ymm0
    vmulps ymm15, ymm14, ymm10
    vfmadd213ps ymm0, ymm15, YMMWORD PTR [rcx]
    vmovups YMMWORD PTR [r8], ymm0
    vpmulld ymm13, ymm9, ymm12
    vpaddd ymm4, ymm4, ymm13
    vcvtdq2ps ymm4, ymm4
    vmulps ymm15, ymm14, ymm11
    vfmadd213ps ymm4, ymm15, YMMWORD PTR [rdx]
    vmovups YMMWORD PTR [r9], ymm4
    add r8, r10
    add r9, r10
    mov eax, DWORD PTR [rdi+116]
    neg eax
    vmovd xmm12, eax
    vpbroadcastd ymm12, xmm12
    vbroadcastss ymm14, DWORD PTR [rdi+100]
    vpmulld ymm13, ymm8, ymm12
    vpaddd ymm1, ymm1, ymm13
    vcvtdq2ps ymm1, ymm1
    vmulps ymm15, ymm14, ymm10
    vfmadd213ps ymm1, ymm15, YMMWORD PTR [rcx]
    vmovups YMMWORD PTR [r8], ymm1
    vpmulld ymm13, ymm9, ymm12
    vpaddd ymm5, ymm5, ymm13
    vcvtdq2ps ymm5, ymm5
    vmulps ymm15, ymm14, ymm11
    vfmadd213ps ymm5, ymm15, YMMWORD PTR [rdx]
    vmovups YMMWORD PTR [r9], ymm5
    add r8, r10
    add r9, r10
    mov eax, DWORD PTR [rdi+120]
    neg eax
    vmovd xmm12, eax
    vpbroadcastd ymm12, xmm12
    vbroadcastss ymm14, DWORD PTR [rdi+104]
    vpmulld ymm13, ymm8, ymm12
    vpaddd ymm2, ymm2, ymm13
    vcvtdq2ps ymm2, ymm2
    vmulps ymm15, ymm14, ymm10
    vfmadd213ps ymm2, ymm15, YMMWORD PTR [rcx]
    vmovups YMMWORD PTR [r8], ymm2
    vpmulld ymm13, ymm9, ymm12
    vpaddd ymm6, ymm6, ymm13
    vcvtdq2ps ymm6, ymm6
    vmulps ymm15, ymm14, ymm11
    vfmadd213ps ymm6, ymm15, YMMWORD PTR [rdx]
    vmovups YMMWORD PTR [r9], ymm6
    add r8, r10
    add r9, r10
    mov eax, DWORD PTR [rdi+124]
    neg eax
    vmovd xmm12, eax
    vpbroadcastd ymm12, xmm12
    vbroadcastss ymm14, DWORD PTR [rdi+108]
    vpmulld ymm13, ymm8, ymm12
    vpaddd ymm3, ymm3, ymm13
    vcvtdq2ps ymm3, ymm3
    vmulps ymm15, ymm14, ymm10
    vfmadd213ps ymm3, ymm15, YMMWORD PTR [rcx]
    vmovups YMMWORD PTR [r8], ymm3
    vpmulld ymm13, ymm9, ymm12
    vpaddd ymm7, ymm7, ymm13
    vcvtdq2ps ymm7, ymm7
    vmulps ymm15, ymm14, ymm11
    vfmadd213ps ymm7, ymm15, YMMWORD PTR [rdx]
    vmovups YMMWORD PTR [r9], ymm7
    vzeroupper
    ret
    .cfi_endproc
.size dpdf_qaffine4pair64_fused_avx2,.-dpdf_qaffine4pair64_fused_avx2
.section .note.GNU-stack,"",@progbits
