# vaapi-tegra

VA-API driver for the video engines of the Tegra210 (Jetson Nano), on mainline's tegra-drm.
NVDEC decodes, VIC copies the pictures to memory the CPU and the GPU can read.

| Codec | Compared with software decoding |
|---|---|
| H.264, 8 bits, up to 4096x4096, interlaced too | identical |
| H.265, 8 and 10 bits 4:2:0 | identical |
| VP9 profile 0, VP8 | identical |
| MPEG-2 | rounding differences (about 70 dB) |

The hardware has no AV1, 10-bit VP9 or 10-bit H.264.

Pictures can be exported (`vaExportSurfaceHandle`) as linear dma-bufs of two planes, in the shape
NVIDIA's GPU driver imports: lines of a multiple of 128 bytes, buffers of a multiple of 64 KB.

Needed: the NVDEC firmware (`nvidia-l4t-gm20b-firmware`), and a device tree that powers NVJPG
together with NVDEC (`images/mainline-kernel/fix-video-dtb.sh`). The driver refuses to decode
without the latter, as the first access to NVDEC would stop the board.

It started from Mikko Perttunen's vaapi-tegra-driver (NVIDIA, MIT). `decoders/hevc`, `vp8` and `vp9`
are LGPL-2.1-or-later: what the hardware expects comes from the nvtegra decoders of averne's FFmpeg.
`engine_headers/clc5b0.h` and `nvdec_drv.h` are NVIDIA's (open-gpu-doc, MIT).
