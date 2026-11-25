// SPDX-License-Identifier: MIT
#ifndef FENCE_H
#define FENCE_H

#include <cstdint>

/* a host1x syncpoint reaching a value: the end of a job */
struct Fence {
    uint32_t id = 0xffffffff, value = 0;

    bool valid() const { return id != 0xffffffff; }
};

#endif
