"""Compute the UDP checksum for the exact packet the guest sends (reference check)."""
import struct


def csum(data):
    if len(data) % 2:
        data += b"\x00"
    s = 0
    for i in range(0, len(data), 2):
        s += (data[i] << 8) | data[i + 1]
    while s >> 16:
        s = (s & 0xFFFF) + (s >> 16)
    return (~s) & 0xFFFF


def dns_query(qid, name):
    out = struct.pack(">HHHHHH", qid, 0x0100, 1, 0, 0, 0)
    for label in name.split("."):
        out += bytes([len(label)]) + label.encode()
    out += b"\x00" + struct.pack(">HH", 1, 1)
    return out


src_ip = bytes([10, 0, 2, 15])
dst_ip = bytes([10, 0, 2, 3])
src_port = 32768  # slot 0 ephemeral
dst_port = 53
payload = dns_query(0x000C, "example.com")
udp_len = 8 + len(payload)

udp_hdr = struct.pack(">HHHH", src_port, dst_port, udp_len, 0)
print("payload len:", len(payload), "udp_len:", udp_len)

# Pseudo-header + UDP header + payload (checksum field zero).
pseudo = src_ip + dst_ip + bytes([0, 17]) + struct.pack(">H", udp_len)
chk = csum(pseudo + udp_hdr + payload)
print(f"udp checksum: 0x{chk:04x}")

# Also build the full datagram with IP header to eyeball.
total = 20 + udp_len
ip = struct.pack(">BBHHHBBH4s4s", 0x45, 0, total, 0x1234, 0, 64, 17, 0, src_ip, dst_ip)
ip = ip[:10] + struct.pack(">H", csum(ip)) + ip[12:]
pkt = ip + udp_hdr + payload
print("full packet bytes:", pkt.hex())
