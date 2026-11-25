// SPDX-License-Identifier: MIT
#include "h264.h"

#include <cstdio>
#include <linux/kernel.h>

#include "../engines/nvdec.h"
#include "../objects.h"

H264Decoder::H264Decoder(NvdecDevice &nvdec, Objects &objects, unsigned width, unsigned height)
    : Decoder(nvdec, objects, width, height)
{
    _slots.fill(VA_INVALID_SURFACE);
}

void H264Decoder::beginPicture()
{
    _valid = false;
    _bitstream.clear();
    _offsets.clear();
    _refs_l0 = _refs_l1 = 0;
}

VAStatus H264Decoder::renderPicture(const Buffer &buffer)
{
    if (buffer.data.size() < sizeof(_pp))
        return VA_STATUS_ERROR_INVALID_BUFFER;

    memcpy(&_pp, buffer.data.data(), sizeof(_pp));
    memset(&_iq, 16, sizeof(_iq));
    _valid = true;
    return VA_STATUS_SUCCESS;
}

VAStatus H264Decoder::renderIQMatrix(const Buffer &buffer)
{
    if (buffer.data.size() < sizeof(_iq))
        return VA_STATUS_ERROR_INVALID_BUFFER;

    memcpy(&_iq, buffer.data.data(), sizeof(_iq));
    return VA_STATUS_SUCCESS;
}

VAStatus H264Decoder::renderSlice(const void *param, const uint8_t *data, uint32_t size)
{
    auto *slice = (const VASliceParameterBufferH264 *)param;

    if (!_valid || size < 2)
        return VA_STATUS_ERROR_INVALID_PARAMETER;

    /*
     * NVDEC reads the slice headers itself, with the default numbers of references of the PPS,
     * that libva does not give: take them from a slice that does not override them.
     */
    unsigned type = slice->slice_type % 5;
    if (type != 2 && type != 4) {
        const auto &seq = _pp.seq_fields.bits;
        const auto &pic = _pp.pic_fields.bits;
        BitReader r(data, size);

        unsigned nal_type = r.u(8) & 0x1f;
        r.ue();
        r.ue();
        r.ue();
        if (seq.residual_colour_transform_flag)
            r.u(2);
        r.u(seq.log2_max_frame_num_minus4 + 4);
        if (!seq.frame_mbs_only_flag && r.u(1))
            r.u(1);
        if (nal_type == 5)
            r.ue();
        if (seq.pic_order_cnt_type == 0) {
            r.u(seq.log2_max_pic_order_cnt_lsb_minus4 + 4);
            if (pic.pic_order_present_flag && !pic.field_pic_flag)
                r.ue();
        } else if (seq.pic_order_cnt_type == 1 && !seq.delta_pic_order_always_zero_flag) {
            r.ue();
            if (pic.pic_order_present_flag && !pic.field_pic_flag)
                r.ue();
        }
        if (pic.redundant_pic_cnt_present_flag)
            r.ue();
        if (type == 1)
            r.u(1);
        if (!r.u(1) && !r.overrun()) {
            _refs_l0 = slice->num_ref_idx_l0_active_minus1;
            if (type == 1)
                _refs_l1 = slice->num_ref_idx_l1_active_minus1;
        }
    }

    static const uint8_t start[3] = { 0, 0, 1 };
    _offsets.push_back(_bitstream.size());
    _bitstream.insert(_bitstream.end(), start, start + 3);
    _bitstream.insert(_bitstream.end(), data, data + size);
    return VA_STATUS_SUCCESS;
}

int H264Decoder::findSlot(VASurfaceID id) const
{
    for (size_t i = 0; i < _slots.size(); i++)
        if (_slots[i] == id)
            return i;
    return -1;
}

/* the slot of the picture being decoded, after dropping the pictures nobody refers to any more */
int H264Decoder::currentSlot()
{
    for (auto &slot : _slots) {
        bool used = slot == _pp.CurrPic.picture_id;
        for (const VAPictureH264 &ref : _pp.ReferenceFrames)
            if (!(ref.flags & VA_PICTURE_H264_INVALID) && ref.picture_id == slot)
                used = true;
        if (!used)
            slot = VA_INVALID_SURFACE;
    }

    int slot = findSlot(_pp.CurrPic.picture_id);
    if (slot < 0) {
        slot = findSlot(VA_INVALID_SURFACE);
        if (slot >= 0)
            _slots[slot] = _pp.CurrPic.picture_id;
    }
    return slot;
}

VAStatus H264Decoder::build(NvdecJob &job)
{
    static const uint8_t end[16] = { 0x00, 0x00, 0x01, 0x0b, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x01, 0x0b, 0x00, 0x00, 0x00, 0x00 };
    const auto &seq = _pp.seq_fields.bits;
    const auto &pic = _pp.pic_fields.bits;
    nvdec_h264_pic_s c;

    if (!_valid || _offsets.empty())
        return VA_STATUS_ERROR_INVALID_PARAMETER;

    int slot = currentSlot();
    if (slot < 0) {
        fprintf(stderr, "tegra: too many reference pictures\n");
        return VA_STATUS_ERROR_DECODING_ERROR;
    }

    unsigned width_mbs = _pp.picture_width_in_mbs_minus1 + 1, height_mbs = _pp.picture_height_in_mbs_minus1 + 1;
    if (!fits(*_target, width_mbs * 16, height_mbs * 16))
        return VA_STATUS_ERROR_INVALID_PARAMETER;

    size_t coloc_size = __ALIGN_KERNEL(__ALIGN_KERNEL(height_mbs, 2) * width_mbs * 64, 256);
    size_t history_size = __ALIGN_KERNEL(width_mbs * 0x200 + 0x1100, 0x200);
    size_t mbhist_size = __ALIGN_KERNEL(width_mbs * 104, 0x100);
    if (coloc_size > _coloc_size || history_size > _history_size) {
        _coloc = _nvdec.allocate(17 * coloc_size); /* 16 references and the current picture */
        _history = _nvdec.allocate(history_size);
        _mbhist = _nvdec.allocate(mbhist_size);
        if (!_coloc || !_history || !_mbhist)
            return VA_STATUS_ERROR_ALLOCATION_FAILED;
        _coloc_size = coloc_size;
        _history_size = history_size;
        _mbhist_size = mbhist_size;
    }

    memset(&c, 0, sizeof(c));

    c.slice_count = _offsets.size();
    _offsets.push_back(_bitstream.size());
    _bitstream.insert(_bitstream.end(), end, end + sizeof(end));
    c.stream_len = _bitstream.size();
    c.mbhist_buffer_size = _mbhist_size;

    c.log2_max_pic_order_cnt_lsb_minus4 = seq.log2_max_pic_order_cnt_lsb_minus4;
    c.delta_pic_order_always_zero_flag = seq.delta_pic_order_always_zero_flag;
    c.frame_mbs_only_flag = seq.frame_mbs_only_flag;
    c.PicWidthInMbs = width_mbs;
    c.FrameHeightInMbs = height_mbs;
    c.entropy_coding_mode_flag = pic.entropy_coding_mode_flag;
    c.pic_order_present_flag = pic.pic_order_present_flag;
    c.num_ref_idx_l0_active_minus1 = _refs_l0;
    c.num_ref_idx_l1_active_minus1 = _refs_l1;
    c.deblocking_filter_control_present_flag = pic.deblocking_filter_control_present_flag;
    c.redundant_pic_cnt_present_flag = pic.redundant_pic_cnt_present_flag;
    c.transform_8x8_mode_flag = pic.transform_8x8_mode_flag;
    c.pitch_luma = _target->pitch;
    c.pitch_chroma = _target->pitch;
    c.HistBufferSize = _history_size / 256;

    c.MbaffFrameFlag = seq.mb_adaptive_frame_field_flag && !pic.field_pic_flag;
    c.direct_8x8_inference_flag = seq.direct_8x8_inference_flag;
    c.weighted_pred_flag = pic.weighted_pred_flag;
    c.constrained_intra_pred_flag = pic.constrained_intra_pred_flag;
    c.ref_pic_flag = pic.reference_pic_flag;
    c.field_pic_flag = pic.field_pic_flag;
    c.bottom_field_flag = !!(_pp.CurrPic.flags & VA_PICTURE_H264_BOTTOM_FIELD);
    c.second_field = pic.field_pic_flag && _previous == _pp.CurrPic.picture_id;
    c.log2_max_frame_num_minus4 = seq.log2_max_frame_num_minus4;
    c.chroma_format_idc = seq.chroma_format_idc;
    c.pic_order_cnt_type = seq.pic_order_cnt_type;
    c.pic_init_qp_minus26 = _pp.pic_init_qp_minus26;
    c.chroma_qp_index_offset = _pp.chroma_qp_index_offset;
    c.second_chroma_qp_index_offset = _pp.second_chroma_qp_index_offset;
    c.weighted_bipred_idc = pic.weighted_bipred_idc;
    c.CurrPicIdx = slot;
    c.CurrColIdx = slot;
    c.frame_num = _pp.frame_num;
    c.CurrFieldOrderCnt[0] = _pp.CurrPic.TopFieldOrderCnt;
    c.CurrFieldOrderCnt[1] = _pp.CurrPic.BottomFieldOrderCnt;
    c.lossless_ipred8x8_filter_enable = 1;

    static_assert(sizeof(c.WeightScale) == sizeof(_iq.ScalingList4x4));
    static_assert(sizeof(c.WeightScale8x8) == sizeof(_iq.ScalingList8x8));
    memcpy(c.WeightScale, _iq.ScalingList4x4, sizeof(c.WeightScale));
    memcpy(c.WeightScale8x8, _iq.ScalingList8x8, sizeof(c.WeightScale8x8));

    /* a place nobody refers to holds the current picture */
    std::array<Surface *, 17> surfaces;
    surfaces.fill(_target);

    for (int i = 0; i < 16; i++) {
        const VAPictureH264 &ref = _pp.ReferenceFrames[i];
        auto &dpb = c.dpb[i];

        if (ref.flags & VA_PICTURE_H264_INVALID)
            continue;
        int ref_slot = findSlot(ref.picture_id);
        if (ref_slot < 0)
            continue;

        unsigned fields = ref.flags & (VA_PICTURE_H264_TOP_FIELD | VA_PICTURE_H264_BOTTOM_FIELD);
        unsigned state = 0;
        if (ref.flags & VA_PICTURE_H264_TOP_FIELD)
            state |= 1;
        if (ref.flags & VA_PICTURE_H264_BOTTOM_FIELD)
            state |= 2;
        if (!state)
            state = 3;

        dpb.index = ref_slot;
        dpb.col_idx = ref_slot;
        dpb.state = state;
        dpb.is_long_term = !!(ref.flags & VA_PICTURE_H264_LONG_TERM_REFERENCE);
        dpb.is_field = !!fields;
        unsigned marking = dpb.is_long_term ? 2 : 1;
        dpb.top_field_marking = state & 1 ? marking : 0;
        dpb.bottom_field_marking = state & 2 ? marking : 0;
        dpb.FieldOrderCnt[0] = ref.TopFieldOrderCnt;
        dpb.FieldOrderCnt[1] = ref.BottomFieldOrderCnt;
        dpb.FrameIdx = ref.frame_idx;

        surfaces[ref_slot] = sameSizeReference(ref.picture_id);
    }

    job.codec = NVC5B0_SET_CONTROL_PARAMS_CODEC_TYPE_H264;
    job.setSetup(c);
    job.bitstream = std::move(_bitstream);
    job.slice_offsets = std::move(_offsets);
    for (size_t i = 0; i < surfaces.size(); i++)
        addSurface(job, i, surfaces[i]);
    job.addBuffer(NVC5B0_SET_COLOC_DATA_OFFSET, _coloc, 0);
    job.addBuffer(NVC5B0_SET_HISTORY_OFFSET, _history, 0);
    job.addBuffer(NVC5B0_H264_SET_MBHIST_BUF_OFFSET, _mbhist, 0);

    _previous = _previous == _pp.CurrPic.picture_id ? VA_INVALID_SURFACE : _pp.CurrPic.picture_id;
    return VA_STATUS_SUCCESS;
}
