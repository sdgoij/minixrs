// Where the host's block device keeps its bytes, on the page's side.
//
// A store is the four calls a *disk* has rather than the ones a database has, and
// `host.js`'s device is the only caller:
//
//   { imageId, setImageId(id), read(offset, length) -> bytes, write(offset, bytes) }
//
// Both halves are *synchronous*. The guest's `host_block_read`/`host_block_write`
// return a value to `virtio_blk`, so nothing below the device may make it wait; that is
// the constraint this file is shaped around, and the reason the disk is held in memory
// rather than looked up page by page as it is asked for. The other implementation is
// `file-store.js`, for the Node front ends.
//
// The granularity is each store's own business — a file in Node, a record per page here
// — which is why neither offset nor length is assumed to be small.
//
// The page's store has one call the device never makes: `clear()`, which throws the disk away so
// the next boot seeds itself from the image again. The page's "start over from the boot image"
// control is its only caller, and it is the way out of a store the device refuses — this file's
// contents having come from another image — and of a filesystem a previous session left unclean.
//
// `imageId` is the guard on the one failure mode this design has that a real disk does
// not: the store outlives the image it was seeded from, so a rebuilt image over an old
// store would be two filesystems mixed. A store whose identity does not match the image
// is refused and the device is not attached at all, which is loud — the guest falls back
// to the ramdisk and the host says why — whereas the mix would be silent. It is `null`
// until something seeds the store, which is `setImageId`'s job: the device calls it the
// first time, when the store was empty.

/// One record per this many bytes of disk, and the guest's block size on this port,
/// which is why it is this number.
export const PAGE_SIZE = 4096;

const PAGES = 'pages';
const META = 'meta';
const META_KEY = 'imageId';

/// The page's disk: the boot image seeded once and then the guest's own bytes, held in
/// IndexedDB as one record per page.
///
/// IndexedDB is asynchronous and the device is not, so the disk lives in memory for the
/// session: `open` reads the image, overlays every page the store already holds, and
/// every read after that is a slice of that array. A *write* goes the other way — into
/// the array, and on into IndexedDB before returning, with the record's bytes copied
/// first, because the buffer the device hands over is a view on the guest's own memory
/// and the guest is free to overwrite it the moment the call returns.
///
/// What that gives up is worth stating: the reference's `block_write` is durable when it
/// returns, and this is durable when the browser commits the transaction, a tick or two
/// later — not something a page can wait for. A page killed inside that window loses the
/// blocks it was writing, which is what a disk with a write cache does too, and why the
/// guest has `sync` and a shutdown: those are what make the filesystem's *state* answer
/// for its blocks rather than trusting each write to survive.
///
/// `onError` is called when a write cannot reach the database. Nothing else can report
/// it: by then the guest has been told the write landed.
export async function indexedDbStore({
  name = 'minixrs-disk',
  imageBytes,
  indexedDB = globalThis.indexedDB,
  pageSize = PAGE_SIZE,
  onError = () => {},
}) {
  if (indexedDB === undefined || indexedDB === null) {
    throw new Error('this browser has no IndexedDB');
  }
  const db = await openDiskDatabase(indexedDB, name);

  // The disk's initial contents: the only thing a store that was never written to has to
  // answer with. A copy, and explicitly a `Uint8Array` one: `slice` on a Node `Buffer` is a
  // *view*, so a store handed one would be writing into its caller's bytes — which, for the
  // check harnesses that hand out the image they read from disk, is the image every later
  // boot is identified and seeded from (finding 55).
  const disk = new Uint8Array(imageBytes);

  // Overlay what is already on the disk. All three requests are issued before any is
  // awaited: an IndexedDB transaction autocommits between tasks, so a request started
  // after another one resolved would find its transaction inactive. `getAllKeys` and
  // `getAll` visit an object store in the same key order, so they line up index for
  // index.
  let imageId = null;
  {
    const tx = db.transaction([PAGES, META], 'readonly');
    const pageKeys = request(tx.objectStore(PAGES).getAllKeys());
    const pageValues = request(tx.objectStore(PAGES).getAll());
    const stored = request(tx.objectStore(META).get(META_KEY));
    const [keys, values, meta] = await Promise.all([pageKeys, pageValues, stored]);
    for (let i = 0; i < keys.length; i += 1) {
      const at = keys[i] * pageSize;
      if (at >= disk.length) continue;
      const page = values[i];
      disk.set(page.subarray(0, Math.min(page.length, disk.length - at)), at);
    }
    imageId = typeof meta === 'string' ? meta : null;
  }

  return {
    get imageId() {
      return imageId;
    },
    setImageId(id) {
      imageId = id;
      const tx = db.transaction(META, 'readwrite');
      const put = tx.objectStore(META).put(id, META_KEY);
      put.onerror = () => onError(put.error);
      tx.onabort = () => onError(tx.error ?? new Error('the write was aborted'));
    },
    read(offset, length) {
      return disk.subarray(offset, offset + length);
    },
    /// Throw the disk away, so the next boot starts from the image again. Two states need it and
    /// neither has another way out: a store whose contents came from a different image (the
    /// device is never attached, and nothing on the page can clear what refuses it) and a
    /// filesystem a tab left unclean (the next mount is read-only — finding 48).
    ///
    /// Closing the connection first is what makes the delete possible: a database with an open
    /// connection blocks its own deletion, this page's included.
    async clear() {
      db.close();
      await new Promise((resolve, reject) => {
        const request = indexedDB.deleteDatabase(name);
        request.onsuccess = () => resolve();
        request.onerror = () => reject(request.error ?? new Error(`${name} could not be deleted`));
        request.onblocked = () => reject(new Error(`${name} is still open in another tab`));
      });
    },
    write(offset, bytes) {
      disk.set(bytes, offset);
      // Every page the write touched becomes its own record. A filesystem's pages are a
      // small subset of the image, so this is what keeps a write to one block from
      // rewriting the whole disk.
      const first = Math.floor(offset / pageSize);
      const last = Math.floor((offset + bytes.length - 1) / pageSize);
      const tx = db.transaction(PAGES, 'readwrite');
      const pages = tx.objectStore(PAGES);
      for (let page = first; page <= last; page += 1) {
        const at = page * pageSize;
        const record = new Uint8Array(pageSize);
        record.set(disk.subarray(at, Math.min(at + pageSize, disk.length)));
        const put = pages.put(record, page);
        put.onerror = () => onError(put.error);
      }
      tx.onabort = () => onError(tx.error ?? new Error('the write was aborted'));
    },
  };
}

/// Open (creating on first use) the database the disk lives in.
function openDiskDatabase(factory, name) {
  return new Promise((resolve, reject) => {
    const open = factory.open(name, 1);
    open.onupgradeneeded = () => {
      const db = open.result;
      if (!db.objectStoreNames.contains(PAGES)) db.createObjectStore(PAGES);
      if (!db.objectStoreNames.contains(META)) db.createObjectStore(META);
    };
    open.onsuccess = () => resolve(open.result);
    open.onerror = () => reject(open.error ?? new Error(`cannot open ${name}`));
    open.onblocked = () => reject(new Error(`${name} is open at an older version in another tab`));
  });
}

/// An IndexedDB request as a promise.
function request(req) {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}
