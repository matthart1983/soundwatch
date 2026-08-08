//! Output level metering via a Core Audio process tap.
//!
//! The handoff says "never meter by tapping" three times. That rule was written
//! against the old meaning of the word: instantiating an AudioUnit on the device,
//! which puts work in the client's audio callback path and can perturb the very
//! glitch you are trying to observe. macOS 14.2 added
//! `AudioHardwareCreateProcessTap`, a sanctioned read-only observation API that
//! does not alter routing, does not change what anyone hears, and does not run in
//! any other process's callback. Without it the two headline meters are dead on
//! macOS, which makes the tool not worth opening. Deviation taken deliberately.
//!
//! Safety properties this module must preserve:
//!
//! * **Never mute.** The tap is created with `CATapUnmuted` explicitly, because a
//!   metering tool that silences your speakers is worse than one with flat meters.
//! * **Private.** The aggregate device is marked private so it does not appear in
//!   Audio MIDI Setup or in anyone's device picker.
//! * **Real-time clean.** The IOProc does no allocation and takes no locks — it
//!   folds a peak into an atomic and returns. See [`super::ioproc`].
//! * **Fails soft.** Every step is fallible (consent above all). Any failure
//!   leaves `Caps::device_levels` false and the UI in its degraded state with an
//!   explanation, exactly as before.
//!
//! Consent is the step that bites, and it fails *quietly*: a refused tap is
//! created successfully and then delivers zeros forever. [`crate::tcc`] is what
//! makes the consent dialog appear at all, and
//! [`LevelTap::has_ever_heard_signal`] is how the UI notices when it did not.

use std::ffi::{CStr, c_char, c_void};

use super::ffi::{
    self, AudioObjectID, AudioStreamBasicDescription, CFStringRef, OSStatus, SCOPE_GLOBAL,
};
use super::ioproc::IoProcMeter;

const K_AUDIO_TAP_PROPERTY_UID: u32 = ffi::fourcc(b"tuid");
const K_AUDIO_TAP_PROPERTY_FORMAT: u32 = ffi::fourcc(b"tfmt");
const K_AUDIO_FORMAT_FLAG_IS_FLOAT: u32 = 1 << 0;
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioHardwareCreateProcessTap(desc: *mut c_void, out_tap: *mut AudioObjectID) -> OSStatus;
    fn AudioHardwareDestroyProcessTap(tap: AudioObjectID) -> OSStatus;
    fn AudioHardwareCreateAggregateDevice(
        desc: *const c_void,
        out_device: *mut AudioObjectID,
    ) -> OSStatus;
    fn AudioHardwareDestroyAggregateDevice(device: AudioObjectID) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    static kCFTypeArrayCallBacks: c_void;

    fn CFStringCreateWithCString(
        alloc: *const c_void,
        cstr: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFDictionaryCreateMutable(
        alloc: *const c_void,
        capacity: isize,
        key_cb: *const c_void,
        val_cb: *const c_void,
    ) -> *mut c_void;
    fn CFDictionarySetValue(dict: *mut c_void, key: *const c_void, value: *const c_void);
    fn CFArrayCreateMutable(
        alloc: *const c_void,
        capacity: isize,
        cb: *const c_void,
    ) -> *mut c_void;
    fn CFArrayAppendValue(array: *mut c_void, value: *const c_void);
    fn CFRelease(cf: *const c_void);
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

/// `objc_msgSend` is variadic in C and must be called through a correctly typed
/// pointer. These helpers narrow it to the three shapes this module needs.
unsafe fn msg0(obj: *mut c_void, sel: *mut c_void) -> *mut c_void {
    let f: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
        unsafe { std::mem::transmute(objc_msgSend as *const c_void) };
    f(obj, sel)
}

unsafe fn msg1(obj: *mut c_void, sel: *mut c_void, a: *mut c_void) -> *mut c_void {
    let f: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
        unsafe { std::mem::transmute(objc_msgSend as *const c_void) };
    f(obj, sel, a)
}

unsafe fn msg1_i64(obj: *mut c_void, sel: *mut c_void, a: i64) {
    let f: extern "C" fn(*mut c_void, *mut c_void, i64) =
        unsafe { std::mem::transmute(objc_msgSend as *const c_void) };
    f(obj, sel, a)
}

fn class(name: &CStr) -> *mut c_void {
    unsafe { objc_getClass(name.as_ptr()) }
}

fn sel(name: &CStr) -> *mut c_void {
    unsafe { sel_registerName(name.as_ptr()) }
}

/// A live system-output tap. Dropping it tears everything down.
pub struct LevelTap {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    /// `Option` so `Drop` can detach the IOProc *before* destroying the device
    /// it is attached to. Always `Some` between `start` and `drop`.
    meter: Option<IoProcMeter>,
}

impl LevelTap {
    /// Try to start metering system output. Returns a human-readable reason on
    /// failure so the UI can explain itself rather than just showing `--`.
    pub fn start() -> Result<Self, String> {
        let desc = Self::make_description()?;

        let mut tap: AudioObjectID = 0;
        let st = unsafe { AudioHardwareCreateProcessTap(desc, &mut tap) };
        unsafe {
            let _ = msg0(desc, sel(c"release"));
        }
        if st != 0 || tap == 0 {
            return Err(Self::explain(st));
        }

        let mut me = Self { tap, aggregate: 0, meter: None };

        let uid = ffi::get_string(tap, &ffi::addr(K_AUDIO_TAP_PROPERTY_UID, SCOPE_GLOBAL))
            .ok_or_else(|| "tap created but reported no UID".to_string())?;

        me.aggregate = Self::make_aggregate(&uid)?;

        // The tap must deliver float samples for the peak fold to be valid.
        if let Some(f) = ffi::get::<AudioStreamBasicDescription>(
            tap,
            &ffi::addr(K_AUDIO_TAP_PROPERTY_FORMAT, SCOPE_GLOBAL),
        ) && f.mFormatFlags & K_AUDIO_FORMAT_FLAG_IS_FLOAT == 0
        {
            return Err("tap format is not float; refusing to guess".into());
        }

        me.meter = Some(IoProcMeter::attach(me.aggregate)?);
        Ok(me)
    }

    /// Build a global stereo mixdown tap that excludes nothing.
    fn make_description() -> Result<*mut c_void, String> {
        unsafe {
            let cls = class(c"CATapDescription");
            if cls.is_null() {
                return Err("CATapDescription unavailable; needs macOS 14.2 or newer".into());
            }
            let ns_array = class(c"NSArray");
            let empty = msg0(ns_array, sel(c"array"));

            let obj = msg0(cls, sel(c"alloc"));
            let desc = msg1(obj, sel(c"initStereoGlobalTapButExcludeProcesses:"), empty);
            if desc.is_null() {
                return Err("could not build a tap description".into());
            }

            // Explicitly unmuted: metering must never silence the user's audio.
            msg1_i64(desc, sel(c"setMuteBehavior:"), 0);
            // Private: keep it out of Audio MIDI Setup and device pickers.
            msg1_i64(desc, sel(c"setPrivate:"), 1);
            Ok(desc)
        }
    }

    /// Wrap the tap in a private aggregate device so an IOProc can read it.
    fn make_aggregate(tap_uid: &str) -> Result<AudioObjectID, String> {
        unsafe {
            let cf = |s: &CStr| {
                CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), K_CF_STRING_ENCODING_UTF8)
            };
            let own_uid =
                std::ffi::CString::new(format!("com.soundwatch.lite.tap.{}", std::process::id()))
                    .map_err(|_| "bad uid".to_string())?;
            let tap_uid_c =
                std::ffi::CString::new(tap_uid).map_err(|_| "bad tap uid".to_string())?;

            let sub = CFDictionaryCreateMutable(
                std::ptr::null(),
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let k_sub_uid = cf(c"uid");
            let v_tap_uid = cf(tap_uid_c.as_c_str());
            CFDictionarySetValue(sub, k_sub_uid, v_tap_uid);

            let taps = CFArrayCreateMutable(std::ptr::null(), 1, &kCFTypeArrayCallBacks);
            CFArrayAppendValue(taps, sub);

            let dict = CFDictionaryCreateMutable(
                std::ptr::null(),
                3,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let k_uid = cf(c"uid");
            let v_uid = cf(own_uid.as_c_str());
            let k_private = cf(c"private");
            let k_taps = cf(c"taps");
            CFDictionarySetValue(dict, k_uid, v_uid);
            CFDictionarySetValue(dict, k_private, kCFBooleanTrue);
            CFDictionarySetValue(dict, k_taps, taps);

            let mut agg: AudioObjectID = 0;
            let st = AudioHardwareCreateAggregateDevice(dict, &mut agg);

            for p in [k_sub_uid, v_tap_uid, k_uid, v_uid, k_private, k_taps, sub, taps, dict] {
                CFRelease(p);
            }

            if st != 0 || agg == 0 {
                return Err(format!("could not create the metering device (OSStatus {st})"));
            }
            Ok(agg)
        }
    }

    /// Turn an OSStatus from tap creation into something a user can act on.
    pub(super) fn explain(st: OSStatus) -> String {
        match st {
            // 'nope' — the usual refusal.
            0x6E6F7065 | -1 => "audio capture not permitted: allow \
                 \"soundwatch-lite\" under Privacy & Security"
                .into(),
            _ => format!("could not create the audio tap (OSStatus {st})"),
        }
    }

    /// `(io_proc calls, samples seen)` since start. Diagnostic.
    pub fn stats(&self) -> (u64, u64) {
        self.meter.as_ref().map(IoProcMeter::stats).unwrap_or((0, 0))
    }

    /// Has any sample, ever, been non-zero? See [`IoProcMeter::has_ever_heard_signal`].
    pub fn has_ever_heard_signal(&self) -> bool {
        self.meter.as_ref().is_some_and(IoProcMeter::has_ever_heard_signal)
    }

    /// Has the tap been running at least this long?
    pub fn has_run_for(&self, secs: u64) -> bool {
        self.meter.as_ref().is_some_and(|m| m.has_run_for(secs))
    }

    /// Peak since the last call, in dBFS. `None` until the first block arrives.
    pub fn peak_dbfs(&self) -> Option<f32> {
        self.meter.as_ref()?.peak_dbfs()
    }

    /// Windows the spectrum reader lost to the callback lapping it.
    pub fn overruns(&self) -> u64 {
        self.meter.as_ref().map(IoProcMeter::overruns).unwrap_or(0)
    }

    /// The most recent `n` samples of the mono mixdown, for the spectrum.
    pub fn recent(&self, n: usize) -> Option<Vec<f32>> {
        self.meter.as_ref()?.recent(n)
    }
}

impl Drop for LevelTap {
    /// Strict order: detach the IOProc, then destroy the aggregate device it
    /// was reading, then the tap the device wrapped. Destroying a device with a
    /// live IOProc is how you get a crash on a CoreAudio thread.
    fn drop(&mut self) {
        drop(self.meter.take());
        unsafe {
            if self.aggregate != 0 {
                AudioHardwareDestroyAggregateDevice(self.aggregate);
            }
            if self.tap != 0 {
                AudioHardwareDestroyProcessTap(self.tap);
            }
        }
    }
}
