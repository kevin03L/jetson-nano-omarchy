// SPDX-License-Identifier: MIT
#ifndef DECODER_H
#define DECODER_H

#include <memory>
#include <vector>

#include <va/va.h>
#include <va/va_backend.h>

#include "../fence.h"
#include "../gem.h"

class Buffer;
class NvdecDevice;
class Objects;
class Surface;
struct NvdecJob;

/* What a codec does: it collects the buffers of a picture and turns them into a job for NVDEC. */
class Decoder
{
public:
    Decoder(NvdecDevice &nvdec, Objects &objects, unsigned width, unsigned height)
        : _nvdec(nvdec), _objects(objects), _width(width), _height(height) {}
    virtual ~Decoder() {}

    /* nullptr: no such decoder */
    static std::unique_ptr<Decoder> create(VAProfile profile, NvdecDevice &nvdec, Objects &objects,
        unsigned width, unsigned height);

    void begin(VASurfaceID id, const std::shared_ptr<Surface> &surface);
    VAStatus render(const Buffer &buffer);

    /* fill in the job of the picture */
    virtual VAStatus build(NvdecJob &job) = 0;
    virtual void submitted(const Fence &) {}

protected:
    virtual void beginPicture() = 0;
    virtual VAStatus renderPicture(const Buffer &buffer) = 0;
    virtual VAStatus renderIQMatrix(const Buffer &) { return VA_STATUS_SUCCESS; }
    /* one slice: its parameters and its data */
    virtual VAStatus renderSlice(const void *param, const uint8_t *data, uint32_t size) = 0;
    virtual VAStatus renderOther(const Buffer &) { return VA_STATUS_SUCCESS; }

    /* the surface of a reference picture, the one being decoded when there is none */
    Surface *reference(VASurfaceID id);
    /* the same for the codecs whose pictures all have one size */
    Surface *sameSizeReference(VASurfaceID id);
    /* a picture of this size is inside the memory of the surface: the engine does not know it */
    static bool fits(const Surface &surface, unsigned width, unsigned height);
    /* the surfaces of the job, at their place in NVDEC's list */
    void addSurface(NvdecJob &job, unsigned slot, Surface *surface);

    NvdecDevice &_nvdec;
    Objects &_objects;
    unsigned _width, _height;

    VASurfaceID _target_id = VA_INVALID_SURFACE;
    Surface *_target = nullptr;

private:
    /* the surfaces of the picture stay until the next one */
    std::vector<std::shared_ptr<Surface>> _held;
    std::vector<uint8_t> _slice_params;
    size_t _slice_param_size = 0;
};

/* reads the bits of a NAL unit, without its emulation prevention bytes */
class BitReader
{
public:
    BitReader(const uint8_t *data, size_t size) : _data(data), _size(size) {}

    uint32_t u(unsigned n)
    {
        uint32_t v = 0;
        while (n--)
            v = v << 1 | bit();
        return v;
    }

    uint32_t ue()
    {
        unsigned n = 0;
        while (n < 32 && !bit())
            n++;
        if (n >= 32) {
            _overrun = true;
            return 0;
        }
        return (1u << n) - 1 + u(n);
    }

    bool overrun() const { return _overrun; }
    /* bits read so far, emulation prevention bytes left out */
    size_t position() const { return _count; }

private:
    uint32_t bit()
    {
        if (_bit == 0 && _zeros >= 2 && _pos < _size && _data[_pos] == 3) {
            _pos++;
            _zeros = 0;
        }
        if (_pos >= _size) {
            _overrun = true;
            return 0;
        }

        uint32_t v = _data[_pos] >> (7 - _bit) & 1;
        _count++;
        if (++_bit == 8) {
            _zeros = _data[_pos] ? 0 : _zeros + 1;
            _bit = 0;
            _pos++;
        }
        return v;
    }

    const uint8_t *_data;
    size_t _size, _pos = 0, _count = 0;
    unsigned _bit = 0, _zeros = 0;
    bool _overrun = false;
};

#endif
