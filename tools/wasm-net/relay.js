// The relay: a WebSocket server that runs the same gateway the page can run in-process (M6).
//
//   node tools/wasm-net/relay.js [port]        # default 8787
//
// then open the demo with `?net=ws://127.0.0.1:8787/`. A browser cannot open a raw socket, so the
// guest's NIC needs *something* at the other end of a WebSocket — this is that something, and what it
// does with a frame is what `net.js`'s gateway does in a page: ARP for its own address, an ICMP echo
// reply, and nothing else invented. One implementation of the wire's behaviour, two places it can
// run, which is the point of the link being a seam rather than a transport.
//
// The server is hand-rolled over `node:http`'s `upgrade` event: RFC 6455's handshake, binary frames
// each way, no extensions, no fragmentation, no compression. Node ships a WebSocket *client* and no
// server, and the alternative was a dependency in a project whose point is that it builds its own.
// The framing is small enough to be worth that — and `relay.test.js` drives this with the client Node
// does ship, so the codec is checked against an implementation that is not this one.

import crypto from 'node:crypto';
import http from 'node:http';
import { pathToFileURL } from 'node:url';

import { createGateway } from '../wasm-browser/net.js';

/// RFC 6455's handshake magic, and the four opcodes this relay understands.
const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';
const OPCODE_BINARY = 0x2;
const OPCODE_CLOSE = 0x8;
const OPCODE_PING = 0x9;
const OPCODE_PONG = 0xa;

/// The upgrade answer: `Sec-WebSocket-Accept` is the client's key hashed with the magic string, which
/// is what makes a WebSocket connection more than an HTTP request that never ends.
const acceptKey = (key) => crypto.createHash('sha1').update(key + WS_GUID).digest('base64');

/// One frame from the server, which never masks (RFC 6455 §5.1 — a client masks, a server does not).
function sendFrame(socket, opcode, payload) {
  const body = Buffer.from(payload);
  const header = [0x80 | opcode];
  if (body.length < 126) {
    header.push(body.length);
  } else if (body.length < 65536) {
    header.push(126, body.length >> 8, body.length & 0xff);
  } else {
    const length = Buffer.alloc(8);
    length.writeBigUInt64BE(BigInt(body.length));
    header.push(127, ...length);
  }
  socket.write(Buffer.concat([Buffer.from(header), body]));
}

/// Start the relay. `port` 0 asks the OS for one, which is what `relay.test.js` does.
export function startRelay({ port = 8787, host = '127.0.0.1', quiet = false } = {}) {
  const stats = { connections: 0, framesIn: 0, framesOut: 0, bytesIn: 0, bytesOut: 0, refused: 0 };
  const sockets = new Set();

  const server = http.createServer((_req, res) => {
    // Anything that is not an upgrade is a browser's probe or a reader's curiosity; say what this is
    // rather than leaving the request hanging.
    res.writeHead(426, { 'content-type': 'text/plain' });
    res.end('the minixrs WebSocket relay (M6): connect with ?net=ws://host:port/\n');
  });

  server.on('upgrade', (req, socket) => {
    const key = req.headers['sec-websocket-key'];
    if (typeof key !== 'string') {
      stats.refused += 1;
      socket.destroy();
      return;
    }
    socket.write(
      'HTTP/1.1 101 Switching Protocols\r\n' +
        'Upgrade: websocket\r\n' +
        'Connection: Upgrade\r\n' +
        `Sec-WebSocket-Accept: ${acceptKey(key)}\r\n\r\n`
    );
    socket.setNoDelay(true);
    stats.connections += 1;
    sockets.add(socket);

    // One gateway per connection: two tabs are two guests on one relay, and neither can see the
    // other's frames. A wire is stateful in exactly that way — what it knows, it learned from the
    // frames on it.
    const gateway = createGateway();
    const into = new Uint8Array(2048);

    const drain = () => {
      for (;;) {
        const n = gateway.recv(into);
        if (n < 0) break;
        sendFrame(socket, OPCODE_BINARY, into.subarray(0, n));
        stats.framesOut += 1;
        stats.bytesOut += n;
      }
    };

    let buffer = Buffer.alloc(0);
    socket.on('data', (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      for (;;) {
        if (buffer.length < 2) return;
        const opcode = buffer[0] & 0x0f;
        const fin = (buffer[0] & 0x80) !== 0;
        let length = buffer[1] & 0x7f;
        let offset = 2;
        if (length === 126) {
          if (buffer.length < 4) return;
          length = buffer.readUInt16BE(2);
          offset = 4;
        } else if (length === 127) {
          if (buffer.length < 10) return;
          length = Number(buffer.readBigUInt64BE(2));
          offset = 10;
        }
        let mask = null;
        if ((buffer[1] & 0x80) !== 0) {
          if (buffer.length < offset + 4) return;
          mask = buffer.subarray(offset, offset + 4);
          offset += 4;
        }
        if (buffer.length < offset + length) return;
        const payload = Buffer.from(buffer.subarray(offset, offset + length));
        buffer = buffer.subarray(offset + length);
        if (mask !== null) {
          // A client's frames are masked; the key is the same four bytes applied in a cycle, which is
          // also how this is undone. (A server's own frames are not masked, so this is the only
          // unmasking the relay does.)
          for (let i = 0; i < payload.length; i += 1) payload[i] ^= mask[i % 4];
        }

        if (opcode === OPCODE_CLOSE) {
          socket.end();
          return;
        }
        if (opcode === OPCODE_PING) {
          sendFrame(socket, OPCODE_PONG, payload);
          continue;
        }
        // A frame the guest would have to reassemble is refused rather than guessed at: this relay
        // carries Ethernet frames, and each one is far below the length that needs splitting.
        if (opcode !== OPCODE_BINARY || !fin) {
          stats.refused += 1;
          continue;
        }
        stats.framesIn += 1;
        stats.bytesIn += payload.length;
        gateway.send(payload);
        drain();
      }
    });

    const done = () => sockets.delete(socket);
    socket.on('close', done);
    socket.on('error', done);
  });

  return new Promise((resolve) => {
    server.listen(port, host, () => {
      const address = server.address();
      const url = `ws://${host}:${address.port}/`;
      if (!quiet) {
        console.log(`minixrs relay listening on ${url}`);
        console.log(`open the demo with  ?net=${url}`);
        console.log('(it answers ARP and ICMP echo for 10.0.2.2; ^C to stop)');
      }
      resolve({
        url,
        port: address.port,
        stats,
        /// Close the listener and every live connection, so a caller can end the test.
        close() {
          for (const socket of sockets) socket.destroy();
          sockets.clear();
          return new Promise((done) => server.close(done));
        },
      });
    });
  });
}

// Run as a program when invoked directly (`node tools/wasm-net/relay.js [port]`).
if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const relay = await startRelay({ port: Number(process.argv[2] ?? 8787) });
  const report = () => {
    const s = relay.stats;
    console.log(
      `  ${s.connections} connection(s), ${s.framesIn} frame(s) in (${s.bytesIn} bytes), ` +
        `${s.framesOut} out (${s.bytesOut} bytes), ${s.refused} refused`
    );
  };
  process.on('SIGINT', () => {
    report();
    relay.close().then(() => process.exit(0));
  });
  // A heartbeat, so a reader watching the terminal sees the count move as they ping from the page.
  setInterval(report, 15000).unref();
}
