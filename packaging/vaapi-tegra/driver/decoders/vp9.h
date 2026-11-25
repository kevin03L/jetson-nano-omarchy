// SPDX-License-Identifier: LGPL-2.1-or-later
#ifndef VP9_H
#define VP9_H

#include <map>

#include "../objects.h"
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
    size_t sliceParamSize() const override { return sizeof(VASliceParameterBufferVP9); }
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

    /*
     * Kept between frames. A player makes another context when the size changes, in the middle of a
     * stream: the surfaces remember what they belong to, the new decoder carries on with it.
     */
    struct Stream : public Object {
        Vp9Probs probs, contexts[4];
        bool lf_delta_enabled = false;
        int8_t lf_ref[4] = { 1, 0, -1, -1 }, lf_mode[2] = { 0, 0 };
        bool seg_abs = false;
        bool seg_feature[8][4] = {};
        int16_t seg_value[8][4] = {};
        uint8_t seg_tree[7] = {}, seg_pred[3] = {};
        bool last_key = false, last_show = false, last_valid = false;
        unsigned last_width = 0, last_height = 0;

        /* the last frame changes the probabilities with what NVDEC counted */
        bool adapt = false;
        Header adapt_header;
        Fence fence;

        /* the size of the frame in each surface */
        struct Size {
            unsigned width, height;
        };
        std::map<VASurfaceID, Size> sizes;

        std::shared_ptr<GemBuffer> segments[2], mvs[2], filter, counts;
        size_t segment_size = 0, mv_size = 0, filter_size = 0;
        unsigned segment_read = 0, mv_write = 0;
    };
    std::shared_ptr<Stream> _s;
};

#endif
