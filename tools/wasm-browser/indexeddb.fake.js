// A stand-in for the browser's IndexedDB, so the page's disk can be run under Node.
//
// `page.test.js` drives `page.js` with a stub DOM; the disk needs a stub database for the
// same reason. Only what `store.js` uses is implemented — `open` with an upgrade, object
// stores, transactions, `get`/`put`/`getAll`/`getAllKeys`, and `deleteDatabase` — and two real
// behaviours are load-bearing here:
//
//   * **Requests answer on a later task.** A handler is attached after the method
//     returns, so a fake that answered in place would leave every handler unset and
//     every await hanging.
//   * **`getAll` and `getAllKeys` visit the store in key order**, which is what lets the
//     store zip a page's index to its bytes.
//
// What it is not: transactional, or able to fail. Everything shares the database's maps,
// so nothing rolls back and no write can be rejected — enough for a store whose writes
// are one `put` per page, and not enough to exercise `store.js`'s error reporting. A
// database also cannot be *blocked* from being deleted here, since nothing is holding a
// connection open.

/// An IndexedDB whose databases live in memory, for one process.
export function fakeIndexedDB() {
  const databases = new Map();
  return {
    open(name, version = 1) {
      const req = new FakeRequest();
      later(() => {
        let db = databases.get(name);
        if (db === undefined) {
          db = new FakeDatabase(name, version);
          databases.set(name, db);
          req.result = db;
          if (typeof req.onupgradeneeded === 'function') req.onupgradeneeded(evt(req));
        }
        req.result = db;
        if (typeof req.onsuccess === 'function') req.onsuccess(evt(req));
      });
      return req;
    },
    /// The databases this factory holds, so a test can look at the records themselves.
    databases,
    /// Forget a database, so the next `open` starts it empty. Answered on a later task, as every
    /// other request is.
    deleteDatabase(name) {
      const req = new FakeRequest();
      later(() => {
        databases.delete(name);
        if (typeof req.onsuccess === 'function') req.onsuccess(evt(req));
      });
      return req;
    },
  };
}

class FakeDatabase {
  constructor(name, version) {
    this.name = name;
    this.version = version;
    this.tables = new Map();
    this.objectStoreNames = {
      contains: (store) => this.tables.has(store),
      [Symbol.iterator]: () => this.tables.keys(),
    };
  }

  createObjectStore(name) {
    if (!this.tables.has(name)) this.tables.set(name, new Map());
    return { name };
  }

  /// Nothing to close: the fake keeps no bookkeeping of open connections, which is also why
  /// `deleteDatabase` cannot be *blocked* here the way a real one can.
  close() {}

  transaction(stores) {
    return new FakeTransaction(this, Array.isArray(stores) ? stores : [stores]);
  }
}

class FakeTransaction {
  constructor(db, names) {
    this.db = db;
    this.names = names;
    this.error = null;
    this.oncomplete = null;
    this.onabort = null;
    this.pending = 0;
    this.finished = false;
  }

  objectStore(name) {
    const table = this.db.tables.get(name);
    if (table === undefined) throw new Error(`no object store named ${name}`);
    return new FakeObjectStore(table, this);
  }

  /// Answer one request on a later task, and complete the transaction once the last
  /// request it created has.
  enqueue(work) {
    const req = new FakeRequest();
    this.pending += 1;
    later(() => {
      req.result = work();
      if (typeof req.onsuccess === 'function') req.onsuccess(evt(req));
      this.pending -= 1;
      if (this.pending === 0 && !this.finished) {
        this.finished = true;
        if (typeof this.oncomplete === 'function') this.oncomplete(evt(this));
      }
    });
    return req;
  }
}

class FakeObjectStore {
  constructor(table, tx) {
    this.table = table;
    this.tx = tx;
  }

  get(key) {
    return this.tx.enqueue(() => this.table.get(key));
  }

  getAll() {
    return this.tx.enqueue(() => this.#sorted().map(([, value]) => value));
  }

  getAllKeys() {
    return this.tx.enqueue(() => this.#sorted().map(([key]) => key));
  }

  put(value, key) {
    return this.tx.enqueue(() => {
      // Structured-cloned, as the browser clones: the store keeps what the bytes were at
      // this moment, whatever the caller does with its array afterwards.
      this.table.set(key, structuredClone(value));
      return key;
    });
  }

  delete(key) {
    return this.tx.enqueue(() => this.table.delete(key));
  }

  /// Entries by key, which is the order a real object store enumerates in.
  #sorted() {
    return [...this.table.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  }
}

class FakeRequest {
  constructor() {
    this.result = undefined;
    this.error = null;
    this.onsuccess = null;
    this.onerror = null;
    this.onupgradeneeded = null;
    this.onblocked = null;
  }
}

/// The event a handler is given. Nothing in `store.js` reads it — it closes over the
/// request — but a real handler would, so the shape is there.
const evt = (target) => ({ target });

/// Run `fn` on a later task, the way the browser runs a request's callbacks.
const later = (fn) => queueMicrotask(fn);
