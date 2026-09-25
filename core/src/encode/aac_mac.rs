//! AAC through the system AudioToolbox framework (macOS only, nothing to install).

use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::{Encoder, Spec, enc_err};
use crate::Result;

#[repr(C)]
#[derive(Default)]
struct Asbd {
    rate: f64,
    format: u32,
    flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels: u32,
    bits: u32,
    reserved: u32,
}

#[repr(C)]
struct AudioBuffer {
    channels: u32,
    size: u32,
    data: *const c_void,
}

/// `AudioBufferList` with one buffer; `repr(C)` adds the padding before the pointer.
#[repr(C)]
struct BufferList {
    count: u32,
    buf: AudioBuffer,
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn ExtAudioFileCreateWithURL(
        url: *const c_void,
        kind: u32,
        fmt: *const Asbd,
        layout: *const c_void,
        flags: u32,
        out: *mut *mut c_void,
    ) -> i32;
    fn ExtAudioFileSetProperty(f: *mut c_void, id: u32, size: u32, data: *const c_void) -> i32;
    fn ExtAudioFileGetProperty(f: *mut c_void, id: u32, size: *mut u32, data: *mut c_void) -> i32;
    fn ExtAudioFileWrite(f: *mut c_void, frames: u32, data: *const BufferList) -> i32;
    fn ExtAudioFileDispose(f: *mut c_void) -> i32;
    fn AudioConverterSetProperty(c: *mut c_void, id: u32, size: u32, data: *const c_void) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(a: *const c_void, p: *const u8, len: isize, dir: u8) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

fn check(code: i32, what: &str) -> Result<()> {
    (code == 0)
        .then_some(())
        .ok_or_else(|| enc_err(format!("AudioToolbox {what}: OSStatus {code}")))
}

pub struct AudioToolbox {
    file: *mut c_void,
    channels: u32,
}

// The handle is only ever used from one thread at a time.
unsafe impl Send for AudioToolbox {}

impl AudioToolbox {
    pub fn new(spec: Spec, path: &Path) -> Result<Self> {
        let p = path.as_os_str().as_bytes();
        let aac = Asbd {
            rate: spec.rate as f64,
            format: fourcc(b"aac "),
            frames_per_packet: 1024,
            channels: spec.channels as u32,
            ..Default::default()
        };
        let bytes = 4 * spec.channels as u32;
        let pcm = Asbd {
            rate: spec.rate as f64,
            format: fourcc(b"lpcm"),
            flags: 1 | 8, // float | packed
            bytes_per_packet: bytes,
            frames_per_packet: 1,
            bytes_per_frame: bytes,
            channels: spec.channels as u32,
            bits: 32,
            reserved: 0,
        };
        let mut file = std::ptr::null_mut();
        unsafe {
            let url = CFURLCreateFromFileSystemRepresentation(std::ptr::null(), p.as_ptr(), p.len() as isize, 0);
            let code = ExtAudioFileCreateWithURL(url, fourcc(b"m4af"), &aac, std::ptr::null(), 1, &mut file);
            CFRelease(url);
            check(code, "create")?;
            check(
                ExtAudioFileSetProperty(file, fourcc(b"cfmt"), size_of::<Asbd>() as u32, (&pcm as *const Asbd).cast()),
                "client format",
            )?;
            let mut conv: *mut c_void = std::ptr::null_mut();
            let mut size = size_of::<*mut c_void>() as u32;
            if ExtAudioFileGetProperty(file, fourcc(b"acnv"), &mut size, (&mut conv as *mut *mut c_void).cast()) == 0 && !conv.is_null() {
                let bps: u32 = spec.kbps * 1000;
                AudioConverterSetProperty(conv, fourcc(b"brat"), 4, (&bps as *const u32).cast());
                let none: *const c_void = std::ptr::null();
                ExtAudioFileSetProperty(
                    file,
                    fourcc(b"acfg"),
                    size_of::<*const c_void>() as u32,
                    (&none as *const *const c_void).cast(),
                );
            }
        }
        Ok(Self {
            file,
            channels: spec.channels as u32,
        })
    }
}

impl Encoder for AudioToolbox {
    fn write(&mut self, pcm: &[f32]) -> Result<()> {
        let frames = pcm.len() as u32 / self.channels;
        let list = BufferList {
            count: 1,
            buf: AudioBuffer {
                channels: self.channels,
                size: (pcm.len() * 4) as u32,
                data: pcm.as_ptr().cast(),
            },
        };
        check(unsafe { ExtAudioFileWrite(self.file, frames, &list) }, "write")
    }

    fn finish(self: Box<Self>) -> Result<()> {
        check(unsafe { ExtAudioFileDispose(self.file) }, "close")
    }
}
