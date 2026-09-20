// A static server for the page, so it can be opened without a build step or a network.
//
//   node tools/wasm-browser/serve.js
//
// then open http://127.0.0.1:8080/. A wasm module cannot be fetched from `file://` (the origin is
// opaque, so the fetch is refused), which is the only reason this exists — it serves the
// directory it lives in, and nothing else.
//
// It is also the answer to the one non-obvious way this page can fail to start: a *module script*
// is fetched under strict MIME checking, so a server that serves `page.js` as `text/plain` gets
// its request refused before a line of the page runs — `Expected a JavaScript-or-Wasm module
// script but the server responded with a MIME type of "text/plain"`. The modules are `.js` rather
// than `.mjs` for that reason: `.js` is the one extension essentially every static server maps to
// a JavaScript type, and `package.json` next to them makes Node agree that the contents are ES
// modules. Any server may be used; this one just cannot get it wrong.
//
// Bound to the loopback address on purpose: this serves whatever is in this directory, and it has
// no business being reachable from anywhere else.

import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const port = Number(process.env.PORT ?? 8080);

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  // Tolerated rather than used: an `.mjs` here would be a leftover, and serving it as JavaScript
  // is what keeps it from being the MIME-type error this server exists to avoid.
  '.mjs': 'text/javascript; charset=utf-8',
  '.wasm': 'application/wasm',
  '.img': 'application/octet-stream',
  '.md': 'text/plain; charset=utf-8',
};

const server = http.createServer((request, response) => {
  const url = new URL(request.url, `http://${request.headers.host}`);
  const wanted = url.pathname === '/' ? '/index.html' : url.pathname;
  const file = path.join(here, path.normalize(wanted).replace(/^([/\\])+/, ''));

  // Everything served comes from this directory: a request that resolves outside it is refused
  // rather than followed.
  if (!file.startsWith(here + path.sep)) {
    response.writeHead(403).end('outside the served directory');
    return;
  }

  fs.readFile(file, (error, body) => {
    if (error) {
      response.writeHead(404, { 'content-type': 'text/plain' }).end(`not found: ${wanted}`);
      return;
    }
    response.writeHead(200, {
      'content-type': TYPES[path.extname(file)] ?? 'application/octet-stream',
      'cache-control': 'no-store',
    });
    response.end(body);
  });
});

server.listen(port, '127.0.0.1', () => {
  // The bound port rather than the requested one, so `PORT=0` (let the OS choose) works and the
  // caller can still find out where to connect.
  console.log(`serving ${here}`);
  console.log(`open http://127.0.0.1:${server.address().port}/`);
});
