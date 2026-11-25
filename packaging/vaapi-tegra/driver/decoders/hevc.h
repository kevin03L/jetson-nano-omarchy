// SPDX-License-Identifier: LGPL-2.1-or-later
#ifndef HEVC_H
#define HEVC_H

#include <array>

#include "decoder.h"

class HevcDecoder : public Decoder
{
public:
    HevcDecoder(NvdecDevice &nvdec, Objects &objects, unsigned width, unsigned height);

    VAStatus build(NvdecJob &job) override;

protected:
    void beginPicture() override;
    VAStatus renderPicture(const Buffer &buffer) override;
    size_t sliceParamSize() const override { return sizeof(VASliceParameterBufferHEVC); }
    VAStatus renderIQMatrix(const Buffer &buffer) override;
    VAStatus renderSlice(const void *param, const uint8_t *data, uint32_t size) override;

private:
    bool _valid, _has_iq, _first;
    VAPictureParameterBufferHEVC _pp;
    VAIQMatrixBufferHEVC _iq;
    std::vector<uint8_t> _bitstream;

    /* from the header of the first slice */
    unsigned _skip;
    std::vector<uint8_t> _long_term; /* indices in ReferenceFrames, in the order of the slice header */

    /* the surface in each of NVDEC's slots, kept between pictures as their motion data */
    std::array<VASurfaceID, 16> _slots;
    std::shared_ptr<GemBuffer> _coloc, _filter;
    size_t _coloc_size = 0, _filter_size = 0;

    int findSlot(VASurfaceID id) const;
    int currentSlot();
    void parseSliceHeader(const uint8_t *data, uint32_t size, const VASliceParameterBufferHEVC &slice);
};

#endif
