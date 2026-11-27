//! Root signatures serialized without a host `d3d12.dll`: the `RTS0`
//! layout (versions 1.0, 1.1 and 1.2) in a DXBC container, handed out as an
//! `ID3DBlob`. The container's checksum is left zero: these blobs never
//! reach a runtime that checks it.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::object::{E_INVALIDARG, E_NOINTERFACE, HRESULT, Iid, S_OK, guid};

const TABLE: u32 = 0;
const CONSTANTS: u32 = 1;

/// A root signature description as the title lays it out (PC layout):
/// `(version, parameter count, parameters, sampler count, samplers, flags)`.
pub(crate) struct Description {
    pub version: u32,
    pub parameter_count: u32,
    pub parameters: *const u8,
    pub sampler_count: u32,
    pub samplers: *const u8,
    pub flags: u32,
}

fn word(at: *const u8, i: usize) -> u32 {
    // SAFETY: the caller's description holds the field.
    unsafe { at.add(4 * i).cast::<u32>().read_unaligned() }
}

fn pointer(at: *const u8, offset: usize) -> *const u8 {
    // SAFETY: as above.
    unsafe { at.add(offset).cast::<*const u8>().read_unaligned() }
}

/// The `RTS0` chunk's bytes for `d`.
///
/// # Safety
/// `d` points at the title's arrays of the counts it gives.
pub(crate) unsafe fn rts0(d: &Description) -> Vec<u32> {
    let v11 = d.version >= 2;
    let range_words = if v11 { 6 } else { 5 };
    let sampler_words = if d.version >= 3 { 14 } else { 13 };
    let mut out = vec![d.version, d.parameter_count, 24, d.sampler_count, 0, d.flags];
    let headers = out.len();
    out.resize(headers + 3 * d.parameter_count as usize, 0);
    for p in 0..d.parameter_count as usize {
        // D3D12_ROOT_PARAMETER(1): type, (pad), 16-byte union at 8, visibility at 24.
        let param = unsafe { d.parameters.add(32 * p) };
        let kind = word(param, 0);
        let header = headers + 3 * p;
        out[header] = kind;
        out[header + 1] = word(param, 6);
        out[header + 2] = 4 * out.len() as u32;
        match kind {
            TABLE => {
                let count = word(param, 2);
                let ranges = pointer(param, 16);
                out.extend([count, 4 * (out.len() as u32 + 2)]);
                for r in 0..count as usize {
                    let range = unsafe { ranges.add(4 * range_words * r) };
                    out.extend((0..range_words).map(|i| word(range, i)));
                }
            }
            CONSTANTS => out.extend([word(param, 2), word(param, 3), word(param, 4)]),
            _ => out.extend((0..if v11 { 3 } else { 2 }).map(|i| word(param, 2 + i))),
        }
    }
    out[4] = 4 * out.len() as u32;
    for s in 0..d.sampler_count as usize {
        let sampler = unsafe { d.samplers.add(4 * sampler_words * s) };
        out.extend((0..sampler_words).map(|i| word(sampler, i)));
    }
    out
}

/// A DXBC container of one `RTS0` chunk.
fn container(rts0: &[u32]) -> Vec<u8> {
    let chunk = rts0.len() * 4;
    let total = 36 + 8 + chunk;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"DXBC");
    out.extend_from_slice(&[0; 16]);
    for v in [1u32, total as u32, 1, 36] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(b"RTS0");
    out.extend_from_slice(&(chunk as u32).to_le_bytes());
    for v in rts0 {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Serializes `d` into a new blob at `out`.
///
/// # Safety
/// As [`rts0`]; `out` is the caller's pointer for the blob.
pub(crate) unsafe fn serialize(d: &Description, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() || !(1..=3).contains(&d.version) {
        return E_INVALIDARG;
    }
    // SAFETY: forwarded from the caller.
    let bytes = container(&unsafe { rts0(d) });
    let blob = Box::new(Blob {
        vtable: &BLOB_FUNCTIONS,
        refs: AtomicU32::new(1),
        bytes,
    });
    // SAFETY: the caller's out pointer.
    unsafe { out.write(Box::into_raw(blob).cast()) };
    S_OK
}

/// `ID3DBlob`: IUnknown, then the buffer's pointer and size.
#[repr(C)]
struct Blob {
    vtable: &'static Functions,
    refs: AtomicU32,
    bytes: Vec<u8>,
}

const BLOB_IIDS: [Iid; 2] = [
    guid(0x8ba5_fb08, 0x5195, 0x40e2, [0xac, 0x58, 0x0d, 0x98, 0x9c, 0x3a, 0x01, 0x02]),
    guid(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]),
];

struct Functions([*const (); 5]);
// SAFETY: function addresses only.
unsafe impl Sync for Functions {}

static BLOB_FUNCTIONS: Functions = Functions([
    blob_query as *const (),
    blob_add_ref as *const (),
    blob_release as *const (),
    blob_pointer as *const (),
    blob_size as *const (),
]);

extern "win64" fn blob_query(this: *mut Blob, iid: *const Iid, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: the caller's interface id.
    let known = !iid.is_null() && BLOB_IIDS.contains(unsafe { &*iid });
    // SAFETY: the caller's out pointer; our object.
    unsafe {
        if known {
            (*this).refs.fetch_add(1, Ordering::Relaxed);
            out.write(this.cast());
            S_OK
        } else {
            out.write(std::ptr::null_mut());
            E_NOINTERFACE
        }
    }
}

extern "win64" fn blob_add_ref(this: *mut Blob) -> u32 {
    // SAFETY: our live object.
    unsafe { (*this).refs.fetch_add(1, Ordering::Relaxed) + 1 }
}

extern "win64" fn blob_release(this: *mut Blob) -> u32 {
    // SAFETY: our live object; the last reference frees it.
    unsafe {
        let left = (*this).refs.fetch_sub(1, Ordering::AcqRel) - 1;
        if left == 0 {
            drop(Box::from_raw(this));
        }
        left
    }
}

extern "win64" fn blob_pointer(this: *mut Blob) -> *mut c_void {
    // SAFETY: our live object.
    unsafe { (*this).bytes.as_mut_ptr().cast() }
}

extern "win64" fn blob_size(this: *mut Blob) -> usize {
    // SAFETY: our live object.
    unsafe { (*this).bytes.len() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_constants_and_a_sampler_serialize_in_the_rts0_layout() {
        // A 1.1 table of one SRV range (t0-t3, space 0, flags 0, offset 0),
        // 4 root constants at b0, a root CBV at b1 (flags 2), one sampler.
        let range: [u32; 6] = [0, 4, 0, 0, 0, 0];
        let mut params = [0u8; 96];
        params[0..4].copy_from_slice(&0u32.to_le_bytes());
        params[8..12].copy_from_slice(&1u32.to_le_bytes());
        params[16..24].copy_from_slice(&(range.as_ptr() as u64).to_le_bytes());
        params[24..28].copy_from_slice(&5u32.to_le_bytes()); // pixel
        params[32..36].copy_from_slice(&1u32.to_le_bytes());
        for (i, v) in [0u32, 0, 4].into_iter().enumerate() {
            params[40 + 4 * i..44 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        params[64..68].copy_from_slice(&2u32.to_le_bytes());
        for (i, v) in [1u32, 0, 2].into_iter().enumerate() {
            params[72 + 4 * i..76 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        let sampler: [u32; 13] = [0x15, 1, 1, 1, 0, 16, 0, 0, 0, 0x7f7f_ffff, 0, 0, 0];
        let d = Description {
            version: 2,
            parameter_count: 3,
            parameters: params.as_ptr(),
            sampler_count: 1,
            samplers: sampler.as_ptr().cast(),
            flags: 1,
        };
        // SAFETY: our arrays.
        let w = unsafe { rts0(&d) };
        assert_eq!(&w[..6], &[2, 3, 24, 1, w[4], 1]);
        assert_eq!(&w[6..9], &[0, 5, 60], "table header: type, visibility, payload");
        assert_eq!(&w[15..23], &[1, 68, 0, 4, 0, 0, 0, 0], "range count, offset, then the range");
        assert_eq!(&w[23..26], &[0, 0, 4], "constants");
        assert_eq!(&w[26..29], &[1, 0, 2], "descriptor with flags");
        assert_eq!(w[4], 4 * 29, "samplers after the payloads");
        assert_eq!(&w[29..], &sampler);
        let mut out = std::ptr::null_mut();
        // SAFETY: as above.
        assert_eq!(unsafe { serialize(&d, &mut out) }, S_OK);
        let blob = out.cast::<Blob>();
        let bytes = unsafe { std::slice::from_raw_parts(blob_pointer(blob).cast::<u8>(), blob_size(blob)) };
        assert_eq!(&bytes[..4], b"DXBC");
        assert_eq!(&bytes[36..40], b"RTS0");
        assert_eq!(blob_release(blob), 0);
    }
}
