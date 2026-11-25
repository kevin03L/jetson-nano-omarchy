// SPDX-License-Identifier: MIT
#include "decoder.h"

#include "../engines/nvdec.h"
#include "../objects.h"
#include "h264.h"
#include "hevc.h"
#include "mpeg2.h"
#include "vp8.h"
#include "vp9.h"

std::unique_ptr<Decoder> Decoder::create(VAProfile profile, NvdecDevice &nvdec, Objects &objects,
    unsigned width, unsigned height)
{
    switch (profile) {
    case VAProfileMPEG2Simple:
    case VAProfileMPEG2Main:
        return std::make_unique<Mpeg2Decoder>(nvdec, objects, width, height);
    case VAProfileH264ConstrainedBaseline:
    case VAProfileH264Main:
    case VAProfileH264High:
        return std::make_unique<H264Decoder>(nvdec, objects, width, height);
    case VAProfileHEVCMain:
    case VAProfileHEVCMain10:
        return std::make_unique<HevcDecoder>(nvdec, objects, width, height);
    case VAProfileVP8Version0_3:
        return std::make_unique<Vp8Decoder>(nvdec, objects, width, height);
    case VAProfileVP9Profile0:
        return std::make_unique<Vp9Decoder>(nvdec, objects, width, height);
    default:
        return nullptr;
    }
}

void Decoder::begin(VASurfaceID id, Surface *surface)
{
    _target_id = id;
    _target = surface;
    _slice_params.clear();

    beginPicture();
}

VAStatus Decoder::render(const Buffer &buffer)
{
    switch (buffer.type) {
    case VAPictureParameterBufferType:
        return renderPicture(buffer);
    case VAIQMatrixBufferType:
        return renderIQMatrix(buffer);
    case VASliceParameterBufferType:
        _slice_params = buffer.data;
        _slice_param_size = buffer.element_size;
        return VA_STATUS_SUCCESS;
    case VASliceDataBufferType:
        /* the parameters of one slice or more, then their data */
        for (size_t i = 0; _slice_param_size && i + _slice_param_size <= _slice_params.size(); i += _slice_param_size) {
            auto *base = (const VASliceParameterBufferBase *)&_slice_params[i];

            if (_slice_param_size < sizeof(*base) || base->slice_data_offset > buffer.data.size() ||
                base->slice_data_size > buffer.data.size() - base->slice_data_offset)
                return VA_STATUS_ERROR_INVALID_PARAMETER;

            VAStatus status = renderSlice(base, buffer.data.data() + base->slice_data_offset, base->slice_data_size);
            if (status != VA_STATUS_SUCCESS)
                return status;
        }
        _slice_params.clear();
        return VA_STATUS_SUCCESS;
    default:
        return renderOther(buffer);
    }
}

Surface *Decoder::reference(VASurfaceID id)
{
    Surface *surface = id == VA_INVALID_SURFACE ? nullptr : _objects.surface(id);

    return surface && surface->gem ? surface : _target;
}

void Decoder::addSurface(NvdecJob &job, unsigned slot, Surface *surface)
{
    job.addBuffer(NVC5B0_SET_PICTURE_LUMA_OFFSET0 + 4 * slot, surface->gem, 0);
    job.addBuffer(NVC5B0_SET_PICTURE_CHROMA_OFFSET0 + 4 * slot, surface->gem, surface->chroma_offset);
}
