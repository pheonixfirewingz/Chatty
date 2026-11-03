typedef unsigned char u8;
typedef unsigned int u32;
typedef unsigned long u64;
typedef u32 u32x4 __attribute__((ext_vector_type(4)));

// Fast vectorized GPU memory copy (used for VRAM -> Host staging demotions so the CPU
// never reads write-combined BAR memory directly). `dst` and `src` must be 16-byte aligned.
__attribute__((amdgpu_kernel, amdgpu_flat_work_group_size(256, 256)))
void copy_bytes(u8 * restrict dst,
                const u8 * restrict src,
                u64 len,
                u64 grid_threads) {
    u64 tid = (u64)__builtin_amdgcn_workgroup_id_x() * 256ul +
              (u64)__builtin_amdgcn_workitem_id_x();
    u64 vecs = len >> 4;
    u32x4 * restrict dst16 = (u32x4 * restrict)dst;
    const u32x4 * restrict src16 = (const u32x4 * restrict)src;
    for (u64 i = tid; i < vecs; i += grid_threads) {
        dst16[i] = src16[i];
    }
    if (tid == 0ul) {
        for (u64 b = vecs << 4; b < len; ++b) {
            dst[b] = src[b];
        }
    }
}
