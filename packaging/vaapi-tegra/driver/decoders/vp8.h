// SPDX-License-Identifier: LGPL-2.1-or-later
#ifndef VP8_H
#define VP8_H

#include "decoder.h"

class Vp8Decoder : public Decoder
{
public:
    using Decoder::Decoder;

    VAStatus build(NvdecJob &job) override;

protected:
    void beginPicture() override;
    VAStatus renderPicture(const Buffer &buffer) override;
    VAStatus renderSlice(const void *param, const uint8_t *data, uint32_t size) override;

private:
    bool _valid;
    VAPictureParameterBufferVP8 _pp;
    std::vector<uint8_t> _bitstream;
    uint32_t _first_part;

    std::shared_ptr<GemBuffer> _prob_data, _history;
    size_t _history_size = 0;
};

#endif
