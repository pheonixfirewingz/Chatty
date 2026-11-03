// Kernels for the DeepSeek-V4.1-Flash forward pass, for gfx1100. Matrix multiplies, wave32: out[m, n] = x[m, k] @ w[n, k]^T. Activations are f32,
// already quantized and rescaled.
typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long u64;

#define BLOCK 32u

static inline float bits_f32(u32 bits) { return __builtin_bit_cast(float, bits); }

// One 32-lane wave computes 32 output columns of one output row (the dispatch's y). Everything
// the lanes share is uniform: activations, and FP8 scales, whose blocks span exactly one wave. The
// constant address space lets those load through scalar registers instead of once per lane.
#define CONST __attribute__((address_space(4)))
typedef u32 u32x4 __attribute__((ext_vector_type(4)));
typedef float f32x4 __attribute__((ext_vector_type(4)));

struct args {
    u64 out;
    u64 x;
    u64 weight;
    u64 scale;
    u32 n;
    u32 k;
    // Row distances, in f32 elements.
    u32 x_stride;
    u32 out_stride;
};

// Weights decode without a float multiply. The low bits of an E4M3 or E2M1 value, laid out as an
// f32 exponent and mantissa, equal its magnitude times 2^-120 or 2^-126, subnormals included.
// Activations carry a factor of 2^96, applied once by quant_fp8_gemm, which keeps every product in
// normal range. One fixup per output element then restores the scale. A scale byte of 0 means
// 2^-127 and decodes to 0 here, which no sum in f32 can distinguish once other blocks contribute.
static inline float e4m3_bits(u32 byte) {
    return bits_f32(((byte & 0x7Fu) << 20) | ((byte & 0x80u) << 24));
}

static inline float e2m1_bits(u32 nibble) {
    return bits_f32(((nibble & 7u) << 22) | ((nibble & 8u) << 28));
}

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void gemm_fp8(struct args a) {
    // Weights load in bursts of eight, so one stall serves 128 weights instead of 16. Waves spend
    // most of their time waiting on memory, not multiplying. k is a multiple of 128 for this model.
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 group = __builtin_amdgcn_workgroup_id_x();
    u32 col = group * 32u + __builtin_amdgcn_workitem_id_x();
    u32 blocks = a.k / BLOCK;
    CONST const float *x = (CONST const float *)(a.x + (u64)row * a.x_stride * 4u);
    CONST const u32 *s = (CONST const u32 *)(a.scale + (u64)group * blocks);
    const u32x4 *w = (const u32x4 *)(a.weight + (u64)col * a.k);
    float total = 0.0f;
    for (u32 unit = 0; unit < blocks / 4u; ++unit) {
        u32x4 v0 = w[0], v1 = w[1], v2 = w[2], v3 = w[3];
        u32x4 v4 = w[4], v5 = w[5], v6 = w[6], v7 = w[7];
        w += 8;
        u32 scales = s[unit];
        const u32x4 burst[8] = {v0, v1, v2, v3, v4, v5, v6, v7};
        for (u32 b = 0; b < 4u; ++b) {
            float sum = 0.0f;
            for (u32 q = 0; q < 8u; ++q) {
                u32 word = burst[b * 2u + q / 4u][q % 4u];
                sum += x[0] * e4m3_bits(word);
                sum += x[1] * e4m3_bits(word >> 8);
                sum += x[2] * e4m3_bits(word >> 16);
                sum += x[3] * e4m3_bits(word >> 24);
                x += 4;
            }
            total += sum * bits_f32(((scales >> (b * 8u)) & 0xFFu) << 23);
        }
    }
    ((float *)a.out)[(u64)row * a.out_stride + col] = total * 0x1p24f;
}

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void gemm_fp4(struct args a) {
    // As gemm_fp8: one burst of four loads per 128 weights, plus their four scale bytes.
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 col = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    u32 blocks = a.k / BLOCK;
    CONST const float *x = (CONST const float *)(a.x + (u64)row * a.x_stride * 4u);
    const u32 *s = (const u32 *)(a.scale + (u64)col * blocks);
    const u32x4 *w = (const u32x4 *)(a.weight + (u64)col * (a.k / 2u));
    float total = 0.0f;
    for (u32 unit = 0; unit < blocks / 4u; ++unit) {
        u32x4 v0 = w[0], v1 = w[1], v2 = w[2], v3 = w[3];
        u32 scales = s[unit];
        w += 4;
        const u32x4 burst[4] = {v0, v1, v2, v3};
        for (u32 b = 0; b < 4u; ++b) {
            float sum = 0.0f;
            for (u32 d = 0; d < 4u; ++d) {
                u32 word = burst[b][d];
                sum += x[0] * e2m1_bits(word);
                sum += x[1] * e2m1_bits(word >> 4);
                sum += x[2] * e2m1_bits(word >> 8);
                sum += x[3] * e2m1_bits(word >> 12);
                sum += x[4] * e2m1_bits(word >> 16);
                sum += x[5] * e2m1_bits(word >> 20);
                sum += x[6] * e2m1_bits(word >> 24);
                sum += x[7] * e2m1_bits(word >> 28);
                x += 8;
            }
            total += sum * bits_f32(((scales >> (b * 8u)) & 0xFFu) << 23);
        }
    }
    ((float *)a.out)[(u64)row * a.out_stride + col] = total * 0x1p30f;
}

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void gemm_bf16(struct args a) {
    // Bursts of eight loads per 64 weights, as above. Some inputs are narrower than one burst.
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 col = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    CONST const float *x = (CONST const float *)(a.x + (u64)row * a.x_stride * 4u);
    const u32x4 *w = (const u32x4 *)(a.weight + (u64)col * a.k * 2u);
    float total = 0.0f;
    for (u32 unit = 0; unit < a.k / 64u; ++unit) {
        u32x4 v0 = w[0], v1 = w[1], v2 = w[2], v3 = w[3];
        u32x4 v4 = w[4], v5 = w[5], v6 = w[6], v7 = w[7];
        w += 8;
        const u32x4 burst[8] = {v0, v1, v2, v3, v4, v5, v6, v7};
        for (u32 q = 0; q < 8u; ++q) {
            for (u32 d = 0; d < 4u; ++d) {
                u32 word = burst[q][d];
                total += x[0] * bits_f32(word << 16);
                total += x[1] * bits_f32(word & 0xFFFF0000u);
                x += 2;
            }
        }
    }
    ((float *)a.out)[(u64)row * a.out_stride + col] = total;
}

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void gemm_f32(struct args a) {
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 col = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    CONST const float *x = (CONST const float *)(a.x + (u64)row * a.x_stride * 4u);
    const float *w = (const float *)(a.weight + (u64)col * a.k * 4u);
    float total = 0.0f;
    for (u32 j = 0; j < a.k; ++j) {
        total += x[j] * w[j];
    }
    ((float *)a.out)[(u64)row * a.out_stride + col] = total;
}

// Quantization. One lane handles one block of one row (the dispatch's y).
struct quant_args {
    u64 buf;
    u32 stride;
    u32 pad;
};

static inline float pow2i(int e) { return bits_f32((u32)(e + 127) << 23); }

static inline int log2_ceil(float v) {
    u32 bits = __builtin_bit_cast(u32, v);
    return (int)((bits >> 23) & 0xFFu) - 127 + ((bits & 0x7FFFFFu) != 0u ? 1 : 0);
}

static inline float round_e4m3(float v) {
    u32 bits = __builtin_bit_cast(u32, v);
    int e = (int)((bits >> 23) & 0xFFu) - 127;
    e = e < -6 ? -6 : e;
    float q = pow2i(e - 3);
    return __builtin_copysignf(__builtin_rintf(__builtin_fabsf(v) / q) * q, v);
}

static inline float round_e2m1(float v) {
    float a = __builtin_fabsf(v);
    float r = a <= 0.25f ? 0.0f
            : a < 0.75f  ? 0.5f
            : a <= 1.25f ? 1.0f
            : a < 1.75f  ? 1.5f
            : a <= 2.5f  ? 2.0f
            : a < 3.5f   ? 3.0f
            : a <= 5.0f  ? 4.0f
                         : 6.0f;
    return __builtin_copysignf(r, v);
}

static inline float clampf(float v, float lo, float hi) {
    return v < lo ? lo : (v > hi ? hi : v);
}

static inline float *block_at(struct quant_args a, u32 size) {
    u32 block = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    u64 row = __builtin_amdgcn_workgroup_id_y();
    return (float *)a.buf + row * a.stride + (u64)block * size;
}

static inline float block_absmax(const float *p, u32 size) {
    float m = 0.0f;
    for (u32 i = 0; i < size; ++i) {
        float v = __builtin_fabsf(p[i]);
        m = v > m ? v : m;
    }
    return m;
}

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void quant_fp8(struct quant_args a) {
    float *p = block_at(a, 32u);
    float m = block_absmax(p, 32u);
    float s = pow2i(log2_ceil((m > 1e-4f ? m : 1e-4f) / 448.0f));
    for (u32 i = 0; i < 32u; ++i) {
        p[i] = round_e4m3(clampf(p[i] / s, -448.0f, 448.0f)) * s;
    }
}

// Quantize a copy for multiplying weights, in one pass instead of a copy and a quantize.
struct quant_into_args {
    u64 src;
    u64 dst;
    u32 stride;
    u32 pad;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void quant_fp8_into(struct quant_into_args a) {
    u32 block = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    u64 at = (u64)__builtin_amdgcn_workgroup_id_y() * a.stride + (u64)block * 32u;
    const float *p = (const float *)a.src + at;
    float *q = (float *)a.dst + at;
    float m = block_absmax(p, 32u);
    float s = pow2i(log2_ceil((m > 1e-4f ? m : 1e-4f) / 448.0f));
    for (u32 i = 0; i < 32u; ++i) {
        q[i] = round_e4m3(clampf(p[i] / s, -448.0f, 448.0f)) * s * 0x1p96f;
    }
}

// quant_fp8 for a row about to multiply FP8 or FP4 weights. See e4m3_bits.
__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void quant_fp8_gemm(struct quant_args a) {
    float *p = block_at(a, 32u);
    float m = block_absmax(p, 32u);
    float s = pow2i(log2_ceil((m > 1e-4f ? m : 1e-4f) / 448.0f));
    for (u32 i = 0; i < 32u; ++i) {
        p[i] = round_e4m3(clampf(p[i] / s, -448.0f, 448.0f)) * s * 0x1p96f;
    }
}

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void quant_fp4_pow2(struct quant_args a) {
    float *p = block_at(a, 32u);
    float m = block_absmax(p, 32u);
    float floor = 6.0f * 0x1p-126f;
    float s = pow2i(log2_ceil((m > floor ? m : floor) / 6.0f));
    for (u32 i = 0; i < 32u; ++i) {
        p[i] = round_e2m1(clampf(p[i] / s, -6.0f, 6.0f)) * s;
    }
}

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void quant_fp4_e4m3(struct quant_args a) {
    float *p = block_at(a, 16u);
    float m = block_absmax(p, 16u);
    float floor = 6.0f * 0x1p-9f;
    float s = round_e4m3((m > floor ? m : floor) / 6.0f);
    for (u32 i = 0; i < 16u; ++i) {
        p[i] = round_e2m1(clampf(p[i] / s, -6.0f, 6.0f)) * s;
    }
}

// RMS norm in two steps. The weights are bf16.
struct norm_args {
    u64 buf;
    u64 weight;
    u64 partial;
    u32 dim;
    u32 stride;
    u32 frags;
    u32 pad;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void norm_partial(struct norm_args a) {
    // One wave per fragment of a row, so a whole row costs about one fragment's time.
    u32 y = __builtin_amdgcn_workgroup_id_y();
    u32 row = y / a.frags;
    u32 f = y - row * a.frags;
    u32 size = a.dim / a.frags;
    CONST const float *x = (CONST const float *)(a.buf + ((u64)row * a.stride + (u64)f * size) * 4u);
    float sum = 0.0f;
    for (u32 i = 0; i < size; ++i) {
        sum += x[i] * x[i];
    }
    ((float *)a.partial)[y] = sum;
}

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void norm_apply(struct norm_args a) {
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 d = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    CONST const float *partial = (CONST const float *)(a.partial + (u64)row * a.frags * 4u);
    float sum = 0.0f;
    for (u32 f = 0; f < a.frags; ++f) {
        sum += partial[f];
    }
    float scale = 1.0f / __builtin_sqrtf(sum / (float)a.dim + 1e-20f);
    float *p = (float *)a.buf + (u64)row * a.stride + d;
    *p = *p * scale * bits_f32((u32)((const u16 *)a.weight)[d] << 16);
}

#define HC 4u
#define DIM 5120u
#define HEAD_DIM 512u

static inline float sigmoid(float x) { return 1.0f / (1.0f + __builtin_expf(-x)); }

#define MIX_SLOTS 25u

// Project the stream onto the 24 mix rows. One wave per fragment of the stream: lanes are mix rows,
// and the extra lane gets the stream itself as its weights, which yields the sum of squares.
struct partial_args {
    u64 out;
    u64 x;
    u64 weight;
    u32 frags;
    u32 pad;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void mix_partial(struct partial_args a) {
    u32 y = __builtin_amdgcn_workgroup_id_y();
    u32 row = y / a.frags;
    u32 f = y - row * a.frags;
    u32 lane = __builtin_amdgcn_workitem_id_x();
    u32 size = HC * DIM / a.frags;
    u64 stream = a.x + ((u64)row * HC * DIM + (u64)f * size) * 4u;
    CONST const f32x4 *x = (CONST const f32x4 *)stream;
    u64 weights = a.weight + ((u64)lane * HC * DIM + (u64)f * size) * 4u;
    const f32x4 *w = (const f32x4 *)(lane < 24u ? weights : stream);
    float sum = 0.0f;
    for (u32 i = 0; i < size / 4u; ++i) {
        f32x4 u = x[i];
        f32x4 v = w[i];
        sum += u.x * v.x + u.y * v.y + u.z * v.z + u.w * v.w;
    }
    ((float *)a.out)[(u64)y * MIX_SLOTS + lane] = sum;
}

// Hyper-connection coefficients: one lane per row, summing the fragments above.
struct mix_args {
    u64 mix; // per row and fragment: the 24 projections, then the sum of squares
    u64 frags;
    u64 scale;
    u64 base;
    u64 out; // pre[4], post[4], comb[4][4] per row
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void hc_sinkhorn(struct mix_args a) {
    u64 row = __builtin_amdgcn_workgroup_id_y();
    CONST const float *partial = (CONST const float *)(a.mix + row * a.frags * MIX_SLOTS * 4u);
    CONST const float *scale = (CONST const float *)a.scale;
    CONST const float *base = (CONST const float *)a.base;
    float *out = (float *)a.out + row * 24u;
    float mix[MIX_SLOTS];
    for (u32 i = 0; i < MIX_SLOTS; ++i) {
        mix[i] = 0.0f;
    }
    for (u32 f = 0; f < (u32)a.frags; ++f) {
        for (u32 i = 0; i < MIX_SLOTS; ++i) {
            mix[i] += partial[f * MIX_SLOTS + i];
        }
    }
    float sum = mix[24];
    float rsqrt = 1.0f / __builtin_sqrtf(sum / (float)(HC * DIM) + 1e-20f);
    float comb[4][4];
    for (u32 j = 0; j < HC; ++j) {
        out[j] = sigmoid(mix[j] * rsqrt * scale[0] + base[j]) + 1e-6f;
        out[j + HC] = 2.0f * sigmoid(mix[j + HC] * rsqrt * scale[1] + base[j + HC]);
        float max = -__builtin_inff();
        for (u32 k = 0; k < HC; ++k) {
            u32 at = j * HC + k + 2u * HC;
            comb[j][k] = mix[at] * rsqrt * scale[2] + base[at];
            max = comb[j][k] > max ? comb[j][k] : max;
        }
        float total = 0.0f;
        for (u32 k = 0; k < HC; ++k) {
            comb[j][k] = __builtin_expf(comb[j][k] - max);
            total += comb[j][k];
        }
        for (u32 k = 0; k < HC; ++k) {
            comb[j][k] = comb[j][k] / total + 1e-6f;
        }
    }
    for (u32 iter = 0; iter < 20u; ++iter) {
        if (iter != 0u) {
            for (u32 j = 0; j < HC; ++j) {
                float total = comb[j][0] + comb[j][1] + comb[j][2] + comb[j][3];
                for (u32 k = 0; k < HC; ++k) {
                    comb[j][k] /= total + 1e-6f;
                }
            }
        }
        for (u32 k = 0; k < HC; ++k) {
            float total = comb[0][k] + comb[1][k] + comb[2][k] + comb[3][k];
            for (u32 j = 0; j < HC; ++j) {
                comb[j][k] /= total + 1e-6f;
            }
        }
    }
    for (u32 j = 0; j < HC; ++j) {
        for (u32 k = 0; k < HC; ++k) {
            out[2u * HC + j * HC + k] = comb[j][k];
        }
    }
}

// x[row, d] = sum over copies of pre[row, c] * h[row, c, d].
struct pre_args {
    u64 out;
    u64 h;
    u64 mixes; // 24 per row, or 4 when mix_stride says so
    u32 mix_stride;
    u32 mix_offset;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void hc_pre(struct pre_args a) {
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 d = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    CONST const float *mix = (CONST const float *)(a.mixes + ((u64)row * a.mix_stride + a.mix_offset) * 4u);
    const float *h = (const float *)a.h + (u64)row * HC * DIM + d;
    float sum = 0.0f;
    for (u32 c = 0; c < HC; ++c) {
        sum += mix[c] * h[c * DIM];
    }
    ((float *)a.out)[(u64)row * DIM + d] = sum;
}

// out[row, j, d] = post[row, j] * x[row, d] + sum over i of comb[row, i, j] * residual[row, i, d].
struct post_args {
    u64 out;
    u64 x;
    u64 residual;
    u64 mixes;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void hc_post(struct post_args a) {
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 group = __builtin_amdgcn_workgroup_id_x();
    u32 at = group * 32u + __builtin_amdgcn_workitem_id_x();
    u32 j = group / (DIM / 32u);
    u32 d = at - j * DIM;
    CONST const float *mix = (CONST const float *)(a.mixes + (u64)row * 24u * 4u);
    const float *res = (const float *)a.residual + (u64)row * HC * DIM + d;
    float value = mix[HC + j] * ((const float *)a.x)[(u64)row * DIM + d];
    for (u32 i = 0; i < HC; ++i) {
        value += mix[2u * HC + i * HC + j] * res[i * DIM];
    }
    ((float *)a.out)[(u64)row * HC * DIM + at] = value;
}

// Rotate the last 64 elements of each head as complex pairs. One lane per head of a row.
struct rope_args {
    u64 buf;
    u64 table; // 32 (cos, sin) pairs per row
    u32 stride;
    u32 head_dim;
    float sign;
    u32 pad;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void rope(struct rope_args a) {
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 head = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    float *p = (float *)a.buf + (u64)row * a.stride + (u64)(head + 1u) * a.head_dim - 64u;
    CONST const float *t = (CONST const float *)(a.table + (u64)row * 64u * 4u);
    for (u32 i = 0; i < 32u; ++i) {
        float c = t[i * 2u];
        float n = t[i * 2u + 1u] * a.sign;
        float re = p[i * 2u];
        float im = p[i * 2u + 1u];
        p[i * 2u] = re * c - im * n;
        p[i * 2u + 1u] = re * n + im * c;
    }
}

// Copy rows through a table of source rows; `add` accumulates instead.
struct copy_args {
    u64 dst;
    u64 src;
    u64 table; // dst row index for each source row (y)
    u32 dim;
    u32 add;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void put_rows(struct copy_args a) {
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 d = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    u32 target = *(CONST const u32 *)(a.table + (u64)row * 4u);
    float v = ((const float *)a.src)[(u64)row * a.dim + d];
    float *out = (float *)a.dst + (u64)target * a.dim + d;
    *out = a.add != 0u ? *out + v : v;
}

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void take_rows(struct copy_args a) {
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 d = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    u32 source = *(CONST const u32 *)(a.table + (u64)row * 4u);
    ((float *)a.dst)[(u64)row * a.dim + d] = ((const float *)a.src)[(u64)source * a.dim + d];
}

// SwiGLU middle of an expert. `weights` is one route weight per row, or 0 for the shared expert.
struct swiglu_args {
    u64 out;
    u64 gate;
    u64 up;
    u64 weights;
    u32 dim;
    u32 pad;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void swiglu(struct swiglu_args a) {
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 d = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    u64 at = (u64)row * a.dim + d;
    float g = ((const float *)a.gate)[at];
    float u = ((const float *)a.up)[at];
    g = g < 10.0f ? g : 10.0f;
    u = clampf(u, -10.0f, 10.0f);
    float value = g * sigmoid(g) * u;
    if (a.weights != 0ul) {
        value *= *(CONST const float *)(a.weights + (u64)row * 4u);
    }
    ((float *)a.out)[at] = value;
}

// The middle of an expert in one pass: SwiGLU, route weight, then quantizing for the last matrix.
// One lane per 32-element block.
__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void swiglu_quant(struct swiglu_args a) {
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 block = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    u64 at = (u64)row * a.dim + (u64)block * 32u;
    const float *gate = (const float *)a.gate + at;
    const float *up = (const float *)a.up + at;
    float *out = (float *)a.out + at;
    float weight = a.weights != 0ul ? *(CONST const float *)(a.weights + (u64)row * 4u) : 1.0f;
    float values[32];
    float m = 0.0f;
    for (u32 i = 0; i < 32u; ++i) {
        float g = gate[i] < 10.0f ? gate[i] : 10.0f;
        float u = clampf(up[i], -10.0f, 10.0f);
        values[i] = g * sigmoid(g) * u * weight;
        float magnitude = __builtin_fabsf(values[i]);
        m = magnitude > m ? magnitude : m;
    }
    float s = pow2i(log2_ceil((m > 1e-4f ? m : 1e-4f) / 448.0f));
    for (u32 i = 0; i < 32u; ++i) {
        out[i] = round_e4m3(clampf(values[i] / s, -448.0f, 448.0f)) * s * 0x1p96f;
    }
}

// Sum one row from many buffers, for a single token. Each buffer is uniform across the wave.
struct sum_args {
    u64 out;
    u64 sources;
    u32 count;
    u32 pad;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void sum_row(struct sum_args a) {
    u32 d = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    CONST const u64 *sources = (CONST const u64 *)a.sources;
    float sum = 0.0f;
    for (u32 j = 0; j < a.count; ++j) {
        sum += ((const float *)sources[j])[d];
    }
    ((float *)a.out)[d] = sum;
}

// The token embedding, as four identical stream copies. One wave per 32 dimensions.
struct embed_args {
    u64 out;
    u64 weight;
    u64 ids;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void embed_rows(struct embed_args a) {
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 d = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    u32 id = *(CONST const u32 *)(a.ids + (u64)row * 4u);
    u32 word = ((const u16 *)a.weight)[(u64)id * DIM + d];
    float v = bits_f32(word << 16);
    float *out = (float *)a.out + (u64)row * HC * DIM + d;
    for (u32 c = 0; c < HC; ++c) {
        out[c * DIM] = v;
    }
}

// Engram gate: one lane per stream copy of a row.
struct engram_args {
    u64 h;
    u64 kv; // keys[4][DIM] then value[DIM], per row
    u64 q_weight;
    u64 k_weight;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void engram_gate(struct engram_args a) {
    // One lane per (row, stream copy), both uniform, so loads stay on scalar registers.
    u32 y = __builtin_amdgcn_workgroup_id_y();
    u64 row = y / HC;
    u32 c = y % HC;
    CONST const float *in = (CONST const float *)(a.h + (row * HC * DIM + (u64)c * DIM) * 4u);
    float *h = (float *)a.h + row * HC * DIM + (u64)c * DIM;
    u64 kv = a.kv + row * (HC + 1u) * DIM * 4u;
    CONST const float *key = (CONST const float *)(kv + (u64)c * DIM * 4u);
    CONST const float *value = (CONST const float *)(kv + (u64)HC * DIM * 4u);
    CONST const u32 *qw = (CONST const u32 *)(a.q_weight + (u64)c * DIM * 2u);
    CONST const u32 *kw = (CONST const u32 *)(a.k_weight + (u64)c * DIM * 2u);
    float hh = 0.0f, kk = 0.0f, dot = 0.0f;
    for (u32 i = 0; i < DIM / 2u; ++i) {
        u32 q = qw[i];
        u32 k = kw[i];
        float w0 = bits_f32(q << 16) * bits_f32(k << 16);
        float w1 = bits_f32(q & 0xFFFF0000u) * bits_f32(k & 0xFFFF0000u);
        float h0 = in[i * 2u], h1 = in[i * 2u + 1u];
        float k0 = key[i * 2u], k1 = key[i * 2u + 1u];
        hh += h0 * h0 + h1 * h1;
        kk += k0 * k0 + k1 * k1;
        dot += h0 * w0 * k0 + h1 * w1 * k1;
    }
    float rstd = 1.0f / __builtin_sqrtf(hh / (float)DIM + 1e-20f)
               / __builtin_sqrtf(kk / (float)DIM + 1e-20f);
    dot = dot * rstd / __builtin_sqrtf((float)DIM);
    float root = __builtin_sqrtf(__builtin_fabsf(dot) > 1e-6f ? __builtin_fabsf(dot) : 1e-6f);
    float gate = sigmoid(__builtin_copysignf(root, dot));
    for (u32 d = 0; d < DIM; ++d) {
        h[d] = in[d] + gate * value[d];
    }
}

// Pool one group of two latents with a per-dimension softmax of their gate scores.
struct pool_args {
    u64 out;
    u64 kv;
    u64 score;
    u64 table; // first ring row of each group (y); the second is the next ring row, modulo `ring`
    u32 ring;
    u32 pad;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void pool_pairs(struct pool_args a) {
    u32 row = __builtin_amdgcn_workgroup_id_y();
    u32 d = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    u32 first = *(CONST const u32 *)(a.table + (u64)row * 4u);
    u32 second = (first + 1u) % a.ring;
    const float *kv = (const float *)a.kv;
    const float *score = (const float *)a.score;
    float s0 = score[(u64)first * HEAD_DIM + d];
    float s1 = score[(u64)second * HEAD_DIM + d];
    float max = s0 > s1 ? s0 : s1;
    float w0 = __builtin_expf(s0 - max);
    float w1 = __builtin_expf(s1 - max);
    float value = w0 * kv[(u64)first * HEAD_DIM + d] + w1 * kv[(u64)second * HEAD_DIM + d];
    ((float *)a.out)[(u64)row * HEAD_DIM + d] = value / (w0 + w1);
}

// Sparse attention for one query, in three steps so each can fill the GPU: every (head, source)
// score, then per-head normalized weights, then every (head, dimension) output.
struct attn_args {
    u64 out;
    u64 q;
    u64 sources; // address of each source row
    u64 scores;  // [heads, count]
    u64 sink;
    u32 count;
    u32 pad;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void attn_scores(struct attn_args a) {
    u32 head = __builtin_amdgcn_workgroup_id_y();
    u32 j = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    CONST const f32x4 *q = (CONST const f32x4 *)(a.q + (u64)head * HEAD_DIM * 4u);
    const f32x4 *kv = (const f32x4 *)((const u64 *)a.sources)[j];
    float sum = 0.0f;
    for (u32 d = 0; d < HEAD_DIM / 4u; ++d) {
        f32x4 x = q[d];
        f32x4 v = kv[d];
        sum += x.x * v.x + x.y * v.y + x.z * v.z + x.w * v.w;
    }
    ((float *)a.scores)[(u64)head * a.count + j] = sum / __builtin_sqrtf((float)HEAD_DIM);
}

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void attn_weights(struct attn_args a) {
    u32 head = __builtin_amdgcn_workgroup_id_y();
    CONST const float *in = (CONST const float *)(a.scores + (u64)head * a.count * 4u);
    float *s = (float *)a.scores + (u64)head * a.count;
    float max = -1e30f;
    for (u32 j = 0; j < a.count; ++j) {
        max = in[j] > max ? in[j] : max;
    }
    float total = __builtin_expf(((CONST const float *)a.sink)[head] - max);
    for (u32 j = 0; j < a.count; ++j) {
        total += __builtin_expf(in[j] - max);
    }
    float inverse = 1.0f / total;
    for (u32 j = 0; j < a.count; ++j) {
        s[j] = __builtin_expf(in[j] - max) * inverse;
    }
}

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void attn_output(struct attn_args a) {
    // Lanes are heads, and a wave owns 16 dimensions. Each source row's values are the same for
    // all lanes, so they come through scalar registers, 16 per load, not once per lane.
    u32 block = __builtin_amdgcn_workgroup_id_y();
    u32 head = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    CONST const u64 *sources = (CONST const u64 *)a.sources;
    const float *w = (const float *)a.scores + (u64)head * a.count;
    float sum[16];
    for (u32 d = 0; d < 16u; ++d) {
        sum[d] = 0.0f;
    }
    for (u32 j = 0; j < a.count; ++j) {
        CONST const float *kv = (CONST const float *)(sources[j] + (u64)block * 64u);
        float weight = w[j];
        for (u32 d = 0; d < 16u; ++d) {
            sum[d] += weight * kv[d];
        }
    }
    float *out = (float *)a.out + (u64)head * HEAD_DIM + (u64)block * 16u;
    for (u32 d = 0; d < 16u; ++d) {
        out[d] = sum[d];
    }
}

// Index scores for one query: one lane per compressed position.
struct index_args {
    u64 out;
    u64 q;
    u64 keys;
    u64 weights;
};

#define INDEX_DIM 128u
#define INDEX_HEADS 32u

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void index_scores(struct index_args a) {
    u32 t = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    CONST const float *q = (CONST const float *)a.q;
    CONST const float *w = (CONST const float *)a.weights;
    const float *key = (const float *)a.keys + (u64)t * INDEX_DIM;
    float total = 0.0f;
    for (u32 h = 0; h < INDEX_HEADS; ++h) {
        float sum = 0.0f;
        for (u32 d = 0; d < INDEX_DIM; ++d) {
            sum += q[h * INDEX_DIM + d] * key[d];
        }
        total += (sum > 0.0f ? sum : 0.0f) * w[h];
    }
    ((float *)a.out)[t] = total;
}

// out[i] = x[i] * factor, for scaling a table in place.
struct scale_args {
    u64 buf;
    float factor;
    u32 pad;
};

__attribute__((amdgpu_kernel, reqd_work_group_size(32, 1, 1)))
void scale_values(struct scale_args a) {
    u32 i = __builtin_amdgcn_workgroup_id_x() * 32u + __builtin_amdgcn_workitem_id_x();
    ((float *)a.buf)[i] *= a.factor;
}
