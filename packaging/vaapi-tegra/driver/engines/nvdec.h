// SPDX-License-Identifier: MIT
#ifndef NVDEC_H
#define NVDEC_H

#include <array>
#include <cstdint>
#include <cstring>
#include <memory>
#include <vector>

#include "../engine_headers/clc5b0.h"
#include "../engine_headers/nvdec_drv.h"
#include "../fence.h"
#include "../gem.h"

/* One picture to decode, as the decoders describe it */
struct NvdecJob {
    uint32_t codec; /* NVC5B0_SET_CONTROL_PARAMS_CODEC_TYPE_... */

    /* the description of the picture (nvdec_..._pic_s) */
    std::vector<uint8_t> setup;

    std::vector<uint8_t> bitstream;
    std::vector<uint32_t> slice_offsets;

    struct Buffer {
        uint32_t method;
        std::shared_ptr<GemBuffer> bo;
        uint32_t offset;
    };
    std::vector<Buffer> buffers;

    /* tables of this picture only: they go to the memory of the job */
    struct Table {
        uint32_t method;
        std::vector<uint8_t> data;
    };
    std::vector<Table> tables;

    template <typename T> void setSetup(const T &s)
    {
        setup.resize(sizeof(T));
        memcpy(setup.data(), &s, sizeof(T));
    }

    void addBuffer(uint32_t method, std::shared_ptr<GemBuffer> bo, uint32_t offset)
    {
        buffers.push_back({ method, std::move(bo), offset });
    }

    void addTable(uint32_t method, const void *data, size_t size)
    {
        auto *p = (const uint8_t *)data;
        tables.push_back({ method, std::vector<uint8_t>(p, p + size) });
    }
};

class NvdecDevice {
public:
    NvdecDevice(DrmDevice &dev) : _dev(dev) {}
    ~NvdecDevice();

    int open();
    int submit(const NvdecJob &job, Fence &fence);
    int wait(const Fence &fence) { return _dev.wait(fence); }

    /* zero filled memory for the decoders */
    std::shared_ptr<GemBuffer> allocate(size_t bytes, bool cached = false);

private:
    DrmDevice &_dev;

    uint64_t _context = 0;
    uint32_t _syncpt = 0xffffffff;
    uint32_t _picture = 0;

    /* what a job reads: description, tables, bitstream. Used in turn. */
    struct Slot {
        std::unique_ptr<GemBuffer> bo;
        Fence fence;
    };
    std::array<Slot, 8> _slots;
    unsigned _next = 0;
};

#endif
