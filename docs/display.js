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
//     length of the call. Whatever `present` wants to keep, it copies — which is what the frame
//     buffer below is.
//   * The channel order is the guest's, not the canvas's: a little-endian XRGB8888 word is
//     B,G,R,X in memory, and an `ImageData` wants R,G,B,A. That conversion is the whole of this
//     file's work beyond handing the pixels over, and it is why the bytes are read here rather
//     than blitted as they lie.
//
// The conversion is also why the frame is not drawn where it is presented. A present is one line of
// guest code and it happens per *console write* as much as per frame — the tty's console writes
// arrive in eight-byte pieces, and each one is a flush (M5b) — so converting and uploading per
// present would spend a 3 MiB pass and a canvas upload on eight bytes of shell output. Instead the
// frame is copied (cheap, one pass, no canvas work) and the *display* refreshes on its own clock:
// the next animation frame draws whatever the latest copy holds. That is what a display controller
// does with a framebuffer — it scans out on its own schedule — and it means two hundred presents
// during a burst of output cost two hundred memcpys and one upload, not two hundred uploads.

/// The page's display: a canvas, the frame-to-`ImageData` conversion the canvas needs, and the
/// animation frame that does the drawing.
export function createDisplay(canvas) {
  const context = canvas.getContext('2d');
  const { width, height } = canvas;
  let image = null;
  /// The latest presented frame, in the guest's byte order. Allocated once: at the mode this port
  /// boots with a frame is 3 MiB, and a per-present allocation would be churn for no reason.
  let frame = null;
  /// Whether a draw is already queued. A present while one is queued only updates `frame`.
  let queued = false;

  function draw() {
    queued = false;
    if (frame === null) return;
    // Allocated once for the session rather than per frame: the canvas keeps its own copy of the
    // pixels either way.
    if (image === null) image = context.createImageData(width, height);
    const pixels = image.data;
    for (let i = 0; i < pixels.length; i += 4) {
      pixels[i] = frame[i + 2];
      pixels[i + 1] = frame[i + 1];
      pixels[i + 2] = frame[i];
      pixels[i + 3] = 255;
    }
    context.putImageData(image, 0, 0);
  }

  return {
    width,
    height,
    /// Take one frame. `bytes` is `width * height * 4` long — `host.js` refuses any other size,
    /// because a different one means the two sides disagree about the mode — and this takes a copy
    /// rather than keeping the view, which dies with the guest's call.
    present(bytes) {
      if (frame === null) frame = new Uint8Array(width * height * 4);
      frame.set(bytes);
      if (queued) return;
      queued = true;
      requestAnimationFrame(draw);
    },
  };
}
