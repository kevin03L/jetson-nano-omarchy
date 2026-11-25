// SPDX-License-Identifier: MIT
#ifndef VIC_H
#define VIC_H

#include <array>
#include <cstdint>
#include <memory>

#include "../fence.h"
#include "../gem.h"

/* A picture in memory, 8 or 10 bits 4:2:0 in two planes */
struct VicSurface {
    GemBuffer *bo;
    unsigned width, height;
    unsigned pitch, chroma_offset; /* bytes */
    bool block_linear;
    bool ten_bits;
};

/* VIC copies a picture from one layout to another */
class VicDevice {
public:
    VicDevice(DrmDevice &dev) : _dev(dev) {}
    ~VicDevice();

    int open();
    /* starts after the fence "after" */
    int copy(const VicSurface &out, const VicSurface &in, const Fence &after, Fence &done);

private:
    DrmDevice &_dev;

    uint64_t _context = 0;
    uint32_t _syncpt = 0xffffffff;
    std::unique_ptr<GemBuffer> _filter;

    /* the description of a copy. Used in turn. */
    struct Slot {
        std::unique_ptr<GemBuffer> bo;
        Fence fence;
    };
    std::array<Slot, 8> _slots;
    unsigned _next = 0;
};

#endif
