//! Becoming our own TCC subject, so macOS can ask the user about audio capture.
//!
//! `AudioHardwareCreateProcessTap` is gated on `kTCCServiceAudioCapture`. TCC
//! does not evaluate the process that made the call — it evaluates the
//! *responsible* process, which for anything started from a shell is the
//! terminal emulator. Terminals do not ship `NSAudioCaptureUsageDescription`,
//! so tccd refuses to display a consent dialog at all:
//!
//! ```text
//! AUTHREQ_PROMPTING: service=kTCCServiceAudioCapture, subject=Sub:{com.mitchellh.ghostty}
//! Refusing authorization request for service kTCCServiceAudioCapture and subject
//! Sub:{com.mitchellh.ghostty} ... without NSAudioCaptureUsageDescription key
//! ```
//!
//! The failure is silent by design: the tap is created, `AudioDeviceStart`
//! succeeds, the IOProc fires at the right rate, and every sample is 0.0
//! forever. That is indistinguishable from a quiet machine unless you know to
//! look, and it is why the meters were flat.
//!
//! The fix has two halves:
//!
//! 1. `build.rs` embeds `Info.plist` into `__TEXT,__info_plist`, giving this
//!    binary a bundle identifier and the usage description TCC demands.
//! 2. This module re-executes the process with responsibility *disclaimed*, so
//!    the kernel stops pointing at the terminal and we become the subject of
//!    our own permission. Then the dialog appears, the user can answer it, and
//!    the grant is remembered against this binary.
//!
//! Both halves are required. The plist alone changes nothing, because the
//! terminal is still answering for us.
//!
//! `responsibility_spawnattrs_setdisclaim` is SPI. It is resolved with `dlsym`
//! rather than linked, so a macOS that drops it degrades to "metering
//! unavailable, here is why" instead of failing to launch.

use std::ffi::{CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;

/// Set in the re-executed image so it does not re-execute again.
pub const DISCLAIM_ENV: &str = "SOUNDWATCH_TCC_DISCLAIMED";

/// What `Info.plist` declares, and what a correctly signed binary reports back.
pub const BUNDLE_ID: &str = "com.soundwatch.lite";

const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
/// Replace the current process image rather than forking a child.
const POSIX_SPAWN_SETEXEC: libc::c_short = 0x0040;

type SetDisclaimFn = unsafe extern "C" fn(*mut libc::posix_spawnattr_t, c_int) -> c_int;

/// Is this process already answering for its own audio permissions?
pub fn is_own_subject() -> bool {
    std::env::var_os(DISCLAIM_ENV).is_some()
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecCodeCopySelf(flags: u32, code: *mut *const c_void) -> i32;
    fn SecCodeCopySigningInformation(
        code: *const c_void,
        flags: u32,
        information: *mut *const c_void,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        cstr: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFStringGetCString(s: *const c_void, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFRelease(cf: *const c_void);
}

const KSEC_CS_SIGNING_INFORMATION: u32 = 1 << 1;
const KCF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// The identifier this binary's code signature actually carries.
///
/// `cargo build` produces a *linker-signed* binary, whose signature does not
/// bind the embedded `Info.plist` — `codesign -dvv` says `Info.plist=not
/// bound`, and the identifier is a mangled `soundwatch_lite-<hash>` taken from
/// the filename. TCC then has no usage description to show and no stable
/// identity to record, so metering silently never works.
///
/// Re-signing with `codesign -f -s -` binds the plist and the identifier
/// becomes [`BUNDLE_ID`]. Reading it back is how [`signing_advice`] can tell
/// "the user said no" apart from "this build was never signed properly", which
/// otherwise look identical: a meter full of zeros.
pub fn signing_identifier() -> Option<String> {
    unsafe {
        let mut code: *const c_void = std::ptr::null();
        if SecCodeCopySelf(0, &mut code) != 0 || code.is_null() {
            return None;
        }
        let mut info: *const c_void = std::ptr::null();
        let st = SecCodeCopySigningInformation(code, KSEC_CS_SIGNING_INFORMATION, &mut info);
        CFRelease(code);
        if st != 0 || info.is_null() {
            return None;
        }

        // kSecCodeInfoIdentifier, by name rather than by linking the symbol.
        let key = CFStringCreateWithCString(
            std::ptr::null(),
            c"identifier".as_ptr(),
            KCF_STRING_ENCODING_UTF8,
        );
        let value = CFDictionaryGetValue(info, key);
        let out = if value.is_null() {
            None
        } else {
            let mut buf = vec![0i8; 512];
            if CFStringGetCString(value, buf.as_mut_ptr(), 512, KCF_STRING_ENCODING_UTF8) != 0 {
                std::ffi::CStr::from_ptr(buf.as_ptr()).to_str().ok().map(str::to_owned)
            } else {
                None
            }
        };
        if !key.is_null() {
            CFRelease(key);
        }
        CFRelease(info);
        out
    }
}

/// `Some(reason)` when this build cannot obtain audio consent no matter what
/// the user clicks, because its signature does not carry the `Info.plist`.
pub fn signing_advice() -> Option<String> {
    match signing_identifier() {
        Some(id) if id == BUNDLE_ID => None,
        _ => Some(
            "this build is not signed with its Info.plist, so macOS cannot ask \
             for audio consent — run `make` (or `codesign -f -s -` on the binary)"
                .into(),
        ),
    }
}

/// Look up the disclaim SPI. `None` on a system that no longer exports it.
fn disclaim_fn() -> Option<SetDisclaimFn> {
    let name = c"responsibility_spawnattrs_setdisclaim";
    let sym = unsafe { libc::dlsym(RTLD_DEFAULT, name.as_ptr()) };
    if sym.is_null() {
        return None;
    }
    // SAFETY: the symbol is the documented SPI shape, checked non-null above.
    Some(unsafe { std::mem::transmute::<*mut c_void, SetDisclaimFn>(sym) })
}

/// Re-execute this process as its own TCC subject.
///
/// On success this **does not return** — `POSIX_SPAWN_SETEXEC` replaces the
/// running image in place, so there is no second process, no extra pid, and no
/// change to job control or signal delivery. On failure it returns a reason the
/// caller can show, having changed nothing.
pub fn adopt_own_identity() -> Result<std::convert::Infallible, String> {
    if is_own_subject() {
        return Err("already disclaimed".into());
    }
    let Some(set_disclaim) = disclaim_fn() else {
        return Err("this macOS does not support disclaiming TCC responsibility".into());
    };

    let exe = std::env::current_exe().map_err(|e| format!("cannot locate own binary: {e}"))?;
    let exe_c = CString::new(exe.as_os_str().as_bytes())
        .map_err(|_| "own path contains a NUL byte".to_string())?;

    // argv, verbatim, so flags survive the hand-off.
    let argv_owned: Vec<CString> = std::env::args_os()
        .map(|a| CString::new(a.as_bytes()).unwrap_or_else(|_| CString::new("").unwrap()))
        .collect();
    let mut argv: Vec<*mut c_char> = argv_owned.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());

    // The environment, plus the marker that stops the next image looping.
    let mut env_owned: Vec<CString> = std::env::vars_os()
        .filter(|(k, _)| k != DISCLAIM_ENV)
        .filter_map(|(k, v)| {
            let mut b = k.as_bytes().to_vec();
            b.push(b'=');
            b.extend_from_slice(v.as_bytes());
            CString::new(b).ok()
        })
        .collect();
    env_owned.push(CString::new(format!("{DISCLAIM_ENV}=1")).expect("no NUL"));
    let mut envp: Vec<*mut c_char> = env_owned.iter().map(|e| e.as_ptr() as *mut c_char).collect();
    envp.push(std::ptr::null_mut());

    unsafe {
        let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
        if libc::posix_spawnattr_init(&mut attr) != 0 {
            return Err("posix_spawnattr_init failed".into());
        }
        // Guard every early return past this point.
        let cleanup = |attr: &mut libc::posix_spawnattr_t| {
            libc::posix_spawnattr_destroy(attr);
        };

        if set_disclaim(&mut attr, 1) != 0 {
            cleanup(&mut attr);
            return Err("could not disclaim TCC responsibility".into());
        }
        if libc::posix_spawnattr_setflags(&mut attr, POSIX_SPAWN_SETEXEC) != 0 {
            cleanup(&mut attr);
            return Err("posix_spawnattr_setflags failed".into());
        }

        // With SETEXEC this only returns on failure.
        let rc = libc::posix_spawn(
            std::ptr::null_mut(),
            exe_c.as_ptr(),
            std::ptr::null(),
            &attr,
            argv.as_ptr(),
            envp.as_ptr(),
        );
        cleanup(&mut attr);
        Err(format!("could not re-exec as an audio client (errno {rc})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_disclaim_spi_is_present_on_this_macos() {
        // If this ever fails, metering degrades rather than breaking — but the
        // degradation path is worth knowing about deliberately, not by surprise.
        assert!(
            disclaim_fn().is_some(),
            "responsibility_spawnattrs_setdisclaim is gone; \
             tap metering will fall back to the explanatory degraded state"
        );
    }

    #[test]
    fn the_marker_env_var_is_what_stops_the_loop() {
        // The re-executed image must be able to tell it is the second one, or
        // adopt_own_identity() would exec forever.
        let saved = std::env::var_os(DISCLAIM_ENV);
        // SAFETY: single-threaded assertion on a variable only this test touches.
        unsafe { std::env::set_var(DISCLAIM_ENV, "1") };
        assert!(is_own_subject());
        assert!(adopt_own_identity().is_err(), "must refuse to exec twice");
        unsafe {
            match saved {
                Some(v) => std::env::set_var(DISCLAIM_ENV, v),
                None => std::env::remove_var(DISCLAIM_ENV),
            }
        }
    }
}
