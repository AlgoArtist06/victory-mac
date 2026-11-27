use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSWindow, NSWindowStyleMask};
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
