//! The Mac's speakers: a CoreAudio output unit that plays what the title's
//! WASAPI clients rendered, interleaved stereo float at 48 kHz, from a
//! queue the clients fill each period.
//! ponytail: the queue keeps at most 200 ms (older audio is dropped), and an
//! empty queue plays silence: the title's 10 ms clock and the device's
//! drift apart slowly, which this absorbs.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{Mutex, Once};

/// Interleaved stereo samples waiting to play.
static QUEUE: Mutex<VecDeque<f32>> = Mutex::new(VecDeque::new());

const RATE: f64 = 48_000.0;
/// At most this many samples wait (200 ms of stereo).
const MOST: usize = 48_000 / 5 * 2;

/// Queues interleaved stereo frames to play, starting the output first.
pub(super) fn play(samples: &[f32]) {
    static START: Once = Once::new();
    START.call_once(start);
    static HEARD: Once = Once::new();
    if samples.iter().any(|s| s.abs() > 1e-4) {
        HEARD.call_once(|| tracing::info!("sound: the title's first audible samples"));
    }
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    queue.extend(samples.iter().copied());
    let extra = queue.len().saturating_sub(MOST);
    queue.drain(..extra - extra % 2);
}

#[repr(C)]
struct ComponentDescription {
    kind: u32,
    subtype: u32,
    manufacturer: u32,
    flags: u32,
    mask: u32,
}

#[repr(C)]
struct StreamFormat {
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
struct Buffer {
    channels: u32,
    bytes: u32,
    data: *mut c_void,
}

#[repr(C)]
struct BufferList {
    count: u32,
    buffers: [Buffer; 1],
}

type RenderCallback = extern "C" fn(*mut c_void, *mut u32, *const c_void, u32, u32, *mut BufferList) -> i32;

#[repr(C)]
struct CallbackStruct {
    callback: RenderCallback,
    context: *mut c_void,
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioComponentFindNext(after: *mut c_void, description: *const ComponentDescription) -> *mut c_void;
    fn AudioComponentInstanceNew(component: *mut c_void, out: *mut *mut c_void) -> i32;
    fn AudioUnitSetProperty(unit: *mut c_void, property: u32, scope: u32, element: u32, data: *const c_void, size: u32) -> i32;
    fn AudioUnitInitialize(unit: *mut c_void) -> i32;
    fn AudioOutputUnitStart(unit: *mut c_void) -> i32;
}

const fn four(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

/// Fills CoreAudio's buffer from the queue, silence where it runs short.
extern "C" fn render(_: *mut c_void, _: *mut u32, _: *const c_void, _: u32, frames: u32, data: *mut BufferList) -> i32 {
    // SAFETY: CoreAudio's buffer list: one interleaved buffer of `frames`.
    let out = unsafe {
        let buffer = &(*data).buffers[0];
        std::slice::from_raw_parts_mut(buffer.data.cast::<f32>(), frames as usize * 2)
    };
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    for sample in out.iter_mut() {
        *sample = queue.pop_front().unwrap_or(0.0);
    }
    0
}

/// The default output unit, playing from [`QUEUE`]. A Mac without one
/// stays silent.
fn start() {
    const OUTPUT: u32 = four(b"auou");
    const DEFAULT_OUTPUT: u32 = four(b"def ");
    const APPLE: u32 = four(b"appl");
    const STREAM_FORMAT: u32 = 8;
    const SET_RENDER_CALLBACK: u32 = 23;
    const INPUT_SCOPE: u32 = 1;
    const LINEAR_PCM: u32 = four(b"lpcm");
    const FLOAT_PACKED: u32 = 1 | 8;
    let description = ComponentDescription { kind: OUTPUT, subtype: DEFAULT_OUTPUT, manufacturer: APPLE, flags: 0, mask: 0 };
    let format = StreamFormat {
        rate: RATE,
        format: LINEAR_PCM,
        flags: FLOAT_PACKED,
        bytes_per_packet: 8,
        frames_per_packet: 1,
        bytes_per_frame: 8,
        channels: 2,
        bits: 32,
        reserved: 0,
    };
    let callback = CallbackStruct { callback: render, context: std::ptr::null_mut() };
    // SAFETY: CoreAudio's own calls with our structures, which it copies.
    let status = unsafe {
        let component = AudioComponentFindNext(std::ptr::null_mut(), &description);
        if component.is_null() {
            tracing::warn!("no audio output device: the title plays silently");
            return;
        }
        let mut unit = std::ptr::null_mut();
        let mut status = AudioComponentInstanceNew(component, &mut unit);
        if status == 0 {
            status = AudioUnitSetProperty(unit, STREAM_FORMAT, INPUT_SCOPE, 0, (&raw const format).cast(), size_of::<StreamFormat>() as u32);
        }
        if status == 0 {
            status = AudioUnitSetProperty(unit, SET_RENDER_CALLBACK, INPUT_SCOPE, 0, (&raw const callback).cast(), size_of::<CallbackStruct>() as u32);
        }
        if status == 0 {
            status = AudioUnitInitialize(unit);
        }
        if status == 0 {
            status = AudioOutputUnitStart(unit);
        }
        status
    };
    if status == 0 {
        tracing::info!("sound: playing through the Mac's default output");
    } else {
        tracing::warn!(status, "sound: the output did not start; the title plays silently");
    }
}
