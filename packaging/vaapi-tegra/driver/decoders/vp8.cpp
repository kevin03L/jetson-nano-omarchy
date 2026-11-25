// SPDX-License-Identifier: LGPL-2.1-or-later
/*
 * VP8 with NVDEC: its firmware reads the headers itself. What the hardware expects in its tables
 * comes from the nvtegra decoder of averne's FFmpeg (Copyright (c) 2024 averne, LGPL-2.1-or-later).
 */
#include "vp8.h"

#include <linux/kernel.h>

#include "../engines/nvdec.h"
#include "../objects.h"
#include "vp8_tables.h"

void Vp8Decoder::beginPicture()
{
    _valid = false;
    _bitstream.clear();
    _first_part = 0;
}

VAStatus Vp8Decoder::renderPicture(const Buffer &buffer)
{
    if (buffer.data.size() < sizeof(_pp))
        return VA_STATUS_ERROR_INVALID_BUFFER;

    memcpy(&_pp, buffer.data.data(), sizeof(_pp));
    _valid = true;
    return VA_STATUS_SUCCESS;
}

VAStatus Vp8Decoder::renderSlice(const void *param, const uint8_t *data, uint32_t size)
{
    auto *slice = (const VASliceParameterBufferVP8 *)param;

    /* the size of the first partition without what libva already counted as read */
    _first_part = slice->partition_size[0] + (slice->macroblock_offset + 7) / 8;
    _bitstream.assign(data, data + size);
    return VA_STATUS_SUCCESS;
}

VAStatus Vp8Decoder::build(NvdecJob &job)
{
    nvdec_vp8_pic_s c;

    if (!_valid || _bitstream.empty())
        return VA_STATUS_ERROR_INVALID_PARAMETER;

    if (!fits(*_target, __ALIGN_KERNEL(_pp.frame_width, 16), __ALIGN_KERNEL(_pp.frame_height, 16)))
        return VA_STATUS_ERROR_INVALID_PARAMETER;

    size_t history_size = __ALIGN_KERNEL(_pp.frame_width, 16) / 16 * 0x200;
    if (!_prob_data) {
        /* the firmware keeps the probabilities there, starting from the defaults */
        _prob_data = _nvdec.allocate(0x4b00);
        if (!_prob_data)
            return VA_STATUS_ERROR_ALLOCATION_FAILED;

        auto *p = (uint8_t *)_prob_data->map();
        for (auto &i : vp8_default_coef)
            for (auto &j : i)
                for (auto &k : j) {
                    memcpy(p, k, 11);
                    p += 12;
                }
        memcpy(p, vp8_default_y_mode, 4);
        memcpy(p + 4, vp8_default_uv_mode, 3);
        memcpy(p + 8, vp8_default_mv[0], 19);
        memcpy(p + 28, vp8_default_mv[1], 19);
    }
    if (history_size > _history_size) {
        _history = _nvdec.allocate(history_size);
        _history_size = history_size;
    }
    if (!_history) {
        _history_size = 0;
        return VA_STATUS_ERROR_ALLOCATION_FAILED;
    }

    const auto &pic = _pp.pic_fields.bits;

    memset(&c, 0, sizeof(c));
    c.FrameWidth = __ALIGN_KERNEL(_pp.frame_width, 16);
    c.FrameHeight = __ALIGN_KERNEL(_pp.frame_height, 16);
    c.keyFrame = !pic.key_frame;
    c.version = pic.version;
    c.errorConcealOn = 1;
    c.firstPartSize = _first_part;
    c.HistBufferSize = history_size / 256;
    c.VLDBufferSize = _bitstream.size();
    c.FrameStride[0] = c.FrameStride[1] = _target->pitch / 16;
    c.segmentation_feature_data_update = pic.segmentation_enabled ? pic.update_segment_feature_data : 0;
    c.reserved1[0] = c.segmentation_feature_data_update;

    job.codec = NVC5B0_SET_CONTROL_PARAMS_CODEC_TYPE_VP8;
    job.setSetup(c);
    job.bitstream = std::move(_bitstream);
    addSurface(job, 0, sameSizeReference(_pp.golden_ref_frame));
    addSurface(job, 1, sameSizeReference(_pp.alt_ref_frame));
    addSurface(job, 2, sameSizeReference(_pp.last_ref_frame));
    addSurface(job, 3, _target);
    job.addBuffer(NVC5B0_VP8_SET_PROB_DATA_OFFSET, _prob_data, 0);
    job.addBuffer(NVC5B0_SET_HISTORY_OFFSET, _history, 0);
    return VA_STATUS_SUCCESS;
}
