// SPDX-License-Identifier: MIT
#include "mpeg2.h"

#include <algorithm>

#include "../engines/nvdec.h"
#include "../objects.h"

static const uint8_t default_intra[64] = {
     8, 16, 19, 22, 26, 27, 29, 34,
    16, 16, 22, 24, 27, 29, 34, 37,
    19, 22, 26, 27, 29, 34, 34, 38,
    22, 22, 26, 27, 29, 34, 37, 40,
    22, 26, 27, 29, 32, 35, 40, 48,
    26, 27, 29, 32, 35, 40, 48, 58,
    26, 27, 29, 34, 38, 46, 56, 69,
    27, 29, 35, 38, 46, 56, 69, 83,
};

/* libva gives the matrices in zigzag order, NVDEC takes them line by line */
static const uint8_t zigzag[64] = {
     0,  1,  8, 16,  9,  2,  3, 10, 17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
};

void Mpeg2Decoder::beginPicture()
{
    _valid = false;
    _bitstream.clear();
    _offsets.clear();
    _rows = 0;
}

VAStatus Mpeg2Decoder::renderPicture(const Buffer &buffer)
{
    if (buffer.data.size() < sizeof(_pp))
        return VA_STATUS_ERROR_INVALID_BUFFER;

    memcpy(&_pp, buffer.data.data(), sizeof(_pp));
    memset(&_iq, 0, sizeof(_iq));
    _valid = true;
    return VA_STATUS_SUCCESS;
}

VAStatus Mpeg2Decoder::renderIQMatrix(const Buffer &buffer)
{
    if (buffer.data.size() < sizeof(_iq))
        return VA_STATUS_ERROR_INVALID_BUFFER;

    memcpy(&_iq, buffer.data.data(), sizeof(_iq));
    return VA_STATUS_SUCCESS;
}

VAStatus Mpeg2Decoder::renderSlice(const void *param, const uint8_t *data, uint32_t size)
{
    auto *slice = (const VASliceParameterBufferMPEG2 *)param;

    _rows = std::max(_rows, slice->slice_vertical_position + 1);
    _offsets.push_back(_bitstream.size());
    _bitstream.insert(_bitstream.end(), data, data + size);
    return VA_STATUS_SUCCESS;
}

VAStatus Mpeg2Decoder::build(NvdecJob &job)
{
    static const uint8_t end[16] = { 0x00, 0x00, 0x01, 0xb7, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x01, 0xb7, 0x00, 0x00, 0x00, 0x00 };
    const auto &ext = _pp.picture_coding_extension.bits;
    nvdec_mpeg2_pic_s c;

    if (!_valid || _offsets.empty())
        return VA_STATUS_ERROR_INVALID_PARAMETER;

    memset(&c, 0, sizeof(c));

    /* an interlaced sequence has an even number of rows in a frame */
    unsigned rows = (_pp.vertical_size + 15) / 16;
    if (ext.picture_structure == 3 && _rows == rows + 1)
        rows = _rows;

    if (!fits(*_target, (_pp.horizontal_size + 15) & ~15, rows * 16))
        return VA_STATUS_ERROR_INVALID_PARAMETER;

    c.slice_count = _offsets.size();
    _offsets.push_back(_bitstream.size());
    _bitstream.insert(_bitstream.end(), end, end + sizeof(end));
    c.stream_len = _bitstream.size();

    c.FrameWidth = (_pp.horizontal_size + 15) & ~15;
    c.FrameHeight = rows * 16;
    c.picture_structure = ext.picture_structure;
    c.picture_coding_type = _pp.picture_coding_type;
    c.intra_dc_precision = ext.intra_dc_precision;
    c.frame_pred_frame_dct = ext.frame_pred_frame_dct;
    c.concealment_motion_vectors = ext.concealment_motion_vectors;
    c.intra_vlc_format = ext.intra_vlc_format;
    for (int i = 0; i < 4; i++)
        c.f_code[i] = _pp.f_code >> (12 - 4 * i) & 0xf;
    c.PicWidthInMbs = c.FrameWidth / 16;
    c.FrameHeightInMbs = rows;
    c.pitch_luma = _target->pitch;
    c.pitch_chroma = _target->pitch;
    c.alternate_scan = ext.alternate_scan;
    c.secondfield = ext.picture_structure != 3 && !ext.is_first_field;
    c.q_scale_type = ext.q_scale_type;
    c.top_field_first = ext.top_field_first;

    for (int i = 0; i < 64; i++) {
        c.quant_mat_8x8intra[zigzag[i]] = _iq.load_intra_quantiser_matrix ?
            _iq.intra_quantiser_matrix[i] : default_intra[zigzag[i]];
        c.quant_mat_8x8nonintra[zigzag[i]] = _iq.load_non_intra_quantiser_matrix ?
            _iq.non_intra_quantiser_matrix[i] : 16;
    }

    job.codec = NVC5B0_SET_CONTROL_PARAMS_CODEC_TYPE_MPEG2;
    job.setSetup(c);
    job.bitstream = std::move(_bitstream);
    job.slice_offsets = std::move(_offsets);

    VASurfaceID forward = _pp.forward_reference_picture, backward = _pp.backward_reference_picture;
    if (forward == VA_INVALID_SURFACE)
        forward = backward;
    addSurface(job, 0, _target);
    addSurface(job, 1, sameSizeReference(forward));
    addSurface(job, 2, sameSizeReference(backward));
    return VA_STATUS_SUCCESS;
}
