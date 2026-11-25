// SPDX-License-Identifier: LGPL-2.1-or-later
#ifndef VP9_H
#define VP9_H

#include <map>

#include "decoder.h"

/* The probabilities of a frame, in the order of the specification */
struct Vp9Probs {
    uint8_t tx8[2][1], tx16[2][2], tx32[2][3];
    uint8_t coef[4][2][2][6][6][3];
    uint8_t skip[3];
    uint8_t inter_mode[7][3];
    uint8_t interp_filter[4][2];
    uint8_t is_inter[4];
    uint8_t comp_mode[5];
    uint8_t single_ref[5][2];
    uint8_t comp_ref[5];
    uint8_t y_mode[4][9];
    uint8_t uv_mode[10][9];
    uint8_t partition[16][3];
    uint8_t mv_joint[3];
    uint8_t mv_sign[2];
    uint8_t mv_classes[2][10];
    uint8_t mv_class0_bit[2];
    uint8_t mv_bits[2][10];
    uint8_t mv_class0_fr[2][2][3];
    uint8_t mv_fr[2][3];
    uint8_t mv_class0_hp[2];
    uint8_t mv_hp[2];

    void setDefault();
};

class Vp9Decoder : public Decoder
{
public:
    using Decoder::Decoder;

    VAStatus build(NvdecJob &job) override;
    void submitted(const Fence &fence) override;

protected:
    void beginPicture() override;
    VAStatus renderPicture(const Buffer &buffer) override;
    VAStatus renderSlice(const void *param, const uint8_t *data, uint32_t size) override;

private:
    /* what the uncompressed and the compressed headers say */
    struct Header {
        bool key_frame, show_frame, error_res, intra_only;
        unsigned reset_frame_context;
        bool after_key;
        unsigned ref_idx[3];
        bool sign_bias[4];
        bool allow_hp;
        unsigned interp_filter; /* as in the bitstream: smooth, regular, sharp, bilinear, switchable */
        bool refresh_context, parallel;
        unsigned context;
        unsigned width, height;
        unsigned filter_level, filter_sharpness;
        unsigned base_q;
        int y_dc, uv_dc, uv_ac;
        bool lossless;
        bool seg_enabled, seg_update_map, seg_temporal;
        unsigned tile_columns, tile_rows; /* log2 */
        unsigned header_size;             /* uncompressed */
        unsigned compressed_size;
        unsigned tx_mode;
        unsigned comp_mode; /* single, compound, both */
        unsigned comp_fixed, comp_var[2];

        bool intra() const { return key_frame || intra_only; }
    };

    bool parseHeader(const uint8_t *data, size_t size);
    bool parseCompressedHeader(const uint8_t *data, size_t size);
    void adapt();
    void fillProbs(struct nvdec_vp9EntropyProbs_s &out);

    bool _valid;
    VADecPictureParameterBufferVP9 _pp;
    std::vector<uint8_t> _frame;
    Header _h;

    /* kept between frames */
    Vp9Probs _probs, _contexts[4];
    bool _lf_delta_enabled = false;
    int8_t _lf_ref[4] = { 1, 0, -1, -1 }, _lf_mode[2] = { 0, 0 };
    bool _seg_abs = false;
    bool _seg_feature[8][4] = {};
    int16_t _seg_value[8][4] = {};
    uint8_t _seg_tree[7] = {}, _seg_pred[3] = {};
    bool _last_key = false, _last_show = false, _last_valid = false;
    unsigned _last_width = 0, _last_height = 0;

    /* the last frame changes the probabilities with what NVDEC counted */
    bool _adapt = false;
    Header _adapt_header;
    Fence _fence;

    /* the size of the frame in each surface */
    struct Size {
        unsigned width, height;
    };
    std::map<VASurfaceID, Size> _sizes;

    std::shared_ptr<GemBuffer> _segments[2], _mvs[2], _filter, _counts;
    size_t _segment_size = 0, _mv_size = 0, _filter_size = 0;
    unsigned _segment_read = 0, _mv_write = 0;
};

#endif
