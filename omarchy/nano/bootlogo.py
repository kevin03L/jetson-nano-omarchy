#!/usr/bin/env python3
"""Replace the NVIDIA logo that cboot shows on HDMI (eMMC partition BMP) with another picture.

    bootlogo.py build PARTITION_OR_BACKUP PICTURE.png OUT.blob [#rrggbb]   (PICTURE: 1920x1080)
    bootlogo.py install OUT.blob     write it to the BMP partition (the old one is kept in /root)
    bootlogo.py restore BACKUP       put a saved partition back

The pictures keep their headers, sizes and places; only the pixels of the logo (type 0) change. The
blob is LZ4 (legacy frame) and must fit in the 192 KiB partition: few colours, no gradients.
"""
import os, struct, subprocess, sys, time

PART = '/dev/disk/by-partlabel/BMP'
HDR = struct.Struct('<16sIIIII')
MAGIC = b'NVIDIA__BLOB__V2'


def run(*cmd, **kw):
    return subprocess.run(cmd, check=True, stdout=subprocess.PIPE, **kw).stdout


def unpack(raw):
    magic, ver, size, hdr, n, comp = HDR.unpack_from(raw)
    if magic != MAGIC or hdr != HDR.size or comp != 1 or not 0 < n < 16 or size > len(raw):
        sys.exit('not a compressed NVIDIA boot picture blob')
    return bytearray(run('lz4', '-dc', input=raw[hdr:size])), ver, n


def pictures(body, n):
    for i in range(n):
        kind, off, length, res = struct.unpack_from('<4I', body, i * 52)
        off -= HDR.size
        if body[off:off + 2] != b'BM':
            sys.exit('picture %d is not a BMP' % i)
        pix, = struct.unpack_from('<I', body, off + 10)
        w, h, _, bpp, comp = struct.unpack_from('<iiHHI', body, off + 18)
        if bpp != 24 or comp or h < 0 or (w * 3) % 4 or pix + w * h * 3 > length:
            sys.exit('picture %d has an unexpected format' % i)
        yield kind, off + pix, w, h


def build(src, logo, out, bg='black'):
    body, ver, n = unpack(open(src, 'rb').read())
    old = len(body)
    for kind, at, w, h in pictures(body, n):
        if kind:
            continue        # e.g. 7: the microSD error screen
        size = '%dx%d' % (w, h)
        bgr = run('magick', logo, '-resize', size, '-background', bg, '-gravity', 'center',
                  '-extent', size, '-alpha', 'remove', '-flip', '-depth', '8', 'bgr:-')
        assert len(bgr) == w * h * 3
        body[at:at + len(bgr)] = bgr
    assert len(body) == old
    lz = run('lz4', '-l', '-12', '-c', input=bytes(body))
    blob = HDR.pack(MAGIC, ver, HDR.size + len(lz), HDR.size, n, 1) + lz
    if unpack(blob)[0] != body:
        sys.exit('the new blob does not decompress to itself')
    open(out, 'wb').write(blob)
    print('%s: %d bytes, %d pictures' % (out, len(blob), n))


def write(blob, size):
    with open(PART, 'r+b') as f:
        f.write(blob.ljust(size, b'\0'))
        f.flush()
        os.fsync(f.fileno())
    subprocess.run(['sh', '-c', 'sync; echo 3 > /proc/sys/vm/drop_caches'])
    if open(PART, 'rb').read() != blob.ljust(size, b'\0'):
        sys.exit('VERIFICATION FAILED: restore the backup before rebooting')
    print('written and verified')


def install(path, backup):
    blob = open(path, 'rb').read()
    body, _, n = unpack(blob)
    old = open(PART, 'rb').read()
    cur, _, m = unpack(old)
    if [p for p in pictures(body, n)] != [p for p in pictures(cur, m)] or len(blob) > len(old):
        sys.exit('the new pictures do not match the ones of this board')
    if backup:
        name = '/root/BMP-%s.img' % time.strftime('%Y%m%d-%H%M%S')
        open(name, 'wb').write(old)
        print('backup: ' + name)
    write(blob, len(old))


if __name__ == '__main__':
    a = sys.argv[1:]
    if len(a) in (4, 5) and a[0] == 'build':
        build(*a[1:])
    elif len(a) == 2 and a[0] in ('install', 'restore'):
        install(a[1], a[0] == 'install')
    else:
        sys.exit(__doc__)
