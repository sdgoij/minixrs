#!/usr/bin/env python3
"""Summarise a `virtio_blk_handle_read` trace: request count, bytes, size histogram.

Boot QEMU with the trace log backend and one command, then read the log here:

    qemu-system-x86_64 ... -trace enable=virtio_blk_handle_read -D target/read.trace
    python tools/probes/virtio_read_hist.py target/read.trace

Each `virtio_blk_handle_read` line is one request. A filesystem block read should be one
request of the zone size (8 sectors = 4096 bytes for MFS with 4 KiB zones); several small
requests per block, or a request count above the file's zone count plus a few, is the
amplification this measures. Compare two boots (a command that only execs the reader, and
one that reads the file) because the reader's own image is read too.

Note: the trace backend perturbs the stdio console badly enough that the guest can lose
input bytes, so check the command's own output in the console log before trusting a count.
"""

import re
import sys


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    path = sys.argv[1]
    counts = {}
    total = 0
    first = None
    with open(path, "r", errors="replace") as f:
        for line in f:
            if "virtio_blk_handle_read" not in line:
                continue
            m = re.search(r"sector (\d+) nsectors (\d+)", line)
            if not m:
                continue
            if first is None:
                first = line.strip()
            n = int(m.group(2))
            counts[n] = counts.get(n, 0) + 1
            total += n
    print("requests=%d sectors=%d bytes=%d" % (sum(counts.values()), total, total * 512))
    print("nsectors histogram:", dict(sorted(counts.items())))
    if first:
        print("first:", first)
    return 0


if __name__ == "__main__":
    sys.exit(main())
