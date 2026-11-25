// SPDX-License-Identifier: MIT
#ifndef H264_H
#define H264_H

#include <array>

#include "decoder.h"

class H264Decoder : public Decoder
{
public:
    H264Decoder(NvdecDevice &nvdec, Objects &objects, unsigned width, unsigned height);

    VAStatus build(NvdecJob &job) override;

protected:
    void beginPicture() override;
    VAStatus renderPicture(const Buffer &buffer) override;
    size_t sliceParamSize() const override { return sizeof(VASliceParameterBufferH264); }
    VAStatus renderIQMatrix(const Buffer &buffer) override;
    VAStatus renderSlice(const void *param, const uint8_t *data, uint32_t size) override;

private:
    bool _valid;
    VAPictureParameterBufferH264 _pp;
    VAIQMatrixBufferH264 _iq;
    std::vector<uint8_t> _bitstream;
    std::vector<uint32_t> _offsets;
    unsigned _refs_l0, _refs_l1;

    /* the surface in each of NVDEC's slots and their motion data, kept between pictures */
    std::array<VASurfaceID, 17> _slots;
    std::shared_ptr<GemBuffer> _coloc, _history, _mbhist;
    size_t _coloc_size = 0, _history_size = 0, _mbhist_size = 0;
    VASurfaceID _previous = VA_INVALID_SURFACE;

    int findSlot(VASurfaceID id) const;
    int currentSlot();
};

#endif
