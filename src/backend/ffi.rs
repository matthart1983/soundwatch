//! Hand-rolled CoreAudio HAL bindings.
//!
//! `coreaudio-sys` would drag bindgen and libclang into the build (against the
//! single-static-binary family invariant) and its generated bindings predate the
//! macOS 14 process-object API, which is the only supported way to see per-app
//! audio without inserting a tap. Every selector below was read out of
//! `MacOSX.sdk/.../CoreAudio.framework/Headers`, not from memory.

// The constant table below is a deliberate map of the slice of the HAL this
// tool uses, including a few selectors read during development that the current
// code paths no longer call. They are cheap to keep and expensive to re-derive.
#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::ffi::{CStr, c_char, c_void};
use std::mem::{MaybeUninit, align_of, size_of};
use std::ptr::null;

pub type OSStatus = i32;
pub type AudioObjectID = u32;
pub type CFStringRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioObjectPropertyAddress {
    pub mSelector: u32,
    pub mScope: u32,
    pub mElement: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioStreamBasicDescription {
    pub mSampleRate: f64,
    pub mFormatID: u32,
    pub mFormatFlags: u32,
    pub mBytesPerPacket: u32,
    pub mFramesPerPacket: u32,
    pub mBytesPerFrame: u32,
    pub mChannelsPerFrame: u32,
    pub mBitsPerChannel: u32,
    pub mReserved: u32,
}

/// Four-character-code constants, the way the headers write them.
pub const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

pub const SYSTEM_OBJECT: AudioObjectID = 1;

pub const SCOPE_GLOBAL: u32 = fourcc(b"glob");
pub const SCOPE_INPUT: u32 = fourcc(b"inpt");
pub const SCOPE_OUTPUT: u32 = fourcc(b"outp");
pub const ELEMENT_MAIN: u32 = 0;

// Hardware
pub const HW_DEVICES: u32 = fourcc(b"dev#");
pub const HW_DEFAULT_INPUT: u32 = fourcc(b"dIn ");
pub const HW_DEFAULT_OUTPUT: u32 = fourcc(b"dOut");
/// macOS 14+.
pub const HW_PROCESS_LIST: u32 = fourcc(b"prs#");

// Object
pub const OBJ_NAME: u32 = fourcc(b"lnam");

// Device
pub const DEV_UID: u32 = fourcc(b"uid ");
pub const DEV_NOMINAL_RATE: u32 = fourcc(b"nsrt");
pub const DEV_STREAM_CONFIG: u32 = fourcc(b"slay");
pub const DEV_BUFFER_FRAMES: u32 = fourcc(b"fsiz");
pub const DEV_LATENCY: u32 = fourcc(b"ltnc");
pub const DEV_SAFETY_OFFSET: u32 = fourcc(b"saft");
pub const DEV_IS_RUNNING: u32 = fourcc(b"goin");
pub const DEV_IS_RUNNING_SOMEWHERE: u32 = fourcc(b"gone");
pub const DEV_STREAMS: u32 = fourcc(b"stm#");
pub const DEV_PROCESSOR_OVERLOAD: u32 = fourcc(b"over");

// Stream
pub const STREAM_PHYSICAL_FORMAT: u32 = fourcc(b"pft ");
pub const STREAM_VIRTUAL_FORMAT: u32 = fourcc(b"sfmt");

// Process (macOS 14+)
pub const PROC_PID: u32 = fourcc(b"ppid");
pub const PROC_BUNDLE_ID: u32 = fourcc(b"pbid");
pub const PROC_DEVICES: u32 = fourcc(b"pdv#");
pub const PROC_IS_RUNNING_INPUT: u32 = fourcc(b"piri");
pub const PROC_IS_RUNNING_OUTPUT: u32 = fourcc(b"piro");

const KCF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

pub type AudioObjectPropertyListenerProc = extern "C" fn(
    inObjectID: AudioObjectID,
    inNumberAddresses: u32,
    inAddresses: *const AudioObjectPropertyAddress,
    inClientData: *mut c_void,
) -> OSStatus;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectHasProperty(
        inObjectID: AudioObjectID,
        inAddress: *const AudioObjectPropertyAddress,
    ) -> u8;

    fn AudioObjectGetPropertyDataSize(
        inObjectID: AudioObjectID,
        inAddress: *const AudioObjectPropertyAddress,
        inQualifierDataSize: u32,
        inQualifierData: *const c_void,
        outDataSize: *mut u32,
    ) -> OSStatus;

    fn AudioObjectGetPropertyData(
        inObjectID: AudioObjectID,
        inAddress: *const AudioObjectPropertyAddress,
        inQualifierDataSize: u32,
        inQualifierData: *const c_void,
        ioDataSize: *mut u32,
        outData: *mut c_void,
    ) -> OSStatus;

    fn AudioObjectAddPropertyListener(
        inObjectID: AudioObjectID,
        inAddress: *const AudioObjectPropertyAddress,
        inListener: AudioObjectPropertyListenerProc,
        inClientData: *mut c_void,
    ) -> OSStatus;

    fn AudioObjectRemovePropertyListener(
        inObjectID: AudioObjectID,
        inAddress: *const AudioObjectPropertyAddress,
        inListener: AudioObjectPropertyListenerProc,
        inClientData: *mut c_void,
    ) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringGetCString(
        theString: CFStringRef,
        buffer: *mut c_char,
        bufferSize: isize,
        encoding: u32,
    ) -> u8;
    fn CFStringGetLength(theString: CFStringRef) -> isize;
    fn CFRelease(cf: *const c_void);
}

// libproc and the BSD layer live in libSystem, which is always linked.
unsafe extern "C" {
    fn proc_name(pid: i32, buffer: *mut c_char, buffersize: u32) -> i32;
    fn gethostname(name: *mut c_char, len: usize) -> i32;
}

/// Short hostname for the header, e.g. `jules-mbp`.
pub fn hostname() -> String {
    unsafe {
        let mut buf = vec![0i8; 256];
        if gethostname(buf.as_mut_ptr(), buf.len() - 1) != 0 {
            return "localhost".into();
        }
        CStr::from_ptr(buf.as_ptr())
            .to_str()
            .ok()
            .map(|s| s.split('.').next().unwrap_or(s).to_owned())
            .unwrap_or_else(|| "localhost".into())
    }
}

pub fn addr(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: ELEMENT_MAIN }
}

/// Does this object expose this property at all? Used to detect the macOS 14
/// process-object API without version sniffing.
pub fn has(id: AudioObjectID, a: &AudioObjectPropertyAddress) -> bool {
    unsafe { AudioObjectHasProperty(id, a) != 0 }
}

/// Read one fixed-size value.
pub fn get<T: Copy>(id: AudioObjectID, a: &AudioObjectPropertyAddress) -> Option<T> {
    unsafe {
        let mut size = size_of::<T>() as u32;
        let mut val = MaybeUninit::<T>::uninit();
        let st = AudioObjectGetPropertyData(
            id,
            a,
            0,
            null(),
            &mut size,
            val.as_mut_ptr() as *mut c_void,
        );
        if st == 0 && size as usize == size_of::<T>() { Some(val.assume_init()) } else { None }
    }
}

/// Read a variable-length array of fixed-size values.
pub fn get_array<T: Copy>(id: AudioObjectID, a: &AudioObjectPropertyAddress) -> Vec<T> {
    unsafe {
        let mut size: u32 = 0;
        if AudioObjectGetPropertyDataSize(id, a, 0, null(), &mut size) != 0 || size == 0 {
            return Vec::new();
        }
        let cap = size as usize / size_of::<T>();
        let mut v: Vec<T> = Vec::with_capacity(cap);
        if AudioObjectGetPropertyData(id, a, 0, null(), &mut size, v.as_mut_ptr() as *mut c_void)
            != 0
        {
            return Vec::new();
        }
        v.set_len((size as usize / size_of::<T>()).min(cap));
        v
    }
}

/// Read a property's raw bytes, for structs with trailing variable-length data
/// such as `AudioBufferList`.
pub fn get_bytes(id: AudioObjectID, a: &AudioObjectPropertyAddress) -> Option<Vec<u8>> {
    unsafe {
        let mut size: u32 = 0;
        if AudioObjectGetPropertyDataSize(id, a, 0, null(), &mut size) != 0 || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        if AudioObjectGetPropertyData(id, a, 0, null(), &mut size, buf.as_mut_ptr() as *mut c_void)
            != 0
        {
            return None;
        }
        buf.truncate(size as usize);
        Some(buf)
    }
}

/// Read a `CFStringRef` property and take ownership of it.
pub fn get_string(id: AudioObjectID, a: &AudioObjectPropertyAddress) -> Option<String> {
    let cf: CFStringRef = get(id, a)?;
    if cf.is_null() {
        return None;
    }
    let out = cfstring(cf);
    unsafe { CFRelease(cf) };
    out
}

fn cfstring(cf: CFStringRef) -> Option<String> {
    unsafe {
        let len = CFStringGetLength(cf);
        if len <= 0 {
            return None;
        }
        // Worst case 4 UTF-8 bytes per UTF-16 unit, plus the NUL.
        let cap = (len as usize) * 4 + 1;
        let mut buf = vec![0i8; cap];
        if CFStringGetCString(cf, buf.as_mut_ptr(), cap as isize, KCF_STRING_ENCODING_UTF8) == 0 {
            return None;
        }
        CStr::from_ptr(buf.as_ptr()).to_str().ok().map(str::to_owned)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioBuffer {
    pub mNumberChannels: u32,
    pub mDataByteSize: u32,
    pub mData: *mut c_void,
}

/// Total channels in a scope, summed across the buffers of an `AudioBufferList`.
///
/// Layout: `UInt32 mNumberBuffers`, then a pointer-aligned `AudioBuffer[]`. The
/// array does *not* begin at offset 4 — `AudioBuffer` is 8-aligned because of
/// `mData`, so there are four bytes of padding and `mBuffers` starts at offset 8.
pub fn channel_count(id: AudioObjectID, scope: u32) -> u32 {
    let Some(bytes) = get_bytes(id, &addr(DEV_STREAM_CONFIG, scope)) else {
        return 0;
    };
    if bytes.len() < 4 {
        return 0;
    }
    let n = u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let stride = size_of::<AudioBuffer>();
    let base = size_of::<u32>().next_multiple_of(align_of::<AudioBuffer>());
    (0..n)
        .filter_map(|i| {
            let off = base + i * stride;
            bytes.get(off..off + 4).map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
        })
        .sum()
}

/// Best-effort process name for a pid.
pub fn process_name(pid: i32) -> Option<String> {
    unsafe {
        let mut buf = vec![0i8; 256];
        let n = proc_name(pid, buf.as_mut_ptr(), buf.len() as u32);
        if n <= 0 {
            return None;
        }
        CStr::from_ptr(buf.as_ptr()).to_str().ok().map(str::to_owned)
    }
}

/// Register a listener. `data` must outlive the listener.
pub fn add_listener(
    id: AudioObjectID,
    a: &AudioObjectPropertyAddress,
    f: AudioObjectPropertyListenerProc,
    data: *mut c_void,
) -> bool {
    unsafe { AudioObjectAddPropertyListener(id, a, f, data) == 0 }
}

/// Unregister a listener. The `(address, proc, data)` triple must match the one
/// passed to [`add_listener`], which is how the HAL identifies the
/// registration. Failure is expected and ignorable when the object has already
/// gone away.
pub fn remove_listener(
    id: AudioObjectID,
    a: &AudioObjectPropertyAddress,
    f: AudioObjectPropertyListenerProc,
    data: *mut c_void,
) -> bool {
    unsafe { AudioObjectRemovePropertyListener(id, a, f, data) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_matches_the_headers() {
        // 'dev#' = 0x64657623
        assert_eq!(HW_DEVICES, 0x6465_7623);
        assert_eq!(SCOPE_GLOBAL, 0x676c_6f62); // 'glob'
        assert_eq!(HW_DEFAULT_INPUT, 0x6449_6e20); // 'dIn ' — note trailing space
        assert_eq!(STREAM_PHYSICAL_FORMAT, 0x7066_7420); // 'pft ' — trailing space
        assert_eq!(PROC_IS_RUNNING_INPUT, 0x7069_7269); // 'piri'
    }

    #[test]
    fn the_system_object_reports_devices() {
        // This is a real machine; there is always at least one audio device.
        let ids: Vec<AudioObjectID> = get_array(SYSTEM_OBJECT, &addr(HW_DEVICES, SCOPE_GLOBAL));
        assert!(!ids.is_empty(), "no audio devices found");
    }

    #[test]
    fn devices_have_names_and_rates() {
        let ids: Vec<AudioObjectID> = get_array(SYSTEM_OBJECT, &addr(HW_DEVICES, SCOPE_GLOBAL));
        let named = ids
            .iter()
            .filter(|&&id| get_string(id, &addr(OBJ_NAME, SCOPE_GLOBAL)).is_some())
            .count();
        assert!(named > 0, "no device reported a name");

        let rated = ids
            .iter()
            .filter_map(|&id| get::<f64>(id, &addr(DEV_NOMINAL_RATE, SCOPE_GLOBAL)))
            .filter(|r| *r > 0.0)
            .count();
        assert!(rated > 0, "no device reported a sample rate");
    }

    #[test]
    fn channel_counts_parse() {
        let ids: Vec<AudioObjectID> = get_array(SYSTEM_OBJECT, &addr(HW_DEVICES, SCOPE_GLOBAL));
        let total: u32 = ids
            .iter()
            .map(|&id| channel_count(id, SCOPE_OUTPUT) + channel_count(id, SCOPE_INPUT))
            .sum();
        assert!(total > 0, "every device reported zero channels");
    }
}
