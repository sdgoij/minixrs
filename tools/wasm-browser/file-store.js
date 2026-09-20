// The device's contents in a file: a disk for the Node front ends, not a cache of one.
//
// One of the two implementations of the store contract `store.js` documents.

import fs from 'node:fs';

/// The file *is* the device and is created from the boot image the first time, so a
/// fresh store boots an installed system and every run after it sees what the previous
/// run wrote. Writes are positional and land before the call returns, which is what
/// `block_write` promises the guest — visible to the next run, and to the tools that end
/// up looking at the file.
///
/// The sidecar records which image the disk's contents were made from, because that is
/// the one assumption this design adds over a real disk: a store outlives the image it
/// was seeded from.
export function fileStore(diskPath, sourceImage) {
  const metaPath = `${diskPath}.json`;
  const meta = fs.existsSync(metaPath)
    ? JSON.parse(fs.readFileSync(metaPath, 'utf8'))
    : { imageId: null };
  if (!fs.existsSync(diskPath)) fs.writeFileSync(diskPath, sourceImage);

  let fd = null;
  const open = () => {
    if (fd === null) fd = fs.openSync(diskPath, 'r+');
    return fd;
  };

  return {
    get imageId() {
      return meta.imageId;
    },
    setImageId(id) {
      meta.imageId = id;
      fs.writeFileSync(metaPath, JSON.stringify(meta));
    },
    read(offset, length) {
      const buf = Buffer.alloc(length);
      const got = fs.readSync(open(), buf, 0, length, offset);
      // Past the end of the device is zeros, as it is on a real one.
      buf.fill(0, got);
      return buf;
    },
    write(offset, bytes) {
      fs.writeSync(open(), bytes, 0, bytes.length, offset);
    },
    close() {
      if (fd !== null) {
        fs.closeSync(fd);
        fd = null;
      }
    },
  };
}
