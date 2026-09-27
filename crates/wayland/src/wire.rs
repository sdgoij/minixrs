//! Wayland message framing and argument coding.
//!
//! A message is an 8-byte header followed by its arguments: an `object_id`
//! (u32), then a u32 packing `size << 16 | opcode`. `size` counts the whole
//! message, header included, and is a multiple of four — every argument is
//! padded to a 4-byte boundary, and strings and arrays additionally carry a NUL
//! / their own padding. Every value is little-endian, which all three hardware
//! targets are.
//!
//! Arguments are coded by *signature* (`i` int, `u` uint, `f` fixed, `s`
//! string, `o` object, `n` new_id, `a` array, `h` fd), so one reader serves
//! every interface; the per-interface signatures live in [`crate::protocol`].
//! An `h` is not a descriptor on the wire: it is the index of one in the
//! message's out-of-band `SCM_RIGHTS` list, which the transport resolves.

use core::fmt;

/// Length of a message header.
pub const HEADER_LEN: usize = 8;

/// Largest message this code will frame. A header claiming more is a protocol
/// error, not an allocation.
pub const MAX_MESSAGE: usize = 4096;

/// Largest argument count one message may carry.
pub const MAX_ARGS: usize = 32;

/// A coding error. Wayland has no error reply for a malformed request beyond
/// `wl_display.error`, so a server reports one and closes the connection; the
/// variant names why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// A header, or a `size`, shorter than the header length.
    Truncated,
    /// `size` is not a multiple of four.
    Misaligned,
    /// `size` exceeds [`MAX_MESSAGE`].
    TooLarge,
    /// An argument ran past the end of the body.
    ArgOverrun,
    /// A string or array length was negative, or overran the body.
    BadLength,
    /// More arguments than the signature, or [`MAX_ARGS`], allows.
    TooManyArgs,
    /// A signature byte named no argument type.
    BadSignature,
    /// The buffer given to a [`Writer`], or a [`MessageBuffer`], was too small.
    NoSpace,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            WireError::Truncated => "truncated message",
            WireError::Misaligned => "message size is not 4-byte aligned",
            WireError::TooLarge => "message larger than the frame limit",
            WireError::ArgOverrun => "argument ran past the message body",
            WireError::BadLength => "string or array length is invalid",
            WireError::TooManyArgs => "too many arguments",
            WireError::BadSignature => "unknown argument type in signature",
            WireError::NoSpace => "not enough room",
        };
        f.write_str(s)
    }
}

/// A message header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub object_id: u32,
    pub opcode: u16,
    pub size: u32,
}

impl Header {
    /// Parse a header from at least the first 8 bytes of a message.
    pub fn parse(buf: &[u8]) -> Result<Self, WireError> {
        if buf.len() < HEADER_LEN {
            return Err(WireError::Truncated);
        }
        let object_id = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let so = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let size = so >> 16;
        let opcode = (so & 0xffff) as u16;
        if (size as usize) < HEADER_LEN {
            return Err(WireError::Truncated);
        }
        if !size.is_multiple_of(4) {
            return Err(WireError::Misaligned);
        }
        if size as usize > MAX_MESSAGE {
            return Err(WireError::TooLarge);
        }
        Ok(Header {
            object_id,
            opcode,
            size,
        })
    }

    /// The 8 header bytes for a message of `size` bytes.
    pub fn encode(object_id: u32, opcode: u16, size: usize) -> [u8; HEADER_LEN] {
        let so = ((size as u32) << 16) | opcode as u32;
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(&object_id.to_le_bytes());
        b[4..8].copy_from_slice(&so.to_le_bytes());
        b
    }
}

/// One decoded argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg<'a> {
    Int(i32),
    Uint(u32),
    /// 24.8 fixed point, carried as its raw i32.
    Fixed(i32),
    /// A string, without its NUL terminator.
    Str(&'a [u8]),
    Array(&'a [u8]),
    Object(u32),
    NewId(u32),
    /// The index into the message's fd list, not a descriptor.
    Fd(u32),
}

impl<'a> Arg<'a> {
    /// The argument as a `uint`-shaped value (also object/new_id/fd).
    pub fn as_uint(&self) -> Option<u32> {
        match self {
            Arg::Uint(v) | Arg::Object(v) | Arg::NewId(v) | Arg::Fd(v) => Some(*v),
            _ => None,
        }
    }

    /// The argument as an `int`-shaped value (also fixed).
    pub fn as_int(&self) -> Option<i32> {
        match self {
            Arg::Int(v) | Arg::Fixed(v) => Some(*v),
            _ => None,
        }
    }

    /// The argument as a string, without its NUL.
    pub fn as_str(&self) -> Option<&'a [u8]> {
        match self {
            Arg::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// `n` rounded up to a multiple of four.
const fn round4(n: usize) -> usize {
    (n + 3) & !3
}

/// A cursor over a message body.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes not yet read.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// True once every byte has been read.
    pub fn is_done(&self) -> bool {
        self.pos == self.buf.len()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if self.pos + n > self.buf.len() {
            return Err(WireError::ArgOverrun);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn int(&mut self) -> Result<i32, WireError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn uint(&mut self) -> Result<u32, WireError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn fixed(&mut self) -> Result<i32, WireError> {
        self.int()
    }

    pub fn object(&mut self) -> Result<u32, WireError> {
        self.uint()
    }

    pub fn new_id(&mut self) -> Result<u32, WireError> {
        self.uint()
    }

    pub fn fd(&mut self) -> Result<u32, WireError> {
        self.uint()
    }

    /// A string: `int` length *including* the NUL, the bytes, then padding. The
    /// returned slice excludes the NUL.
    pub fn string(&mut self) -> Result<&'a [u8], WireError> {
        let len = self.int()?;
        if len < 0 {
            return Err(WireError::BadLength);
        }
        let len = len as usize;
        let bytes = self.take(round4(len))?;
        // `len` counts the NUL, so an empty string is len 1 (a lone NUL). A
        // zero length is tolerated as empty rather than rejected.
        Ok(&bytes[..len.saturating_sub(1)])
    }

    /// An array: `int` byte count (not counting padding), the bytes, padding.
    pub fn array(&mut self) -> Result<&'a [u8], WireError> {
        let len = self.int()?;
        if len < 0 {
            return Err(WireError::BadLength);
        }
        let len = len as usize;
        let bytes = self.take(round4(len))?;
        Ok(&bytes[..len])
    }
}

/// Decode `sig`'s arguments from `r` into `out`, returning the count. `?`
/// (nullable) and version digits are skipped, so tables may store either form.
pub fn decode<'a>(sig: &str, r: &mut Reader<'a>, out: &mut [Arg<'a>]) -> Result<usize, WireError> {
    let mut n = 0usize;
    for ch in sig.bytes() {
        let ty = match ch {
            b'?' | b'0'..=b'9' => continue,
            c => c,
        };
        if n >= out.len() || n >= MAX_ARGS {
            return Err(WireError::TooManyArgs);
        }
        out[n] = match ty {
            b'i' => Arg::Int(r.int()?),
            b'u' => Arg::Uint(r.uint()?),
            b'f' => Arg::Fixed(r.fixed()?),
            b's' => Arg::Str(r.string()?),
            b'a' => Arg::Array(r.array()?),
            b'o' => Arg::Object(r.object()?),
            b'n' => Arg::NewId(r.new_id()?),
            b'h' => Arg::Fd(r.fd()?),
            _ => return Err(WireError::BadSignature),
        };
        n += 1;
    }
    Ok(n)
}

/// Builds one message into a caller-supplied buffer. The header is written
/// last, by [`Writer::finish`], once the size is known.
pub struct Writer<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> Writer<'a> {
    /// A writer over `buf`, with the header's room reserved.
    pub fn new(buf: &'a mut [u8]) -> Result<Self, WireError> {
        if buf.len() < HEADER_LEN {
            return Err(WireError::NoSpace);
        }
        Ok(Self {
            buf,
            pos: HEADER_LEN,
        })
    }

    /// Bytes written so far, header included.
    pub fn len(&self) -> usize {
        self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.pos == 0
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), WireError> {
        if self.pos + bytes.len() > self.buf.len() {
            return Err(WireError::NoSpace);
        }
        self.buf[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
        self.pos += bytes.len();
        Ok(())
    }

    fn pad4(&mut self) -> Result<(), WireError> {
        while !self.pos.is_multiple_of(4) {
            self.put(&[0u8])?;
        }
        Ok(())
    }

    pub fn int(&mut self, v: i32) -> Result<(), WireError> {
        self.put(&v.to_le_bytes())
    }

    pub fn uint(&mut self, v: u32) -> Result<(), WireError> {
        self.put(&v.to_le_bytes())
    }

    pub fn fixed(&mut self, v: i32) -> Result<(), WireError> {
        self.put(&v.to_le_bytes())
    }

    pub fn object(&mut self, v: u32) -> Result<(), WireError> {
        self.uint(v)
    }

    pub fn new_id(&mut self, v: u32) -> Result<(), WireError> {
        self.uint(v)
    }

    pub fn fd(&mut self, index: u32) -> Result<(), WireError> {
        self.uint(index)
    }

    pub fn string(&mut self, s: &[u8]) -> Result<(), WireError> {
        self.int((s.len() + 1) as i32)?;
        self.put(s)?;
        self.put(&[0u8])?;
        self.pad4()
    }

    pub fn array(&mut self, a: &[u8]) -> Result<(), WireError> {
        self.int(a.len() as i32)?;
        self.put(a)?;
        self.pad4()
    }

    /// Write the header and return the finished message.
    pub fn finish(&mut self, object_id: u32, opcode: u16) -> Result<&[u8], WireError> {
        let header = Header::encode(object_id, opcode, self.pos);
        self.buf[0..HEADER_LEN].copy_from_slice(&header);
        Ok(&self.buf[..self.pos])
    }
}

/// A buffer of framed events, built one message at a time.
///
/// The server's dispatcher writes each event's arguments through
/// [`DispatchBuf::event`], which adds the header; `bytes` is then the whole
/// concatenated run, ready to `send`.
pub struct DispatchBuf<'a> {
    buf: &'a mut [u8],
    len: usize,
    events: usize,
}

impl<'a> DispatchBuf<'a> {
    /// A buffer over `buf`, empty.
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            len: 0,
            events: 0,
        }
    }

    /// Bytes of framed events written so far.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// How many events have been written.
    pub fn events(&self) -> usize {
        self.events
    }

    /// The framed events.
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Frame one event: `f` writes its arguments, the header is added here.
    pub fn event<F>(&mut self, object_id: u32, opcode: u16, f: F) -> Result<(), WireError>
    where
        F: FnOnce(&mut Writer<'_>) -> Result<(), WireError>,
    {
        let msg_len = {
            let mut w = Writer::new(&mut self.buf[self.len..])?;
            f(&mut w)?;
            w.finish(object_id, opcode)?.len()
        };
        self.len += msg_len;
        self.events += 1;
        Ok(())
    }
}

/// Capacity of a [`MessageBuffer`] — room for a full message plus a partial one.
pub const BUFFER_CAPACITY: usize = 2 * MAX_MESSAGE;

/// Frames Wayland messages out of `recv` chunks.
///
/// A socket read returns an arbitrary split of a byte stream: one message may
/// arrive in pieces, or several together. `push` appends a chunk; `next`
/// returns the next whole message if one is buffered; `consume` drops it.
pub struct MessageBuffer {
    buf: [u8; BUFFER_CAPACITY],
    len: usize,
}

impl Default for MessageBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageBuffer {
    pub const fn new() -> Self {
        Self {
            buf: [0u8; BUFFER_CAPACITY],
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Append a chunk read from the socket.
    pub fn push(&mut self, data: &[u8]) -> Result<(), WireError> {
        if self.len + data.len() > BUFFER_CAPACITY {
            return Err(WireError::NoSpace);
        }
        self.buf[self.len..self.len + data.len()].copy_from_slice(data);
        self.len += data.len();
        Ok(())
    }

    /// The next complete message (header + body), or `None` if the rest has not
    /// arrived. A header that cannot be parsed is an error.
    pub fn next(&self) -> Result<Option<&[u8]>, WireError> {
        if self.len < HEADER_LEN {
            return Ok(None);
        }
        let h = Header::parse(&self.buf[..self.len])?;
        if self.len < h.size as usize {
            return Ok(None);
        }
        Ok(Some(&self.buf[..h.size as usize]))
    }

    /// Drop the first `n` bytes (the message just handled).
    pub fn consume(&mut self, n: usize) {
        if n >= self.len {
            self.len = 0;
            return;
        }
        self.buf.copy_within(n..self.len, 0);
        self.len -= n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc<F: Fn(&mut Writer<'_>) -> Result<(), WireError>>(f: F) -> ([u8; 64], usize) {
        let mut buf = [0u8; 64];
        let size = {
            let mut w = Writer::new(&mut buf).unwrap();
            f(&mut w).unwrap();
            w.finish(7, 3).unwrap().len()
        };
        (buf, size)
    }

    #[test]
    fn header_roundtrips() {
        let h = Header::parse(&Header::encode(0x1234, 5, 24)).unwrap();
        assert_eq!(
            h,
            Header {
                object_id: 0x1234,
                opcode: 5,
                size: 24
            }
        );
    }

    #[test]
    fn header_rejects_short_and_misaligned() {
        assert_eq!(Header::parse(&[0u8; 4]), Err(WireError::Truncated));
        // size 9 is not 4-byte aligned
        let mut b = [0u8; 8];
        b[4..8].copy_from_slice(&((9u32 << 16) | 1).to_le_bytes());
        assert_eq!(Header::parse(&b), Err(WireError::Misaligned));
        // size 4 is under the header length
        let mut b = [0u8; 8];
        b[4..8].copy_from_slice(&((4u32 << 16) | 1).to_le_bytes());
        assert_eq!(Header::parse(&b), Err(WireError::Truncated));
    }

    #[test]
    fn ints_and_uints_roundtrip() {
        let (buf, size) = enc(|w| {
            w.int(-7)?;
            w.uint(0xdead_beef)?;
            w.fixed(1 << 8)?;
            Ok(())
        });
        assert_eq!(size, HEADER_LEN + 12);
        let mut r = Reader::new(&buf[HEADER_LEN..size]);
        assert_eq!(r.int().unwrap(), -7);
        assert_eq!(r.uint().unwrap(), 0xdead_beef);
        assert_eq!(r.fixed().unwrap(), 1 << 8);
        assert!(r.is_done());
    }

    #[test]
    fn strings_are_nul_terminated_and_padded() {
        // "hi" is len 3 including the NUL, padded to 4.
        let (buf, size) = enc(|w| w.string(b"hi"));
        assert_eq!(size, HEADER_LEN + 4 + 4);
        let body = &buf[HEADER_LEN..size];
        assert_eq!(i32::from_le_bytes(body[0..4].try_into().unwrap()), 3);
        assert_eq!(&body[4..7], b"hi\0");
        assert_eq!(body[7], 0); // padding
        let mut r = Reader::new(body);
        assert_eq!(r.string().unwrap(), b"hi");
        assert!(r.is_done());

        // An empty string is a lone NUL, length 1.
        let (buf, size) = enc(|w| w.string(b""));
        let body = &buf[HEADER_LEN..size];
        assert_eq!(i32::from_le_bytes(body[0..4].try_into().unwrap()), 1);
        assert_eq!(body[4], 0);
        assert_eq!(Reader::new(body).string().unwrap(), b"");
    }

    #[test]
    fn arrays_carry_their_length_and_padding() {
        let (buf, size) = enc(|w| w.array(&[1, 2, 3]));
        // len 3, 3 bytes, 1 pad = 8
        assert_eq!(size, HEADER_LEN + 8);
        let body = &buf[HEADER_LEN..size];
        assert_eq!(i32::from_le_bytes(body[0..4].try_into().unwrap()), 3);
        assert_eq!(&body[4..7], &[1, 2, 3]);
        assert_eq!(body[7], 0);
        let mut r = Reader::new(body);
        assert_eq!(r.array().unwrap(), &[1, 2, 3]);
    }

    #[test]
    fn signature_drives_the_decode() {
        // Mimic `wl_registry.bind`: name(u) interface(s) version(u) id(n).
        let (buf, size) = enc(|w| {
            w.uint(1)?;
            w.string(b"wl_compositor")?;
            w.uint(4)?;
            w.new_id(2)?;
            Ok(())
        });
        let mut r = Reader::new(&buf[HEADER_LEN..size]);
        let mut args = [Arg::Uint(0); MAX_ARGS];
        let n = decode("usun", &mut r, &mut args).unwrap();
        assert_eq!(n, 4);
        assert_eq!(args[0].as_uint(), Some(1));
        assert_eq!(args[1].as_str(), Some(&b"wl_compositor"[..]));
        assert_eq!(args[2].as_uint(), Some(4));
        assert_eq!(args[3].as_uint(), Some(2));
        assert!(r.is_done());
    }

    #[test]
    fn a_short_body_is_an_overrun_not_a_panic() {
        let body = [0u8, 0, 0];
        let mut r = Reader::new(&body);
        assert_eq!(r.int(), Err(WireError::ArgOverrun));
    }

    #[test]
    fn message_buffer_frames_split_and_coalesced_messages() {
        let mut mb = MessageBuffer::new();
        // Two complete messages back to back.
        let a = Header::encode(1, 0, HEADER_LEN);
        let b = Header::encode(1, 1, HEADER_LEN);
        let mut both = [0u8; 16];
        both[0..8].copy_from_slice(&a);
        both[8..16].copy_from_slice(&b);

        // Deliver the stream one byte at a time and count the messages that
        // become whole.
        let mut whole = 0;
        for byte in both {
            mb.push(&[byte]).unwrap();
            while let Some(msg) = mb.next().unwrap() {
                let h = Header::parse(msg).unwrap();
                whole += 1;
                mb.consume(h.size as usize);
            }
        }
        assert_eq!(whole, 2);
        assert!(mb.is_empty());
    }

    #[test]
    fn message_buffer_waits_for_a_whole_message() {
        let mut mb = MessageBuffer::new();
        // A header claiming 16 bytes, with only 12 present.
        let hdr = Header::encode(1, 0, 16);
        mb.push(&hdr).unwrap();
        assert_eq!(mb.next().unwrap(), None);
        mb.push(&[0u8; 4]).unwrap();
        assert_eq!(mb.next().unwrap(), None);
        mb.push(&[0u8; 4]).unwrap();
        assert!(mb.next().unwrap().is_some());
    }
}
