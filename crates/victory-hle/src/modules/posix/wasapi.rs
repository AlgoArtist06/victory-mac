//! WASAPI on macOS: one default output device, the audio client a title's
//! mixer (fmod) renders into, and a clock that consumes what it wrote at
//! the device's pace, signalling the title's event every period.
//! ponytail: the audio is discarded (silent); CoreAudio output takes the
//! rendered periods from `Client::ring` when sound is wanted.
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::*};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Hresult = i32;
const S_OK: Hresult = 0;
const E_NOINTERFACE: Hresult = 0x8000_4002_u32 as i32;
const E_POINTER: Hresult = 0x8000_4003_u32 as i32;
const E_NOTIMPL: Hresult = 0x8000_4001_u32 as i32;
const AUDCLNT_E_NOT_INITIALIZED: Hresult = 0x8889_0001_u32 as i32;
const AUDCLNT_E_BUFFER_TOO_LARGE: Hresult = 0x8889_0006_u32 as i32;

type Guid = [u8; 16];

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]]
}

pub(super) const CLSID_MM_DEVICE_ENUMERATOR: Guid =
    guid(0xbcde_0395, 0xe52f, 0x467c, [0x8e, 0x3d, 0xc4, 0x57, 0x92, 0x91, 0x69, 0x2e]);
const IID_UNKNOWN: Guid = guid(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_ENUMERATOR: Guid = guid(0xa956_64d2, 0x9614, 0x4f35, [0xa7, 0x46, 0xde, 0x8d, 0xb6, 0x36, 0x17, 0xe6]);
const IID_DEVICE: Guid = guid(0xd666_063f, 0x1587, 0x4e43, [0x81, 0xf1, 0xb9, 0x48, 0xe8, 0x07, 0x36, 0x3f]);
const IID_CLIENT: Guid = guid(0x1cb9_ad4c, 0xdbfa, 0x4c32, [0xb1, 0x78, 0xc2, 0xf5, 0x68, 0xa7, 0x03, 0xb2]);
const IID_CLIENT2: Guid = guid(0x7267_78d7, 0x2ef1, 0x4c2c, [0x95, 0x35, 0x1b, 0xf4, 0xa2, 0xfb, 0x4a, 0x07]);
const IID_CLIENT3: Guid = guid(0x7ed4_ee07, 0x8e67, 0x4cd4, [0x8c, 0x1a, 0x2b, 0x7a, 0x59, 0x87, 0xad, 0x42]);
const IID_RENDER: Guid = guid(0xf294_acfc, 0x3146, 0x4483, [0xa7, 0xbf, 0xad, 0xdc, 0xa7, 0xc2, 0x60, 0xe2]);
const IID_CLOCK: Guid = guid(0xcd63_314f, 0x3fba, 0x4a1b, [0x81, 0x2c, 0xef, 0x96, 0x35, 0x87, 0x28, 0xe7]);
const IID_PROPERTY_STORE: Guid = guid(0x886d_8eeb, 0x8cf2, 0x4446, [0x8d, 0x02, 0xcd, 0xba, 0x1d, 0xbd, 0xcf, 0x99]);

/// The device's format: 48 kHz, stereo, 32-bit float (WAVEFORMATEXTENSIBLE).
const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const FRAME_BYTES: u32 = 8;
/// The device period: 10 ms, in 100 ns units, and in frames.
const PERIOD: i64 = 100_000;
const PERIOD_FRAMES: u32 = RATE / 100;

#[repr(C, packed)]
struct WaveFormat {
    tag: u16,
    channels: u16,
    rate: u32,
    bytes_per_second: u32,
    block_align: u16,
    bits: u16,
    extra: u16,
    valid_bits: u16,
    channel_mask: u32,
    sub_format: Guid,
}

const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: Guid = guid(3, 0, 0x10, [0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]);

fn mix_format() -> WaveFormat {
    WaveFormat {
        tag: 0xfffe,
        channels: CHANNELS,
        rate: RATE,
        bytes_per_second: RATE * FRAME_BYTES,
        block_align: FRAME_BYTES as u16,
        bits: 32,
        extra: 22,
        valid_bits: 32,
        channel_mask: 3,
        sub_format: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    }
}

/// A COM object of ours: its function table, a count, and its state.
#[repr(C)]
struct Com<T> {
    vtable: &'static Table,
    refs: AtomicU32,
    state: T,
}

struct Table(&'static [*const ()]);
// SAFETY: function addresses only.
unsafe impl Sync for Table {}

fn make<T>(vtable: &'static Table, state: T) -> *mut c_void {
    Box::into_raw(Box::new(Com { vtable, refs: AtomicU32::new(1), state })).cast()
}

/// # Safety
/// `this` is a live `Com<T>`.
unsafe fn state<'a, T>(this: *mut c_void) -> &'a T {
    // SAFETY: guaranteed by the caller.
    unsafe { &(*this.cast::<Com<T>>()).state }
}

extern "win64" fn add_ref<T>(this: *mut c_void) -> u32 {
    // SAFETY: our live object.
    unsafe { (*this.cast::<Com<T>>()).refs.fetch_add(1, Relaxed) + 1 }
}

extern "win64" fn release<T>(this: *mut c_void) -> u32 {
    // SAFETY: our live object; the last reference frees it.
    unsafe {
        let left = (*this.cast::<Com<T>>()).refs.fetch_sub(1, AcqRel) - 1;
        if left == 0 {
            drop(Box::from_raw(this.cast::<Com<T>>()));
        }
        left
    }
}

/// `QueryInterface` for objects answering `known` interfaces.
fn query<T>(this: *mut c_void, iid: *const Guid, out: *mut *mut c_void, known: &[Guid]) -> Hresult {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's interface id and out pointer.
    unsafe {
        if !iid.is_null() && (*iid == IID_UNKNOWN || known.contains(&*iid)) {
            add_ref::<T>(this);
            out.write(this);
            S_OK
        } else {
            out.write(std::ptr::null_mut());
            E_NOINTERFACE
        }
    }
}

extern "win64" fn not_implemented() -> Hresult {
    tracing::debug!("WASAPI method not implemented");
    E_NOTIMPL
}

fn put<T>(out: *mut T, value: T) -> Hresult {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's out pointer.
    unsafe { out.write(value) };
    S_OK
}

/// A new enumerator, asked for as `iid`.
pub(super) fn create_enumerator(iid: *const Guid, out: *mut *mut c_void) -> Hresult {
    let enumerator = make(&ENUMERATOR, ());
    let answer = enumerator_query(enumerator, iid, out);
    release::<()>(enumerator);
    answer
}

static ENUMERATOR: Table = Table(&[
    enumerator_query as *const (),
    add_ref::<()> as *const (),
    release::<()> as *const (),
    not_implemented as *const (),     // EnumAudioEndpoints
    default_endpoint as *const (),    // GetDefaultAudioEndpoint
    default_endpoint_by_id as *const (), // GetDevice
    register_notification as *const (),
    register_notification as *const (),
]);

extern "win64" fn enumerator_query(this: *mut c_void, iid: *const Guid, out: *mut *mut c_void) -> Hresult {
    query::<()>(this, iid, out, &[IID_ENUMERATOR])
}

extern "win64" fn default_endpoint(_: *mut c_void, _flow: u32, _role: u32, out: *mut *mut c_void) -> Hresult {
    put(out, make(&DEVICE, ()))
}

extern "win64" fn default_endpoint_by_id(this: *mut c_void, _id: *const u16, out: *mut *mut c_void) -> Hresult {
    default_endpoint(this, 0, 0, out)
}

/// Devices never change, so notifications never come.
extern "win64" fn register_notification(_: *mut c_void, _: *mut c_void) -> Hresult {
    S_OK
}

static DEVICE: Table = Table(&[
    device_query as *const (),
    add_ref::<()> as *const (),
    release::<()> as *const (),
    activate as *const (),
    open_property_store as *const (),
    device_id as *const (),
    device_state as *const (),
]);

extern "win64" fn device_query(this: *mut c_void, iid: *const Guid, out: *mut *mut c_void) -> Hresult {
    query::<()>(this, iid, out, &[IID_DEVICE])
}

extern "win64" fn activate(_: *mut c_void, iid: *const Guid, _: u32, _: *const c_void, out: *mut *mut c_void) -> Hresult {
    // SAFETY: the caller's interface id.
    let wanted = if iid.is_null() { [0; 16] } else { unsafe { *iid } };
    if ![IID_CLIENT, IID_CLIENT2, IID_CLIENT3].contains(&wanted) {
        return put(out, std::ptr::null_mut());
    }
    tracing::info!("WASAPI: audio client made (silent output)");
    put(out, make(&CLIENT, Arc::new(Client::default())))
}

extern "win64" fn open_property_store(_: *mut c_void, _: u32, out: *mut *mut c_void) -> Hresult {
    put(out, make(&PROPERTIES, ()))
}

extern "win64" fn device_id(_: *mut c_void, out: *mut *mut u16) -> Hresult {
    let id: Vec<u16> = "{0.0.0.00000000}.{victory-output}".encode_utf16().chain([0]).collect();
    // SAFETY: a block the caller frees with CoTaskMemFree (libc's free).
    let block = unsafe { libc::malloc(id.len() * 2) }.cast::<u16>();
    // SAFETY: the block holds the id.
    unsafe { std::ptr::copy_nonoverlapping(id.as_ptr(), block, id.len()) };
    put(out, block)
}

extern "win64" fn device_state(_: *mut c_void, out: *mut u32) -> Hresult {
    put(out, 1) // DEVICE_STATE_ACTIVE
}

static PROPERTIES: Table = Table(&[
    properties_query as *const (),
    add_ref::<()> as *const (),
    release::<()> as *const (),
    property_count as *const (),
    not_implemented as *const (),
    property_value as *const (),
    not_implemented as *const (),
    not_implemented as *const (),
]);

extern "win64" fn properties_query(this: *mut c_void, iid: *const Guid, out: *mut *mut c_void) -> Hresult {
    query::<()>(this, iid, out, &[IID_PROPERTY_STORE])
}

extern "win64" fn property_count(_: *mut c_void, out: *mut u32) -> Hresult {
    put(out, 0)
}

/// Every property is empty (`VT_EMPTY`): names and formats are optional.
extern "win64" fn property_value(_: *mut c_void, _key: *const c_void, out: *mut [u64; 3]) -> Hresult {
    put(out, [0; 3])
}

/// The client's state: the ring the title renders into, how much of it is
/// still to play, and the clock's count of frames played.
#[derive(Default)]
struct Client {
    ring: Mutex<Vec<u8>>,
    frames: AtomicU32,
    padding: AtomicU32,
    played: AtomicU64,
    event: AtomicU64,
    running: std::sync::atomic::AtomicBool,
    started: std::sync::atomic::AtomicBool,
}

static CLIENT: Table = Table(&[
    client_query as *const (),
    add_ref::<Arc<Client>> as *const (),
    release::<Arc<Client>> as *const (),
    initialize as *const (),
    buffer_size as *const (),
    stream_latency as *const (),
    current_padding as *const (),
    format_supported as *const (),
    get_mix_format as *const (),
    device_period as *const (),
    start as *const (),
    stop as *const (),
    reset as *const (),
    set_event as *const (),
    get_service as *const (),
    // IAudioClient2
    not_implemented as *const (),
    set_client_properties as *const (),
    not_implemented as *const (),
    // IAudioClient3
    not_implemented as *const (),
    not_implemented as *const (),
    not_implemented as *const (),
]);

fn client<'a>(this: *mut c_void) -> &'a Arc<Client> {
    // SAFETY: our client object.
    unsafe { state::<Arc<Client>>(this) }
}

extern "win64" fn client_query(this: *mut c_void, iid: *const Guid, out: *mut *mut c_void) -> Hresult {
    query::<Arc<Client>>(this, iid, out, &[IID_CLIENT, IID_CLIENT2, IID_CLIENT3])
}

extern "win64" fn initialize(this: *mut c_void, _mode: u32, flags: u32, duration: i64, _period: i64, _format: *const c_void, _session: *const Guid) -> Hresult {
    let frames = ((duration.max(2 * PERIOD) * i64::from(RATE)) / 10_000_000) as u32;
    let c = client(this);
    *c.ring.lock().unwrap_or_else(|e| e.into_inner()) = vec![0; (frames * FRAME_BYTES) as usize];
    c.frames.store(frames, Relaxed);
    tracing::info!(frames, flags = format_args!("{flags:#x}"), "WASAPI: client initialized");
    S_OK
}

extern "win64" fn buffer_size(this: *mut c_void, out: *mut u32) -> Hresult {
    match client(this).frames.load(Relaxed) {
        0 => AUDCLNT_E_NOT_INITIALIZED,
        n => put(out, n),
    }
}

extern "win64" fn stream_latency(_: *mut c_void, out: *mut i64) -> Hresult {
    put(out, PERIOD)
}

extern "win64" fn current_padding(this: *mut c_void, out: *mut u32) -> Hresult {
    put(out, client(this).padding.load(Acquire))
}

/// Only the mix format: the title converts (as shared mode expects).
extern "win64" fn format_supported(_: *mut c_void, _mode: u32, _format: *const c_void, closest: *mut *mut c_void) -> Hresult {
    if !closest.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { closest.write(std::ptr::null_mut()) };
    }
    S_OK
}

extern "win64" fn get_mix_format(_: *mut c_void, out: *mut *mut WaveFormat) -> Hresult {
    // SAFETY: a block the caller frees with CoTaskMemFree (libc's free).
    let block = unsafe { libc::malloc(size_of::<WaveFormat>()) }.cast::<WaveFormat>();
    // SAFETY: the block holds a format.
    unsafe { block.write_unaligned(mix_format()) };
    put(out, block)
}

extern "win64" fn device_period(_: *mut c_void, default: *mut i64, minimum: *mut i64) -> Hresult {
    for out in [default, minimum] {
        if !out.is_null() {
            put(out, PERIOD);
        }
    }
    S_OK
}

/// Starts the device clock: every period it plays a period's frames of
/// what was written, and signals the title's event.
extern "win64" fn start(this: *mut c_void) -> Hresult {
    let c = client(this).clone();
    c.running.store(true, Release);
    if !c.started.swap(true, AcqRel) {
        std::thread::Builder::new()
            .name("wasapi-clock".into())
            .spawn(move || {
                let mut next = Instant::now();
                loop {
                    next += Duration::from_millis(10);
                    std::thread::sleep(next.saturating_duration_since(Instant::now()));
                    if !c.running.load(Acquire) {
                        continue;
                    }
                    let had = c.padding.load(Acquire);
                    let played = had.min(PERIOD_FRAMES);
                    c.padding.fetch_sub(played, AcqRel);
                    c.played.fetch_add(u64::from(PERIOD_FRAMES), Relaxed);
                    let event = c.event.load(Acquire) as usize;
                    if event != 0 {
                        // SAFETY: the title's event handle.
                        unsafe { super::events::SetEvent(event as *mut c_void) };
                    }
                }
            })
            .ok();
    }
    S_OK
}

extern "win64" fn stop(this: *mut c_void) -> Hresult {
    client(this).running.store(false, Release);
    S_OK
}

extern "win64" fn reset(this: *mut c_void) -> Hresult {
    client(this).padding.store(0, Release);
    S_OK
}

extern "win64" fn set_event(this: *mut c_void, event: usize) -> Hresult {
    client(this).event.store(event as u64, Release);
    S_OK
}

extern "win64" fn set_client_properties(_: *mut c_void, _: *const c_void) -> Hresult {
    S_OK
}

extern "win64" fn get_service(this: *mut c_void, iid: *const Guid, out: *mut *mut c_void) -> Hresult {
    // SAFETY: the caller's interface id.
    let wanted = if iid.is_null() { [0; 16] } else { unsafe { *iid } };
    let c = client(this).clone();
    match wanted {
        w if w == IID_RENDER => put(out, make(&RENDER, c)),
        w if w == IID_CLOCK => put(out, make(&CLOCK, c)),
        _ => {
            put(out, std::ptr::null_mut());
            E_NOINTERFACE
        }
    }
}

static RENDER: Table = Table(&[
    render_query as *const (),
    add_ref::<Arc<Client>> as *const (),
    release::<Arc<Client>> as *const (),
    get_buffer as *const (),
    release_buffer as *const (),
]);

extern "win64" fn render_query(this: *mut c_void, iid: *const Guid, out: *mut *mut c_void) -> Hresult {
    query::<Arc<Client>>(this, iid, out, &[IID_RENDER])
}

/// Room for `frames` more frames, past what is still to play.
extern "win64" fn get_buffer(this: *mut c_void, frames: u32, out: *mut *mut u8) -> Hresult {
    let c = client(this);
    let total = c.frames.load(Relaxed);
    if frames > total - c.padding.load(Acquire).min(total) {
        return AUDCLNT_E_BUFFER_TOO_LARGE;
    }
    let mut ring = c.ring.lock().unwrap_or_else(|e| e.into_inner());
    put(out, ring.as_mut_ptr())
}

extern "win64" fn release_buffer(this: *mut c_void, frames: u32, _flags: u32) -> Hresult {
    client(this).padding.fetch_add(frames, AcqRel);
    S_OK
}

static CLOCK: Table = Table(&[
    clock_query as *const (),
    add_ref::<Arc<Client>> as *const (),
    release::<Arc<Client>> as *const (),
    clock_frequency as *const (),
    clock_position as *const (),
    clock_characteristics as *const (),
]);

extern "win64" fn clock_query(this: *mut c_void, iid: *const Guid, out: *mut *mut c_void) -> Hresult {
    query::<Arc<Client>>(this, iid, out, &[IID_CLOCK])
}

/// Positions are in bytes per second of the mix format.
extern "win64" fn clock_frequency(_: *mut c_void, out: *mut u64) -> Hresult {
    put(out, u64::from(RATE * FRAME_BYTES))
}

extern "win64" fn clock_position(this: *mut c_void, position: *mut u64, qpc: *mut u64) -> Hresult {
    let played = client(this).played.load(Relaxed) * u64::from(FRAME_BYTES);
    if !qpc.is_null() {
        let mut now = 0i64;
        let counter = crate::registry::lookup("kernel32.dll", "QueryPerformanceCounter").expect("ours");
        // SAFETY: our own QueryPerformanceCounter.
        let counter: extern "win64" fn(*mut i64) -> i32 = unsafe { std::mem::transmute(counter) };
        counter(&mut now);
        put(qpc, now as u64);
    }
    put(position, played)
}

extern "win64" fn clock_characteristics(_: *mut c_void, out: *mut u32) -> Hresult {
    put(out, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call<F: Copy>(object: *mut c_void, slot: usize) -> F {
        // SAFETY: our objects start with their table.
        unsafe { std::mem::transmute_copy(&(*object.cast::<Com<()>>()).vtable.0[slot]) }
    }

    #[test]
    fn a_title_renders_and_the_clock_plays_it() {
        let mut enumerator = std::ptr::null_mut();
        assert_eq!(create_enumerator(&IID_ENUMERATOR, &mut enumerator), S_OK);
        let mut device = std::ptr::null_mut();
        let get: extern "win64" fn(*mut c_void, u32, u32, *mut *mut c_void) -> Hresult = call(enumerator, 4);
        assert_eq!(get(enumerator, 0, 0, &mut device), S_OK);
        let mut client = std::ptr::null_mut();
        let activate: extern "win64" fn(*mut c_void, *const Guid, u32, *const c_void, *mut *mut c_void) -> Hresult = call(device, 3);
        assert_eq!(activate(device, &IID_CLIENT, 0, std::ptr::null(), &mut client), S_OK);
        let init: extern "win64" fn(*mut c_void, u32, u32, i64, i64, *const c_void, *const Guid) -> Hresult = call(client, 3);
        assert_eq!(init(client, 0, 0, 200_000, 0, std::ptr::null(), std::ptr::null()), S_OK);
        let mut frames = 0u32;
        let size: extern "win64" fn(*mut c_void, *mut u32) -> Hresult = call(client, 4);
        assert_eq!(size(client, &mut frames), S_OK);
        assert_eq!(frames, 960, "20 ms at 48 kHz");
        let mut render = std::ptr::null_mut();
        let service: extern "win64" fn(*mut c_void, *const Guid, *mut *mut c_void) -> Hresult = call(client, 14);
        assert_eq!(service(client, &IID_RENDER, &mut render), S_OK);
        let mut at = std::ptr::null_mut();
        let get_buffer: extern "win64" fn(*mut c_void, u32, *mut *mut u8) -> Hresult = call(render, 3);
        let release_buffer: extern "win64" fn(*mut c_void, u32, u32) -> Hresult = call(render, 4);
        assert_eq!(get_buffer(render, 960, &mut at), S_OK);
        assert_eq!(release_buffer(render, 960, 0), S_OK);
        assert_eq!(get_buffer(render, 1, &mut at), AUDCLNT_E_BUFFER_TOO_LARGE, "full");
        let start: extern "win64" fn(*mut c_void) -> Hresult = call(client, 10);
        assert_eq!(start(client), S_OK);
        std::thread::sleep(Duration::from_millis(60));
        let mut padding = 0u32;
        let pad: extern "win64" fn(*mut c_void, *mut u32) -> Hresult = call(client, 6);
        assert_eq!(pad(client, &mut padding), S_OK);
        assert!(padding < 960, "the clock played some of it: {padding}");
    }
}
