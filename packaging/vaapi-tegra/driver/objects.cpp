// SPDX-License-Identifier: MIT
#include "objects.h"

void Objects::clear()
{
    std::lock_guard<std::mutex> g(_lock);

    _objects.clear();
}

/* ids are used again: a player makes several buffers for each picture */
VAGenericID Objects::addGeneric(std::shared_ptr<Object> object)
{
    std::lock_guard<std::mutex> g(_lock);

    for (VAGenericID id = 1; id <= _objects.size(); id++) {
        if (!_objects[id-1]) {
            _objects[id-1] = std::move(object);
            return id;
        }
    }

    _objects.push_back(std::move(object));
    return _objects.size();
}

std::shared_ptr<Object> Objects::get(VAGenericID id)
{
    std::lock_guard<std::mutex> g(_lock);

    if (id == 0 || id > _objects.size())
        return nullptr;

    return _objects[id-1];
}

void Objects::remove(VAGenericID id)
{
    std::shared_ptr<Object> object; /* destroyed without the lock */
    std::lock_guard<std::mutex> g(_lock);

    if (id == 0 || id > _objects.size())
        return;

    object = std::move(_objects[id-1]);
    _objects[id-1] = nullptr;
}
