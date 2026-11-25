// SPDX-License-Identifier: MIT
#include "nvdec.h"

#include <cerrno>
#include <cstdio>
#include <unistd.h>

#include <libdrm/drm.h>
#include <linux/kernel.h>

#include "../engine_headers/host1x.h"
#include "../uapi_headers/tegra_drm.h"

#define NVDEC_UCLASS_METHOD_OFFSET 0x10

/* a link to the dependency in the device tree, from a place a sandbox lets programs read */
#define NVJPG_LINK "/usr/lib/vaapi-tegra/nvjpg-with-nvdec"

NvdecDevice::~NvdecDevice()
{
    for (Slot &slot : _slots) {
        if (slot.fence.valid())
            _dev.wait(slot.fence);
        slot.bo.reset();
    }

    if (_syncpt != 0xffffffff)
        _dev.free_syncpoint(_syncpt);
    if (_context)
        _dev.close_channel(_context);
}

/*
 * Tegra210's NVDEC answers only when the NVJPG power domain is on too. Without this dependency in the
 * device tree the first access stops the whole system, so it takes a proof. Where a sandbox hides the
 * device tree (Firefox), the link lets its broker look for us. No proof, no decoding.
 */
static bool powered()
{
    static const char tree[] = "/proc/device-tree";
    static const char pmc[] = "/proc/device-tree/pmc@7000e400";

    if (!access("/proc/device-tree/pmc@7000e400/powergates/nvdec/power-domains", F_OK))
        return true;
    if (!access(tree, R_OK | X_OK) && !access("/proc/device-tree/compatible", R_OK))
        return access(pmc, F_OK) == -1 && errno == ENOENT; /* another SoC has no such thing */
    return !access(NVJPG_LINK, F_OK);
}

int NvdecDevice::open()
{
    if (_context)
        return 0;

    if (!powered()) {
        fprintf(stderr, "tegra: no hardware decoding, it is not known that this device tree powers NVJPG with NVDEC\n");
        return -1;
    }

    if (_dev.open_channel(HOST1X_CLASS_NVDEC, &_context) == -1) {
        perror("tegra: no NVDEC");
        _context = 0;
        return -1;
    }

    if (_dev.allocate_syncpoint(_context, &_syncpt) == -1) {
        perror("tegra: no syncpoint for NVDEC");
        _dev.close_channel(_context);
        _context = 0;
        _syncpt = 0xffffffff;
        return -1;
    }

    return 0;
}

std::shared_ptr<GemBuffer> NvdecDevice::allocate(size_t bytes, bool cached)
{
    auto bo = std::make_shared<GemBuffer>(_dev);

    bytes = __ALIGN_KERNEL(bytes, 4096);
    if ((cached ? bo->allocateCached(bytes) : bo->allocate(bytes)) || !bo->map())
        return nullptr;

    bo->cpuAccess(true);
    memset(bo->map(), 0, bytes);
    bo->cpuAccess(false);
    return bo;
}

int NvdecDevice::submit(const NvdecJob &job, Fence &fence)
{
    Slot &slot = _slots[_next++ % _slots.size()];
    uint32_t cmd[512];
    unsigned n = 0;
    std::vector<drm_tegra_submit_buf> bufs;

    if (slot.fence.valid()) {
        if (_dev.wait(slot.fence))
            return -1;
        slot.fence = Fence();
    }

    /* layout: description, status, slice offsets, tables, bitstream */
    size_t status = __ALIGN_KERNEL(job.setup.size(), 256);
    size_t slices = status + 4096;
    size_t size = __ALIGN_KERNEL(slices + job.slice_offsets.size() * 4, 256);
    std::vector<size_t> tables;
    for (auto &table : job.tables) {
        tables.push_back(size);
        size = __ALIGN_KERNEL(size + table.data.size(), 256);
    }
    size_t bitstream = size;
    size += job.bitstream.size() + 512; /* NVDEC reads ahead */

    if (!slot.bo || slot.bo->size() < size) {
        slot.bo = std::make_unique<GemBuffer>(_dev);
        if (slot.bo->allocate(__ALIGN_KERNEL(size + size / 2, 0x10000))) {
            slot.bo.reset();
            return -1;
        }
    }

    auto *mem = (uint8_t *)slot.bo->map();
    if (!mem)
        return -1;
    memcpy(mem, job.setup.data(), job.setup.size());
    if (!job.slice_offsets.empty())
        memcpy(mem + slices, job.slice_offsets.data(), job.slice_offsets.size() * 4);
    for (size_t i = 0; i < tables.size(); i++)
        memcpy(mem + tables[i], job.tables[i].data.data(), job.tables[i].data.size());
    memcpy(mem + bitstream, job.bitstream.data(), job.bitstream.size());
    memset(mem + bitstream + job.bitstream.size(), 0, 512);

    auto method = [&](uint32_t reg, uint32_t value) {
        cmd[n++] = host1x_opcode_incr(NVDEC_UCLASS_METHOD_OFFSET, 2);
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

    if (job.buffers.size() + job.tables.size() > 80)
        return -1;

    /* wait until the job of another program is done, as for VIC */
    unsigned condition = 1 << (_dev.isTegra210() ? 8 : 10);
    cmd[n++] = host1x_opcode_nonincr(0, 1);
    cmd[n++] = _syncpt | condition;
    unsigned prologue = n;

    method(NVC5B0_SET_APPLICATION_ID, job.codec ? job.codec : 1);
    /* codec, GPTIMER_ON, ERR_CONCEAL_ON */
    method(NVC5B0_SET_CONTROL_PARAMS, job.codec | 1 << 4 | 1 << 6);
    method(NVC5B0_SET_PICTURE_INDEX, _picture++);

    int err = address(NVC5B0_SET_DRV_PIC_SETUP_OFFSET, *slot.bo, 0);
    err |= address(NVC5B0_SET_IN_BUF_BASE_OFFSET, *slot.bo, bitstream);
    if (!job.slice_offsets.empty())
        err |= address(NVC5B0_SET_SLICE_OFFSETS_BUF_OFFSET, *slot.bo, slices);
    err |= address(NVC5B0_SET_NVDEC_STATUS_OFFSET, *slot.bo, status);
    for (size_t i = 0; i < tables.size(); i++)
        err |= address(job.tables[i].method, *slot.bo, tables[i]);
    for (auto &buffer : job.buffers)
        err |= address(buffer.method, *buffer.bo, buffer.offset);
    if (err)
        return -1;

    method(NVC5B0_EXECUTE, 1 << 8); /* AWAKEN_ENABLE */

    /* count the syncpoint up when the engine is done */
    cmd[n++] = host1x_opcode_nonincr(0, 1);
    cmd[n++] = _syncpt | condition;

    drm_tegra_submit_cmd cmds[3] = {};
    cmds[0].type = DRM_TEGRA_SUBMIT_CMD_GATHER_UPTR;
    cmds[0].gather_uptr.words = prologue;
    cmds[1].type = DRM_TEGRA_SUBMIT_CMD_WAIT_SYNCPT_RELATIVE;
    cmds[1].wait_syncpt.id = _syncpt;
    cmds[1].wait_syncpt.value = 1;
    cmds[2].type = DRM_TEGRA_SUBMIT_CMD_GATHER_UPTR;
    cmds[2].gather_uptr.words = n - prologue;

    drm_tegra_channel_submit submit = { 0 };
    submit.context = _context;
    submit.num_bufs = bufs.size();
    submit.num_cmds = 3;
    submit.gather_data_words = n;
    submit.bufs_ptr = (uintptr_t)bufs.data();
    submit.cmds_ptr = (uintptr_t)cmds;
    submit.gather_data_ptr = (uintptr_t)cmd;
    submit.syncpt.id = _syncpt;
    submit.syncpt.increments = 2;

    if (_dev.ioctl(DRM_IOCTL_TEGRA_CHANNEL_SUBMIT, &submit) == -1) {
        perror("tegra: NVDEC job refused");
        return -1;
    }

    fence.id = _syncpt;
    fence.value = submit.syncpt.value;
    slot.fence = fence;
    return 0;
}
