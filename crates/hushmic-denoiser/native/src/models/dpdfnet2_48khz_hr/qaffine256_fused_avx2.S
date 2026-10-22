/* Research-only Linux x86-64 SysV, AVX2/FMA, fixed K=256.
 * Eight independent output vectors per tile; native N768 dispatch uses12 tiles.
 * Weight grid [-63,63], unsigned activation grid [0,254]. */
.intel_syntax noprefix
.text
.p2align 5
.globl dpdf_qaffine256_fused_avx2
.hidden dpdf_qaffine256_fused_avx2
.type dpdf_qaffine256_fused_avx2,@function
dpdf_qaffine256_fused_avx2:
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
.Lwide_tile:
    vpxor ymm0, ymm0, ymm0
    vpxor ymm1, ymm1, ymm1
    vpxor ymm2, ymm2, ymm2
    vpxor ymm3, ymm3, ymm3
    vpxor ymm4, ymm4, ymm4
    vpxor ymm5, ymm5, ymm5
    vpxor ymm6, ymm6, ymm6
    vpxor ymm7, ymm7, ymm7
    xor r11d, r11d
.p2align 5
.Lwide_k:
    vpbroadcastd ymm9, DWORD PTR [rdi+r11]
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+r11*8+0]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+r11*8+2048]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+r11*8+4096]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+r11*8+6144]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm0, ymm0, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm1, ymm1, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm2, ymm2, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm3, ymm3, ymm13
    vpmaddubsw ymm10, ymm9, YMMWORD PTR [rsi+r11*8+8192]
    vpmaddubsw ymm11, ymm9, YMMWORD PTR [rsi+r11*8+10240]
    vpmaddubsw ymm12, ymm9, YMMWORD PTR [rsi+r11*8+12288]
    vpmaddubsw ymm13, ymm9, YMMWORD PTR [rsi+r11*8+14336]
    vpmaddwd ymm10, ymm10, ymm8
    vpaddd ymm4, ymm4, ymm10
    vpmaddwd ymm11, ymm11, ymm8
    vpaddd ymm5, ymm5, ymm11
    vpmaddwd ymm12, ymm12, ymm8
    vpaddd ymm6, ymm6, ymm12
    vpmaddwd ymm13, ymm13, ymm8
    vpaddd ymm7, ymm7, ymm13
    add r11d, 4
    cmp r11d, 256
    jb .Lwide_k
    vpmulld ymm9, ymm14, YMMWORD PTR [rdx+0]
    vpaddd ymm0, ymm0, ymm9
    vcvtdq2ps ymm0, ymm0
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+0]
    vfmadd213ps ymm0, ymm9, YMMWORD PTR [r8+0]
    vmovups YMMWORD PTR [r9+0], ymm0
    vpmulld ymm9, ymm14, YMMWORD PTR [rdx+32]
    vpaddd ymm1, ymm1, ymm9
    vcvtdq2ps ymm1, ymm1
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+32]
    vfmadd213ps ymm1, ymm9, YMMWORD PTR [r8+32]
    vmovups YMMWORD PTR [r9+32], ymm1
    vpmulld ymm9, ymm14, YMMWORD PTR [rdx+64]
    vpaddd ymm2, ymm2, ymm9
    vcvtdq2ps ymm2, ymm2
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+64]
    vfmadd213ps ymm2, ymm9, YMMWORD PTR [r8+64]
    vmovups YMMWORD PTR [r9+64], ymm2
    vpmulld ymm9, ymm14, YMMWORD PTR [rdx+96]
    vpaddd ymm3, ymm3, ymm9
    vcvtdq2ps ymm3, ymm3
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+96]
    vfmadd213ps ymm3, ymm9, YMMWORD PTR [r8+96]
    vmovups YMMWORD PTR [r9+96], ymm3
    vpmulld ymm9, ymm14, YMMWORD PTR [rdx+128]
    vpaddd ymm4, ymm4, ymm9
    vcvtdq2ps ymm4, ymm4
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+128]
    vfmadd213ps ymm4, ymm9, YMMWORD PTR [r8+128]
    vmovups YMMWORD PTR [r9+128], ymm4
    vpmulld ymm9, ymm14, YMMWORD PTR [rdx+160]
    vpaddd ymm5, ymm5, ymm9
    vcvtdq2ps ymm5, ymm5
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+160]
    vfmadd213ps ymm5, ymm9, YMMWORD PTR [r8+160]
    vmovups YMMWORD PTR [r9+160], ymm5
    vpmulld ymm9, ymm14, YMMWORD PTR [rdx+192]
    vpaddd ymm6, ymm6, ymm9
    vcvtdq2ps ymm6, ymm6
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+192]
    vfmadd213ps ymm6, ymm9, YMMWORD PTR [r8+192]
    vmovups YMMWORD PTR [r9+192], ymm6
    vpmulld ymm9, ymm14, YMMWORD PTR [rdx+224]
    vpaddd ymm7, ymm7, ymm9
    vcvtdq2ps ymm7, ymm7
    vmulps ymm9, ymm15, YMMWORD PTR [rcx+224]
    vfmadd213ps ymm7, ymm9, YMMWORD PTR [r8+224]
    vmovups YMMWORD PTR [r9+224], ymm7
    add rsi, 16384
    add rdx, 256
    add rcx, 256
    add r8, 256
    add r9, 256
    dec r10d
    jnz .Lwide_tile
    vzeroupper
    ret
    .cfi_endproc
.size dpdf_qaffine256_fused_avx2,.-dpdf_qaffine256_fused_avx2
.section .note.GNU-stack,"",@progbits
