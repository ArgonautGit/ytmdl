#!/usr/bin/env python3
"""Copies one logical partition out of an Android dynamic-partition super image.

    lpextract.py super.img system_a out.img

Reads the primary LP metadata (liblp format: geometry at 4096, metadata slot 0 at
12288) and writes the partition's linear extents to a sparse output file. Only raw
(non-sparse) super images are supported; Cuttlefish ships raw ones.
"""

import struct
import sys

SECTOR = 512
GEOMETRY_OFFSET = 4096
GEOMETRY_MAGIC = 0x616C4467
HEADER_MAGIC = 0x414C5030
TARGET_LINEAR, TARGET_ZERO = 0, 1


def read_metadata(f):
    f.seek(GEOMETRY_OFFSET)
    magic, _size = struct.unpack("<II", f.read(8))
    if magic != GEOMETRY_MAGIC:
        sys.exit("not a raw super image (bad LP geometry magic)")
    f.seek(GEOMETRY_OFFSET + 8 + 32)
    metadata_max_size, _slots, _block = struct.unpack("<III", f.read(12))
    del metadata_max_size

    base = GEOMETRY_OFFSET * 3  # reserved + primary geometry + backup geometry
    f.seek(base)
    hdr = f.read(256)
    magic, _major, _minor, header_size = struct.unpack_from("<IHHI", hdr, 0)
    if magic != HEADER_MAGIC:
        sys.exit("bad LP metadata header magic")
    # header: magic, major, minor, header_size, checksum[32], tables_size, checksum[32], then 4 table descriptors
    off = 4 + 2 + 2 + 4 + 32 + 4 + 32
    tables = [struct.unpack_from("<III", hdr, off + 12 * i) for i in range(4)]
    (p_off, p_num, p_size), (e_off, e_num, e_size) = tables[0], tables[1]

    def table(t_off, num, size):
        f.seek(base + header_size + t_off)
        data = f.read(num * size)
        return [data[i * size : (i + 1) * size] for i in range(num)]

    partitions = {}
    extents = [struct.unpack_from("<QIQI", e, 0) for e in table(e_off, e_num, e_size)]
    for p in table(p_off, p_num, p_size):
        name = p[:36].split(b"\0", 1)[0].decode()
        _attrs, first, count, _group = struct.unpack_from("<IIII", p, 36)
        partitions[name] = extents[first : first + count]
    return partitions


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__.strip())
    src, name, dst = sys.argv[1:]
    with open(src, "rb") as f:
        parts = read_metadata(f)
        if name not in parts:
            sys.exit(f"no partition {name!r}; have: {', '.join(sorted(parts))}")
        with open(dst, "wb") as out:
            pos = 0
            for num_sectors, target_type, target_data, _source in parts[name]:
                length = num_sectors * SECTOR
                if target_type == TARGET_LINEAR:
                    f.seek(target_data * SECTOR)
                    out.seek(pos)
                    left = length
                    while left:
                        chunk = f.read(min(left, 8 << 20))
                        if not chunk:
                            sys.exit("super image truncated")
                        out.write(chunk)
                        left -= len(chunk)
                elif target_type != TARGET_ZERO:
                    sys.exit(f"unsupported extent type {target_type}")
                pos += length
            out.truncate(pos)
    print(f"{name}: {pos // (1 << 20)} MiB -> {dst}")


if __name__ == "__main__":
    main()
