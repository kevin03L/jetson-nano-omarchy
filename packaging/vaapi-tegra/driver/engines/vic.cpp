// SPDX-License-Identifier: MIT
#include "vic.h"

#include <cstdio>
#include <cstring>
#include <vector>

#include <libdrm/drm.h>
#include <linux/kernel.h>

#include "../engine_headers/host1x.h"
#include "../engine_headers/vic04.h"
#include "../uapi_headers/tegra_drm.h"

VicDevice::~VicDevice()
{
    for (Slot &slot : _slots) {
        if (slot.fence.valid())
            _dev.wait(slot.fence);
        slot.bo.reset();
    }
    _filter.reset();

    if (_syncpt != 0xffffffff)
        _dev.free_syncpoint(_syncpt);
    if (_context)
        _dev.close_channel(_context);
}

int VicDevice::open()
{
    if (_context)
        return 0;

    if (_dev.open_channel(HOST1X_CLASS_VIC, &_context) == -1) {
        perror("tegra: no VIC");
        _context = 0;
        return -1;
    }

    _filter = std::make_unique<GemBuffer>(_dev);
    int err = _filter->allocate(0x3000) || !_filter->map();
    if (!err)
        memset(_filter->map(), 0, 0x3000);
    for (Slot &slot : _slots) {
        slot.bo = std::make_unique<GemBuffer>(_dev);
        err = err || slot.bo->allocate(__ALIGN_KERNEL(sizeof(ConfigStruct_VIC41), 4096)) || !slot.bo->map();
    }
    if (err || _dev.allocate_syncpoint(_context, &_syncpt) == -1) {
        perror("tegra: VIC");
        for (Slot &slot : _slots)
            slot.bo.reset();
        _filter.reset();
        _dev.close_channel(_context);
        _context = 0;
        _syncpt = 0xffffffff;
        return -1;
    }

    return 0;
}

int VicDevice::copy(const VicSurface &out, const VicSurface &in, const Fence &after, Fence &done)
{
    Slot &slot = _slots[_next++ % _slots.size()];
    static thread_local ConfigStruct_VIC41 c; /* built in cached memory: full of bit fields */
    bool is41 = !_dev.isTegra210();
    uint32_t cmd[64];
    unsigned n = 0;
    std::vector<drm_tegra_submit_buf> bufs;

    if (slot.fence.valid()) {
        if (_dev.wait(slot.fence))
            return -1;
        slot.fence = Fence();
    }

    memset(&c, 0, sizeof(c));

    /*
     * The VIC of Tegra210 hangs with formats of 10 bits. It only has to move bytes: 16 bits samples
     * are the same bytes as an 8 bits picture that is twice as wide.
     */
    unsigned out_width = out.width << out.ten_bits, in_width = in.width << in.ten_bits;

    c.outputConfig.TargetRectRight = out_width - 1;
    c.outputConfig.TargetRectBottom = out.height - 1;
    c.outputConfig.BackgroundAlpha = 1023;

    OutputSurfaceConfig &o = c.outputSurfaceConfig;
    o.OutPixelFormat = VIC_PIXEL_FORMAT_Y8_U8V8_N420;
    o.OutBlkKind = out.block_linear ? VIC_BLK_KIND_GENERIC_16Bx2 : VIC_BLK_KIND_PITCH;
    o.OutBlkHeight = out.block_linear ? 1 : 0;
    o.OutSurfaceWidth = out_width - 1;
    o.OutSurfaceHeight = out.height - 1;
    o.OutLumaWidth = out.pitch - 1;
    o.OutLumaHeight = out.height - 1;
    o.OutChromaWidth = out.pitch / 2 - 1;
    o.OutChromaHeight = out.height / 2 - 1;

    SlotConfig &s = c.slotStruct[0].slotConfig;
    s.SlotEnable = 1;
    s.CurrentFieldEnable = 1;
    s.PlanarAlpha = 1023;
    s.ConstantAlpha = 1;
    s.SourceRectRight = (in_width - 1) << 16;
    s.SourceRectBottom = (in.height - 1) << 16;
    s.DestRectRight = out_width - 1;
    s.DestRectBottom = out.height - 1;
    s.SoftClampHigh = 1023;

    SlotSurfaceConfig &i = c.slotStruct[0].slotSurfaceConfig;
    i.SlotPixelFormat = VIC_PIXEL_FORMAT_Y8_U8V8_N420;
    i.SlotBlkKind = in.block_linear ? VIC_BLK_KIND_GENERIC_16Bx2 : VIC_BLK_KIND_PITCH;
    i.SlotBlkHeight = in.block_linear ? 1 : 0;
    i.SlotCacheWidth = in.block_linear ? VIC_CACHE_WIDTH_32Bx8 : VIC_CACHE_WIDTH_64Bx4;
    i.SlotSurfaceWidth = in_width - 1;
    i.SlotSurfaceHeight = in.height - 1;
    i.SlotLumaWidth = in.pitch - 1;
    i.SlotLumaHeight = in.height - 1;
    i.SlotChromaWidth = in.pitch / 2 - 1;
    i.SlotChromaHeight = in.height / 2 - 1;
    i.SlotChromaLocHoriz = 1;
    i.SlotChromaLocVert = 1;

    memcpy(slot.bo->map(), &c, sizeof(c));

    auto method = [&](uint32_t reg, uint32_t value) {
        cmd[n++] = host1x_opcode_incr(VIC_UCLASS_METHOD_OFFSET, 2);
        cmd[n++] = reg >> 2;
        cmd[n++] = value;
    };
    auto address = [&](uint32_t reg, GemBuffer &bo, uint32_t offset) {
        if (bo.channelMap(_context, true))
            return -1;
        method(reg, 0xdeadbeef);

        drm_tegra_submit_buf buf = { 0 };
        buf.mapping = bo.mappingId(_context);
        buf.reloc.target_offset = offset;
        buf.reloc.gather_offset_words = n - 1;
        buf.reloc.shift = 8;
        bufs.push_back(buf);
        return 0;
    };

    /*
     * The kernel keeps the jobs of one program apart, but not the ones of two programs, and VIC
     * hangs when it gets a job while it finishes another: wait until it is idle.
     */
    cmd[n++] = host1x_opcode_nonincr(0, 1);
    cmd[n++] = _syncpt | 1 << (is41 ? 10 : 8);
    unsigned prologue = n;

    method(NVB0B6_VIDEO_COMPOSITOR_SET_APPLICATION_ID, 1);
    method(NVB0B6_VIDEO_COMPOSITOR_SET_CONTROL_PARAMS,
        ((is41 ? sizeof(ConfigStruct_VIC41) : sizeof(ConfigStruct_VIC40)) / 16) << 16);
    int err = address(NVB0B6_VIDEO_COMPOSITOR_SET_CONFIG_STRUCT_OFFSET, *slot.bo, 0);
    err |= address(NVB0B6_VIDEO_COMPOSITOR_SET_FILTER_STRUCT_OFFSET, *_filter, 0);
    err |= address(NVB0B6_VIDEO_COMPOSITOR_SET_OUTPUT_SURFACE_LUMA_OFFSET, *out.bo, 0);
    err |= address(NVB0B6_VIDEO_COMPOSITOR_SET_OUTPUT_SURFACE_CHROMA_U_OFFSET, *out.bo, out.chroma_offset);
    err |= address(is41 ? NVB1B6_VIDEO_COMPOSITOR_SET_SURFACE0_SLOT0_LUMA_OFFSET :
        NVB0B6_VIDEO_COMPOSITOR_SET_SURFACE0_SLOT0_LUMA_OFFSET, *in.bo, 0);
    err |= address(is41 ? NVB1B6_VIDEO_COMPOSITOR_SET_SURFACE0_SLOT0_CHROMA_U_OFFSET :
        NVB0B6_VIDEO_COMPOSITOR_SET_SURFACE0_SLOT0_CHROMA_U_OFFSET, *in.bo, in.chroma_offset);
    if (err)
        return -1;

    method(NVB0B6_VIDEO_COMPOSITOR_EXECUTE, 1 << 8);
    cmd[n++] = host1x_opcode_nonincr(0, 1);
    cmd[n++] = _syncpt | 1 << (is41 ? 10 : 8);

    drm_tegra_submit_cmd cmds[4] = {};
    unsigned num = 0;
    if (after.valid()) {
        cmds[num].type = DRM_TEGRA_SUBMIT_CMD_WAIT_SYNCPT;
        cmds[num].wait_syncpt.id = after.id;
        cmds[num].wait_syncpt.value = after.value;
        num++;
    }
    cmds[num].type = DRM_TEGRA_SUBMIT_CMD_GATHER_UPTR;
    cmds[num].gather_uptr.words = prologue;
    num++;
    cmds[num].type = DRM_TEGRA_SUBMIT_CMD_WAIT_SYNCPT_RELATIVE;
    cmds[num].wait_syncpt.id = _syncpt;
    cmds[num].wait_syncpt.value = 1;
    num++;
    cmds[num].type = DRM_TEGRA_SUBMIT_CMD_GATHER_UPTR;
    cmds[num].gather_uptr.words = n - prologue;
    num++;

    drm_tegra_channel_submit submit = { 0 };
    submit.context = _context;
    submit.num_bufs = bufs.size();
    submit.num_cmds = num;
    submit.gather_data_words = n;
    submit.bufs_ptr = (uintptr_t)bufs.data();
    submit.cmds_ptr = (uintptr_t)cmds;
    submit.gather_data_ptr = (uintptr_t)cmd;
    submit.syncpt.id = _syncpt;
    submit.syncpt.increments = 2;

    if (_dev.ioctl(DRM_IOCTL_TEGRA_CHANNEL_SUBMIT, &submit) == -1) {
        perror("tegra: VIC job refused");
        return -1;
    }

    done.id = _syncpt;
    done.value = submit.syncpt.value;
    slot.fence = done;
    return 0;
}
