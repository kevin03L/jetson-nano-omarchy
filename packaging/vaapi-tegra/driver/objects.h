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
    std::shared_ptr<Object> stream; /* what the decoder keeps between the pictures of a stream */

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

    std::shared_ptr<Surface> createSurface(VASurfaceID *id) { return add<Surface>(id); }
    std::shared_ptr<Buffer> createBuffer(VABufferID *id) { return add<Buffer>(id); }
    std::shared_ptr<Context> createContext(VAContextID *id) { return add<Context>(id); }
    std::shared_ptr<Image> createImage(VAImageID *id) { return add<Image>(id); }

    /* an object stays while somebody uses it, even when another thread removes it */
    std::shared_ptr<Surface> surface(VASurfaceID id) { return std::dynamic_pointer_cast<Surface>(get(id)); }
    std::shared_ptr<Buffer> buffer(VABufferID id) { return std::dynamic_pointer_cast<Buffer>(get(id)); }
    std::shared_ptr<Context> context(VAContextID id) { return std::dynamic_pointer_cast<Context>(get(id)); }
    std::shared_ptr<Image> image(VAImageID id) { return std::dynamic_pointer_cast<Image>(get(id)); }

    void remove(VAGenericID id);

private:
    std::mutex _lock;
    std::vector<std::shared_ptr<Object>> _objects;

    template <typename T> std::shared_ptr<T> add(VAGenericID *id)
    {
        auto object = std::make_shared<T>();
        *id = addGeneric(object);
        return object;
    }

    VAGenericID addGeneric(std::shared_ptr<Object> object);
    std::shared_ptr<Object> get(VAGenericID id);
};

#endif
