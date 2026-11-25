// SPDX-License-Identifier: MIT
/*
 * VA-API driver for the video engines of the Tegra210: NVDEC decodes, VIC copies.
 * Copyright (c) 2017-2021 NVIDIA CORPORATION (vaapi-tegra-driver by Mikko Perttunen)
 */
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <mutex>
#include <unistd.h>
#include <vector>

#include <libdrm/drm_fourcc.h>
#include <linux/kernel.h>
#include <va/va_backend.h>
#include <va/va_drmcommon.h>

#include "decoders/decoder.h"
#include "engines/nvdec.h"
#include "engines/vic.h"
#include "gem.h"
#include "objects.h"

#define FUNC(name, ...) extern "C" VAStatus tegra_##name(VADriverContextP ctx, ##__VA_ARGS__)

#define DRIVER_DATA ((DriverData *)ctx->pDriverData)

struct DriverData {
    DrmDevice drm;
    VicDevice vic{drm};
    NvdecDevice nvdec{drm};
    Objects objects;
    std::mutex lock; /* the engines and the pools */

    /* the memory of destroyed images, used again: a player makes an image for each picture */
    std::vector<std::shared_ptr<GemBuffer>> image_pool;
};

static const VAProfile profiles[] = {
    VAProfileMPEG2Simple,
    VAProfileMPEG2Main,
    VAProfileH264ConstrainedBaseline,
    VAProfileH264Main,
    VAProfileH264High,
    VAProfileHEVCMain,
    VAProfileHEVCMain10,
    VAProfileVP8Version0_3,
    VAProfileVP9Profile0,
};

#define MAX_WIDTH 4096
#define MAX_HEIGHT 4096

static bool hasProfile(VAProfile profile)
{
    for (VAProfile p : profiles)
        if (p == profile)
            return true;
    return false;
}

/* a configuration is its profile */
#define CONFIG_BASE 1000

static bool configProfile(VAConfigID config, VAProfile *profile)
{
    *profile = (VAProfile)((int)config - CONFIG_BASE);
    return hasProfile(*profile);
}

static unsigned rtFormats(VAProfile profile)
{
    switch (profile) {
    case VAProfileHEVCMain10:
        return VA_RT_FORMAT_YUV420 | VA_RT_FORMAT_YUV420_10;
    default:
        return VA_RT_FORMAT_YUV420;
    }
}

FUNC(Terminate)
{
    DRIVER_DATA->objects.clear();
    DRIVER_DATA->image_pool.clear();
    delete DRIVER_DATA;
    ctx->pDriverData = nullptr;

    return VA_STATUS_SUCCESS;
}

FUNC(QueryConfigProfiles, VAProfile *profile_list, int *num_profiles)
{
    *num_profiles = 0;
    for (VAProfile p : profiles)
        profile_list[(*num_profiles)++] = p;

    return VA_STATUS_SUCCESS;
}

FUNC(QueryConfigEntrypoints, VAProfile profile, VAEntrypoint *entrypoint_list, int *num_entrypoints)
{
    *num_entrypoints = 0;
    if (!hasProfile(profile))
        return VA_STATUS_ERROR_UNSUPPORTED_PROFILE;

    entrypoint_list[(*num_entrypoints)++] = VAEntrypointVLD;
    return VA_STATUS_SUCCESS;
}

FUNC(GetConfigAttributes, VAProfile profile, VAEntrypoint entrypoint, VAConfigAttrib *attrib_list,
    int num_attribs)
{
    if (!hasProfile(profile))
        return VA_STATUS_ERROR_UNSUPPORTED_PROFILE;
    if (entrypoint != VAEntrypointVLD)
        return VA_STATUS_ERROR_UNSUPPORTED_ENTRYPOINT;

    for (int i = 0; i < num_attribs; i++) {
        switch (attrib_list[i].type) {
        case VAConfigAttribRTFormat:
            attrib_list[i].value = rtFormats(profile);
            break;
        case VAConfigAttribDecSliceMode:
            attrib_list[i].value = VA_DEC_SLICE_MODE_NORMAL;
            break;
        case VAConfigAttribMaxPictureWidth:
            attrib_list[i].value = MAX_WIDTH;
            break;
        case VAConfigAttribMaxPictureHeight:
            attrib_list[i].value = MAX_HEIGHT;
            break;
        default:
            attrib_list[i].value = VA_ATTRIB_NOT_SUPPORTED;
        }
    }

    return VA_STATUS_SUCCESS;
}

FUNC(CreateConfig, VAProfile profile, VAEntrypoint entrypoint, VAConfigAttrib *attrib_list,
    int num_attribs, VAConfigID *config_id)
{
    if (!hasProfile(profile))
        return VA_STATUS_ERROR_UNSUPPORTED_PROFILE;
    if (entrypoint != VAEntrypointVLD)
        return VA_STATUS_ERROR_UNSUPPORTED_ENTRYPOINT;

    *config_id = CONFIG_BASE + (int)profile;
    return VA_STATUS_SUCCESS;
}

FUNC(DestroyConfig, VAConfigID config_id)
{
    return VA_STATUS_SUCCESS;
}

FUNC(QueryConfigAttributes, VAConfigID config_id, VAProfile *profile, VAEntrypoint *entrypoint,
    VAConfigAttrib *attrib_list, int *num_attribs)
{
    if (!configProfile(config_id, profile))
        return VA_STATUS_ERROR_INVALID_CONFIG;

    *entrypoint = VAEntrypointVLD;
    attrib_list[0].type = VAConfigAttribRTFormat;
    attrib_list[0].value = rtFormats(*profile);
    *num_attribs = 1;
    return VA_STATUS_SUCCESS;
}

FUNC(QuerySurfaceAttributes, VAConfigID config, VASurfaceAttrib *attrib_list,
    unsigned int *num_attribs)
{
    VAProfile profile;
    VASurfaceAttrib list[8];
    unsigned n = 0;

    if (!configProfile(config, &profile))
        return VA_STATUS_ERROR_INVALID_CONFIG;

    auto add = [&](VASurfaceAttribType type, unsigned flags, int value) {
        list[n].type = type;
        list[n].flags = flags;
        list[n].value.type = VAGenericValueTypeInteger;
        list[n].value.value.i = value;
        n++;
    };
    const unsigned rw = VA_SURFACE_ATTRIB_GETTABLE | VA_SURFACE_ATTRIB_SETTABLE;

    add(VASurfaceAttribPixelFormat, rw, VA_FOURCC_NV12);
    if (rtFormats(profile) & VA_RT_FORMAT_YUV420_10)
        add(VASurfaceAttribPixelFormat, rw, VA_FOURCC_P010);
    add(VASurfaceAttribMinWidth, VA_SURFACE_ATTRIB_GETTABLE, 16);
    add(VASurfaceAttribMinHeight, VA_SURFACE_ATTRIB_GETTABLE, 16);
    add(VASurfaceAttribMaxWidth, VA_SURFACE_ATTRIB_GETTABLE, MAX_WIDTH);
    add(VASurfaceAttribMaxHeight, VA_SURFACE_ATTRIB_GETTABLE, MAX_HEIGHT);
    add(VASurfaceAttribMemoryType, rw, VA_SURFACE_ATTRIB_MEM_TYPE_VA | VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2);

    if (attrib_list) {
        if (*num_attribs < n) {
            *num_attribs = n;
            return VA_STATUS_ERROR_MAX_NUM_EXCEEDED;
        }
        memcpy(attrib_list, list, n * sizeof(list[0]));
    }
    *num_attribs = n;
    return VA_STATUS_SUCCESS;
}

static void destroySurface(DriverData *dd, VASurfaceID id)
{
    if (!dd->objects.surface(id))
        return;

    /* the kernel keeps the memory of jobs that are not done */
    dd->objects.remove(id);
}

FUNC(CreateSurfaces2, unsigned int format, unsigned int width, unsigned int height,
    VASurfaceID *surfaces, unsigned int num_surfaces, VASurfaceAttrib *attrib_list,
    unsigned int num_attribs)
{
    bool ten_bits = format == VA_RT_FORMAT_YUV420_10;

    if (format != VA_RT_FORMAT_YUV420 && format != VA_RT_FORMAT_YUV420_10)
        return VA_STATUS_ERROR_UNSUPPORTED_RT_FORMAT;
    if (!width || !height || width > MAX_WIDTH || height > MAX_HEIGHT)
        return VA_STATUS_ERROR_RESOLUTION_NOT_SUPPORTED;

    for (unsigned i = 0; attrib_list && i < num_attribs; i++) {
        if (!(attrib_list[i].flags & VA_SURFACE_ATTRIB_SETTABLE))
            continue;
        switch (attrib_list[i].type) {
        case VASurfaceAttribMemoryType:
            if (attrib_list[i].value.value.i != VA_SURFACE_ATTRIB_MEM_TYPE_VA)
                return VA_STATUS_ERROR_UNSUPPORTED_MEMORY_TYPE;
            break;
        case VASurfaceAttribPixelFormat:
            if (attrib_list[i].value.value.i == VA_FOURCC_P010)
                ten_bits = true;
            else if (attrib_list[i].value.value.i != VA_FOURCC_NV12)
                return VA_STATUS_ERROR_UNSUPPORTED_RT_FORMAT;
            break;
        case VASurfaceAttribExternalBufferDescriptor:
            if (attrib_list[i].value.value.p)
                return VA_STATUS_ERROR_UNSUPPORTED_MEMORY_TYPE;
            break;
        default:
            break;
        }
    }

    for (unsigned i = 0; i < num_surfaces; i++) {
        auto surface = DRIVER_DATA->objects.createSurface(&surfaces[i]);

        /* blocks of two GOBs are 16 lines: 32 lines of luma, 16 of chroma */
        surface->width = width;
        surface->height = height;
        surface->ten_bits = ten_bits;
        surface->pitch = __ALIGN_KERNEL(width << ten_bits, 256);
        surface->chroma_offset = surface->pitch * __ALIGN_KERNEL(height, 32);
        surface->gem = std::make_shared<GemBuffer>(DRIVER_DATA->drm);

        if (surface->gem->allocate(__ALIGN_KERNEL(surface->chroma_offset * 3 / 2, 4096))) {
            for (unsigned j = 0; j <= i; j++)
                destroySurface(DRIVER_DATA, surfaces[j]);
            return VA_STATUS_ERROR_ALLOCATION_FAILED;
        }
    }

    return VA_STATUS_SUCCESS;
}

FUNC(CreateSurfaces, int width, int height, int format, int num_surfaces, VASurfaceID *surfaces)
{
    return tegra_CreateSurfaces2(ctx, format, width, height, surfaces, num_surfaces, nullptr, 0);
}

FUNC(DestroySurfaces, VASurfaceID *surface_list, int num_surfaces)
{
    std::lock_guard<std::mutex> g(DRIVER_DATA->lock);

    for (int i = 0; i < num_surfaces; i++)
        destroySurface(DRIVER_DATA, surface_list[i]);

    return VA_STATUS_SUCCESS;
}

static VicSurface vicSurface(const Surface &s)
{
    return { s.gem.get(), s.width, s.height, s.pitch, s.chroma_offset, true, s.ten_bits };
}

/* the linear copy of a surface for other devices: the line length NVIDIA's GPU driver expects */
static VAStatus copyToLinear(DriverData *dd, Surface &s)
{
    if (!s.linear) {
        s.linear_pitch = __ALIGN_KERNEL(s.width << s.ten_bits, 128);
        s.linear_chroma_offset = __ALIGN_KERNEL(s.linear_pitch * __ALIGN_KERNEL(s.height, 2), 4096);
        auto gem = std::make_shared<GemBuffer>(dd->drm);
        /* the GPU driver only imports a multiple of 64 KB */
        if (gem->allocateCached(__ALIGN_KERNEL(s.linear_chroma_offset + s.linear_pitch * __ALIGN_KERNEL(s.height, 2) / 2, 0x10000)))
            return VA_STATUS_ERROR_ALLOCATION_FAILED;
        s.linear = gem;
    }

    /* nothing to copy yet */
    if (!s.decoded.valid())
        return VA_STATUS_SUCCESS;

    if (dd->vic.open())
        return VA_STATUS_ERROR_OPERATION_FAILED;

    VicSurface out = { s.linear.get(), s.width, s.height, s.linear_pitch, s.linear_chroma_offset, false, s.ten_bits };
    if (dd->vic.copy(out, vicSurface(s), s.decoded, s.copied))
        return VA_STATUS_ERROR_OPERATION_FAILED;
    return VA_STATUS_SUCCESS;
}

FUNC(ExportSurfaceHandle, VASurfaceID surface_id, uint32_t mem_type, uint32_t flags,
    void *descriptor)
{
    std::lock_guard<std::mutex> g(DRIVER_DATA->lock);

    auto surface = DRIVER_DATA->objects.surface(surface_id);
    if (!surface)
        return VA_STATUS_ERROR_INVALID_SURFACE;
    if (mem_type != VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2)
        return VA_STATUS_ERROR_UNSUPPORTED_MEMORY_TYPE;
    if ((flags & VA_EXPORT_SURFACE_READ_WRITE) == VA_EXPORT_SURFACE_WRITE_ONLY)
        return VA_STATUS_ERROR_UNIMPLEMENTED;

    if (!surface->linear) {
        VAStatus status = copyToLinear(DRIVER_DATA, *surface);
        if (status != VA_STATUS_SUCCESS)
            return status;
    }

    int fd = surface->linear->exportFd(true);
    if (fd == -1)
        return VA_STATUS_ERROR_OPERATION_FAILED;

    auto *desc = (VADRMPRIMESurfaceDescriptor *)descriptor;
    memset(desc, 0, sizeof(*desc));
    desc->fourcc = surface->ten_bits ? VA_FOURCC_P010 : VA_FOURCC_NV12;
    desc->width = surface->width;
    desc->height = surface->height;
    desc->num_objects = 1;
    desc->objects[0].fd = fd;
    desc->objects[0].size = surface->linear->size();
    desc->objects[0].drm_format_modifier = DRM_FORMAT_MOD_LINEAR;

    uint32_t offsets[2] = { 0, surface->linear_chroma_offset };
    if (flags & VA_EXPORT_SURFACE_COMPOSED_LAYERS) {
        desc->num_layers = 1;
        desc->layers[0].drm_format = surface->ten_bits ? DRM_FORMAT_P010 : DRM_FORMAT_NV12;
        desc->layers[0].num_planes = 2;
        for (int i = 0; i < 2; i++) {
            desc->layers[0].offset[i] = offsets[i];
            desc->layers[0].pitch[i] = surface->linear_pitch;
        }
    } else {
        desc->num_layers = 2;
        desc->layers[0].drm_format = surface->ten_bits ? DRM_FORMAT_R16 : DRM_FORMAT_R8;
        desc->layers[1].drm_format = surface->ten_bits ? DRM_FORMAT_GR1616 : DRM_FORMAT_GR88;
        for (int i = 0; i < 2; i++) {
            desc->layers[i].num_planes = 1;
            desc->layers[i].offset[0] = offsets[i];
            desc->layers[i].pitch[0] = surface->linear_pitch;
        }
    }

    return VA_STATUS_SUCCESS;
}

FUNC(CreateContext, VAConfigID config_id, int picture_width, int picture_height, int flag,
    VASurfaceID *render_targets, int num_render_targets, VAContextID *context_id)
{
    VAProfile profile;

    if (!configProfile(config_id, &profile))
        return VA_STATUS_ERROR_INVALID_CONFIG;

    /* refuse here, when the player can still decode with the CPU */
    {
        std::lock_guard<std::mutex> g(DRIVER_DATA->lock);
        if (DRIVER_DATA->nvdec.open() || DRIVER_DATA->vic.open())
            return VA_STATUS_ERROR_HW_BUSY;
    }

    auto decoder = Decoder::create(profile, DRIVER_DATA->nvdec, DRIVER_DATA->objects, picture_width, picture_height);
    if (!decoder)
        return VA_STATUS_ERROR_UNSUPPORTED_PROFILE;

    DRIVER_DATA->objects.createContext(context_id)->decoder = std::move(decoder);
    return VA_STATUS_SUCCESS;
}

FUNC(DestroyContext, VAContextID context_id)
{
    if (!DRIVER_DATA->objects.context(context_id))
        return VA_STATUS_ERROR_INVALID_CONTEXT;

    DRIVER_DATA->objects.remove(context_id);
    return VA_STATUS_SUCCESS;
}

FUNC(CreateBuffer, VAContextID context, VABufferType type, unsigned int size,
    unsigned int num_elements, void *data, VABufferID *buf_id)
{
    auto buffer = DRIVER_DATA->objects.createBuffer(buf_id);

    buffer->type = type;
    buffer->element_size = size;
    buffer->data.resize((size_t)size * num_elements);
    if (data)
        memcpy(buffer->data.data(), data, buffer->data.size());

    return VA_STATUS_SUCCESS;
}

FUNC(BufferSetNumElements, VABufferID buf_id, unsigned int num_elements)
{
    auto buffer = DRIVER_DATA->objects.buffer(buf_id);
    if (!buffer || buffer->gem)
        return VA_STATUS_ERROR_INVALID_BUFFER;

    buffer->data.resize(buffer->element_size * num_elements);
    return VA_STATUS_SUCCESS;
}

FUNC(BufferInfo, VABufferID buf_id, VABufferType *type, unsigned int *size, unsigned int *num_elements)
{
    auto buffer = DRIVER_DATA->objects.buffer(buf_id);
    if (!buffer)
        return VA_STATUS_ERROR_INVALID_BUFFER;

    *type = buffer->type;
    *size = buffer->element_size;
    *num_elements = buffer->element_size ? buffer->data.size() / buffer->element_size : 0;
    return VA_STATUS_SUCCESS;
}

FUNC(MapBuffer, VABufferID buf_id, void **pbuf)
{
    auto buffer = DRIVER_DATA->objects.buffer(buf_id);
    if (!buffer)
        return VA_STATUS_ERROR_INVALID_BUFFER;

    if (buffer->gem) {
        *pbuf = buffer->gem->map();
        if (!*pbuf)
            return VA_STATUS_ERROR_OPERATION_FAILED;
        buffer->gem->cpuAccess(true);
    } else {
        *pbuf = buffer->data.data();
    }

    return VA_STATUS_SUCCESS;
}

FUNC(UnmapBuffer, VABufferID buf_id)
{
    auto buffer = DRIVER_DATA->objects.buffer(buf_id);
    if (!buffer)
        return VA_STATUS_ERROR_INVALID_BUFFER;

    if (buffer->gem)
        buffer->gem->cpuAccess(false);

    return VA_STATUS_SUCCESS;
}

FUNC(DestroyBuffer, VABufferID buffer_id)
{
    auto buffer = DRIVER_DATA->objects.buffer(buffer_id);
    if (!buffer)
        return VA_STATUS_ERROR_INVALID_BUFFER;

    /* the buffer of an image goes with the image */
    if (!buffer->gem)
        DRIVER_DATA->objects.remove(buffer_id);

    return VA_STATUS_SUCCESS;
}

FUNC(BeginPicture, VAContextID context_id, VASurfaceID render_target)
{
    auto context = DRIVER_DATA->objects.context(context_id);
    if (!context)
        return VA_STATUS_ERROR_INVALID_CONTEXT;

    auto surface = DRIVER_DATA->objects.surface(render_target);
    if (!surface)
        return VA_STATUS_ERROR_INVALID_SURFACE;

    context->target = render_target;
    context->decoder->begin(render_target, surface);
    return VA_STATUS_SUCCESS;
}

FUNC(RenderPicture, VAContextID context_id, VABufferID *buffers, int num_buffers)
{
    auto context = DRIVER_DATA->objects.context(context_id);
    if (!context)
        return VA_STATUS_ERROR_INVALID_CONTEXT;

    for (int i = 0; i < num_buffers; i++) {
        auto buffer = DRIVER_DATA->objects.buffer(buffers[i]);
        if (!buffer)
            return VA_STATUS_ERROR_INVALID_BUFFER;

        VAStatus status = context->decoder->render(*buffer);
        if (status != VA_STATUS_SUCCESS)
            return status;
    }

    return VA_STATUS_SUCCESS;
}

FUNC(EndPicture, VAContextID context_id)
{
    auto context = DRIVER_DATA->objects.context(context_id);
    if (!context)
        return VA_STATUS_ERROR_INVALID_CONTEXT;

    std::lock_guard<std::mutex> g(DRIVER_DATA->lock);

    auto surface = DRIVER_DATA->objects.surface(context->target);
    if (!surface)
        return VA_STATUS_ERROR_INVALID_SURFACE;

    NvdecJob job;
    VAStatus status = context->decoder->build(job);
    if (status != VA_STATUS_SUCCESS)
        return status;

    if (DRIVER_DATA->nvdec.submit(job, surface->decoded))
        return VA_STATUS_ERROR_DECODING_ERROR;
    context->decoder->submitted(surface->decoded);

    /* somebody has the copy: make it again */
    if (surface->linear)
        return copyToLinear(DRIVER_DATA, *surface);

    return VA_STATUS_SUCCESS;
}

static VAStatus syncSurface(DriverData *dd, VASurfaceID id)
{
    Fence decoded, copied;
    {
        std::lock_guard<std::mutex> g(dd->lock);

        auto surface = dd->objects.surface(id);
        if (!surface)
            return VA_STATUS_ERROR_INVALID_SURFACE;
        decoded = surface->decoded;
        copied = surface->copied;
    }

    if (dd->drm.wait(decoded) || dd->drm.wait(copied))
        return VA_STATUS_ERROR_HW_BUSY;
    return VA_STATUS_SUCCESS;
}

FUNC(SyncSurface, VASurfaceID render_target)
{
    return syncSurface(DRIVER_DATA, render_target);
}

FUNC(SyncSurface2, VASurfaceID surface, uint64_t timeout_ns)
{
    return syncSurface(DRIVER_DATA, surface);
}

FUNC(QuerySurfaceStatus, VASurfaceID render_target, VASurfaceStatus *status)
{
    VAStatus err = syncSurface(DRIVER_DATA, render_target);

    *status = VASurfaceReady;
    return err;
}

FUNC(QuerySurfaceError, VASurfaceID render_target, VAStatus error_status, void **error_info)
{
    return VA_STATUS_ERROR_UNIMPLEMENTED;
}

FUNC(PutSurface, VASurfaceID surface, void *draw, short srcx, short srcy, unsigned short srcw,
    unsigned short srch, short destx, short desty, unsigned short destw, unsigned short desth,
    VARectangle *cliprects, unsigned int number_cliprects, unsigned int flags)
{
    return VA_STATUS_ERROR_UNIMPLEMENTED;
}

FUNC(QueryImageFormats, VAImageFormat *format_list, int *num_formats)
{
    static const VAImageFormat formats[] = {
        { VA_FOURCC_NV12, VA_LSB_FIRST, 12 },
        { VA_FOURCC_P010, VA_LSB_FIRST, 24 },
    };

    *num_formats = 2;
    memcpy(format_list, formats, sizeof(formats));
    return VA_STATUS_SUCCESS;
}

FUNC(CreateImage, VAImageFormat *format, int width, int height, VAImage *image_data)
{
    bool ten_bits = format->fourcc == VA_FOURCC_P010;

    if (!ten_bits && format->fourcc != VA_FOURCC_NV12)
        return VA_STATUS_ERROR_INVALID_IMAGE_FORMAT;
    if (width <= 0 || height <= 0 || width > MAX_WIDTH || height > MAX_HEIGHT)
        return VA_STATUS_ERROR_RESOLUTION_NOT_SUPPORTED;

    unsigned pitch = __ALIGN_KERNEL(width << ten_bits, 256);
    unsigned lines = __ALIGN_KERNEL(height, 2);
    size_t size = __ALIGN_KERNEL(pitch * lines * 3 / 2, 4096);

    std::lock_guard<std::mutex> g(DRIVER_DATA->lock);
    std::shared_ptr<GemBuffer> gem;
    for (auto it = DRIVER_DATA->image_pool.begin(); it != DRIVER_DATA->image_pool.end(); ++it) {
        if ((*it)->size() == size) {
            gem = std::move(*it);
            DRIVER_DATA->image_pool.erase(it);
            break;
        }
    }
    if (!gem) {
        gem = std::make_shared<GemBuffer>(DRIVER_DATA->drm);
        if (gem->allocateCached(size))
            return VA_STATUS_ERROR_ALLOCATION_FAILED;
    }

    memset(image_data, 0, sizeof(*image_data));
    auto image = DRIVER_DATA->objects.createImage(&image_data->image_id);
    auto buffer = DRIVER_DATA->objects.createBuffer(&image_data->buf);
    buffer->gem = std::move(gem);
    image->buffer = image_data->buf;
    image->width = width;
    image->height = height;
    image->ten_bits = ten_bits;
    image->pitch = pitch;
    image->chroma_offset = pitch * lines;

    image_data->format = *format;
    image_data->width = width;
    image_data->height = height;
    image_data->data_size = size;
    image_data->num_planes = 2;
    image_data->pitches[0] = image_data->pitches[1] = pitch;
    image_data->offsets[1] = image->chroma_offset;

    return VA_STATUS_SUCCESS;
}

FUNC(DeriveImage, VASurfaceID surface, VAImage *image)
{
    /* surfaces are block linear: only GetImage gives lines the CPU can read */
    return VA_STATUS_ERROR_OPERATION_FAILED;
}

FUNC(DestroyImage, VAImageID image_id)
{
    std::lock_guard<std::mutex> g(DRIVER_DATA->lock);

    auto image = DRIVER_DATA->objects.image(image_id);
    if (!image)
        return VA_STATUS_ERROR_INVALID_IMAGE;

    auto buffer = DRIVER_DATA->objects.buffer(image->buffer);
    if (buffer) {
        if (buffer->gem && DRIVER_DATA->image_pool.size() < 8)
            DRIVER_DATA->image_pool.push_back(std::move(buffer->gem));
        DRIVER_DATA->objects.remove(image->buffer);
    }
    DRIVER_DATA->objects.remove(image_id);

    return VA_STATUS_SUCCESS;
}

FUNC(SetImagePalette, VAImageID image, unsigned char *palette)
{
    return VA_STATUS_ERROR_UNIMPLEMENTED;
}

/* both ways between a surface and an image of the same size */
static VAStatus copyImage(DriverData *dd, VASurfaceID surface_id, VAImageID image_id, bool to_image,
    unsigned width, unsigned height, Fence &done)
{
    std::lock_guard<std::mutex> g(dd->lock);

    auto surface = dd->objects.surface(surface_id);
    if (!surface)
        return VA_STATUS_ERROR_INVALID_SURFACE;

    auto image = dd->objects.image(image_id);
    auto buffer = image ? dd->objects.buffer(image->buffer) : nullptr;
    if (!buffer || !buffer->gem)
        return VA_STATUS_ERROR_INVALID_IMAGE;
    if (image->ten_bits != surface->ten_bits)
        return VA_STATUS_ERROR_INVALID_IMAGE_FORMAT;
    if (width > surface->width || height > surface->height || width > image->width || height > image->height)
        return VA_STATUS_ERROR_INVALID_PARAMETER;
    if (dd->vic.open())
        return VA_STATUS_ERROR_OPERATION_FAILED;

    VicSurface linear = { buffer->gem.get(), width, height, image->pitch, image->chroma_offset, false, surface->ten_bits };
    VicSurface blocks = vicSurface(*surface);
    blocks.width = width;
    blocks.height = height;
    if (to_image)
        return dd->vic.copy(linear, blocks, surface->decoded, done) ? VA_STATUS_ERROR_OPERATION_FAILED : VA_STATUS_SUCCESS;

    if (dd->vic.copy(blocks, linear, surface->decoded, done))
        return VA_STATUS_ERROR_OPERATION_FAILED;
    surface->decoded = done;
    return surface->linear ? copyToLinear(dd, *surface) : VA_STATUS_SUCCESS;
}

FUNC(GetImage, VASurfaceID surface, int x, int y, unsigned int width, unsigned int height,
    VAImageID image)
{
    Fence done;

    if (x || y)
        return VA_STATUS_ERROR_INVALID_PARAMETER;

    VAStatus status = copyImage(DRIVER_DATA, surface, image, true, width, height, done);
    if (status == VA_STATUS_SUCCESS && DRIVER_DATA->drm.wait(done))
        status = VA_STATUS_ERROR_HW_BUSY;
    return status;
}

FUNC(PutImage, VASurfaceID surface, VAImageID image, int src_x, int src_y, unsigned int src_width,
    unsigned int src_height, int dest_x, int dest_y, unsigned int dest_width,
    unsigned int dest_height)
{
    Fence done;

    if (src_x || src_y || dest_x || dest_y || src_width != dest_width || src_height != dest_height)
        return VA_STATUS_ERROR_INVALID_PARAMETER;

    VAStatus status = copyImage(DRIVER_DATA, surface, image, false, src_width, src_height, done);
    /* the image can change when this returns */
    if (status == VA_STATUS_SUCCESS && DRIVER_DATA->drm.wait(done))
        status = VA_STATUS_ERROR_HW_BUSY;
    return status;
}

FUNC(QuerySubpictureFormats, VAImageFormat *format_list, unsigned int *flags,
    unsigned int *num_formats)
{
    *num_formats = 0;
    return VA_STATUS_SUCCESS;
}

FUNC(CreateSubpicture, VAImageID image, VASubpictureID *subpicture)
{
    return VA_STATUS_ERROR_UNIMPLEMENTED;
}

FUNC(DestroySubpicture, VASubpictureID subpicture)
{
    return VA_STATUS_ERROR_UNIMPLEMENTED;
}

FUNC(SetSubpictureImage, VASubpictureID subpicture, VAImageID image)
{
    return VA_STATUS_ERROR_UNIMPLEMENTED;
}

FUNC(SetSubpictureChromakey, VASubpictureID subpicture, unsigned int chromakey_min,
    unsigned int chromakey_max, unsigned int chromakey_mask)
{
    return VA_STATUS_ERROR_UNIMPLEMENTED;
}

FUNC(SetSubpictureGlobalAlpha, VASubpictureID subpicture, float global_alpha)
{
    return VA_STATUS_ERROR_UNIMPLEMENTED;
}

FUNC(AssociateSubpicture, VASubpictureID subpicture, VASurfaceID *target_surfaces, int num_surfaces,
    short src_x, short src_y, unsigned short src_width, unsigned short src_height, short dest_x,
    short dest_y, unsigned short dest_width, unsigned short dest_height, unsigned int flags)
{
    return VA_STATUS_ERROR_UNIMPLEMENTED;
}

FUNC(DeassociateSubpicture, VASubpictureID subpicture, VASurfaceID *target_surfaces,
    int num_surfaces)
{
    return VA_STATUS_ERROR_UNIMPLEMENTED;
}

FUNC(QueryDisplayAttributes, VADisplayAttribute *attr_list, int *num_attributes)
{
    *num_attributes = 0;
    return VA_STATUS_SUCCESS;
}

FUNC(GetDisplayAttributes, VADisplayAttribute *attr_list, int num_attributes)
{
    return VA_STATUS_ERROR_UNIMPLEMENTED;
}

FUNC(SetDisplayAttributes, VADisplayAttribute *attr_list, int num_attributes)
{
    return VA_STATUS_ERROR_UNIMPLEMENTED;
}

extern "C" __attribute__((visibility("default"))) VAStatus __vaDriverInit_1_0(VADriverContextP ctx)
{
    struct VADriverVTable *const vtbl = ctx->vtable;

    auto dd = std::make_unique<DriverData>();
    if (dd->drm.fd() == -1)
        return VA_STATUS_ERROR_UNKNOWN;

    ctx->version_major = VA_MAJOR_VERSION;
    ctx->version_minor = VA_MINOR_VERSION;
    ctx->max_profiles = sizeof(profiles) / sizeof(profiles[0]);
    ctx->max_entrypoints = 1;
    ctx->max_attributes = 8;
    ctx->max_image_formats = 2;
    ctx->max_subpic_formats = 1;
    ctx->max_display_attributes = 1;
    ctx->str_vendor = "Tegra NVDEC/VIC driver";
    ctx->pDriverData = dd.release();

    vtbl->vaTerminate = tegra_Terminate;
    vtbl->vaQueryConfigProfiles = tegra_QueryConfigProfiles;
    vtbl->vaQueryConfigEntrypoints = tegra_QueryConfigEntrypoints;
    vtbl->vaGetConfigAttributes = tegra_GetConfigAttributes;
    vtbl->vaCreateConfig = tegra_CreateConfig;
    vtbl->vaDestroyConfig = tegra_DestroyConfig;
    vtbl->vaQueryConfigAttributes = tegra_QueryConfigAttributes;
    vtbl->vaCreateSurfaces = tegra_CreateSurfaces;
    vtbl->vaCreateSurfaces2 = tegra_CreateSurfaces2;
    vtbl->vaExportSurfaceHandle = tegra_ExportSurfaceHandle;
    vtbl->vaDestroySurfaces = tegra_DestroySurfaces;
    vtbl->vaCreateContext = tegra_CreateContext;
    vtbl->vaDestroyContext = tegra_DestroyContext;
    vtbl->vaCreateBuffer = tegra_CreateBuffer;
    vtbl->vaBufferSetNumElements = tegra_BufferSetNumElements;
    vtbl->vaBufferInfo = tegra_BufferInfo;
    vtbl->vaMapBuffer = tegra_MapBuffer;
    vtbl->vaUnmapBuffer = tegra_UnmapBuffer;
    vtbl->vaDestroyBuffer = tegra_DestroyBuffer;
    vtbl->vaBeginPicture = tegra_BeginPicture;
    vtbl->vaRenderPicture = tegra_RenderPicture;
    vtbl->vaEndPicture = tegra_EndPicture;
    vtbl->vaSyncSurface = tegra_SyncSurface;
    vtbl->vaSyncSurface2 = tegra_SyncSurface2;
    vtbl->vaQuerySurfaceStatus = tegra_QuerySurfaceStatus;
    vtbl->vaQuerySurfaceError = tegra_QuerySurfaceError;
    vtbl->vaPutSurface = tegra_PutSurface;
    vtbl->vaQueryImageFormats = tegra_QueryImageFormats;
    vtbl->vaCreateImage = tegra_CreateImage;
    vtbl->vaDeriveImage = tegra_DeriveImage;
    vtbl->vaDestroyImage = tegra_DestroyImage;
    vtbl->vaSetImagePalette = tegra_SetImagePalette;
    vtbl->vaGetImage = tegra_GetImage;
    vtbl->vaPutImage = tegra_PutImage;
    vtbl->vaQuerySubpictureFormats = tegra_QuerySubpictureFormats;
    vtbl->vaCreateSubpicture = tegra_CreateSubpicture;
    vtbl->vaDestroySubpicture = tegra_DestroySubpicture;
    vtbl->vaSetSubpictureImage = tegra_SetSubpictureImage;
    vtbl->vaSetSubpictureChromakey = tegra_SetSubpictureChromakey;
    vtbl->vaSetSubpictureGlobalAlpha = tegra_SetSubpictureGlobalAlpha;
    vtbl->vaAssociateSubpicture = tegra_AssociateSubpicture;
    vtbl->vaDeassociateSubpicture = tegra_DeassociateSubpicture;
    vtbl->vaQueryDisplayAttributes = tegra_QueryDisplayAttributes;
    vtbl->vaGetDisplayAttributes = tegra_GetDisplayAttributes;
    vtbl->vaSetDisplayAttributes = tegra_SetDisplayAttributes;
    vtbl->vaQuerySurfaceAttributes = tegra_QuerySurfaceAttributes;

    return VA_STATUS_SUCCESS;
}
