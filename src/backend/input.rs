//! Input level metering, by opening a capture stream on the default input.
//!
//! This is the one place where the read-only rule genuinely bends, and it is
//! why the feature is behind `--meter-input` instead of on by default.
//!
//! Output has `AudioHardwareCreateProcessTap`, an API whose entire purpose is
//! observation without participation. Input has no equivalent. Reading a
//! microphone level means being a microphone client: the orange indicator comes
//! on, the device may be started if nothing else is using it, and this process
//! appears in the very mic-in-use audit the stream table exists to show. None
//! of that is hidden — the tool reports itself honestly rather than filtering
//! itself out — but it is a decision the user makes, not one made for them.
//!
//! What is still guaranteed: nothing is recorded, nothing is written to disk,
//! no sample leaves the IOProc, and the only state that escapes the real-time
//! thread is one atomic peak. See [`super::ioproc`].

use super::ffi::{self, AudioObjectID, AudioStreamBasicDescription};
use super::ioproc::IoProcMeter;

const K_AUDIO_FORMAT_FLAG_IS_FLOAT: u32 = 1 << 0;

/// A running meter on the default input device.
pub struct InputMeter {
    meter: IoProcMeter,
}

impl InputMeter {
    /// Open the default input device and start metering it.
    pub fn start() -> Result<Self, String> {
        let device = ffi::get::<AudioObjectID>(
            ffi::SYSTEM_OBJECT,
            &ffi::addr(ffi::HW_DEFAULT_INPUT, ffi::SCOPE_GLOBAL),
        )
        .filter(|id| *id != 0)
        .ok_or_else(|| "no default input device".to_string())?;

        if ffi::channel_count(device, ffi::SCOPE_INPUT) == 0 {
            return Err("the default input device has no input channels".into());
        }

        // The IOProc is handed the stream's *virtual* format, which the HAL
        // converts to on our behalf. In practice that is always Float32, but
        // reinterpreting integer samples as floats would produce a meter that
        // is confidently wrong rather than absent — so check, and decline.
        let streams: Vec<AudioObjectID> =
            ffi::get_array(device, &ffi::addr(ffi::DEV_STREAMS, ffi::SCOPE_INPUT));
        if let Some(&sid) = streams.first()
            && let Some(f) = ffi::get::<AudioStreamBasicDescription>(
                sid,
                &ffi::addr(ffi::STREAM_VIRTUAL_FORMAT, ffi::SCOPE_GLOBAL),
            )
            && f.mFormatFlags & K_AUDIO_FORMAT_FLAG_IS_FLOAT == 0
        {
            return Err("input format is not float; refusing to guess".into());
        }

        let meter = IoProcMeter::attach(device).map_err(|e| {
            // The overwhelmingly likely cause, and the one with a fix.
            format!("{e} \u{2014} microphone access may not be allowed")
        })?;
        Ok(Self { meter })
    }

    /// Peak since the last call, in dBFS. `None` until the first block arrives.
    pub fn peak_dbfs(&self) -> Option<f32> {
        self.meter.peak_dbfs()
    }

    /// Has any sample, ever, been non-zero? Microphone consent fails the same
    /// quiet way audio-capture consent does: zeros, not an error.
    pub fn has_ever_heard_signal(&self) -> bool {
        self.meter.has_ever_heard_signal()
    }

    /// Has the meter been running at least this long?
    pub fn has_run_for(&self, secs: u64) -> bool {
        self.meter.has_run_for(secs)
    }
}
