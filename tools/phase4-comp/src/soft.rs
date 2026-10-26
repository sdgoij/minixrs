//! A software renderer behind smithay's `Renderer` seam (`.notes/wayland.md` 6.13, D17).
//!
//! Everything is CPU-side and top-row-first: `SoftTarget` is the offscreen drawable, its row 0
//! is the top row of the output, and `copy_framebuffer` hands that order back unchanged. A
//! `wl_shm` buffer keeps the client's own order, which is the same one (row 0 is the top of the
//! surface), so `ImportMemWl` copies it as it is and no flip is recorded.
//!
//! Sampling is nearest-neighbour: `downscale_filter` and `upscale_filter` are accepted and have
//! no effect, because a linear filter would need the pixel's neighbours and a fractional
//! coordinate, and nothing above this renderer asks for either yet. Blending is premultiplied,
//! `dst = src + dst * (1 - a)`, which is what smithay's `wl_shm` clients and its element alpha
//! both mean.
//!
//! Rotations and reflections are composed with `Transform::matrix` and the same translation
//! terms the GLES renderer's `build_texture_mat` uses, so a surface lands on the same pixels
//! through either renderer.
//!
//! dmabufs are not importable: `dmabuf_formats` stays empty, so a client is never offered one,
//! and `import_dmabuf` answers `DmabufUnsupported` rather than pretending.

use std::fmt;
use std::sync::{Arc, Mutex};

use cgmath::{Matrix3, Vector2, Vector3};
use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{
    Bind, Color32F, ContextId, DebugFlags, ExportMem, Frame, ImportDma, ImportDmaWl, ImportMem,
    ImportMemWl, Offscreen, Renderer, RendererSuper, Texture, TextureFilter, TextureMapping,
};
use smithay::reexports::wayland_server::protocol::{wl_buffer, wl_shm};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};
use smithay::wayland::compositor::SurfaceData;
use smithay::wayland::shm::{BufferAccessError, shm_format_to_fourcc, with_buffer_contents};

/// Every format this renderer accepts is four bytes per pixel.
const BPP: usize = 4;

#[derive(Debug)]
pub enum SoftError {
    BufferAccess(BufferAccessError),
    UnsupportedFormat(Fourcc),
    UnsupportedShmFormat(wl_shm::Format),
    /// A source mapping is smaller than its own geometry says it is.
    BufferTooSmall {
        need: usize,
        got: usize,
    },
    BadGeometry {
        width: i32,
        height: i32,
        stride: i32,
    },
    /// A texture lock was poisoned by a panic in another thread.
    Poisoned,
    DmabufUnsupported,
    SyncInterrupted,
}

impl fmt::Display for SoftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferAccess(e) => write!(f, "shm buffer access: {e}"),
            Self::UnsupportedFormat(format) => write!(f, "unsupported format {format:?}"),
            Self::UnsupportedShmFormat(format) => write!(f, "unsupported wl_shm format {format:?}"),
            Self::BufferTooSmall { need, got } => {
                write!(f, "buffer too small: need {need} bytes, got {got}")
            }
            Self::BadGeometry {
                width,
                height,
                stride,
            } => write!(f, "bad buffer geometry {width}x{height} stride {stride}"),
            Self::Poisoned => write!(f, "texture lock poisoned"),
            Self::DmabufUnsupported => write!(f, "dmabuf import is not implemented"),
            Self::SyncInterrupted => write!(f, "waiting for a sync point was interrupted"),
        }
    }
}

impl std::error::Error for SoftError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BufferAccess(e) => Some(e),
            _ => None,
        }
    }
}

impl From<BufferAccessError> for SoftError {
    fn from(value: BufferAccessError) -> Self {
        Self::BufferAccess(value)
    }
}

/// The channel order of the four 4-byte formats this renderer accepts.
#[derive(Clone, Copy)]
struct Layout {
    /// True when the stored order is B, G, R (the `*rgb8888` FourCCs), false for R, G, B.
    bgr: bool,
    /// False for `Xrgb8888`/`Xbgr8888`, whose fourth byte is padding rather than alpha.
    alpha: bool,
}

fn layout(format: Fourcc) -> Option<Layout> {
    match format {
        Fourcc::Argb8888 => Some(Layout {
            bgr: true,
            alpha: true,
        }),
        Fourcc::Xrgb8888 => Some(Layout {
            bgr: true,
            alpha: false,
        }),
        Fourcc::Abgr8888 => Some(Layout {
            bgr: false,
            alpha: true,
        }),
        Fourcc::Xbgr8888 => Some(Layout {
            bgr: false,
            alpha: false,
        }),
        _ => None,
    }
}

fn to_rgba(layout: Layout, px: [u8; BPP]) -> [u8; BPP] {
    if layout.bgr {
        [px[2], px[1], px[0], if layout.alpha { px[3] } else { 255 }]
    } else {
        [px[0], px[1], px[2], if layout.alpha { px[3] } else { 255 }]
    }
}

fn to_stored(layout: Layout, rgba: [u8; BPP]) -> [u8; BPP] {
    if layout.bgr {
        [
            rgba[2],
            rgba[1],
            rgba[0],
            if layout.alpha { rgba[3] } else { 255 },
        ]
    } else {
        [
            rgba[0],
            rgba[1],
            rgba[2],
            if layout.alpha { rgba[3] } else { 255 },
        ]
    }
}

/// An image imported from the client, in the client's row order.
#[derive(Clone, Debug)]
pub struct SoftTexture {
    width: u32,
    height: u32,
    format: Fourcc,
    /// Mirrors the `flipped` argument of `import_memory`: true when row 0 is the bottom row.
    /// `wl_shm` buffers and this renderer's own targets all start at the top, so it is false
    /// for everything the compositor imports.
    y_inverted: bool,
    pixels: Arc<Mutex<Vec<u8>>>,
}

impl SoftTexture {
    fn new(width: u32, height: u32, format: Fourcc, y_inverted: bool, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            format,
            y_inverted,
            pixels: Arc::new(Mutex::new(pixels)),
        }
    }

    fn stride(&self) -> usize {
        self.width as usize * BPP
    }
}

impl Texture for SoftTexture {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn format(&self) -> Option<Fourcc> {
        Some(self.format)
    }
}

/// A downloaded copy of a region, in the region's row order (top first).
#[derive(Debug)]
pub struct SoftMapping {
    width: u32,
    height: u32,
    format: Fourcc,
    data: Vec<u8>,
}

impl Texture for SoftMapping {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn format(&self) -> Option<Fourcc> {
        Some(self.format)
    }
}

impl TextureMapping for SoftMapping {
    fn flipped(&self) -> bool {
        false
    }
}

/// The offscreen drawable: rows top first, `format` per pixel.
#[derive(Debug)]
pub struct SoftTarget {
    data: Vec<u8>,
    width: i32,
    height: i32,
    format: Fourcc,
}

impl SoftTarget {
    pub fn new(format: Fourcc, size: Size<i32, Buffer>) -> Result<Self, SoftError> {
        if layout(format).is_none() {
            return Err(SoftError::UnsupportedFormat(format));
        }
        if size.w <= 0 || size.h <= 0 {
            return Err(SoftError::BadGeometry {
                width: size.w,
                height: size.h,
                stride: 0,
            });
        }
        let len = size.w as usize * size.h as usize * BPP;
        Ok(Self {
            data: vec![0; len],
            width: size.w,
            height: size.h,
            format,
        })
    }
}

/// A frame's handle on the target's pixels.
pub struct SoftSurface<'a> {
    data: &'a mut [u8],
    width: i32,
    height: i32,
    format: Fourcc,
}

impl fmt::Debug for SoftSurface<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SoftSurface")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .finish()
    }
}

impl Texture for SoftSurface<'_> {
    fn width(&self) -> u32 {
        self.width.max(0) as u32
    }

    fn height(&self) -> u32 {
        self.height.max(0) as u32
    }

    fn format(&self) -> Option<Fourcc> {
        Some(self.format)
    }
}

#[derive(Debug)]
pub struct SoftRenderer {
    context: ContextId<SoftTexture>,
    debug: DebugFlags,
}

impl SoftRenderer {
    pub fn new() -> Self {
        Self {
            context: ContextId::new(),
            debug: DebugFlags::empty(),
        }
    }
}

impl Default for SoftRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl RendererSuper for SoftRenderer {
    type Error = SoftError;
    type TextureId = SoftTexture;
    type Framebuffer<'buffer> = SoftSurface<'buffer>;
    type Frame<'frame, 'buffer>
        = SoftFrame<'frame, 'buffer>
    where
        'buffer: 'frame,
        Self: 'frame;
}

impl Renderer for SoftRenderer {
    fn context_id(&self) -> ContextId<SoftTexture> {
        self.context.clone()
    }

    fn downscale_filter(&mut self, _filter: TextureFilter) -> Result<(), SoftError> {
        Ok(())
    }

    fn upscale_filter(&mut self, _filter: TextureFilter) -> Result<(), SoftError> {
        Ok(())
    }

    fn set_debug_flags(&mut self, flags: DebugFlags) {
        self.debug = flags;
    }

    fn debug_flags(&self) -> DebugFlags {
        self.debug
    }

    fn render<'frame, 'buffer>(
        &'frame mut self,
        framebuffer: &'frame mut SoftSurface<'buffer>,
        _output_size: Size<i32, Physical>,
        dst_transform: Transform,
    ) -> Result<SoftFrame<'frame, 'buffer>, SoftError>
    where
        'buffer: 'frame,
    {
        Ok(SoftFrame {
            context: self.context.clone(),
            surface: framebuffer,
            transform: dst_transform,
        })
    }

    fn wait(&mut self, sync: &SyncPoint) -> Result<(), SoftError> {
        sync.wait().map_err(|_| SoftError::SyncInterrupted)
    }
}

pub struct SoftFrame<'frame, 'buffer> {
    context: ContextId<SoftTexture>,
    surface: &'frame mut SoftSurface<'buffer>,
    transform: Transform,
}

impl SoftFrame<'_, '_> {
    /// The rect in the target, or `None` when it falls outside it.
    fn clipped(&self, rect: Rectangle<i32, Physical>) -> Option<(i32, i32, i32, i32)> {
        let x0 = rect.loc.x.max(0);
        let y0 = rect.loc.y.max(0);
        let x1 = (rect.loc.x.saturating_add(rect.size.w)).min(self.surface.width);
        let y1 = (rect.loc.y.saturating_add(rect.size.h)).min(self.surface.height);
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
    }

    fn fill(&mut self, rect: Rectangle<i32, Physical>, color: Color32F, blend: bool) {
        let Some((x0, y0, x1, y1)) = self.clipped(rect) else {
            return;
        };
        let Some(layout) = layout(self.surface.format) else {
            return;
        };
        let literal = [
            (color.r().clamp(0.0, 1.0) * 255.0).round() as u8,
            (color.g().clamp(0.0, 1.0) * 255.0).round() as u8,
            (color.b().clamp(0.0, 1.0) * 255.0).round() as u8,
            (color.a().clamp(0.0, 1.0) * 255.0).round() as u8,
        ];
        let stored = to_stored(layout, literal);
        let stride = self.surface.width.max(0) as usize * BPP;
        for y in y0..y1 {
            let start = y as usize * stride + x0 as usize * BPP;
            let row = &mut self.surface.data[start..start + (x1 - x0) as usize * BPP];
            for dst in row.chunks_exact_mut(BPP) {
                if blend {
                    blend_px(dst, &stored);
                } else {
                    dst.copy_from_slice(&stored);
                }
            }
        }
    }
}

/// `dst = src + dst * (1 - src.a)`, on premultiplied components in the format's stored order.
/// All four accepted formats keep alpha in byte 3, so the blend is per byte.
fn blend_px(dst: &mut [u8], src: &[u8; BPP]) {
    let inverse = 1.0 - src[3] as f32 / 255.0;
    for (target, source) in dst.iter_mut().zip(src.iter()) {
        let value = *source as f32 + *target as f32 * inverse;
        *target = value.clamp(0.0, 255.0).round() as u8;
    }
}

impl Frame for SoftFrame<'_, '_> {
    type Error = SoftError;
    type TextureId = SoftTexture;

    fn context_id(&self) -> ContextId<SoftTexture> {
        self.context.clone()
    }

    fn clear(&mut self, color: Color32F, at: &[Rectangle<i32, Physical>]) -> Result<(), SoftError> {
        for rect in at {
            self.fill(*rect, color, false);
        }
        Ok(())
    }

    fn draw_solid(
        &mut self,
        dst: Rectangle<i32, Physical>,
        _damage: &[Rectangle<i32, Physical>],
        color: Color32F,
    ) -> Result<(), SoftError> {
        self.fill(dst, color, true);
        Ok(())
    }

    fn render_texture_from_to(
        &mut self,
        texture: &SoftTexture,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        _damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        src_transform: Transform,
        alpha: f32,
    ) -> Result<(), SoftError> {
        let Some(texture_layout) = layout(texture.format) else {
            return Err(SoftError::UnsupportedFormat(texture.format));
        };
        let Some(target_layout) = layout(self.surface.format) else {
            return Err(SoftError::UnsupportedFormat(self.surface.format));
        };
        if texture.width == 0 || texture.height == 0 || src.size.is_empty() || dst.size.is_empty() {
            return Ok(());
        }
        let Some((x0, y0, x1, y1)) = self.clipped(dst) else {
            return Ok(());
        };
        let matrix = texture_matrix(
            src,
            dst,
            (texture.width, texture.height),
            src_transform,
            texture.y_inverted,
        );
        let pixels = texture.pixels.lock().map_err(|_| SoftError::Poisoned)?;
        let stride = self.surface.width.max(0) as usize * BPP;
        let alpha = alpha.clamp(0.0, 1.0);
        let (tw, th) = (texture.width as f32, texture.height as f32);

        for y in y0..y1 {
            let start = y as usize * stride + x0 as usize * BPP;
            let row = &mut self.surface.data[start..start + (x1 - x0) as usize * BPP];
            for (i, target_px) in row.chunks_exact_mut(BPP).enumerate() {
                let px = (x0 + i as i32 - dst.loc.x) as f32 + 0.5;
                let py = (y - dst.loc.y) as f32 + 0.5;
                let uv = matrix * Vector3::new(px, py, 1.0);
                if uv.z.abs() < f32::EPSILON {
                    continue;
                }
                let u = (uv.x / uv.z).clamp(0.0, 1.0);
                let v = (uv.y / uv.z).clamp(0.0, 1.0);
                let column = ((u * tw) as usize).min(texture.width as usize - 1);
                let line = ((v * th) as usize).min(texture.height as usize - 1);
                let offset = (line * texture.width as usize + column) * BPP;
                let Some(source) = pixels.get(offset..offset + BPP) else {
                    continue;
                };
                let mut rgba =
                    to_rgba(texture_layout, [source[0], source[1], source[2], source[3]]);
                if alpha != 1.0 {
                    for channel in &mut rgba {
                        *channel = (*channel as f32 * alpha).round() as u8;
                    }
                }
                blend_px(target_px, &to_stored(target_layout, rgba));
            }
        }
        Ok(())
    }

    fn transformation(&self) -> Transform {
        self.transform
    }

    fn wait(&mut self, sync: &SyncPoint) -> Result<(), SoftError> {
        sync.wait().map_err(|_| SoftError::SyncInterrupted)
    }

    fn finish(self) -> Result<SyncPoint, SoftError> {
        Ok(SyncPoint::signaled())
    }
}

/// Destination pixel -> texture coordinate, the composition the GLES renderer's
/// `build_texture_mat` builds: scale to the source, apply the buffer transform, move to the
/// source's corner, then normalise by the texture's size. `y_inverted` is the final `1 - v`.
fn texture_matrix(
    src: Rectangle<f64, Buffer>,
    dst: Rectangle<i32, Physical>,
    texture: (u32, u32),
    transform: Transform,
    y_inverted: bool,
) -> Matrix3<f32> {
    let transformed = transform.transform_size(src.size).to_f64();
    let dst_size = dst.size.to_f64();
    let scale = Vector2::new(
        (transformed.w / dst_size.w) as f32,
        (transformed.h / dst_size.h) as f32,
    );
    let mut matrix = Matrix3::from_nonuniform_scale(scale.x, scale.y);

    let translation = match transform {
        Transform::Normal => Vector2::new(0.0, 0.0),
        Transform::_90 => Vector2::new(0.0, transformed.h as f32),
        Transform::_180 => Vector2::new(transformed.w as f32, transformed.h as f32),
        Transform::_270 => Vector2::new(transformed.h as f32, 0.0),
        Transform::Flipped => Vector2::new(transformed.w as f32, 0.0),
        Transform::Flipped90 => Vector2::new(0.0, 0.0),
        Transform::Flipped180 => Vector2::new(0.0, transformed.h as f32),
        Transform::Flipped270 => Vector2::new(transformed.h as f32, transformed.w as f32),
    };
    matrix = transform.matrix() * matrix;
    matrix = Matrix3::from_translation(translation) * matrix;
    matrix = Matrix3::from_translation(Vector2::new(src.loc.x as f32, src.loc.y as f32)) * matrix;
    matrix =
        Matrix3::from_nonuniform_scale(1.0 / texture.0 as f32, 1.0 / texture.1 as f32) * matrix;

    if y_inverted {
        matrix = Matrix3::from_nonuniform_scale(1.0, -1.0) * matrix;
    }
    matrix
}

impl Bind<SoftTarget> for SoftRenderer {
    fn bind<'a>(&mut self, target: &'a mut SoftTarget) -> Result<SoftSurface<'a>, SoftError> {
        let width = target.width;
        let height = target.height;
        let format = target.format;
        Ok(SoftSurface {
            data: &mut target.data,
            width,
            height,
            format,
        })
    }
}

impl Offscreen<SoftTarget> for SoftRenderer {
    fn create_buffer(
        &mut self,
        format: Fourcc,
        size: Size<i32, Buffer>,
    ) -> Result<SoftTarget, SoftError> {
        SoftTarget::new(format, size)
    }
}

impl ImportMem for SoftRenderer {
    fn import_memory(
        &mut self,
        data: &[u8],
        format: Fourcc,
        size: Size<i32, Buffer>,
        flipped: bool,
    ) -> Result<SoftTexture, SoftError> {
        if layout(format).is_none() {
            return Err(SoftError::UnsupportedFormat(format));
        }
        if size.w <= 0 || size.h <= 0 {
            return Err(SoftError::BadGeometry {
                width: size.w,
                height: size.h,
                stride: 0,
            });
        }
        let need = size.w as usize * size.h as usize * BPP;
        if data.len() < need {
            return Err(SoftError::BufferTooSmall {
                need,
                got: data.len(),
            });
        }
        Ok(SoftTexture::new(
            size.w as u32,
            size.h as u32,
            format,
            flipped,
            data[..need].to_vec(),
        ))
    }

    fn update_memory(
        &mut self,
        texture: &SoftTexture,
        data: &[u8],
        region: Rectangle<i32, Buffer>,
    ) -> Result<(), SoftError> {
        let stride = texture.stride();
        let need = stride * texture.height as usize;
        if data.len() < need {
            return Err(SoftError::BufferTooSmall {
                need,
                got: data.len(),
            });
        }
        let x0 = region.loc.x.max(0) as usize;
        let y0 = region.loc.y.max(0) as usize;
        let x1 =
            (region.loc.x.saturating_add(region.size.w)).clamp(0, texture.width as i32) as usize;
        let y1 =
            (region.loc.y.saturating_add(region.size.h)).clamp(0, texture.height as i32) as usize;
        let mut pixels = texture.pixels.lock().map_err(|_| SoftError::Poisoned)?;
        for y in y0..y1 {
            let start = y * stride + x0 * BPP;
            let len = (x1.saturating_sub(x0)) * BPP;
            if let (Some(src), Some(dst)) = (
                data.get(start..start + len),
                pixels.get_mut(start..start + len),
            ) {
                dst.copy_from_slice(src);
            }
        }
        Ok(())
    }

    fn mem_formats(&self) -> Box<dyn Iterator<Item = Fourcc>> {
        Box::new(
            [
                Fourcc::Argb8888,
                Fourcc::Xrgb8888,
                Fourcc::Abgr8888,
                Fourcc::Xbgr8888,
            ]
            .into_iter(),
        )
    }
}

impl ImportMemWl for SoftRenderer {
    fn import_shm_buffer(
        &mut self,
        buffer: &wl_buffer::WlBuffer,
        _surface: Option<&SurfaceData>,
        _damage: &[Rectangle<i32, Buffer>],
    ) -> Result<SoftTexture, SoftError> {
        with_buffer_contents(buffer, |ptr, len, data| {
            let format = shm_format_to_fourcc(data.format)
                .ok_or(SoftError::UnsupportedShmFormat(data.format))?;
            if data.width <= 0 || data.height <= 0 || data.stride < data.width * BPP as i32 {
                return Err(SoftError::BadGeometry {
                    width: data.width,
                    height: data.height,
                    stride: data.stride,
                });
            }
            let (width, height) = (data.width as usize, data.height as usize);
            let stride = data.stride as usize;
            let offset = data.offset.max(0) as usize;
            let need = offset + (height - 1) * stride + width * BPP;
            if len < need {
                return Err(SoftError::BufferTooSmall { need, got: len });
            }
            // SAFETY: the shm handler maps `len` readable bytes at `ptr`, and the geometry was
            // checked against that length above.
            let source = unsafe { core::slice::from_raw_parts(ptr, len) };
            let mut pixels = Vec::with_capacity(width * height * BPP);
            for row in 0..height {
                let start = offset + row * stride;
                pixels.extend_from_slice(&source[start..start + width * BPP]);
            }
            Ok(SoftTexture::new(
                width as u32,
                height as u32,
                format,
                false,
                pixels,
            ))
        })?
    }
}

impl ImportDma for SoftRenderer {
    fn import_dmabuf(
        &mut self,
        _dmabuf: &Dmabuf,
        _damage: Option<&[Rectangle<i32, Buffer>]>,
    ) -> Result<SoftTexture, SoftError> {
        Err(SoftError::DmabufUnsupported)
    }
}

impl ImportDmaWl for SoftRenderer {}

impl ExportMem for SoftRenderer {
    type TextureMapping = SoftMapping;

    fn copy_framebuffer(
        &mut self,
        target: &SoftSurface<'_>,
        region: Rectangle<i32, Buffer>,
        format: Fourcc,
    ) -> Result<SoftMapping, SoftError> {
        copy_region(
            target.data,
            target.width,
            target.height,
            target.format,
            region,
            format,
        )
    }

    fn copy_texture(
        &mut self,
        texture: &SoftTexture,
        region: Rectangle<i32, Buffer>,
        format: Fourcc,
    ) -> Result<SoftMapping, SoftError> {
        let pixels = texture.pixels.lock().map_err(|_| SoftError::Poisoned)?;
        copy_region(
            &pixels,
            texture.width as i32,
            texture.height as i32,
            texture.format,
            region,
            format,
        )
    }

    fn can_read_texture(&mut self, _texture: &SoftTexture) -> Result<bool, SoftError> {
        Ok(true)
    }

    fn map_texture<'a>(&mut self, texture_mapping: &'a SoftMapping) -> Result<&'a [u8], SoftError> {
        Ok(&texture_mapping.data)
    }
}

/// Copies a region out of a top-row-first image, converting to `format` when it differs.
fn copy_region(
    data: &[u8],
    width: i32,
    height: i32,
    source_format: Fourcc,
    region: Rectangle<i32, Buffer>,
    format: Fourcc,
) -> Result<SoftMapping, SoftError> {
    let source_layout = layout(source_format).ok_or(SoftError::UnsupportedFormat(source_format))?;
    let target_layout = layout(format).ok_or(SoftError::UnsupportedFormat(format))?;
    let (x0, y0) = (region.loc.x.max(0), region.loc.y.max(0));
    let x1 = region.loc.x.saturating_add(region.size.w).min(width);
    let y1 = region.loc.y.saturating_add(region.size.h).min(height);
    if x1 <= x0 || y1 <= y0 {
        return Err(SoftError::BadGeometry {
            width: region.size.w,
            height: region.size.h,
            stride: 0,
        });
    }
    let stride = width.max(0) as usize * BPP;
    let row_len = (x1 - x0) as usize * BPP;
    let mut out = Vec::with_capacity(row_len * (y1 - y0) as usize);
    for y in y0..y1 {
        let start = y as usize * stride + x0 as usize * BPP;
        let row = data
            .get(start..start + row_len)
            .ok_or(SoftError::BufferTooSmall {
                need: start + row_len,
                got: data.len(),
            })?;
        if source_format == format {
            out.extend_from_slice(row);
            continue;
        }
        for px in row.chunks_exact(BPP) {
            let rgba = to_rgba(source_layout, [px[0], px[1], px[2], px[3]]);
            out.extend_from_slice(&to_stored(target_layout, rgba));
        }
    }
    Ok(SoftMapping {
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
        format,
        data: out,
    })
}
