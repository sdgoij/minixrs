// The network link — what a frame the guest transmits is put on, and where arriving ones come from
// (M6).
//
// A browser has no raw sockets, so the guest's NIC cannot reach a real network the way it does under
// QEMU. What it *can* reach is a link: something that holds the guest's frames and answers them.
// `ARCH_WASM32.md` §9 names two, and the guest above this boundary cannot tell them apart —
// `virtio_net`'s wasm transport calls four imports, the DL protocol is untouched, and which link is
// behind those imports is the front end's choice:
//
//   * `createGateway` — a model of the gateway the port already targets. The arches run under QEMU's
//     SLIRP, where the guest is 10.0.2.15 and 10.0.2.2 answers ARP and ICMP; this answers the same
//     two, in-process, so the guest's own stack has a real peer with no server and no network. That
//     is what makes the published demo able to `ping 10.0.2.2` in a tab.
//   * `createWebSocketLink` — the same frames tunnelled to a relay at the other end of a WebSocket
//     (`tools/wasm-net/relay.js`), which runs *this same gateway* on its side. Real connectivity
//     when a relay is running, and no change above the link either way.
//
// The gateway answers only what the port's own client asks of a peer: ARP for its address, and an
// ICMP echo. Anything else is counted and dropped rather than half-answered, because a link that
// invented a reply would be a bug the guest could not see.

/// A 6-byte Ethernet address.
const macOf = (bytes) => Uint8Array.from(bytes);

/// The MAC the guest's own NIC reports. The top two bits are locally administered and unicast, the
/// convention every virtual NIC follows, and the last octet is the guest's — so a packet capture of
/// the demo reads like a capture of the arch it mirrors.
const DEFAULT_GUEST_MAC = macOf([0x02, 0x00, 0x00, 0x00, 0x00, 0x15]);

/// The gateway's own MAC. Distinct from the guest's and from the broadcast address, which is the
/// only thing ARP needs of it.
const DEFAULT_GATEWAY_MAC = macOf([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);

const ETH_ARP = 0x0806;
const ETH_IPV4 = 0x0800;
const IP_ICMP = 1;
const ICMP_ECHO_REQUEST = 8;
const ICMP_ECHO_REPLY = 0;

/// The internet checksum over `bytes[start..end)` (RFC 1071), already complemented.
///
/// Both checksums the gateway has to recompute are this one: the IPv4 header's, over 20 bytes, and
/// ICMP's, over the message.
function inetChecksum(bytes, start, end) {
  let sum = 0;
  let i = start;
  for (; i + 1 < end; i += 2) sum += (bytes[i] << 8) | bytes[i + 1];
  if (i < end) sum += bytes[i] << 8;
  while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
  return ~sum & 0xffff;
}

const sameIp = (bytes, off, ip) =>
  bytes[off] === ip[0] && bytes[off + 1] === ip[1] && bytes[off + 2] === ip[2] && bytes[off + 3] === ip[3];

/// One Ethernet frame around `payload`.
function etherFrame(dst, src, ethertype, payload) {
  const frame = new Uint8Array(14 + payload.length);
  frame.set(dst, 0);
  frame.set(src, 6);
  frame[12] = ethertype >> 8;
  frame[13] = ethertype & 0xff;
  frame.set(payload, 14);
  return frame;
}

/// A link that answers the guest the way the gateway it already targets does.
///
/// `mac` is what the guest's NIC reports for itself — the driver asks the link, because on an arch
/// with a bus that answer comes from the device config and a link has no config to read. The
/// gateway's own address and the guest's are the two the port's checks use.
export function createGateway({
  mac = DEFAULT_GUEST_MAC,
  gatewayMac = DEFAULT_GATEWAY_MAC,
  gatewayIp = Uint8Array.from([10, 0, 2, 2]),
  guestIp = Uint8Array.from([10, 0, 2, 15]),
} = {}) {
  const outbox = [];
  const stats = {
    frames: 0,
    arpRequests: 0,
    arpReplies: 0,
    icmpRequests: 0,
    icmpReplies: 0,
    /// Frames addressed to something this link does not answer: another host, or a protocol with no
    /// peer here. Counted rather than ignored, because "the guest sent something nobody answered" is
    /// the first thing a network that does not work looks like.
    unanswered: 0,
  };

  /// An ARP request for `gatewayIp` becomes the reply the requester needs: its MAC, its address.
  function answerArp(frame) {
    const arp = 14;
    if (frame.length < arp + 28) return false;
    // htype, ptype, hlen, plen, oper — Ethernet/IPv4 with the operation ARP_REQUEST.
    const oper = (frame[arp + 6] << 8) | frame[arp + 7];
    if (frame[arp] !== 0 || frame[arp + 1] !== 1) return false;
    if (frame[arp + 2] !== ETH_IPV4 >> 8 || frame[arp + 3] !== (ETH_IPV4 & 0xff)) return false;
    if (frame[arp + 4] !== 6 || frame[arp + 5] !== 4) return false;
    if (oper !== 1) return false;
    const senderMac = frame.subarray(arp + 8, arp + 14);
    const senderIp = frame.subarray(arp + 14, arp + 18);
    const targetIp = frame.subarray(arp + 24, arp + 28);
    if (!sameIp(targetIp, 0, gatewayIp)) return false;

    stats.arpRequests += 1;
    const reply = new Uint8Array(28);
    reply[0] = 0;
    reply[1] = 1; // Ethernet
    reply[2] = ETH_IPV4 >> 8;
    reply[3] = ETH_IPV4 & 0xff;
    reply[4] = 6;
    reply[5] = 4;
    reply[6] = 0;
    reply[7] = 2; // ARP_REPLY
    reply.set(gatewayMac, 8);
    reply.set(gatewayIp, 14);
    reply.set(senderMac, 18);
    reply.set(senderIp, 24);
    outbox.push(etherFrame(senderMac, gatewayMac, ETH_ARP, reply));
    stats.arpReplies += 1;
    return true;
  }

  /// An ICMP echo request becomes an echo reply: the same message with the addresses swapped and the
  /// two checksums recomputed, which is what makes it a reply rather than a copy.
  function answerIcmp(frame) {
    const ip = 14;
    if (frame.length < ip + 20 + 8) return false;
    // IPv4, 20-byte header (IHL 5), no fragmentation, and ICMP. A request with options or a
    // fragment offset is not something the port's client sends, so there is nothing to answer.
    if (frame[ip] >> 4 !== 4 || (frame[ip] & 0x0f) !== 5) return false;
    if (frame[ip + 9] !== IP_ICMP) return false;
    if (!sameIp(frame, ip + 16, gatewayIp)) return false;
    const icmp = ip + 20;
    if (frame[icmp] !== ICMP_ECHO_REQUEST) return false;

    stats.icmpRequests += 1;
    const total = (frame[ip + 2] << 8) | frame[ip + 3];
    const icmpLen = total - 20;
    if (icmpLen < 8 || icmp + icmpLen > frame.length) return false;

    const reply = Uint8Array.from(frame.subarray(ip, ip + total));
    // Both addresses move: the reply's source is the request's destination (this gateway) and its
    // destination is the request's source (whoever asked). Leaving the first alone is not enough —
    // it is the *guest's* address in the request, so a reply that only moved the destination would
    // be addressed from the guest to the guest, which a stack accepts and a reader does not.
    reply.set(frame.subarray(ip + 16, ip + 20), 12);
    reply.set(frame.subarray(ip + 12, ip + 16), 16);
    // The IPv4 header checksum covers the header and nothing else, so it is redone over the address
    // that changed. TTL is left alone: a reply from a host on the same link is not forwarded.
    reply[10] = 0;
    reply[11] = 0;
    const ipSum = inetChecksum(reply, 0, 20);
    reply[10] = ipSum >> 8;
    reply[11] = ipSum & 0xff;
    // The ICMP message: type 0, and the checksum over the whole message including the payload the
    // request carried — ping's identity is its own bytes coming back.
    const icmpOff = 20;
    reply[icmpOff] = ICMP_ECHO_REPLY;
    reply[icmpOff + 2] = 0;
    reply[icmpOff + 3] = 0;
    const icmpSum = inetChecksum(reply, icmpOff, total);
    reply[icmpOff + 2] = icmpSum >> 8;
    reply[icmpOff + 3] = icmpSum & 0xff;

    outbox.push(etherFrame(frame.subarray(6, 12), gatewayMac, ETH_IPV4, reply));
    stats.icmpReplies += 1;
    return true;
  }

  return {
    /// What the guest's NIC reports as its own address.
    mac,
    /// Frames waiting for the guest.
    pending: () => outbox.length,
    /// The next frame for the guest at the front of `into`; -1 when there is none.
    recv(into) {
      const frame = outbox.shift();
      if (frame === undefined) return -1;
      if (frame.length > into.length) return -1;
      into.set(frame);
      return frame.length;
    },
    /// Put one frame from the guest on the link.
    send(frame) {
      stats.frames += 1;
      const ethertype = frame.length >= 14 ? (frame[12] << 8) | frame[13] : 0;
      const answered =
        ethertype === ETH_ARP ? answerArp(frame) : ethertype === ETH_IPV4 ? answerIcmp(frame) : false;
      if (!answered) stats.unanswered += 1;
      return true;
    },
    stats,
  };
}

/// The same guest, on a wire that leaves the page: frames are tunnelled to a relay over a WebSocket,
/// and the relay is what answers them (it runs this file's `createGateway` on its side).
///
/// The link is *asynchronous* and the guest is not: `virtio_net_probe` asks for a MAC once and the
/// DL client polls for frames, so what this link has to be is a queue that fills when the socket
/// says something. A socket that is not open yet is a queue that never fills, and a send on one is
/// refused — which is a NIC that exists but has no carrier, not a NIC that is not there.
export function createWebSocketLink({ url, mac = DEFAULT_GUEST_MAC, socket = null } = {}) {
  const inbound = [];
  const stats = { frames: 0, sent: 0, received: 0, dropped: 0, opened: 0, closed: 0 };
  const ws = socket ?? new WebSocket(url);
  ws.binaryType = 'arraybuffer';

  ws.addEventListener('open', () => {
    stats.opened += 1;
  });
  ws.addEventListener('message', (event) => {
    const frame = new Uint8Array(event.data);
    if (inbound.length >= 256) {
      // A page that stopped reading must not grow without bound; the guest is told nothing, and the
      // count is what says so. 256 is a batch no NIC would hold either.
      stats.dropped += 1;
      return;
    }
    inbound.push(frame);
    stats.received += 1;
  });
  ws.addEventListener('close', () => {
    stats.closed += 1;
  });

  const isOpen = () => ws.readyState === 1; // WebSocket.OPEN

  return {
    mac,
    pending: () => inbound.length,
    recv(into) {
      const frame = inbound.shift();
      if (frame === undefined) return -1;
      if (frame.length > into.length) return -1;
      into.set(frame);
      return frame.length;
    },
    send(frame) {
      stats.frames += 1;
      if (!isOpen()) return false;
      ws.send(frame.slice());
      stats.sent += 1;
      return true;
    },
    stats,
  };
}
