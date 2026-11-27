use std::collections::BTreeSet;
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};

use block2::RcBlock;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent, NSEventMask, NSEventType, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_metal::{
    MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice,
    MTLLoadAction, MTLPixelFormat, MTLRenderPassDescriptor, MTLStoreAction,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

/// What [`present`] draws with: the device's queue and the window's layer.
struct Display {
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    layer: Retained<CAMetalLayer>,
}

// SAFETY: a command queue and a CAMetalLayer may be used from any thread
// (Metal's and Core Animation's thread-safety rules for these objects).
unsafe impl Send for Display {}
// SAFETY: as above.
unsafe impl Sync for Display {}

static DISPLAY: OnceLock<Display> = OnceLock::new();

/// The window's content size, in points: the console's 1080p at half size.
const WIDTH: f64 = 960.0;
const HEIGHT: f64 = 540.0;

/// Makes the window and runs the event loop on the main thread; never
/// returns (the process ends when the title does).
pub fn run_app() -> ! {
    let mtm = MainThreadMarker::new().expect("run_app runs on the main thread");
    let device = MTLCreateSystemDefaultDevice().expect("a Metal device");
    let queue = device.newCommandQueue().expect("a Metal command queue");
    let layer = CAMetalLayer::new();
    layer.setDevice(Some(&device));
    layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let rect = NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(WIDTH, HEIGHT));
    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable | NSWindowStyleMask::Resizable;
    // SAFETY: a new window on the main thread, kept alive by the app.
    let window = unsafe { NSWindow::initWithContentRect_styleMask_backing_defer(mtm.alloc(), rect, style, NSBackingStoreType::Buffered, false) };
    // SAFETY: the window is released by us only; AppKit must not too.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("VIctory"));
    let view = window.contentView().expect("a content view");
    view.setWantsLayer(true);
    view.setLayer(Some(&layer));
    layer.setDrawableSize(objc2_core_foundation::CGSize::new(WIDTH * 2.0, HEIGHT * 2.0));
    window.center();
    window.makeKeyAndOrderFront(None);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    watch_keys();
    let _ = DISPLAY.set(Display { queue, layer });
    tracing::info!(device = %device.name(), "Metal device and window ready");
    app.run();
    std::process::exit(0)
}

/// Shows frame `frame`: the next drawable, cleared to a colour that changes
/// with it. Nothing happens before the window exists.
pub fn present(frame: u64) {
    let Some(display) = DISPLAY.get() else { return };
    objc2::rc::autoreleasepool(|_| {
        let Some(drawable) = display.layer.nextDrawable() else { return };
        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: attachment 0 exists on every render pass descriptor.
        let colour = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        let t = (frame % 240) as f64 / 240.0;
        colour.setTexture(Some(&drawable.texture()));
        colour.setLoadAction(MTLLoadAction::Clear);
        colour.setStoreAction(MTLStoreAction::Store);
        colour.setClearColor(MTLClearColor { red: 0.1, green: 0.1 + 0.4 * t, blue: 0.3, alpha: 1.0 });
        let Some(commands) = display.queue.commandBuffer() else { return };
        if let Some(encoder) = commands.renderCommandEncoderWithDescriptor(&pass) {
            encoder.endEncoding();
        }
        commands.presentDrawable(ProtocolObject::from_ref(&*drawable));
        commands.commit();
    });
}

/// Keys held in our window, as Windows virtual keys.
static HELD: Mutex<BTreeSet<u8>> = Mutex::new(BTreeSet::new());

/// The Windows virtual key of a Mac key code (US layout positions).
fn virtual_key(code: u16) -> Option<u8> {
    const LETTERS: [(u16, u8); 26] = [
        (0x00, b'A'), (0x0B, b'B'), (0x08, b'C'), (0x02, b'D'), (0x0E, b'E'), (0x03, b'F'), (0x05, b'G'),
        (0x04, b'H'), (0x22, b'I'), (0x26, b'J'), (0x28, b'K'), (0x25, b'L'), (0x2E, b'M'), (0x2D, b'N'),
        (0x1F, b'O'), (0x23, b'P'), (0x0C, b'Q'), (0x0F, b'R'), (0x01, b'S'), (0x11, b'T'), (0x20, b'U'),
        (0x09, b'V'), (0x0D, b'W'), (0x07, b'X'), (0x10, b'Y'), (0x06, b'Z'),
    ];
    const DIGITS: [(u16, u8); 10] = [
        (0x1D, b'0'), (0x12, b'1'), (0x13, b'2'), (0x14, b'3'), (0x15, b'4'),
        (0x17, b'5'), (0x16, b'6'), (0x1A, b'7'), (0x1C, b'8'), (0x19, b'9'),
    ];
    const OTHERS: [(u16, u8); 13] = [
        (0x24, 0x0D), (0x30, 0x09), (0x31, 0x20), (0x33, 0x08), (0x35, 0x1B),
        (0x7B, 0x25), (0x7E, 0x26), (0x7C, 0x27), (0x7D, 0x28),
        (0x38, 0xA0), (0x3C, 0xA1), (0x3B, 0xA2), (0x3A, 0xA4),
    ];
    LETTERS.iter().chain(&DIGITS).chain(&OTHERS).find(|(c, _)| *c == code).map(|(_, vk)| *vk)
}

/// The keys held in our window now, as Windows virtual keys.
pub fn keys_held() -> Vec<u8> {
    HELD.lock().unwrap_or_else(|e| e.into_inner()).iter().copied().collect()
}

/// Notes key presses and releases in our window (and keeps them from
/// beeping); modifiers come as flag changes.
fn watch_keys() {
    let mask = NSEventMask::KeyDown | NSEventMask::KeyUp | NSEventMask::FlagsChanged;
    let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes a live event.
        let event = unsafe { event.as_ref() };
        let code = event.keyCode();
        let Some(vk) = virtual_key(code) else { return (event as *const NSEvent).cast_mut() };
        let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
        match event.r#type() {
            NSEventType::KeyDown => {
                held.insert(vk);
            }
            NSEventType::KeyUp => {
                held.remove(&vk);
            }
            _ => {
                // A modifier: held if it was not, released if it was.
                if !held.remove(&vk) {
                    held.insert(vk);
                }
            }
        }
        std::ptr::null_mut()
    });
    // SAFETY: the block lives as long as the monitor (for ever).
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
    std::mem::forget(monitor);
    std::mem::forget(block);
}

#[cfg(test)]
mod tests {
    use super::virtual_key;

    #[test]
    fn mac_key_codes_become_windows_virtual_keys() {
        assert_eq!(virtual_key(0x24), Some(0x0D), "Return is VK_RETURN (the A button)");
        assert_eq!(virtual_key(0x35), Some(0x1B), "Escape");
        assert_eq!(virtual_key(0x7E), Some(0x26), "up arrow");
        assert_eq!(virtual_key(0x0D), Some(b'W'));
        assert_eq!(virtual_key(0x12), Some(b'1'));
        assert_eq!(virtual_key(0x7A), None, "F1 is not mapped");
    }
}
