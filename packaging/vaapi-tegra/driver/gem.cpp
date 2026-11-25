/* kate: replace-tabs true; indent-width 4
 *
 * Copyright (c) 2017, NVIDIA CORPORATION. All rights reserved.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a
 * copy of this software and associated documentation files (the "Software"),
 * to deal in the Software without restriction, including without limitation
 * the rights to use, copy, modify, merge, publish, distribute, sublicense,
 * and/or sell copies of the Software, and to permit persons to whom the
 * Software is furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.  IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
 * DEALINGS IN THE SOFTWARE.
 */

#include "gem.h"

#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <cerrno>
#include <cstdio>
#include <cstring>
#include <poll.h>
#include <stdlib.h>
#include <ctime>
#include <linux/dma-buf.h>
#include <linux/dma-heap.h>

#include "uapi_headers/tegra_drm.h"

DrmDevice::DrmDevice()
{
    _fd = -1;
    _tegra210 = true;

    /* the render node of tegra-drm: the GPU may have one too */
    for (int minor = 128; minor < 144 && _fd == -1; minor++) {
        char path[32], name[32] = { 0 };
        struct drm_version version = { 0 };

        snprintf(path, sizeof(path), "/dev/dri/renderD%d", minor);
        int fd = open(path, O_RDWR | O_CLOEXEC);
        if (fd == -1)
            continue;

        version.name = name;
        version.name_len = sizeof(name) - 1;
        if (::ioctl(fd, DRM_IOCTL_VERSION, &version) == 0 && !strcmp(name, "tegra"))
            _fd = fd;
        else
            close(fd);
    }

    if (_fd == -1)
        fprintf(stderr, "tegra: no tegra DRM device\n");
}

DrmDevice::~DrmDevice()
{
    if (_fd != -1)
        close(_fd);
}

int DrmDevice::ioctl(int request, void *ptr)
{
    return ::ioctl(_fd, request, ptr);
}

int DrmDevice::open_channel(uint32_t cl, uint64_t *context)
{
    struct drm_tegra_channel_open args = { 0 };

    args.host1x_class = cl;
    if (ioctl(DRM_IOCTL_TEGRA_CHANNEL_OPEN, &args) == -1)
        return -1;

    /* the sandbox of a browser hides /sys: the kernel tells the SoC here */
    _tegra210 = args.version == 0x21;

    *context = args.context;
    return 0;
}

int DrmDevice::close_channel(uint64_t context)
{
    struct drm_tegra_channel_close args = { 0 };

    args.context = context;
    return ioctl(DRM_IOCTL_TEGRA_CHANNEL_CLOSE, &args);
}

int DrmDevice::allocate_syncpoint(uint64_t context, uint32_t *syncpt)
{
    struct drm_tegra_syncpoint_allocate args = { 0 };

    if (ioctl(DRM_IOCTL_TEGRA_SYNCPOINT_ALLOCATE, &args) == -1)
        return -1;

    *syncpt = args.id;
    return 0;
}

void DrmDevice::free_syncpoint(uint32_t syncpt)
{
    struct drm_tegra_syncpoint_free args = { 0 };

    args.id = syncpt;
    ioctl(DRM_IOCTL_TEGRA_SYNCPOINT_FREE, &args);
}

int64_t DrmDevice::now()
{
    struct timespec ts;

    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

int DrmDevice::waitUntil(const Fence &fence, int64_t deadline_ns)
{
    struct drm_tegra_syncpoint_wait args = { 0 };

    if (!fence.valid())
        return 0;

    args.id = fence.id;
    args.threshold = fence.value;
    args.timeout_ns = deadline_ns;

    while (ioctl(DRM_IOCTL_TEGRA_SYNCPOINT_WAIT, &args) == -1) {
        if (errno == EINTR)
            continue;
        if (errno == EAGAIN || errno == ETIMEDOUT || errno == ETIME)
            return 1;
        perror("tegra: waiting for a video engine");
        return -1;
    }
    return 0;
}

int DrmDevice::wait(const Fence &fence)
{
    int err = waitUntil(fence, now() + 5000000000);

    if (err == 1)
        fprintf(stderr, "tegra: the video engine does not answer\n");
    return err;
}

GemBuffer::GemBuffer(DrmDevice &dev)
: _dev(dev), _valid(false), _handle(0), _map(nullptr), _dmabuf(-1)
{
}

GemBuffer::~GemBuffer()
{
    for (const auto& [context, mapping] : _mapping_ids) {
        struct drm_tegra_channel_unmap channel_unmap_args = { 0 };
        channel_unmap_args.context = context;
        channel_unmap_args.mapping = mapping;
        _dev.ioctl(DRM_IOCTL_TEGRA_CHANNEL_UNMAP, &channel_unmap_args);
    }

    if (_map) {
        munmap(_map, _size);
    }

    if (_valid) {
        struct drm_gem_close close_args;
        memset(&close_args, 0, sizeof(close_args));

        close_args.handle = _handle;

        _dev.ioctl(DRM_IOCTL_GEM_CLOSE, &close_args);
    }

    if (_dmabuf != -1)
        close(_dmabuf);
}

int GemBuffer::channelMap(uint32_t channel_ctx, bool readwrite)
{
    struct drm_tegra_channel_map channel_map_args = { 0 };
    int err;

    if (_mapping_ids.find(channel_ctx) != _mapping_ids.end())
        return 0;

    channel_map_args.context = channel_ctx;
    channel_map_args.handle = _handle;
    channel_map_args.flags = readwrite ? DRM_TEGRA_CHANNEL_MAP_READ_WRITE : DRM_TEGRA_CHANNEL_MAP_READ;

    err = _dev.ioctl(DRM_IOCTL_TEGRA_CHANNEL_MAP, &channel_map_args);
    if (err == -1) {
        perror("GEM channel map failed");
        return err;
    }

    _mapping_ids.insert({channel_ctx, channel_map_args.mapping});

    return 0;
}

int GemBuffer::allocate(size_t bytes)
{
    struct drm_tegra_gem_create gem_create_args;
    int err;

    memset(&gem_create_args, 0, sizeof(gem_create_args));
    gem_create_args.size = bytes;

    err = _dev.ioctl(DRM_IOCTL_TEGRA_GEM_CREATE, &gem_create_args);
    if (err == -1) {
        perror("GEM create failed");
        return err;
    }

    _handle = gem_create_args.handle;
    _size = bytes;
    _valid = true;

    return 0;
}

/* cached memory from the system dma-buf heap, imported into the DRM device */
int GemBuffer::allocateCached(size_t bytes)
{
    struct dma_heap_allocation_data alloc = { 0 };
    struct drm_prime_handle prime = { 0 };
    int heap = open("/dev/dma_heap/system", O_RDONLY | O_CLOEXEC);

    if (heap == -1)
        return allocate(bytes);

    alloc.len = bytes;
    alloc.fd_flags = O_RDWR | O_CLOEXEC;
    int err = ::ioctl(heap, DMA_HEAP_IOCTL_ALLOC, &alloc);
    close(heap);
    if (err == -1)
        return allocate(bytes);

    prime.fd = alloc.fd;
    if (_dev.ioctl(DRM_IOCTL_PRIME_FD_TO_HANDLE, &prime) == -1) {
        close(alloc.fd);
        return allocate(bytes);
    }

    _handle = prime.handle;
    _dmabuf = alloc.fd;
    _size = bytes;
    _valid = true;
    return 0;
}

void GemBuffer::cpuAccess(bool begin)
{
    if (_dmabuf == -1)
        return;

    struct dma_buf_sync sync = { 0 };
    sync.flags = DMA_BUF_SYNC_RW | (begin ? DMA_BUF_SYNC_START : DMA_BUF_SYNC_END);
    ::ioctl(_dmabuf, DMA_BUF_IOCTL_SYNC, &sync);
}

void * GemBuffer::map()
{
    int err;

    if (!_valid)
        return nullptr;
    if (_map)
        return _map;

    if (_dmabuf != -1) {
        _map = mmap(0, _size, PROT_READ | PROT_WRITE, MAP_SHARED, _dmabuf, 0);
        if (_map == MAP_FAILED) {
            _map = nullptr;
            perror("mmap failed");
        }
        return _map;
    }

    struct drm_tegra_gem_mmap gem_mmap_args;
    memset(&gem_mmap_args, 0, sizeof(gem_mmap_args));
    gem_mmap_args.handle = _handle;

    err = _dev.ioctl(DRM_IOCTL_TEGRA_GEM_MMAP, &gem_mmap_args);
    if (err == -1) {
        perror("GEM mmap failed");
        return nullptr;
    }

    _map = mmap(0, _size, PROT_READ | PROT_WRITE, MAP_SHARED,
                _dev.fd(), gem_mmap_args.offset);
    if (_map == MAP_FAILED) {
        _map = nullptr;
        perror("mmap failed");
        return nullptr;
    }

    return _map;
}

int32_t GemBuffer::exportFd(bool readwrite)
{
    struct drm_prime_handle args;
    int err;

    if (_dmabuf != -1)
        return fcntl(_dmabuf, F_DUPFD_CLOEXEC, 0);

    memset(&args, 0, sizeof(args));

    args.handle = _handle;
    args.flags = DRM_CLOEXEC | (readwrite ? DRM_RDWR : 0);

    err = _dev.ioctl(DRM_IOCTL_PRIME_HANDLE_TO_FD, &args);
    if (err == -1) {
        perror("GEM export failed");
        return -1;
    }

    return args.fd;
}
