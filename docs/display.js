// Where the guest's frames go on the page: a canvas.
//
// A display is what `host.js`'s `display` parameter is — `{width, height, present(bytes)}` — and
// this is the page's implementation of it. The mode is the canvas's, because a canvas has the
// size the page gave it the way a panel has one the hardware fixed, and the guest's `fb` driver
// adopts whatever the host names (M5a, `ARCH_WASM32.md` §11). A host with no display at all is
// `display = null`, and the driver then finds no device — which is what a headless run is.
//
// `present` is handed one frame of guest memory in the layout the driver's own
// `FbVarScreeninfo` describes: XRGB8888, four bytes per pixel, rows back to back. Two things
// follow from where those bytes are:
//
//   * They are in an instance's linear memory, so they are a *view* that is good only for the
//     length of the call. Whatever `present` wants to keep, it copies — which is what the
//     canvas's own storage is.
//   * The channel order is the guest's, not the canvas's: a little-endian XRGB8888 word is
//     B,G,R,X in memory, and an `ImageData` wants R,G,B,A. That conversion is the whole of this
//     file's work beyond handing the pixels over, and it is why the bytes are read here rather
//     than blitted as they lie.

/// The page's display: a canvas, and the frame-to-`ImageData` conversion the canvas needs.
export function createDisplay(canvas) {
  const context = canvas.getContext('2d');
  const { width, height } = canvas;
  let image = null;

  return {
    width,
    height,
    /// Draw one frame. `bytes` is `width * height * 4` long — `host.js` refuses any other size,
    /// because a different one means the two sides disagree about the mode.
    present(bytes) {
      // Allocated once for the session rather than per frame: at the mode this port boots with a
      // frame is 3 MiB, and the canvas keeps its own copy of it either way.
      if (image === null) image = context.createImageData(width, height);
      const pixels = image.data;
      for (let i = 0; i < pixels.length; i += 4) {
        pixels[i] = bytes[i + 2];
        pixels[i + 1] = bytes[i + 1];
        pixels[i + 2] = bytes[i];
        pixels[i + 3] = 255;
      }
      context.putImageData(image, 0, 0);
    },
  };
}
