// SPDX-License-Identifier: LGPL-2.1-or-later
/*
 * H.265 with NVDEC. What the hardware expects in its tables comes from the nvtegra decoder of
 * averne's FFmpeg (Copyright (c) 2024 averne, LGPL-2.1-or-later).
 */
#include "hevc.h"

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <linux/kernel.h>

#include "../engines/nvdec.h"
#include "../objects.h"

HevcDecoder::HevcDecoder(NvdecDevice &nvdec, Objects &objects, unsigned width, unsigned height)
    : Decoder(nvdec, objects, width, height)
{
    _slots.fill(VA_INVALID_SURFACE);
}

void HevcDecoder::beginPicture()
{
    _valid = _has_iq = false;
    _first = true;
    _skip = 0;
    _bitstream.clear();
    _long_term.clear();
}

VAStatus HevcDecoder::renderPicture(const Buffer &buffer)
{
    if (buffer.data.size() < sizeof(_pp))
        return VA_STATUS_ERROR_INVALID_BUFFER;

    memcpy(&_pp, buffer.data.data(), sizeof(_pp));
    _valid = true;
    return VA_STATUS_SUCCESS;
}

VAStatus HevcDecoder::renderIQMatrix(const Buffer &buffer)
{
    if (buffer.data.size() < sizeof(_iq))
        return VA_STATUS_ERROR_INVALID_BUFFER;

    memcpy(&_iq, buffer.data.data(), sizeof(_iq));
    _has_iq = true;
    return VA_STATUS_SUCCESS;
}

static unsigned ceilLog2(unsigned v)
{
    unsigned n = 0;
    while ((1u << n) < v)
        n++;
    return n;
}

/*
 * NVDEC reads the slice headers itself, but not the part about the reference pictures, that it
 * skips: it wants to know the length of that part.
 */
void HevcDecoder::parseSliceHeader(const uint8_t *data, uint32_t size, const VASliceParameterBufferHEVC &slice)
{
    const auto &pic = _pp.slice_parsing_fields.bits;
    BitReader r(data, size);

    unsigned nal_type = r.u(16) >> 9 & 0x3f;
    bool first = r.u(1);
    if (nal_type >= 16 && nal_type <= 23)
        r.u(1);
    r.ue();

    if (!first) {
        unsigned log2_ctb = _pp.log2_min_luma_coding_block_size_minus3 + 3 + _pp.log2_diff_max_min_luma_coding_block_size;
        unsigned ctbs = __KERNEL_DIV_ROUND_UP(_pp.pic_width_in_luma_samples, 1 << log2_ctb) *
            __KERNEL_DIV_ROUND_UP(_pp.pic_height_in_luma_samples, 1 << log2_ctb);

        if (pic.dependent_slice_segments_enabled_flag && r.u(1))
            return;
        r.u(ceilLog2(ctbs));
    }

    r.u(_pp.num_extra_slice_header_bits);
    r.ue();
    if (pic.output_flag_present_flag)
        r.u(1);

    if (nal_type == 19 || nal_type == 20)
        return;

    size_t start = r.position();
    unsigned lsb_bits = _pp.log2_max_pic_order_cnt_lsb_minus4 + 4;
    r.u(lsb_bits);
    if (!r.u(1))
        for (unsigned i = 0; i < _pp.st_rps_bits; i++)
            r.u(1);
    else if (_pp.num_short_term_ref_pic_sets > 1)
        r.u(ceilLog2(_pp.num_short_term_ref_pic_sets));

    if (pic.long_term_ref_pics_present_flag) {
        unsigned sps = _pp.num_long_term_ref_pic_sps > 0 ? r.ue() : 0;
        unsigned pics = r.ue();

        for (unsigned i = 0; i < sps + pics && i < 33; i++) {
            if (i < sps) {
                if (_pp.num_long_term_ref_pic_sps > 1)
                    r.u(ceilLog2(_pp.num_long_term_ref_pic_sps));
            } else {
                r.u(lsb_bits);
                r.u(1);
            }
            if (r.u(1))
                r.ue();
        }
    }

    if (!r.overrun())
        _skip = r.position() - start;

    /* without changes to the lists, they give the order of the long term pictures */
    if (!pic.lists_modification_present_flag) {
        for (int i = 0; i < 15; i++) {
            uint8_t ref = slice.RefPicList[0][i];

            if (ref < 15 && (_pp.ReferenceFrames[ref].flags & VA_PICTURE_HEVC_RPS_LT_CURR) &&
                std::find(_long_term.begin(), _long_term.end(), ref) == _long_term.end())
                _long_term.push_back(ref);
        }
    }
}

VAStatus HevcDecoder::renderSlice(const void *param, const uint8_t *data, uint32_t size)
{
    static const uint8_t start[4] = { 0, 0, 0, 1 };

    if (!_valid || size < 3)
        return VA_STATUS_ERROR_INVALID_PARAMETER;

    if (_first) {
        parseSliceHeader(data, size, *(const VASliceParameterBufferHEVC *)param);
        _first = false;
    }

    _bitstream.insert(_bitstream.end(), start, start + 4);
    _bitstream.insert(_bitstream.end(), data, data + size);
    return VA_STATUS_SUCCESS;
}

int HevcDecoder::findSlot(VASurfaceID id) const
{
    for (size_t i = 0; i < _slots.size(); i++)
        if (_slots[i] == id)
            return i;
    return -1;
}

int HevcDecoder::currentSlot()
{
    for (auto &slot : _slots) {
        bool used = false;
        for (const VAPictureHEVC &ref : _pp.ReferenceFrames)
            if (!(ref.flags & VA_PICTURE_HEVC_INVALID) && ref.picture_id == slot)
                used = true;
        if (!used)
            slot = VA_INVALID_SURFACE;
    }

    int slot = findSlot(VA_INVALID_SURFACE);
    if (slot >= 0)
        _slots[slot] = _pp.CurrPic.picture_id;
    return slot;
}

VAStatus HevcDecoder::build(NvdecJob &job)
{
    const auto &seq = _pp.pic_fields.bits;
    const auto &pic = _pp.slice_parsing_fields.bits;
    nvdec_hevc_pic_s c;

    if (!_valid || _bitstream.empty())
        return VA_STATUS_ERROR_INVALID_PARAMETER;
    if (!fits(*_target, _pp.pic_width_in_luma_samples, _pp.pic_height_in_luma_samples) ||
        seq.chroma_format_idc != 1 || _pp.bit_depth_luma_minus8 > 2 || _pp.bit_depth_chroma_minus8 > 2)
        return VA_STATUS_ERROR_INVALID_PARAMETER;

    int slot = currentSlot();
    if (slot < 0) {
        fprintf(stderr, "tegra: too many reference pictures\n");
        return VA_STATUS_ERROR_DECODING_ERROR;
    }

    unsigned width = __ALIGN_KERNEL(_pp.pic_width_in_luma_samples, 64);
    unsigned height = __ALIGN_KERNEL(_pp.pic_height_in_luma_samples, 64);
    size_t colmv = width * height / 16;
    size_t filter = (480 + 3840 + 60) * height;
    if (17 * colmv > _coloc_size || filter > _filter_size) {
        _coloc = _nvdec.allocate(17 * colmv);
        _filter = _nvdec.allocate(filter);
        if (!_coloc || !_filter)
            return VA_STATUS_ERROR_ALLOCATION_FAILED;
        _coloc_size = 17 * colmv;
        _filter_size = filter;
    }

    memset(&c, 0, sizeof(c));

    unsigned depth = _pp.bit_depth_luma_minus8 + 8;
    c.stream_len = _bitstream.size();
    c.sw_start_code_e = 1;
    /* 10 bits to 10 bits, to 8 bits with Rec. 709 otherwise */
    c.disp_output_mode = depth == 8 ? 0 : _target->ten_bits ? 1 : 2;
    c.framestride[0] = c.framestride[1] = _target->pitch / (c.disp_output_mode == 1 ? 2 : 1);
    c.colMvBuffersize = colmv / 256;
    c.HevcSaoBufferOffset = 480 * height / 256;
    c.HevcBsdCtrlOffset = (480 + 3840) * height / 256;

    c.pic_width_in_luma_samples = _pp.pic_width_in_luma_samples;
    c.pic_height_in_luma_samples = _pp.pic_height_in_luma_samples;
    c.chroma_format_idc = 1;
    c.bit_depth_luma = depth;
    c.bit_depth_chroma = _pp.bit_depth_chroma_minus8 + 8;
    c.log2_min_luma_coding_block_size = _pp.log2_min_luma_coding_block_size_minus3 + 3;
    c.log2_max_luma_coding_block_size = c.log2_min_luma_coding_block_size + _pp.log2_diff_max_min_luma_coding_block_size;
    c.log2_min_transform_block_size = _pp.log2_min_transform_block_size_minus2 + 2;
    c.log2_max_transform_block_size = c.log2_min_transform_block_size + _pp.log2_diff_max_min_transform_block_size;
    c.max_transform_hierarchy_depth_inter = _pp.max_transform_hierarchy_depth_inter;
    c.max_transform_hierarchy_depth_intra = _pp.max_transform_hierarchy_depth_intra;
    c.scalingListEnable = seq.scaling_list_enabled_flag;
    c.amp_enable_flag = seq.amp_enabled_flag;
    c.sample_adaptive_offset_enabled_flag = pic.sample_adaptive_offset_enabled_flag;
    c.pcm_enabled_flag = seq.pcm_enabled_flag;
    if (seq.pcm_enabled_flag) {
        c.pcm_sample_bit_depth_luma = _pp.pcm_sample_bit_depth_luma_minus1 + 1;
        c.pcm_sample_bit_depth_chroma = _pp.pcm_sample_bit_depth_chroma_minus1 + 1;
        c.log2_min_pcm_luma_coding_block_size = _pp.log2_min_pcm_luma_coding_block_size_minus3 + 3;
        c.log2_max_pcm_luma_coding_block_size = c.log2_min_pcm_luma_coding_block_size + _pp.log2_diff_max_min_pcm_luma_coding_block_size;
    }
    c.pcm_loop_filter_disabled_flag = seq.pcm_loop_filter_disabled_flag;
    c.sps_temporal_mvp_enabled_flag = pic.sps_temporal_mvp_enabled_flag;
    c.strong_intra_smoothing_enabled_flag = seq.strong_intra_smoothing_enabled_flag;

    c.dependent_slice_segments_enabled_flag = pic.dependent_slice_segments_enabled_flag;
    c.output_flag_present_flag = pic.output_flag_present_flag;
    c.num_extra_slice_header_bits = _pp.num_extra_slice_header_bits;
    c.sign_data_hiding_enabled_flag = seq.sign_data_hiding_enabled_flag;
    c.cabac_init_present_flag = pic.cabac_init_present_flag;
    c.num_ref_idx_l0_default_active = _pp.num_ref_idx_l0_default_active_minus1 + 1;
    c.num_ref_idx_l1_default_active = _pp.num_ref_idx_l1_default_active_minus1 + 1;
    c.init_qp = _pp.init_qp_minus26 + 26 + (depth - 8) * 6;
    c.constrained_intra_pred_flag = seq.constrained_intra_pred_flag;
    c.transform_skip_enabled_flag = seq.transform_skip_enabled_flag;
    c.cu_qp_delta_enabled_flag = seq.cu_qp_delta_enabled_flag;
    c.diff_cu_qp_delta_depth = _pp.diff_cu_qp_delta_depth;
    c.pps_cb_qp_offset = _pp.pps_cb_qp_offset;
    c.pps_cr_qp_offset = _pp.pps_cr_qp_offset;
    c.pps_beta_offset = _pp.pps_beta_offset_div2 * 2;
    c.pps_tc_offset = _pp.pps_tc_offset_div2 * 2;
    c.pps_slice_chroma_qp_offsets_present_flag = pic.pps_slice_chroma_qp_offsets_present_flag;
    c.weighted_pred_flag = seq.weighted_pred_flag;
    c.weighted_bipred_flag = seq.weighted_bipred_flag;
    c.transquant_bypass_enabled_flag = seq.transquant_bypass_enabled_flag;
    c.tiles_enabled_flag = seq.tiles_enabled_flag;
    c.entropy_coding_sync_enabled_flag = seq.entropy_coding_sync_enabled_flag;
    if (seq.tiles_enabled_flag) {
        c.num_tile_columns = _pp.num_tile_columns_minus1 + 1;
        c.num_tile_rows = _pp.num_tile_rows_minus1 + 1;
        c.loop_filter_across_tiles_enabled_flag = seq.loop_filter_across_tiles_enabled_flag;
    }
    c.loop_filter_across_slices_enabled_flag = seq.pps_loop_filter_across_slices_enabled_flag;
    c.deblocking_filter_control_present_flag = pic.deblocking_filter_override_enabled_flag ||
        pic.pps_disable_deblocking_filter_flag || _pp.pps_beta_offset_div2 || _pp.pps_tc_offset_div2;
    c.deblocking_filter_override_enabled_flag = pic.deblocking_filter_override_enabled_flag;
    c.pps_deblocking_filter_disabled_flag = pic.pps_disable_deblocking_filter_flag;
    c.lists_modification_present_flag = pic.lists_modification_present_flag;
    c.log2_parallel_merge_level = _pp.log2_parallel_merge_level_minus2 + 2;
    c.slice_segment_header_extension_present_flag = pic.slice_segment_header_extension_present_flag;

    c.IDR_picture_flag = pic.IdrPicFlag;
    c.RAP_picture_flag = pic.RapPicFlag;
    c.curr_pic_idx = slot;
    c.pattern_id = 2; /* no dithering */
    c.sw_hdr_skip_length = _skip;

    /* the reference picture set, in the order of the standard: nearest pictures first */
    int32_t poc = _pp.CurrPic.pic_order_cnt;
    std::vector<uint8_t> before, after, longs = _long_term;
    for (uint8_t i = 0; i < 15; i++) {
        const VAPictureHEVC &ref = _pp.ReferenceFrames[i];

        if (ref.flags & VA_PICTURE_HEVC_INVALID)
            continue;
        if (ref.flags & VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE)
            before.push_back(i);
        else if (ref.flags & VA_PICTURE_HEVC_RPS_ST_CURR_AFTER)
            after.push_back(i);
        else if ((ref.flags & VA_PICTURE_HEVC_RPS_LT_CURR) && std::find(longs.begin(), longs.end(), i) == longs.end())
            longs.push_back(i);
    }
    auto nearer = [&](uint8_t a, uint8_t b) {
        return abs(_pp.ReferenceFrames[a].pic_order_cnt - poc) < abs(_pp.ReferenceFrames[b].pic_order_cnt - poc);
    };
    std::sort(before.begin(), before.end(), nearer);
    std::sort(after.begin(), after.end(), nearer);
    c.num_ref_frames = before.size() + after.size() + longs.size();

    auto fill = [&](unsigned char *list, std::initializer_list<const std::vector<uint8_t> *> sets) {
        for (unsigned n = 0; c.num_ref_frames && n < 16;)
            for (auto *set : sets)
                for (uint8_t ref : *set)
                    if (n < 16) {
                        int s = findSlot(_pp.ReferenceFrames[ref].picture_id);
                        list[n++] = s < 0 ? slot : s;
                    }
    };
    fill(c.initreflistidxl0, { &before, &after, &longs });
    fill(c.initreflistidxl1, { &after, &before, &longs });

    /* a place nobody refers to holds a picture that is there */
    Surface *scratch = _target;
    int scratch_poc = 0;
    auto clip = [](int v) { return std::max(-128, std::min(127, v)); };
    for (auto *set : { &before, &after, &longs }) {
        if (!set->empty()) {
            scratch = sameSizeReference(_pp.ReferenceFrames[set->front()].picture_id);
            scratch_poc = clip(poc - _pp.ReferenceFrames[set->front()].pic_order_cnt);
            break;
        }
    }

    std::array<Surface *, 16> surfaces;
    surfaces.fill(scratch);
    for (int i = 0; i < 16; i++)
        c.RefDiffPicOrderCnts[i] = i == slot ? 0 : scratch_poc;
    for (const VAPictureHEVC &ref : _pp.ReferenceFrames) {
        int s = ref.flags & VA_PICTURE_HEVC_INVALID ? -1 : findSlot(ref.picture_id);

        if (s < 0 || s == slot)
            continue;
        surfaces[s] = sameSizeReference(ref.picture_id);
        c.RefDiffPicOrderCnts[s] = clip(poc - ref.pic_order_cnt);
        if (ref.flags & VA_PICTURE_HEVC_LONG_TERM_REFERENCE)
            c.longtermflag |= 1 << (15 - s);
    }
    surfaces[slot] = _target;

    if (_has_iq && seq.scaling_list_enabled_flag) {
        /* 16x16 and 32x32 DC, then the lists, as the standard scans them */
        struct {
            uint8_t dc16[6], dc32[2], reserved[8];
            uint8_t list4[6][16], list8[6][64], list16[6][64], list32[2][64];
        } lists;

        memset(&lists, 0, sizeof(lists));
        memcpy(lists.dc16, _iq.ScalingListDC16x16, 6);
        memcpy(lists.dc32, _iq.ScalingListDC32x32, 2);
        memcpy(lists.list4, _iq.ScalingList4x4, sizeof(lists.list4));
        memcpy(lists.list8, _iq.ScalingList8x8, sizeof(lists.list8));
        memcpy(lists.list16, _iq.ScalingList16x16, sizeof(lists.list16));
        memcpy(lists.list32, _iq.ScalingList32x32, sizeof(lists.list32));
        job.addTable(NVC5B0_HEVC_SET_SCALING_LIST_OFFSET, &lists, sizeof(lists));
    }

    /* the size of each tile, then where the columns and the rows end */
    uint16_t tiles[0x480] = { 0 };
    unsigned log2_ctb = c.log2_max_luma_coding_block_size;
    unsigned ctb_width = __KERNEL_DIV_ROUND_UP(_pp.pic_width_in_luma_samples, 1 << log2_ctb);
    unsigned ctb_height = __KERNEL_DIV_ROUND_UP(_pp.pic_height_in_luma_samples, 1 << log2_ctb);
    if (seq.tiles_enabled_flag) {
        unsigned columns = std::min<unsigned>(c.num_tile_columns, 20), rows = std::min<unsigned>(c.num_tile_rows, 22);
        uint16_t w[20], h[22];
        unsigned sum = 0;

        for (unsigned i = 0; i < columns; i++) {
            w[i] = i + 1 < columns ? _pp.column_width_minus1[i] + 1 : ctb_width - sum;
            sum += w[i];
            tiles[0x380 + i] = sum << (log2_ctb - 4);
        }
        sum = 0;
        for (unsigned i = 0; i < rows; i++) {
            h[i] = i + 1 < rows ? _pp.row_height_minus1[i] + 1 : ctb_height - sum;
            sum += h[i];
            tiles[0x380 + columns + i] = sum << (log2_ctb - 4);
        }
        uint16_t *t = tiles;
        for (unsigned y = 0; y < rows; y++)
            for (unsigned x = 0; x < columns; x++) {
                *t++ = w[x];
                *t++ = h[y];
            }
    } else {
        tiles[0] = ctb_width;
        tiles[1] = ctb_height;
    }
    job.addTable(NVC5B0_HEVC_SET_TILE_SIZES_OFFSET, tiles, sizeof(tiles));

    job.codec = NVC5B0_SET_CONTROL_PARAMS_CODEC_TYPE_HEVC;
    job.setSetup(c);
    job.bitstream = std::move(_bitstream);
    for (size_t i = 0; i < surfaces.size(); i++)
        addSurface(job, i, surfaces[i]);
    job.addBuffer(NVC5B0_SET_COLOC_DATA_OFFSET, _coloc, 0);
    job.addBuffer(NVC5B0_HEVC_SET_FILTER_BUFFER_OFFSET, _filter, 0);
    return VA_STATUS_SUCCESS;
}
