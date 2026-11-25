// SPDX-License-Identifier: LGPL-2.1-or-later
/*
 * VP9 with NVDEC. The hardware decodes the blocks; the headers and the probabilities, with their
 * adaptation after each frame, are up to the driver (VP9 bitstream specification, 6, 8.4, 9.3).
 * What the hardware expects in its tables comes from the nvtegra decoder of averne's FFmpeg
 * (Copyright (c) 2024 averne, LGPL-2.1-or-later).
 */
#include "vp9.h"

#include <algorithm>
#include <cstdlib>
#include <linux/kernel.h>

#include "../engines/nvdec.h"
#include "../objects.h"
#include "vp9_tables.h"

void Vp9Probs::setDefault()
{
#define COPY(field, table)                                          \
    do {                                                            \
        static_assert(sizeof(field) == sizeof(table), "wrong size"); \
        memcpy(field, table, sizeof(field));                        \
    } while (0)

    COPY(tx8, vp9_default_tx8);
    COPY(tx16, vp9_default_tx16);
    COPY(tx32, vp9_default_tx32);
    COPY(coef, vp9_default_coef);
    COPY(skip, vp9_default_skip);
    COPY(inter_mode, vp9_default_inter_mode);
    COPY(interp_filter, vp9_default_interp_filter);
    COPY(is_inter, vp9_default_is_inter);
    COPY(comp_mode, vp9_default_comp_mode);
    COPY(single_ref, vp9_default_single_ref);
    COPY(comp_ref, vp9_default_comp_ref);
    COPY(y_mode, vp9_default_y_mode);
    COPY(uv_mode, vp9_default_uv_mode);
    COPY(partition, vp9_default_partition);
    COPY(mv_joint, vp9_default_mv_joint);
    COPY(mv_sign, vp9_default_mv_sign);
    COPY(mv_classes, vp9_default_mv_classes);
    COPY(mv_class0_bit, vp9_default_mv_class0_bit);
    COPY(mv_bits, vp9_default_mv_bits);
    COPY(mv_class0_fr, vp9_default_mv_class0_fr);
    COPY(mv_fr, vp9_default_mv_fr);
    COPY(mv_class0_hp, vp9_default_mv_class0_hp);
    COPY(mv_hp, vp9_default_mv_hp);
}

namespace {

/* plain bits: VP9 has no emulation prevention bytes */
class Bits
{
public:
    Bits(const uint8_t *data, size_t size) : _data(data), _size(size * 8) {}

    uint32_t u(unsigned n)
    {
        uint32_t v = 0;
        while (n--) {
            v <<= 1;
            if (_pos < _size)
                v |= _data[_pos / 8] >> (7 - _pos % 8) & 1;
            _pos++;
        }
        return v;
    }

    int s(unsigned n)
    {
        int v = u(n);
        return u(1) ? -v : v;
    }

    bool overrun() const { return _pos > _size; }
    size_t bytes() const { return (_pos + 7) / 8; }

private:
    const uint8_t *_data;
    size_t _size, _pos = 0;
};

/* the arithmetic decoder of the compressed header (9.2) */
class BoolDecoder
{
public:
    BoolDecoder(const uint8_t *data, size_t size) : _bits(data, size)
    {
        _value = _bits.u(8);
        _range = 255;
        b(128); /* marker */
    }

    bool b(unsigned p)
    {
        unsigned split = 1 + ((_range - 1) * p >> 8);
        bool bit;

        if (_value < split) {
            _range = split;
            bit = false;
        } else {
            _range -= split;
            _value -= split;
            bit = true;
        }
        while (_range < 128) {
            _value = _value << 1 | _bits.u(1);
            _range <<= 1;
        }
        return bit;
    }

    unsigned l(unsigned n)
    {
        unsigned v = 0;
        while (n--)
            v = v << 1 | b(128);
        return v;
    }

    /* a change of a probability (6.3.5) */
    void diff(uint8_t &prob)
    {
        if (!b(252))
            return;

        unsigned delta;
        if (!l(1)) {
            delta = l(4);
        } else if (!l(1)) {
            delta = l(4) + 16;
        } else if (!l(1)) {
            delta = l(5) + 32;
        } else {
            delta = l(7);
            if (delta < 65)
                delta += 64;
            else
                delta = (delta << 1) - 1 + l(1);
        }

        /* the first 20 values are a coarse scale, then come the others */
        unsigned v;
        if (delta < 20) {
            v = 7 + 13 * delta;
        } else {
            v = delta - 20 + 1;
            /* skip the values of the coarse scale */
            for (unsigned i = 0; i < 20; i++)
                if (v >= 7 + 13 * i)
                    v++;
            v = std::min(v, 253u);
        }

        auto recenter = [](unsigned d, unsigned center) -> unsigned {
            if (d > 2 * center)
                return d;
            return d & 1 ? center - ((d + 1) >> 1) : center + (d >> 1);
        };
        unsigned m = prob - 1;
        if (m << 1 <= 254)
            prob = 1 + recenter(v, m);
        else
            prob = 255 - recenter(v, 254 - m);
    }

    void mv(uint8_t &prob)
    {
        if (b(252))
            prob = l(7) << 1 | 1;
    }

private:
    Bits _bits;
    unsigned _value, _range;
};

/* 8.4.2 */
uint8_t mergeProb(uint8_t pre, unsigned c0, unsigned c1, unsigned sat, unsigned max_factor)
{
    unsigned den = c0 + c1;
    if (!den)
        return pre;

    unsigned prob = std::max(1u, std::min(255u, (unsigned)(((uint64_t)c0 * 256 + (den >> 1)) / den)));
    unsigned factor = max_factor * std::min(den, sat) / sat;
    return (pre * (256 - factor) + prob * factor + 128) >> 8;
}

uint8_t merge(uint8_t pre, unsigned c0, unsigned c1)
{
    return mergeProb(pre, c0, c1, 20, 128);
}

/* a chain: each node takes one value off */
template <unsigned N, unsigned P> void mergeChain(uint8_t (&p)[P], const uint8_t (&pre)[P], const unsigned (&c)[N])
{
    for (unsigned i = 0; i + 1 < N; i++) {
        unsigned rest = 0;
        for (unsigned j = i + 1; j < N; j++)
            rest += c[j];
        p[i] = merge(pre[i], c[i], rest);
    }
}

/* DC V H D45 D135 D117 D153 D207 D63 TM */
void mergeIntraMode(uint8_t (&p)[9], const uint8_t (&pre)[9], const unsigned (&c)[10])
{
    unsigned left = c[2] + c[4] + c[5], right = c[3] + c[8] + c[6] + c[7];

    p[0] = merge(pre[0], c[0], c[9] + c[1] + left + right);
    p[1] = merge(pre[1], c[9], c[1] + left + right);
    p[2] = merge(pre[2], c[1], left + right);
    p[3] = merge(pre[3], left, right);
    p[4] = merge(pre[4], c[2], c[4] + c[5]);
    p[5] = merge(pre[5], c[4], c[5]);
    p[6] = merge(pre[6], c[3], c[8] + c[6] + c[7]);
    p[7] = merge(pre[7], c[8], c[6] + c[7]);
    p[8] = merge(pre[8], c[6], c[7]);
}

void mergeMvClasses(uint8_t (&p)[10], const uint8_t (&pre)[10], const unsigned (&c)[11])
{
    unsigned c7_10 = c[7] + c[8] + c[9] + c[10];
    unsigned c4_10 = c[4] + c[5] + c[6] + c7_10;
    unsigned c2_10 = c[2] + c[3] + c4_10;

    p[0] = merge(pre[0], c[0], c[1] + c2_10);
    p[1] = merge(pre[1], c[1], c2_10);
    p[2] = merge(pre[2], c[2] + c[3], c4_10);
    p[3] = merge(pre[3], c[2], c[3]);
    p[4] = merge(pre[4], c[4] + c[5], c[6] + c7_10);
    p[5] = merge(pre[5], c[4], c[5]);
    p[6] = merge(pre[6], c[6], c7_10);
    p[7] = merge(pre[7], c[7] + c[8], c[9] + c[10]);
    p[8] = merge(pre[8], c[7], c[8]);
    p[9] = merge(pre[9], c[9], c[10]);
}

}

void Vp9Decoder::beginPicture()
{
    _valid = false;
    _frame.clear();
}

VAStatus Vp9Decoder::renderPicture(const Buffer &buffer)
{
    if (buffer.data.size() < sizeof(_pp))
        return VA_STATUS_ERROR_INVALID_BUFFER;

    memcpy(&_pp, buffer.data.data(), sizeof(_pp));
    _valid = true;
    return VA_STATUS_SUCCESS;
}

VAStatus Vp9Decoder::renderSlice(const void *param, const uint8_t *data, uint32_t size)
{
    _frame.insert(_frame.end(), data, data + size);
    return VA_STATUS_SUCCESS;
}

/* 6.2: the uncompressed header */
bool Vp9Decoder::parseHeader(const uint8_t *data, size_t size)
{
    Bits r(data, size);

    memset(&_h, 0, sizeof(_h));

    if (r.u(2) != 2)
        return false;
    unsigned profile = r.u(1);
    profile |= r.u(1) << 1;
    if (profile) /* 8 bits 4:2:0 only */
        return false;
    if (r.u(1)) /* show_existing_frame */
        return false;

    _h.key_frame = !r.u(1);
    _h.show_frame = r.u(1);
    _h.error_res = r.u(1);

    auto frameSize = [&]() {
        _h.width = r.u(16) + 1;
        _h.height = r.u(16) + 1;
    };
    auto renderSize = [&]() {
        if (r.u(1))
            r.u(32);
    };

    if (_h.key_frame) {
        if (r.u(24) != 0x498342)
            return false;
        if (r.u(3) == 7) /* RGB */
            return false;
        r.u(1);
        frameSize();
        renderSize();
    } else {
        _h.intra_only = _h.show_frame ? 0 : r.u(1);
        _h.reset_frame_context = _h.error_res ? 0 : r.u(2);
        if (_h.intra_only) {
            if (r.u(24) != 0x498342)
                return false;
            r.u(8);
            frameSize();
            renderSize();
        } else {
            r.u(8);
            for (int i = 0; i < 3; i++) {
                _h.ref_idx[i] = r.u(3);
                _h.sign_bias[i + 1] = r.u(1);
            }
            bool found = false;
            for (int i = 0; i < 3 && !found; i++) {
                found = r.u(1);
                if (found) {
                    auto it = _s->sizes.find(_pp.reference_frames[_h.ref_idx[i]]);
                    _h.width = it == _s->sizes.end() ? _pp.frame_width : it->second.width;
                    _h.height = it == _s->sizes.end() ? _pp.frame_height : it->second.height;
                }
            }
            if (!found)
                frameSize();
            renderSize();
            _h.allow_hp = r.u(1);
            _h.interp_filter = r.u(1) ? 4 : r.u(2);
        }
    }

    _h.parallel = true;
    if (!_h.error_res) {
        _h.refresh_context = r.u(1);
        _h.parallel = r.u(1);
    }
    _h.context = r.u(2);

    /* 8.4.1, setup_past_independence */
    if (_h.intra() || _h.error_res) {
        memset(_s->seg_feature, 0, sizeof(_s->seg_feature));
        memset(_s->seg_value, 0, sizeof(_s->seg_value));
        _s->seg_abs = false;
        _s->lf_ref[0] = 1;
        _s->lf_ref[1] = 0;
        _s->lf_ref[2] = _s->lf_ref[3] = -1;
        _s->lf_mode[0] = _s->lf_mode[1] = 0;
        _s->probs.setDefault();

        if (_h.key_frame || _h.error_res || _h.reset_frame_context == 3) {
            for (auto &c : _s->contexts)
                c = _s->probs;
        } else if (_h.reset_frame_context == 2) {
            _s->contexts[_h.context] = _s->probs;
        }
        _h.context = 0;
    }

    _h.filter_level = r.u(6);
    _h.filter_sharpness = r.u(3);
    _s->lf_delta_enabled = r.u(1);
    if (_s->lf_delta_enabled && r.u(1)) {
        for (auto &d : _s->lf_ref)
            if (r.u(1))
                d = r.s(6);
        for (auto &d : _s->lf_mode)
            if (r.u(1))
                d = r.s(6);
    }

    _h.base_q = r.u(8);
    _h.y_dc = r.u(1) ? r.s(4) : 0;
    _h.uv_dc = r.u(1) ? r.s(4) : 0;
    _h.uv_ac = r.u(1) ? r.s(4) : 0;
    _h.lossless = !_h.base_q && !_h.y_dc && !_h.uv_dc && !_h.uv_ac;

    _h.seg_enabled = r.u(1);
    if (_h.seg_enabled) {
        _h.seg_update_map = r.u(1);
        if (_h.seg_update_map) {
            for (auto &p : _s->seg_tree)
                p = r.u(1) ? r.u(8) : 255;
            _h.seg_temporal = r.u(1);
            for (auto &p : _s->seg_pred)
                p = _h.seg_temporal && r.u(1) ? r.u(8) : 255;
        }
        if (r.u(1)) {
            static const unsigned bits[4] = { 8, 6, 2, 0 };

            _s->seg_abs = r.u(1);
            for (int i = 0; i < 8; i++)
                for (int j = 0; j < 4; j++) {
                    _s->seg_value[i][j] = 0;
                    _s->seg_feature[i][j] = r.u(1);
                    if (!_s->seg_feature[i][j])
                        continue;
                    _s->seg_value[i][j] = r.u(bits[j]);
                    if (j < 2 && r.u(1))
                        _s->seg_value[i][j] = -_s->seg_value[i][j];
                }
        }
    }

    unsigned sb_columns = (_h.width + 63) / 64, min_log2 = 0, max_log2 = 1;
    while ((64u << min_log2) < sb_columns)
        min_log2++;
    while (sb_columns >> max_log2 >= 4)
        max_log2++;
    max_log2--;
    _h.tile_columns = min_log2;
    while (_h.tile_columns < max_log2 && r.u(1))
        _h.tile_columns++;
    _h.tile_rows = r.u(1);
    if (_h.tile_rows)
        _h.tile_rows += r.u(1);

    _h.compressed_size = r.u(16);
    _h.header_size = r.bytes();

    if (r.overrun() || !_h.compressed_size || _h.header_size + _h.compressed_size > size)
        return false;

    /* the references of compound prediction (6.3.12) */
    const bool *sign = _h.sign_bias;
    if (sign[1] == sign[2]) {
        _h.comp_fixed = 3;
        _h.comp_var[0] = 1;
        _h.comp_var[1] = 2;
    } else if (sign[1] == sign[3]) {
        _h.comp_fixed = 2;
        _h.comp_var[0] = 1;
        _h.comp_var[1] = 3;
    } else {
        _h.comp_fixed = 1;
        _h.comp_var[0] = 2;
        _h.comp_var[1] = 3;
    }

    /* load_probs, load_probs2 */
    _s->probs = _s->contexts[_h.context];
    return true;
}

/* 6.3: the compressed header changes the probabilities */
bool Vp9Decoder::parseCompressedHeader(const uint8_t *data, size_t size)
{
    BoolDecoder d(data, size);
    Vp9Probs &p = _s->probs;

    if (!_h.lossless) {
        _h.tx_mode = d.l(2);
        if (_h.tx_mode == 3)
            _h.tx_mode += d.l(1);
        if (_h.tx_mode == 4) {
            for (auto &a : p.tx8)
                for (auto &b : a)
                    d.diff(b);
            for (auto &a : p.tx16)
                for (auto &b : a)
                    d.diff(b);
            for (auto &a : p.tx32)
                for (auto &b : a)
                    d.diff(b);
        }
    }

    for (unsigned tx = 0; tx <= std::min(_h.tx_mode, 3u); tx++)
        if (d.l(1))
            for (auto &i : p.coef[tx])
                for (auto &j : i)
                    for (unsigned k = 0; k < 6; k++)
                        for (unsigned l = 0; l < (k == 0 ? 3 : 6); l++)
                            for (auto &m : j[k][l])
                                d.diff(m);

    for (auto &a : p.skip)
        d.diff(a);

    if (_h.intra())
        return true;

    for (auto &a : p.inter_mode)
        for (auto &b : a)
            d.diff(b);
    if (_h.interp_filter == 4)
        for (auto &a : p.interp_filter)
            for (auto &b : a)
                d.diff(b);
    for (auto &a : p.is_inter)
        d.diff(a);

    const bool *sign = _h.sign_bias;
    if (sign[1] != sign[2] || sign[1] != sign[3])
        _h.comp_mode = d.l(1) ? 1 + d.l(1) : 0;
    if (_h.comp_mode == 2)
        for (auto &a : p.comp_mode)
            d.diff(a);
    if (_h.comp_mode != 1)
        for (auto &a : p.single_ref)
            for (auto &b : a)
                d.diff(b);
    if (_h.comp_mode != 0)
        for (auto &a : p.comp_ref)
            d.diff(a);

    for (auto &a : p.y_mode)
        for (auto &b : a)
            d.diff(b);
    for (auto &a : p.partition)
        for (auto &b : a)
            d.diff(b);

    for (auto &a : p.mv_joint)
        d.mv(a);
    for (int i = 0; i < 2; i++) {
        d.mv(p.mv_sign[i]);
        for (auto &a : p.mv_classes[i])
            d.mv(a);
        d.mv(p.mv_class0_bit[i]);
        for (auto &a : p.mv_bits[i])
            d.mv(a);
    }
    for (int i = 0; i < 2; i++) {
        for (auto &a : p.mv_class0_fr[i])
            for (auto &b : a)
                d.mv(b);
        for (auto &a : p.mv_fr[i])
            d.mv(a);
    }
    if (_h.allow_hp)
        for (int i = 0; i < 2; i++) {
            d.mv(p.mv_class0_hp[i]);
            d.mv(p.mv_hp[i]);
        }

    return true;
}

/* 8.4.3, 8.4.4: after a frame, the probabilities follow what was counted in it */
void Vp9Decoder::adapt()
{
    const Header &h = _s->adapt_header;

    _s->adapt = false;
    if (_nvdec.wait(_s->fence))
        return;

    nvdec_vp9EntropyCounts_t c;
    _s->counts->cpuAccess(true);
    memcpy(&c, _s->counts->map(), sizeof(c));
    _s->counts->cpuAccess(false);

    Vp9Probs &p = _s->probs;
    const Vp9Probs pre = _s->contexts[h.context];

    unsigned factor = !h.intra() && h.after_key ? 128 : 112;
    unsigned(*coefs[4])[2][6][6][4] = { c.countCoeffs, c.countCoeffs8x8, c.countCoeffs16x16, c.countCoeffs32x32 };
    for (int tx = 0; tx < 4; tx++)
        for (int i = 0; i < 2; i++)
            for (int j = 0; j < 2; j++)
                for (int k = 0; k < 6; k++)
                    for (int l = 0; l < (k == 0 ? 3 : 6); l++) {
                        const unsigned *n = coefs[tx][i][j][k][l];
                        uint8_t *out = p.coef[tx][i][j][k][l];
                        const uint8_t *in = pre.coef[tx][i][j][k][l];
                        unsigned eob = c.countEobs[tx][i][j][k][l];

                        out[0] = mergeProb(in[0], n[3], eob - n[3], 24, factor);
                        out[1] = mergeProb(in[1], n[0], n[1] + n[2], 24, factor);
                        out[2] = mergeProb(in[2], n[1], n[2], 24, factor);
                    }

    if (!h.intra()) {
        for (int i = 0; i < 4; i++)
            p.is_inter[i] = merge(pre.is_inter[i], c.intra_inter_count[i][0], c.intra_inter_count[i][1]);
        for (int i = 0; i < 5; i++) {
            p.comp_mode[i] = merge(pre.comp_mode[i], c.comp_inter_count[i][0], c.comp_inter_count[i][1]);
            p.comp_ref[i] = merge(pre.comp_ref[i], c.comp_ref_count[i][0], c.comp_ref_count[i][1]);
            for (int j = 0; j < 2; j++)
                p.single_ref[i][j] = merge(pre.single_ref[i][j], c.single_ref_count[i][j][0], c.single_ref_count[i][j][1]);
        }
        for (int i = 0; i < 7; i++)
            for (int j = 0; j < 3; j++)
                p.inter_mode[i][j] = merge(pre.inter_mode[i][j], c.inter_mode_counts[i][j][0], c.inter_mode_counts[i][j][1]);
        for (int i = 0; i < 4; i++)
            mergeIntraMode(p.y_mode[i], pre.y_mode[i], c.sb_ymode_counts[i]);
        for (int i = 0; i < 10; i++)
            mergeIntraMode(p.uv_mode[i], pre.uv_mode[i], c.uv_mode_counts[i]);
        for (int i = 0; i < 16; i++)
            mergeChain(p.partition[i], pre.partition[i], c.partition_counts[i]);
        for (int i = 0; i < 3; i++)
            p.skip[i] = merge(pre.skip[i], c.mbskip_count[i][0], c.mbskip_count[i][1]);
        if (h.interp_filter == 4)
            for (int i = 0; i < 4; i++)
                mergeChain(p.interp_filter[i], pre.interp_filter[i], c.switchable_interp_counts[i]);
        if (h.tx_mode == 4)
            for (int i = 0; i < 2; i++) {
                mergeChain(p.tx8[i], pre.tx8[i], c.tx8x8_count[i]);
                mergeChain(p.tx16[i], pre.tx16[i], c.tx16x16_count[i]);
                mergeChain(p.tx32[i], pre.tx32[i], c.tx32x32_count[i]);
            }

        const nvdec_nmv_context_counts &m = c.nmvcount;
        mergeChain(p.mv_joint, pre.mv_joint, m.joints);
        for (int i = 0; i < 2; i++) {
            p.mv_sign[i] = merge(pre.mv_sign[i], m.sign[i][0], m.sign[i][1]);
            mergeMvClasses(p.mv_classes[i], pre.mv_classes[i], m.classes[i]);
            p.mv_class0_bit[i] = merge(pre.mv_class0_bit[i], m.class0[i][0], m.class0[i][1]);
            for (int j = 0; j < 10; j++)
                p.mv_bits[i][j] = merge(pre.mv_bits[i][j], m.bits[i][j][0], m.bits[i][j][1]);
            for (int j = 0; j < 2; j++)
                mergeChain(p.mv_class0_fr[i][j], pre.mv_class0_fr[i][j], m.class0_fp[i][j]);
            mergeChain(p.mv_fr[i], pre.mv_fr[i], m.fp[i]);
            if (h.allow_hp) {
                p.mv_class0_hp[i] = merge(pre.mv_class0_hp[i], m.class0_hp[i][0], m.class0_hp[i][1]);
                p.mv_hp[i] = merge(pre.mv_hp[i], m.hp[i][0], m.hp[i][1]);
            }
        }
    }

    _s->contexts[h.context] = p;
}

void Vp9Decoder::fillProbs(nvdec_vp9EntropyProbs_t &o)
{
    const Vp9Probs &p = _s->probs;
    auto &a = o.a;

    memset(&o, 0, sizeof(o));

    for (int i = 0; i < 10; i++) {
        for (int j = 0; j < 10; j++) {
            memcpy(o.kf_bmode_prob[i][j], vp9_kf_y_mode_probs[i][j], 8);
            o.kf_bmode_probB[i][j][0] = vp9_kf_y_mode_probs[i][j][8];
        }
        memcpy(o.kf_uv_mode_prob[i], vp9_kf_uv_mode_probs[i], 8);
        o.kf_uv_mode_probB[i][0] = vp9_kf_uv_mode_probs[i][8];
    }
    memcpy(o.mb_segment_tree_probs, _s->seg_tree, 7);
    if (_h.seg_temporal)
        memcpy(o.segment_pred_probs, _s->seg_pred, 3);
    else
        memset(o.segment_pred_probs, 0xff, 3);

    for (int i = 0; i < 7; i++)
        memcpy(a.inter_mode_prob[i], p.inter_mode[i], 3);
    memcpy(a.intra_inter_prob, p.is_inter, 4);
    for (int i = 0; i < 10; i++) {
        memcpy(a.uv_mode_prob[i], p.uv_mode[i], 8);
        a.uv_mode_probB[i][0] = p.uv_mode[i][8];
    }
    memcpy(a.tx8x8_prob, p.tx8, sizeof(p.tx8));
    memcpy(a.tx16x16_prob, p.tx16, sizeof(p.tx16));
    memcpy(a.tx32x32_prob, p.tx32, sizeof(p.tx32));
    for (int i = 0; i < 4; i++) {
        memcpy(a.sb_ymode_prob[i], p.y_mode[i], 8);
        a.sb_ymode_probB[i][0] = p.y_mode[i][8];
    }
    for (int i = 0; i < 16; i++) {
        memcpy(a.partition_prob[0][i], vp9_kf_partition_probs[i], 3);
        memcpy(a.partition_prob[1][i], p.partition[i], 3);
    }
    memcpy(a.switchable_interp_prob, p.interp_filter, sizeof(p.interp_filter));
    memcpy(a.comp_inter_prob, p.comp_mode, sizeof(p.comp_mode));
    memcpy(a.mbskip_probs, p.skip, sizeof(p.skip));
    memcpy(a.nmvc.joints, p.mv_joint, 3);
    memcpy(a.nmvc.sign, p.mv_sign, 2);
    memcpy(a.nmvc.class0, p.mv_class0_bit, 2);
    memcpy(a.nmvc.fp, p.mv_fr, sizeof(p.mv_fr));
    memcpy(a.nmvc.class0_hp, p.mv_class0_hp, 2);
    memcpy(a.nmvc.hp, p.mv_hp, 2);
    memcpy(a.nmvc.classes, p.mv_classes, sizeof(p.mv_classes));
    memcpy(a.nmvc.class0_fp, p.mv_class0_fr, sizeof(p.mv_class0_fr));
    memcpy(a.nmvc.bits, p.mv_bits, sizeof(p.mv_bits));
    memcpy(a.single_ref_prob, p.single_ref, sizeof(p.single_ref));
    memcpy(a.comp_ref_prob, p.comp_ref, sizeof(p.comp_ref));

    for (int i = 0; i < 2; i++)
        for (int j = 0; j < 2; j++)
            for (int k = 0; k < 6; k++)
                for (int l = 0; l < 6; l++) {
                    memcpy(a.probCoeffs[i][j][k][l], p.coef[0][i][j][k][l], 3);
                    memcpy(a.probCoeffs8x8[i][j][k][l], p.coef[1][i][j][k][l], 3);
                    memcpy(a.probCoeffs16x16[i][j][k][l], p.coef[2][i][j][k][l], 3);
                    memcpy(a.probCoeffs32x32[i][j][k][l], p.coef[3][i][j][k][l], 3);
                }
}

VAStatus Vp9Decoder::build(NvdecJob &job)
{
    nvdec_vp9_pic_s c;

    if (!_valid)
        return VA_STATUS_ERROR_INVALID_PARAMETER;

    /* the first frame of a context in the middle of a stream */
    if (!_s) {
        /* a key frame (type 0) starts over */
        for (int i = 0; _pp.pic_fields.bits.frame_type && !_s && i < 8; i++) {
            VASurfaceID id = _pp.reference_frames[i];
            auto surface = id == VA_INVALID_SURFACE ? nullptr : _objects.surface(id);

            if (surface)
                _s = std::dynamic_pointer_cast<Stream>(surface->stream);
        }
        if (!_s)
            _s = std::make_shared<Stream>();
    }

    if (_s->adapt)
        adapt();

    if (!parseHeader(_frame.data(), _frame.size()))
        return VA_STATUS_ERROR_DECODING_ERROR;
    if (!fits(*_target, _h.width, _h.height) || !_h.compressed_size ||
        (size_t)_h.header_size + _h.compressed_size >= _frame.size())
        return VA_STATUS_ERROR_DECODING_ERROR;
    parseCompressedHeader(_frame.data() + _h.header_size, _h.compressed_size);

    unsigned width = __ALIGN_KERNEL(_h.width, 64), height = __ALIGN_KERNEL(_h.height, 64);
    size_t blocks = width * height / 4096;
    size_t segment_size = __ALIGN_KERNEL(blocks * 32, 0x100), mv_size = blocks * 1024, filter_size = height * 988;
    if (!_s->counts)
        _s->counts = _nvdec.allocate(sizeof(nvdec_vp9EntropyCounts_t), true);
    if (segment_size > _s->segment_size || mv_size > _s->mv_size || filter_size > _s->filter_size) {
        for (int i = 0; i < 2; i++) {
            _s->segments[i] = _nvdec.allocate(segment_size);
            _s->mvs[i] = _nvdec.allocate(mv_size);
        }
        _s->filter = _nvdec.allocate(filter_size);
        if (!_s->segments[0] || !_s->segments[1] || !_s->mvs[0] || !_s->mvs[1] || !_s->filter)
            return VA_STATUS_ERROR_ALLOCATION_FAILED;
        _s->segment_size = segment_size;
        _s->mv_size = mv_size;
        _s->filter_size = filter_size;
    } else if (_h.intra() || _h.error_res) {
        /* the map of the segments starts over */
        auto fresh = _nvdec.allocate(_s->segment_size);
        if (!fresh)
            return VA_STATUS_ERROR_ALLOCATION_FAILED;
        _s->segments[_s->segment_read] = fresh;
    }
    if (!_s->counts)
        return VA_STATUS_ERROR_ALLOCATION_FAILED;

    bool same_size = _s->last_valid && _h.width == _s->last_width && _h.height == _s->last_height;

    memset(&c, 0, sizeof(c));

    const uint8_t *stream = _frame.data() + _h.header_size + _h.compressed_size;
    c.stream_len = _frame.data() + _frame.size() - stream;
    c.Vp9BsdCtrlOffset = height * 912 / 256;

    Surface *refs[3];
    for (int i = 0; i < 3; i++) {
        VASurfaceID id = _h.intra() ? VA_INVALID_SURFACE : _pp.reference_frames[_h.ref_idx[i]];
        auto it = _s->sizes.find(id);
        Stream::Size size = it == _s->sizes.end() ? Stream::Size{ _h.width, _h.height } : it->second;

        refs[i] = reference(id);
        if (!fits(*refs[i], size.width, size.height))
            size = { refs[i]->width, refs[i]->height };
        auto set = [&](unsigned short &w, unsigned short &h, unsigned short *stride) {
            w = _h.intra() ? 0 : size.width;
            h = _h.intra() ? 0 : size.height;
            stride[0] = stride[1] = _h.intra() ? 0 : refs[i]->pitch;
        };
        if (i == 0)
            set(c.ref0_width, c.ref0_height, c.ref0_stride);
        else if (i == 1)
            set(c.ref1_width, c.ref1_height, c.ref1_stride);
        else
            set(c.ref2_width, c.ref2_height, c.ref2_stride);
    }

    c.width = _h.width;
    c.height = _h.height;
    c.framestride[0] = c.framestride[1] = _target->pitch;
    c.keyFrame = _h.key_frame;
    c.prevIsKeyFrame = _s->last_key;
    c.resolutionChange = _s->last_valid && !same_size;
    c.errorResilient = _h.error_res;
    c.prevShowFrame = _s->last_show;
    c.intraOnly = _h.intra_only;
    for (int i = 1; i < 4; i++)
        c.refFrameSignBias[i] = _h.sign_bias[i];
    c.loopFilterLevel = _h.filter_level;
    c.loopFilterSharpness = _h.filter_sharpness;
    c.qpYAc = _h.base_q;
    c.qpYDc = _h.y_dc;
    c.qpChAc = _h.uv_dc;
    c.qpChDc = _h.uv_ac;
    c.lossless = _h.lossless;
    c.transform_mode = _h.tx_mode;
    c.allow_high_precision_mv = _h.allow_hp;
    c.mcomp_filter_type = _h.interp_filter;
    c.comp_pred_mode = _h.comp_mode;
    if (_h.sign_bias[1] != _h.sign_bias[2] || _h.sign_bias[1] != _h.sign_bias[3]) {
        c.comp_fixed_ref = _h.comp_fixed;
        c.comp_var_ref[0] = _h.comp_var[0];
        c.comp_var_ref[1] = _h.comp_var[1];
    }
    c.log2_tile_columns = _h.tile_columns;
    c.log2_tile_rows = _h.tile_rows;
    c.segmentEnabled = _h.seg_enabled;
    c.segmentMapUpdate = _h.seg_update_map;
    c.segmentMapTemporalUpdate = _h.seg_temporal;
    c.segmentFeatureMode = _s->seg_abs;
    for (int i = 0; i < 8; i++)
        for (int j = 0; j < 4; j++) {
            c.segmentFeatureEnable[i][j] = _s->seg_feature[i][j];
            c.segmentFeatureData[i][j] = _s->seg_value[i][j];
        }
    c.modeRefLfEnabled = _s->lf_delta_enabled;
    for (int i = 0; i < 4; i++)
        c.mbRefLfDelta[i] = _s->lf_ref[i];
    for (int i = 0; i < 2; i++)
        c.mbModeLfDelta[i] = _s->lf_mode[i];

    nvdec_vp9EntropyProbs_t probs;
    fillProbs(probs);

    /* the size of the tiles */
    uint16_t tiles[0x400] = { 0 };
    unsigned sb_columns = width / 64, sb_rows = height / 64, n = 0;
    for (unsigned y = 0; y < 1u << _h.tile_rows; y++)
        for (unsigned x = 0; x < 1u << _h.tile_columns; x++) {
            tiles[n++] = (sb_columns * (x + 1) >> _h.tile_columns) - (sb_columns * x >> _h.tile_columns);
            tiles[n++] = (sb_rows * (y + 1) >> _h.tile_rows) - (sb_rows * y >> _h.tile_rows);
        }
    tiles[0x37a] = 9;
    tiles[0x37b] = 1;

    job.codec = NVC5B0_SET_CONTROL_PARAMS_CODEC_TYPE_VP9;
    job.setSetup(c);
    job.bitstream.assign(stream, (const uint8_t *)_frame.data() + _frame.size());
    job.addTable(NVC5B0_VP9_SET_PROB_TAB_BUF_OFFSET, &probs, sizeof(probs));
    job.addTable(NVC5B0_VP9_SET_TILE_SIZE_BUF_OFFSET, tiles, sizeof(tiles));
    for (int i = 0; i < 3; i++)
        addSurface(job, i, refs[i]);
    addSurface(job, 3, _target);
    job.addBuffer(NVC5B0_VP9_SET_CTX_COUNTER_BUF_OFFSET, _s->counts, 0);
    job.addBuffer(NVC5B0_VP9_SET_COL_MVWRITE_BUF_OFFSET, _s->mvs[_s->mv_write], 0);
    job.addBuffer(NVC5B0_VP9_SET_COL_MVREAD_BUF_OFFSET, _s->mvs[!_s->mv_write], 0);
    job.addBuffer(NVC5B0_VP9_SET_SEGMENT_READ_BUF_OFFSET, _s->segments[_s->segment_read], 0);
    job.addBuffer(NVC5B0_VP9_SET_SEGMENT_WRITE_BUF_OFFSET, _s->segments[!_s->segment_read], 0);
    job.addBuffer(NVC5B0_VP9_SET_FILTER_BUFFER_OFFSET, _s->filter, 0);

    if (_h.seg_update_map)
        _s->segment_read = !_s->segment_read;
    _s->mv_write = !_s->mv_write;

    /* the probabilities of the next frame */
    if (!_h.error_res && !_h.parallel) {
        _s->adapt = true;
        _s->adapt_header = _h;
        _s->adapt_header.after_key = _s->last_key;
    }
    if (_h.refresh_context && !_s->adapt)
        _s->contexts[_h.context] = _s->probs;
    else if (_s->adapt && !_h.refresh_context)
        _s->adapt = false;

    _s->sizes[_target_id] = { _h.width, _h.height };
    _s->last_key = _h.key_frame;
    _s->last_show = _h.show_frame;
    _s->last_width = _h.width;
    _s->last_height = _h.height;
    _s->last_valid = true;
    return VA_STATUS_SUCCESS;
}

void Vp9Decoder::submitted(const Fence &fence)
{
    _s->fence = fence;
    _target->stream = _s;
}
