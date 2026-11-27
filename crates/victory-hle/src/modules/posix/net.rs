//! Windows Sockets with no network: start-up succeeds, the byte-order and
//! address helpers work, names are not found and every socket call fails
//! with `WSAENETDOWN`. The title runs offline.
//! ponytail: no sockets at all; the Windows build's `modules::net` lets
//! local traffic through, if a title needs that here too.
#![allow(non_snake_case)]

use std::ffi::{c_char, c_void};

use super::set_last_error;
use crate::HleExport;

const WSAENETDOWN: u32 = 10050;
const WSAHOST_NOT_FOUND: i32 = 11001;
const SOCKET_ERROR: i32 = -1;
const INVALID_SOCKET: usize = usize::MAX;

/// `(ordinal, name, address)` of each export.
fn table() -> Vec<(u16, &'static str, usize)> {
    vec![
        (1, "accept", accept as *const () as usize),
        (2, "bind", fail as *const () as usize),
        (3, "closesocket", fail as *const () as usize),
        (4, "connect", fail as *const () as usize),
        (5, "getpeername", fail as *const () as usize),
        (6, "getsockname", fail as *const () as usize),
        (7, "getsockopt", fail as *const () as usize),
        (8, "htonl", htonl as *const () as usize),
        (9, "htons", htons as *const () as usize),
        (10, "ioctlsocket", fail as *const () as usize),
        (11, "inet_addr", inet_addr as *const () as usize),
        (13, "listen", fail as *const () as usize),
        (14, "ntohl", htonl as *const () as usize),
        (15, "ntohs", htons as *const () as usize),
        (16, "recv", fail as *const () as usize),
        (17, "recvfrom", fail as *const () as usize),
        (18, "select", fail as *const () as usize),
        (19, "send", fail as *const () as usize),
        (20, "sendto", fail as *const () as usize),
        (21, "setsockopt", fail as *const () as usize),
        (22, "shutdown", fail as *const () as usize),
        (23, "socket", socket as *const () as usize),
        (52, "gethostbyname", gethostbyname as *const () as usize),
        (57, "gethostname", gethostname as *const () as usize),
        (111, "WSAGetLastError", WSAGetLastError as *const () as usize),
        (115, "WSAStartup", WSAStartup as *const () as usize),
        (116, "WSACleanup", WSACleanup as *const () as usize),
        (151, "__WSAFDIsSet", fd_is_set as *const () as usize),
        (0, "getaddrinfo", getaddrinfo as *const () as usize),
        (0, "freeaddrinfo", freeaddrinfo as *const () as usize),
    ]
}

pub(super) fn exports() -> Vec<HleExport> {
    let mut out = Vec::new();
    for (ordinal, name, address) in table() {
        out.push(HleExport { name, address });
        if ordinal != 0 {
            // Ordinal imports are looked up as `#n`: a name that lives on.
            let tag: &'static str = Box::leak(format!("#{ordinal}").into_boxed_str());
            out.push(HleExport { name: tag, address });
        }
    }
    out
}

extern "win64" fn fail() -> i32 {
    set_last_error(WSAENETDOWN);
    SOCKET_ERROR
}

extern "win64" fn accept(_: usize, _: *mut c_void, _: *mut i32) -> usize {
    set_last_error(WSAENETDOWN);
    INVALID_SOCKET
}

extern "win64" fn socket(_: i32, _: i32, _: i32) -> usize {
    set_last_error(WSAENETDOWN);
    INVALID_SOCKET
}

extern "win64" fn htonl(v: u32) -> u32 {
    v.swap_bytes()
}

extern "win64" fn htons(v: u16) -> u16 {
    v.swap_bytes()
}

/// A dotted IPv4 address in network order; `INADDR_NONE` if not one.
extern "win64" fn inet_addr(text: *const c_char) -> u32 {
    // SAFETY: the title's string.
    let text = unsafe { super::paths::narrow(text) };
    match text.trim().parse::<std::net::Ipv4Addr>() {
        Ok(a) => u32::from_ne_bytes(a.octets()),
        Err(_) => u32::MAX,
    }
}

extern "win64" fn gethostbyname(_: *const c_char) -> *const c_void {
    set_last_error(WSAHOST_NOT_FOUND as u32);
    std::ptr::null()
}

extern "win64" fn gethostname(out: *mut u8, len: i32) -> i32 {
    let name = b"xbox\0";
    if len < name.len() as i32 {
        set_last_error(10014); // WSAEFAULT
        return SOCKET_ERROR;
    }
    // SAFETY: the caller's buffer of `len` bytes.
    unsafe { std::ptr::copy_nonoverlapping(name.as_ptr(), out, name.len()) };
    0
}

extern "win64" fn WSAGetLastError() -> u32 {
    super::last_error()
}

/// `WSADATA`: version 2.2, the highest 2.2, a description.
extern "win64" fn WSAStartup(_wanted: u16, data: *mut u8) -> i32 {
    if !data.is_null() {
        // SAFETY: the caller's WSADATA (408 bytes on x64).
        unsafe {
            std::ptr::write_bytes(data, 0, 408);
            data.cast::<[u16; 2]>().write([0x0202, 0x0202]);
            std::ptr::copy_nonoverlapping(b"WinSock 2.0".as_ptr(), data.add(4 + 2 + 2 + 8), 11);
        }
    }
    0
}

extern "win64" fn WSACleanup() -> i32 {
    0
}

extern "win64" fn fd_is_set(_: usize, _: *const c_void) -> i32 {
    0
}

extern "win64" fn getaddrinfo(_: *const c_char, _: *const c_char, _: *const c_void, out: *mut *mut c_void) -> i32 {
    if !out.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { out.write(std::ptr::null_mut()) };
    }
    WSAHOST_NOT_FOUND
}

extern "win64" fn freeaddrinfo(_: *mut c_void) {}
