// The link's own test: the frames a gateway has to answer, built and checked byte by byte.
//
//   node tools/wasm-browser/net.test.js
//
// This is deliberately not a guest test. What the guest proves is that a reply arrived; what it
// cannot say is whether the reply is *well formed* — the guest's own stack parses it, so a checksum
// it accepted could still be one only it would accept. So the frames here are built here, the
// replies are validated the way a receiver validates them (recomputing the checksum over the data
// *including* the stored one, which is zero only when it was right), and the fields ARP and ICMP
// promise are read back.

import { createGateway, createWebSocketLink } from './net.js';

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}`);
  if (detail !== undefined && !ok) console.log(`        ${detail}`);
}

/// The internet checksum (RFC 1071), computed here rather than imported: a test that used the
/// gateway's own helper would agree with it about a bug.
function checksum(bytes, start, end) {
  let sum = 0;
  let i = start;
  for (; i + 1 < end; i += 2) sum += (bytes[i] << 8) | bytes[i + 1];
  if (i < end) sum += bytes[i] << 8;
  while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
  return ~sum & 0xffff;
}

/// A field's checksum is right when recomputing it over the data *including* the stored value gives
/// zero — the property a receiver relies on, and the only non-circular way to check one.
const validates = (bytes, start, end) => checksum(bytes, start, end) === 0;

const MAC_GUEST = [0x02, 0x00, 0x00, 0x00, 0x00, 0x15];
const MAC_GATEWAY = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
const BROADCAST = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
const IP_GUEST = [10, 0, 2, 15];
const IP_GATEWAY = [10, 0, 2, 2];

const hex = (bytes) => Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
const eq = (bytes, expected, off = 0) => hex(bytes.subarray(off, off + expected.length)) === hex(Uint8Array.from(expected));

function ether(dst, src, type, payload) {
  const frame = new Uint8Array(14 + payload.length);
  frame.set(dst, 0);
  frame.set(src, 6);
  frame[12] = type >> 8;
  frame[13] = type & 0xff;
  frame.set(payload, 14);
  return frame;
}

/// From the guest: "who has 10.0.2.2?"
function arpRequest() {
  const arp = new Uint8Array(28);
  arp[1] = 1;
  arp[2] = 0x08;
  arp[3] = 0x00;
  arp[4] = 6;
  arp[5] = 4;
  arp[7] = 1; // request
  arp.set(MAC_GUEST, 8);
  arp.set(IP_GUEST, 14);
  arp.set(BROADCAST, 18);
  arp.set(IP_GATEWAY, 24);
  return ether(BROADCAST, MAC_GUEST, 0x0806, arp);
}

/// From the guest: an ICMP echo request the way `ping` builds one — id and seq big-endian, and a
/// payload that has to come back unchanged.
function icmpRequest({ id = 0x1234, seq = 1, payload = [0x61, 0x62, 0x63, 0x64] } = {}) {
  const icmpLen = 8 + payload.length;
  const total = 20 + icmpLen;
  const packet = new Uint8Array(total);
  packet[0] = 0x45;
  packet[2] = total >> 8;
  packet[3] = total & 0xff;
  packet[8] = 64; // TTL
  packet[9] = 1; // ICMP
  packet.set(IP_GUEST, 12);
  packet.set(IP_GATEWAY, 16);
  packet[20] = 8; // echo request
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

// ------------------------------------------------------------------------ ARP

{
  const link = createGateway();
  const request = arpRequest();
  check(
    'the test builds the request ARP defines, which has no checksum to get wrong',
    (request[14] << 8 | request[15]) === 1 &&
      (request[16] << 8 | request[17]) === 0x0800 &&
      request[18] === 6 &&
      request[19] === 4 &&
      (request[20] << 8 | request[21]) === 1 &&
      eq(request, BROADCAST, 32) &&
      eq(request, IP_GATEWAY, 38),
    hex(request.subarray(14))
  );

  const into = new Uint8Array(2048);
  check('nothing is queued before the guest transmits', link.pending() === 0);
  link.send(request);
  const n = link.recv(into);
  check('an ARP request for the gateway is answered', n === 42, `n=${n}`);
  const reply = into.subarray(0, Math.max(n, 0));
  check(
    'the reply is addressed to the requester, from the gateway',
    eq(reply, MAC_GUEST, 0) && eq(reply, MAC_GATEWAY, 6) && reply[12] === 0x08 && reply[13] === 0x06,
    hex(reply.subarray(0, 14))
  );
  check(
    'and carries the gateway MAC and address for the address it was asked about',
    reply[21] === 2 && eq(reply, MAC_GATEWAY, 22) && eq(reply, IP_GATEWAY, 28) && eq(reply, MAC_GUEST, 32) && eq(reply, IP_GUEST, 38),
    hex(reply.subarray(14))
  );
  check('the queue is empty again', link.pending() === 0 && link.recv(into) === -1);
  check('and the link counted it', link.stats.arpRequests === 1 && link.stats.arpReplies === 1 && link.stats.unanswered === 0);
}

// ----------------------------------------------------------------------- ICMP

{
  const link = createGateway();
  const request = icmpRequest({ id: 0x2a2a, seq: 7, payload: [1, 2, 3, 4, 5, 6] });
  check(
    'the test builds an echo request a receiver would accept',
    validates(request, 14, 34) && validates(request, 34, 48),
    hex(request.subarray(14))
  );

  const into = new Uint8Array(2048);
  link.send(request);
  const n = link.recv(into);
  check('an ICMP echo request for the gateway is answered', n === 48, `n=${n}`);
  const reply = into.subarray(0, Math.max(n, 0));
  check(
    'the reply comes back to the requester, from the gateway',
    eq(reply, MAC_GUEST, 0) && eq(reply, MAC_GATEWAY, 6),
    hex(reply.subarray(0, 14))
  );
  const ip = 14;
  check(
    'with the addresses swapped, so the source is the gateway',
    eq(reply, IP_GATEWAY, ip + 12) && eq(reply, IP_GUEST, ip + 16),
    `${hex(reply.subarray(ip + 12, ip + 16))} -> ${hex(reply.subarray(ip + 16, ip + 20))}`
  );
  check('the IP header checksum is one a receiver verifies', validates(reply, ip, ip + 20), hex(reply.subarray(ip + 10, ip + 12)));
  check(
    'the message is an echo *reply*, and its checksum verifies too',
    reply[ip + 20] === 0 && validates(reply, ip + 20, ip + 20 + 34),
    `type=${reply[ip + 20]} csum=${hex(reply.subarray(ip + 22, ip + 24))}`
  );
  check(
    "ping's identity comes back, so the client can tell its own reply",
    reply[ip + 24] === 0x2a && reply[ip + 25] === 0x2a && reply[ip + 26] === 0 && reply[ip + 27] === 7,
    hex(reply.subarray(ip + 24, ip + 28))
  );
  check(
    'and the payload is the same bytes, not a fresh message',
    hex(reply.subarray(ip + 28, ip + 28 + 6)) === '010203040506',
    hex(reply.subarray(ip + 28, ip + 28 + 6))
  );
}

// ---------------------------------------------------------------- what it will not answer

{
  const link = createGateway();
  const into = new Uint8Array(2048);
  // An echo request for somebody else: a link with one peer has no route to invent.
  const elsewhere = icmpRequest();
  elsewhere[14 + 16] = 10;
  elsewhere[14 + 17] = 0;
  elsewhere[14 + 18] = 2;
  elsewhere[14 + 19] = 9;
  link.send(elsewhere);
  check('a frame for another host is not answered', link.pending() === 0 && link.stats.unanswered === 1);

  // A truncated frame, which is what a driver bug looks like from here.
  link.send(new Uint8Array([1, 2, 3]));
  check('and neither is one too short to be a frame', link.pending() === 0 && link.stats.unanswered === 2);
}

// ------------------------------------------------------------------ the WebSocket link

{
  // A stand-in socket, so this needs no server and no port: what is being checked is the queue and
  // the carrier, not the wire.
  const listeners = new Map();
  const sent = [];
  const socket = {
    readyState: 0,
    binaryType: 'arraybuffer',
    addEventListener: (type, fn) => listeners.set(type, fn),
    send: (bytes) => sent.push(bytes),
  };
  const link = createWebSocketLink({ url: 'ws://example.invalid/', socket });
  const into = new Uint8Array(2048);

  check('the guest is given a MAC, so it has a NIC rather than no device', hex(link.mac) === hex(Uint8Array.from(MAC_GUEST)));
  check('nothing has arrived', link.pending() === 0);
  check(
    'a transmit with no carrier is refused, not silently dropped',
    link.send(Uint8Array.from([1, 2, 3, 4])) === false && sent.length === 0,
    `sent=${sent.length}`
  );

  socket.readyState = 1;
  listeners.get('open')();
  check('a transmit once the socket is open goes out whole', link.send(Uint8Array.from([9, 8, 7])) === true && sent.length === 1);
  check('and frame boundaries are kept, which is what a frame is', hex(sent[0]) === '090807');

  listeners.get('message')({ data: Uint8Array.from([1, 2, 3]).buffer });
  listeners.get('message')({ data: Uint8Array.from([4, 5]).buffer });
  check('frames the relay sends are queued for the guest', link.pending() === 2);
  check('and the guest reads them one at a time, oldest first', link.recv(into) === 3 && hex(into.subarray(0, 3)) === '010203' && link.recv(into) === 2 && hex(into.subarray(0, 2)) === '0405');
  check('a read on an empty queue answers "nothing", not zero bytes', link.recv(into) === -1);
  check('and a frame too large for the caller is dropped rather than truncated', (() => {
    listeners.get('message')({ data: Uint8Array.from([1, 2, 3, 4]).buffer });
    return link.recv(new Uint8Array(2)) === -1 && link.pending() === 0;
  })());
  check('the link says what it did', link.stats.opened === 1 && link.stats.received === 3 && link.stats.sent === 1);
}

console.log(`\n${checks.filter((c) => c.ok).length}/${checks.length} checks passed`);
process.exit(checks.every((c) => c.ok) ? 0 : 1);
