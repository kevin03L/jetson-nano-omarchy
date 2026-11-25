// SPDX-License-Identifier: MIT
#ifndef OBJECTS_H
#define OBJECTS_H

#include <memory>
#include <mutex>
#include <vector>

#include <va/va_backend.h>

#include "decoders/decoder.h"
#include "fence.h"
#include "gem.h"

class Object
{
public:
    virtual ~Object() {}
};

class Buffer : public Object
{
public:
    VABufferType type = VABufferTypeMax;
    size_t element_size = 0;
    std::vector<uint8_t> data;

    /* the memory of an image */
    std::shared_ptr<GemBuffer> gem;
};

class Image : public Object
{
public:
    VABufferID buffer;
    unsigned width, height, pitch, chroma_offset;
    bool ten_bits;
};

/*
 * A decoded picture: 16Bx2 block linear, two GOBs, as NVDEC writes it. The CPU and the GPU get a
 * linear copy made by VIC.
 */
class Surface : public Object
{
public:
    unsigned width, height;
    bool ten_bits = false;
    unsigned pitch;         /* bytes */
    unsigned chroma_offset; /* bytes */
    std::shared_ptr<GemBuffer> gem;
    Fence decoded;

    /* the copy other devices read, made again after each picture once it was exported */
    std::shared_ptr<GemBuffer> linear;
    unsigned linear_pitch = 0, linear_chroma_offset = 0;
    Fence copied;
};

class Context : public Object
{
public:
    std::unique_ptr<Decoder> decoder;
    VASurfaceID target = VA_INVALID_SURFACE;
};

class Objects
{
public:
    void clear();

    Surface *createSurface(VASurfaceID *id) { return add(new Surface, id); }
    Buffer *createBuffer(VABufferID *id) { return add(new Buffer, id); }
    Context *createContext(VAContextID *id) { return add(new Context, id); }
    Image *createImage(VAImageID *id) { return add(new Image, id); }

    Surface *surface(VASurfaceID id) { return dynamic_cast<Surface *>(get(id)); }
    Buffer *buffer(VABufferID id) { return dynamic_cast<Buffer *>(get(id)); }
    Context *context(VAContextID id) { return dynamic_cast<Context *>(get(id)); }
    Image *image(VAImageID id) { return dynamic_cast<Image *>(get(id)); }

    void remove(VAGenericID id);

private:
    std::mutex _lock;
    std::vector<Object *> _objects;

    template <typename T> T *add(T *obj, VAGenericID *id)
    {
        *id = addGeneric(obj);
        return obj;
    }

    VAGenericID addGeneric(Object *obj);
    Object *get(VAGenericID id);
};

#endif
