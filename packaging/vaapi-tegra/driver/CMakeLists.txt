cmake_minimum_required(VERSION 3.10)
project(vaapi-tegra CXX)

find_package(PkgConfig REQUIRED)
pkg_check_modules(DRM REQUIRED libdrm)

if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE RelWithDebInfo)
endif()

add_library(tegra_drv_video MODULE
    main.cpp objects.cpp gem.cpp
    engines/nvdec.cpp engines/vic.cpp
    decoders/decoder.cpp decoders/h264.cpp decoders/hevc.cpp decoders/mpeg2.cpp
    decoders/vp8.cpp decoders/vp9.cpp)
set_target_properties(tegra_drv_video PROPERTIES PREFIX "" CXX_STANDARD 17 CXX_STANDARD_REQUIRED ON
    CXX_VISIBILITY_PRESET hidden)
target_compile_options(tegra_drv_video PRIVATE -Wall -Wno-unused-variable -Wno-unused-but-set-variable)
target_include_directories(tegra_drv_video PRIVATE ${DRM_INCLUDE_DIRS})
target_link_libraries(tegra_drv_video ${DRM_LIBRARIES})

if("${LIBVA_DRIVERS_PATH}" STREQUAL "")
    set(LIBVA_DRIVERS_PATH "lib${LIB_SUFFIX}/dri/")
endif()

install(TARGETS tegra_drv_video LIBRARY DESTINATION ${LIBVA_DRIVERS_PATH})
