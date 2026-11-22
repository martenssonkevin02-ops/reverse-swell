//! Dragging a rendered swell out of the plugin window into the DAW (macOS).
//!
//! egui doesn't expose the native view, so we find the window under the mouse (the plugin's
//! window inside FL Studio), hit-test for the view under the cursor, and start a regular AppKit
//! file drag from there, exactly like dragging a file out of Finder.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSEvent, NSEventModifierFlags, NSEventType, NSWindow, NSWorkspace,
};
use objc2_foundation::{NSArray, NSPoint, NSProcessInfo, NSRect, NSSize, NSString, NSURL};
use std::cell::OnceCell;
use std::path::Path;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SecondGenerationReverseSwellDragSource_1_1_0"]
    struct DragSource;

    unsafe impl NSObjectProtocol for DragSource {}

    unsafe impl NSDraggingSource for DragSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }
    }
);

impl DragSource {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

thread_local! {
    // Kept alive for the lifetime of the process; AppKit doesn't retain the source.
    static SOURCE: OnceCell<Retained<DragSource>> = const { OnceCell::new() };
}

fn window_under_mouse(app: &NSApplication, mouse: NSPoint, mtm: MainThreadMarker) -> Option<Retained<NSWindow>> {
    let number = NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(mouse, 0, mtm);
    if let Some(window) = app.windowWithWindowNumber(number) {
        return Some(window);
    }
    // Fallback: any visible window of ours containing the mouse
    app.windows().iter().find(|w| {
        let f = w.frame();
        w.isVisible()
            && mouse.x >= f.origin.x
            && mouse.x <= f.origin.x + f.size.width
            && mouse.y >= f.origin.y
            && mouse.y <= f.origin.y + f.size.height
    })
}

fn start(path: &Path) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("drag must start on the main thread")?;
    let path_str = path.to_str().ok_or("file path isn't valid UTF-8")?;

    let app = NSApplication::sharedApplication(mtm);
    let mouse = NSEvent::mouseLocation();
    let window = window_under_mouse(&app, mouse, mtm).ok_or("no plugin window under the mouse")?;
    let content = window.contentView().ok_or("plugin window has no view")?;
    let location = window.mouseLocationOutsideOfEventStream();
    let view = content.hitTest(location).unwrap_or(content);

    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseDragged,
        location,
        NSEventModifierFlags::empty(),
        NSProcessInfo::processInfo().systemUptime(),
        window.windowNumber(),
        None,
        0,
        1,
        1.0,
    )
    .ok_or("couldn't create the mouse event")?;

    let ns_path = NSString::from_str(path_str);
    let url = NSURL::fileURLWithPath(&ns_path);
    let item = NSDraggingItem::initWithPasteboardWriter(
        NSDraggingItem::alloc(),
        ProtocolObject::from_ref(&*url),
    );
    let icon = NSWorkspace::sharedWorkspace().iconForFile(&ns_path);
    let icon_obj: &AnyObject = &icon;
    let p = view.convertPoint_fromView(location, None);
    let frame = NSRect::new(NSPoint::new(p.x - 24.0, p.y - 24.0), NSSize::new(48.0, 48.0));
    unsafe { item.setDraggingFrame_contents(frame, Some(icon_obj)) };
    let items = NSArray::from_retained_slice(&[item]);

    SOURCE.with(|cell| {
        let source = cell.get_or_init(|| DragSource::new(mtm));
        let _session =
            view.beginDraggingSessionWithItems_event_source(&items, &event, ProtocolObject::from_ref(&**source));
    });
    Ok(())
}

/// Starts dragging `path` as a file. Must be called on the main (GUI) thread while the mouse
/// button is held down.
pub fn start_file_drag(path: &Path) -> Result<(), String> {
    #[cfg(feature = "objc-exception-guard")]
    {
        // Turn any Objective-C exception into an error instead of taking the DAW down with it
        let path = path.to_path_buf();
        match unsafe { objc2::exception::catch(std::panic::AssertUnwindSafe(|| start(&path))) } {
            Ok(result) => result,
            Err(_) => Err("macOS refused to start the drag".to_string()),
        }
    }
    #[cfg(not(feature = "objc-exception-guard"))]
    {
        start(path)
    }
}
