// The relay's test: the page's own WebSocket link, on a real port, with real frames (M6).
//
//   node tools/wasm-net/relay.test.js
//
// `tools/wasm-browser/net.test.js` checks the gateway's replies byte by byte and the link's queue with
// a stand-in socket. This is the half neither can reach: RFC 6455's handshake and framing on an
// actual socket, driven by the WebSocket *client Node ships* — an implementation that is not the one
// under test, which is the only way a codec check means anything. The frames are the two the gateway
// answers, built here rather than imported: what this file is about is the transport, and a shared
// builder would make it about the builder too.

import { createWebSocketLink } from '../wasm-browser/net.js';
import { startRelay } from './relay.js';

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}`);
  if (detail !== undefined && !ok) console.log(`        ${detail}`);
}

const MAC_GUEST = Uint8Array.from([0x02, 0x00, 0x00, 0x00, 0x00, 0x15]);
const MAC_GATEWAY = Uint8Array.from([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
const BROADCAST = Uint8Array.from([0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
const IP_GUEST = Uint8Array.from([10, 0, 2, 15]);
const IP_GATEWAY = Uint8Array.from([10, 0, 2, 2]);

const hex = (bytes) => Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
const eq = (bytes, expected, off = 0) =>
  hex(bytes.subarray(off, off + expected.length)) === hex(Uint8Array.from(expected));

/// The internet checksum, so the requests this file sends are ones a receiver would accept — and so
/// the replies can be checked the way a receiver checks them.
function checksum(bytes, start, end) {
  let sum = 0;
  let i = start;
  for (; i + 1 < end; i += 2) sum += (bytes[i] << 8) | bytes[i + 1];
  if (i < end) sum += bytes[i] << 8;
  while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
  return ~sum & 0xffff;
}

const ether = (dst, src, type, payload) => {
  const frame = new Uint8Array(14 + payload.length);
  frame.set(dst, 0);
  frame.set(src, 6);
  frame[12] = type >> 8;
  frame[13] = type & 0xff;
  frame.set(payload, 14);
  return frame;
};

function arpRequest() {
  const arp = new Uint8Array(28);
  arp[1] = 1;
  arp[2] = 0x08;
  arp[4] = 6;
  arp[5] = 4;
  arp[7] = 1;
  arp.set(MAC_GUEST, 8);
  arp.set(IP_GUEST, 14);
  arp.set(BROADCAST, 18);
  arp.set(IP_GATEWAY, 24);
  return ether(BROADCAST, MAC_GUEST, 0x0806, arp);
}

function icmpRequest({ id = 0x5150, seq = 1, payload = [0x61, 0x62] } = {}) {
  const icmpLen = 8 + payload.length;
  const total = 20 + icmpLen;
  const packet = new Uint8Array(total);
  packet[0] = 0x45;
  packet[2] = total >> 8;
  packet[3] = total & 0xff;
  packet[8] = 64;
  packet[9] = 1;
  packet.set(IP_GUEST, 12);
  packet.set(IP_GATEWAY, 16);
  packet[20] = 8;
  packet[24] = id >> 8;
  packet[25] = id & 0xff;
  packet[26] = seq >> 8;
  packet[27] = seq & 0xff;
  packet.set(payload, 28);
  const ipSum = checksum(packet, 0, 20);
  packet[10] = ipSum >> 8;
  packet[11] = ipSum & 0xff;
  const icmpSum = checksum(packet, 20, total);
  packet[22] = icmpSum >> 8;
  packet[23] = icmpSum & 0xff;
  return ether(MAC_GATEWAY, MAC_GUEST, 0x0800, packet);
}

/// Wait for something only a socket can deliver: a queue fed by a real transport is not ready when
/// it is asked, it becomes ready.
async function until(predicate, what, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return true;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  console.log(`        timed out waiting for ${what}`);
  return false;
}

const relay = await startRelay({ port: 0, quiet: true });
const link = createWebSocketLink({ url: relay.url });
const into = new Uint8Array(2048);

check(
  'the link opens a socket to the relay, and the relay accepts it',
  await until(() => link.stats.opened === 1 && relay.stats.connections === 1, 'the connection'),
  `link=${JSON.stringify(link.stats)} relay=${JSON.stringify(relay.stats)}`
);

// An ARP request, over the socket: the same frame `net.test.js` posts into the link's own queue.
check('a frame goes out once the socket is open', link.send(arpRequest()) === true);
check(
  'and the ARP reply comes back over the socket, addressed to this guest',
  await until(() => link.pending() === 1, 'the ARP reply'),
  `pending=${link.pending()} relay=${JSON.stringify(relay.stats)}`
);
const arpReply = new Uint8Array(2048);
const arpLength = link.recv(arpReply);
check(
  'the reply survived the transport, byte for byte the one the gateway builds',
  arpLength === 42 &&
    eq(arpReply, MAC_GUEST, 0) &&
    eq(arpReply, MAC_GATEWAY, 6) &&
    (arpReply[20] << 8 | arpReply[21]) === 2 &&
    eq(arpReply, MAC_GATEWAY, 22) &&
    eq(arpReply, IP_GATEWAY, 28) &&
    eq(arpReply, IP_GUEST, 38),
  `n=${arpLength} ${hex(arpReply.subarray(0, Math.max(arpLength, 0)))}`
);

// And an ICMP echo, which is the one that proves the *payload* travelled: the checksums are checked
// the way a receiver checks them, over the data including the stored value.
check('an ICMP echo request goes out', link.send(icmpRequest({ id: 0x5150 })) === true);
check(
  'and its reply comes back, with both checksums intact through the transport',
  await until(() => link.pending() === 1, 'the ICMP reply'),
  `pending=${link.pending()}`
);
const icmpReply = new Uint8Array(2048);
const icmpLength = link.recv(icmpReply);
check(
  'the reply is an echo reply, addressed back to this guest, and verifies',
  icmpLength === 44 &&
    eq(icmpReply, MAC_GUEST, 0) &&
    eq(icmpReply, MAC_GATEWAY, 6) &&
    eq(icmpReply, IP_GATEWAY, 26) &&
    eq(icmpReply, IP_GUEST, 30) &&
    icmpReply[34] === 0 &&
    checksum(icmpReply, 14, 34) === 0 &&
    checksum(icmpReply, 34, 44) === 0 &&
    icmpReply[38] === 0x51 &&
    icmpReply[39] === 0x50 &&
    hex(icmpReply.subarray(42, 44)) === '6162',
  `n=${icmpLength} ${hex(icmpReply.subarray(0, Math.max(icmpLength, 0)))}`
);

check(
  'the relay accounted for every frame, and refused none',
  relay.stats.framesIn === 2 &&
    relay.stats.framesOut === 2 &&
    relay.stats.refused === 0 &&
    link.stats.sent === 2 &&
    link.stats.received === 2,
  `relay=${JSON.stringify(relay.stats)} link=${JSON.stringify(link.stats)}`
);

// A second connection is a second guest: the relay builds a gateway per socket, so two tabs on one
// relay cannot see each other's frames. What says so is a reply *not* arriving — the first link's ARP
// reply must be queued for the first link only, which a shared gateway would not manage.
const second = createWebSocketLink({ url: relay.url });
check(
  'a second connection is a guest of its own: a reply to one is not queued for the other',
  await (async () => {
    const opened = await until(
      () => second.stats.opened === 1 && relay.stats.connections === 2,
      'the second connection'
    );
    if (!opened) return false;
    link.send(arpRequest());
    if (!(await until(() => link.pending() === 1, 'the first link\'s reply'))) return false;
    const reply = new Uint8Array(2048);
    const n = link.recv(reply);
    return n === 42 && eq(reply, MAC_GATEWAY, 22) && second.pending() === 0;
  })(),
  `first=${link.pending()} second=${second.pending()} relay=${JSON.stringify(relay.stats)}`
);

await relay.close();
check(
  'the relay closes its listener and its sockets',
  await until(() => relay.stats.connections === 2, 'the tally to stand'),
  JSON.stringify(relay.stats)
);

console.log(`\n${checks.filter((c) => c.ok).length}/${checks.length} checks passed`);
process.exit(checks.every((c) => c.ok) ? 0 : 1);
