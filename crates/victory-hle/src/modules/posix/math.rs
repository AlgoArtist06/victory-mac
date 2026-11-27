//! `api-ms-win-crt-math`. Each function is a wrapper, never libm's own
//! address: the Windows convention keeps `xmm6`-`xmm15` (and `rdi`, `rsi`)
//! across calls, and macOS's does not.
#![allow(non_snake_case)]

use crate::{HleExport, hle_export};

macro_rules! math {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty = $body:expr;)*) => {
        pub(super) fn exports() -> Vec<HleExport> {
            vec![$(hle_export!($name)),*]
        }
        $(extern "win64" fn $name($($arg: $ty),*) -> $ret { $body })*
    };
}

unsafe extern "C" {
    #[link_name = "erff"]
    fn libm_erff(x: f32) -> f32;
    #[link_name = "frexp"]
    fn libm_frexp(x: f64, exp: *mut i32) -> f64;
    #[link_name = "ldexp"]
    fn libm_ldexp(x: f64, exp: i32) -> f64;
    #[link_name = "remainderf"]
    fn libm_remainderf(x: f32, y: f32) -> f32;
    #[link_name = "nextafterf"]
    fn libm_nextafterf(x: f32, y: f32) -> f32;
}

/// `_dtest`/`_fdtest` classes.
fn class(nan: bool, infinite: bool, zero: bool, subnormal: bool) -> i16 {
    match () {
        _ if nan => 2,
        _ if infinite => 1,
        _ if zero => 0,
        _ if subnormal => -2,
        _ => -1,
    }
}

math! {
    acos(x: f64) -> f64 = x.acos();
    asin(x: f64) -> f64 = x.asin();
    atan(x: f64) -> f64 = x.atan();
    atan2(y: f64, x: f64) -> f64 = y.atan2(x);
    cos(x: f64) -> f64 = x.cos();
    sin(x: f64) -> f64 = x.sin();
    tan(x: f64) -> f64 = x.tan();
    exp(x: f64) -> f64 = x.exp();
    exp2(x: f64) -> f64 = x.exp2();
    log(x: f64) -> f64 = x.ln();
    log2(x: f64) -> f64 = x.log2();
    pow(x: f64, y: f64) -> f64 = x.powf(y);
    fmod(x: f64, y: f64) -> f64 = x % y;
    acosf(x: f32) -> f32 = x.acos();
    asinf(x: f32) -> f32 = x.asin();
    atanf(x: f32) -> f32 = x.atan();
    atan2f(y: f32, x: f32) -> f32 = y.atan2(x);
    cosf(x: f32) -> f32 = x.cos();
    sinf(x: f32) -> f32 = x.sin();
    tanf(x: f32) -> f32 = x.tan();
    sinhf(x: f32) -> f32 = x.sinh();
    expf(x: f32) -> f32 = x.exp();
    exp2f(x: f32) -> f32 = x.exp2();
    logf(x: f32) -> f32 = x.ln();
    log2f(x: f32) -> f32 = x.log2();
    log10f(x: f32) -> f32 = x.log10();
    powf(x: f32, y: f32) -> f32 = x.powf(y);
    fmodf(x: f32, y: f32) -> f32 = x % y;
    cbrtf(x: f32) -> f32 = x.cbrt();
    ceilf(x: f32) -> f32 = x.ceil();
    // SAFETY: a pure libm function.
    erff(x: f32) -> f32 = unsafe { libm_erff(x) };
    // SAFETY: as above.
    remainderf(x: f32, y: f32) -> f32 = unsafe { libm_remainderf(x, y) };
    // SAFETY: as above.
    nextafterf(x: f32, y: f32) -> f32 = unsafe { libm_nextafterf(x, y) };
    // `long double` is a double on Windows.
    // SAFETY: as above.
    nexttowardf(x: f32, y: f64) -> f32 = unsafe { libm_nextafterf(x, if (y as f32) == x { x } else if y > f64::from(x) { f32::INFINITY } else { f32::NEG_INFINITY }) };
    // `long` is 32 bits on Windows.
    lroundf(x: f32) -> i32 = x.round() as i32;
    // SAFETY: the caller's out pointer.
    frexp(x: f64, exp: *mut i32) -> f64 = unsafe { libm_frexp(x, exp) };
    // SAFETY: as above.
    ldexp(x: f64, exp: i32) -> f64 = unsafe { libm_ldexp(x, exp) };
    // SAFETY: the caller's out pointer.
    modf(x: f64, whole: *mut f64) -> f64 = unsafe { whole.write(x.trunc()); if x.is_infinite() { 0.0f64.copysign(x) } else { x - x.trunc() } };
    // SAFETY: as above.
    modff(x: f32, whole: *mut f32) -> f32 = unsafe { whole.write(x.trunc()); if x.is_infinite() { 0.0f32.copysign(x) } else { x - x.trunc() } };
    _isnan(x: f64) -> i32 = i32::from(x.is_nan());
    // SAFETY: the caller's double.
    _dtest(x: *const f64) -> i16 = { let x = unsafe { x.read() }; class(x.is_nan(), x.is_infinite(), x == 0.0, x.is_subnormal()) };
    // SAFETY: the caller's float.
    _fdtest(x: *const f32) -> i16 = { let x = unsafe { x.read() }; class(x.is_nan(), x.is_infinite(), x == 0.0, x.is_subnormal()) };
    _fdsign(x: f32) -> i32 = ((x.to_bits() >> 16) & 0x8000) as i32;
    __setusermatherr(_handler: usize) -> () = ();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn math_matches_c() {
        let mut whole = 0.0;
        assert_eq!(modf(-2.75, &mut whole), -0.75);
        assert_eq!(whole, -2.0);
        assert_eq!(lroundf(2.5), 3);
        assert_eq!(_dtest(&f64::NAN), 2);
        assert_eq!(_dtest(&1.0), -1);
        assert_eq!(_fdtest(&0.0), 0);
        assert_ne!(_fdsign(-1.0), 0);
        assert_eq!(fmod(7.5, 2.0), 1.5);
        assert_eq!(nexttowardf(1.0, 2.0), f32::from_bits(1.0f32.to_bits() + 1));
    }
}
