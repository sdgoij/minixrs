#!/usr/bin/env python3
"""Decode target/netdump.pcap (ethernet frames on the SLIRP netdev) and print
the TCP exchanges as {src:port -> dst:port} seq/ack/flags/payload-len."""
import struct
import sys

path = sys.argv[1] if len(sys.argv) > 1 else "target/netdump.pcap"
data = open(path, "rb").read()

# Global header: magic(4) ... network=1 (ethernet)
assert data[:4] == b"\xd4\xc3\xb2\xa1" or data[:4] == b"\xa1\xb2\xc3\xd4", data[:4]
LE = data[:4] == b"\xd4\xc3\xb2\xa1"
off = 24
count = 0
while off + 16 <= len(data):
    if LE:
        ts, _, caplen, _ = struct.unpack_from("<IIII", data, off)
    else:
        ts, _, caplen, _ = struct.unpack_from(">IIII", data, off)
    off += 16
    frame = data[off : off + caplen]
    off += caplen
    count += 1
    if len(frame) < 14:
        continue
    eth = struct.unpack_from("!6s6sH", frame, 0)
    if eth[2] != 0x0800 or len(frame) < 34:
        continue
    ip = frame[14:]
    proto = ip[9]
    if proto != 6 or len(ip) < 40:
        continue
    ihl = (ip[0] & 0xF) * 4
    tcp = ip[ihl:]
    if len(tcp) < 20:
        continue
    src_ip = ".".join(str(b) for b in ip[12:16])
    dst_ip = ".".join(str(b) for b in ip[16:20])
    sp, dp = struct.unpack_from("!HH", tcp, 0)
    seq, ack = struct.unpack_from("!II", tcp, 4)
    off_bits = tcp[12] >> 4
    flags = tcp[13]
    names = []
    if flags & 0x02:
        names.append("SYN")
    if flags & 0x01:
        names.append("FIN")
    if flags & 0x04:
        names.append("RST")
    if flags & 0x10:
        names.append("ACK")
    if flags & 0x08:
        names.append("PSH")
    pay = len(tcp) - off_bits * 4
    f = "|".join(names) if names else "-"
    print(
        f"{src_ip}:{sp} -> {dst_ip}:{dp} seq={seq} ack={ack} "
        f"[{f}] pay={pay} t={ts}"
    )
print(f"--- {count} frames total ---")
