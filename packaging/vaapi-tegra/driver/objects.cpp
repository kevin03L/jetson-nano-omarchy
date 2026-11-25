// SPDX-License-Identifier: MIT
#include "objects.h"

void Objects::clear()
{
    for (Object *obj : _objects)
        delete obj;

    _objects.clear();
}

/* ids are used again: a player makes several buffers for each picture */
VAGenericID Objects::addGeneric(Object *obj)
{
    std::lock_guard<std::mutex> g(_lock);

    for (VAGenericID id = 1; id <= _objects.size(); id++) {
        if (!_objects[id-1]) {
            _objects[id-1] = obj;
            return id;
        }
    }

    _objects.push_back(obj);
    return _objects.size();
}

Object *Objects::get(VAGenericID id)
{
    std::lock_guard<std::mutex> g(_lock);

    if (id == 0 || id > _objects.size())
        return nullptr;

    return _objects[id-1];
}

void Objects::remove(VAGenericID id)
{
    std::lock_guard<std::mutex> g(_lock);

    if (id == 0 || id > _objects.size())
        return;

    delete _objects[id-1];
    _objects[id-1] = nullptr;
}
