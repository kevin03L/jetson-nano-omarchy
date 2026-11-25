// SPDX-License-Identifier: MIT
#ifndef MPEG2_H
#define MPEG2_H

#include "decoder.h"

class Mpeg2Decoder : public Decoder
{
public:
    using Decoder::Decoder;

    VAStatus build(NvdecJob &job) override;

protected:
    void beginPicture() override;
    VAStatus renderPicture(const Buffer &buffer) override;
    size_t sliceParamSize() const override { return sizeof(VASliceParameterBufferMPEG2); }
    VAStatus renderIQMatrix(const Buffer &buffer) override;
    VAStatus renderSlice(const void *param, const uint8_t *data, uint32_t size) override;

private:
    bool _valid;
    VAPictureParameterBufferMPEG2 _pp;
    VAIQMatrixBufferMPEG2 _iq;
    std::vector<uint8_t> _bitstream;
    std::vector<uint32_t> _offsets;
    unsigned _rows;
};

#endif
