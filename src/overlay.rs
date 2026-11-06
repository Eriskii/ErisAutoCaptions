//! The overlay's window: a panel that floats over everything on every space, never takes focus, and
//! can be dragged by the body. Clicks beside the body fall through to whatever's below.

use std::cell::Cell;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSPanel, NSResponder, NSScreen, NSStatusWindowLevel, NSView, NSWindow,
    NSWindowAnimationBehavior, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSObject, NSRect, NSRunLoop, NSRunLoopCommonModes, NSTimer};

use crate::surface::{Content, Surface};

/// Checks of the pointer, ten a second, before the controls close behind it.
const HOVER_GRACE: u32 = 4;

define_class!(
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct Panel;

    impl Panel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            false
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool {
            false
        }
    }
);

/// What a press on the body has started.
#[derive(Clone, Copy, Default)]
enum Gesture {
    #[default]
    None,
    /// Moving the panel, with how far it is from the pointer to its corner.
    Move(f64, f64),
    /// Resizing, from where the pointer started and the size then.
    Resize { x: f64, y: f64, width: f64, lines: u32 },
}

#[derive(Default)]
struct ViewIvars {
    gesture: Cell<Gesture>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ViewIvars]
    struct DragView;

    impl DragView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let mut control = None;
            crate::app::with(|app| {
                control = app.overlay.surface.control_at(point);
                let pointer = NSEvent::mouseLocation();
                let origin = app.overlay.panel.frame().origin;
                let gesture = if control.is_some() {
                    Gesture::None
                } else if app.overlay.surface.handle_at(point) {
                    let (width, lines) = app.overlay.surface.body_size();
                    Gesture::Resize { x: pointer.x, y: pointer.y, width, lines }
                } else {
                    Gesture::Move(origin.x - pointer.x, origin.y - pointer.y)
                };
                app.overlay.gesturing = !matches!(gesture, Gesture::None);
                self.ivars().gesture.set(gesture);
            });
            if let Some(control) = control {
                crate::app::press(control, self);
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, _event: &NSEvent) {
            let pointer = NSEvent::mouseLocation();
            match self.ivars().gesture.get() {
                Gesture::None => {}
                Gesture::Move(x, y) => {
                    if let Some(window) = self.window() {
                        window.setFrameOrigin(CGPoint::new((pointer.x + x).round(), (pointer.y + y).round()));
                    }
                }
                Gesture::Resize { x, y, width, lines } => crate::app::with(|app| {
                    let height = app.overlay.surface.line_height();
                    // Down and to the right is bigger, one line at a time.
                    let more = ((y - pointer.y) / height).round() as i64;
                    let lines = (i64::from(lines) + more).clamp(1, i64::from(u32::MAX)) as u32;
                    app.overlay.resize(width + pointer.x - x, lines);
                }),
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            self.ivars().gesture.set(Gesture::None);
            crate::app::with(|app| {
                app.overlay.gesturing = false;
                app.overlay_changed();
            });
        }
    }
);

pub struct Overlay {
    panel: Retained<Panel>,
    pub surface: Surface,
    animation: Option<Retained<NSTimer>>,
    hover: Option<Retained<NSTimer>>,
    last_step: Instant,
    /// How long the pointer has been off the body, in checks of it.
    away: u32,
    /// The pointer's moving or resizing the body.
    gesturing: bool,
    /// One of the controls' menus is open.
    pub menu_open: bool,
}

impl Overlay {
    pub fn new(mtm: MainThreadMarker, position: Option<(f64, f64)>, width: f64, lines: u32) -> Self {
        let scale = NSScreen::mainScreen(mtm).map_or(2.0, |screen| screen.backingScaleFactor());
        let surface = Surface::new(scale, width, lines);
        let frame = NSRect::new(CGPoint::ZERO, surface.size);
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel = Panel::alloc(mtm).set_ivars(());
        // SAFETY: NSPanel's designated initializer.
        let panel: Retained<Panel> = unsafe {
            msg_send![super(panel), initWithContentRect: frame, styleMask: style, backing: NSBackingStoreType::Buffered, defer: false]
        };
        // SAFETY: an owned window is released through its Retained, not on close.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setHidesOnDeactivate(false);
        panel.setLevel(NSStatusWindowLevel);
        panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        panel.setIgnoresMouseEvents(true);

        let view = DragView::alloc(mtm).set_ivars(ViewIvars::default());
        // SAFETY: NSView's designated initializer.
        let view: Retained<DragView> = unsafe { msg_send![super(view), initWithFrame: frame] };
        view.setWantsLayer(true);
        if let Some(layer) = view.layer() {
            layer.addSublayer(&surface.root);
        }
        panel.setContentView(Some(&view));

        let mut overlay = Self {
            panel,
            surface,
            animation: None,
            hover: None,
            last_step: Instant::now(),
            away: 0,
            gesturing: false,
            menu_open: false,
        };
        overlay.place(position, mtm);
        overlay
    }

    /// Puts the panel where it was left, if that's on a screen, or low in the middle of the main one.
    pub fn place(&mut self, position: Option<(f64, f64)>, mtm: MainThreadMarker) {
        let screens: Vec<CGRect> = NSScreen::screens(mtm).iter().map(|screen| screen.visibleFrame()).collect();
        let main = NSScreen::mainScreen(mtm).map(|screen| screen.visibleFrame());
        let origin = placement(position, self.surface.size, &screens, main);
        self.panel.setFrameOrigin(CGPoint::new(origin.x.round(), origin.y.round()));
    }

    pub fn origin(&self) -> (f64, f64) {
        let origin = self.panel.frame().origin;
        (origin.x, origin.y)
    }

    /// Resizes the body, keeping its top left where it is.
    fn resize(&mut self, width: f64, lines: u32) {
        let (before, frame) = (self.surface.size, self.panel.frame());
        self.surface.set_size(width, lines);
        let size = self.surface.size;
        let origin = CGPoint::new(frame.origin.x, frame.origin.y - (size.height - before.height));
        self.panel.setFrame_display(NSRect::new(origin, size), true);
        self.surface.render();
    }

    pub fn set_content(&mut self, content: Content) {
        self.surface.set_content(content);
        self.animate();
    }

    pub fn show(&mut self) {
        self.panel.orderFrontRegardless();
        self.surface.set_shown(true);
        self.animate();
        if self.hover.is_none() {
            // Off the body from the start, so the controls wait for the pointer to arrive.
            self.away = HOVER_GRACE;
            let block =
                RcBlock::new(|_: std::ptr::NonNull<NSTimer>| crate::app::with(|app| app.overlay.follow_pointer()));
            self.hover = Some(repeating(0.1, &block));
        }
    }

    pub fn hide(&mut self) {
        self.surface.set_shown(false);
        self.animate();
        self.surface.set_hovered(false);
        if let Some(hover) = self.hover.take() {
            hover.invalidate();
        }
    }

    /// Takes clicks only over the body, so the rest of the panel doesn't get in the way, and opens the
    /// controls while the pointer's there.
    fn follow_pointer(&mut self) {
        if let Some(screen) = self.panel.screen() {
            self.surface.set_scale(screen.backingScaleFactor());
        }
        let pointer = NSEvent::mouseLocation();
        let origin = self.panel.frame().origin;
        let body = self.surface.body_rect();
        let inside = pointer.x >= origin.x + body.origin.x
            && pointer.x <= origin.x + body.origin.x + body.size.width
            && pointer.y >= origin.y + body.origin.y
            && pointer.y <= origin.y + body.origin.y + body.size.height;
        self.panel.setIgnoresMouseEvents(!(inside || self.gesturing));
        self.away = if inside || self.gesturing || self.menu_open { 0 } else { self.away + 1 };
        // A moment's grace, for a pointer that only slipped off the edge.
        if self.surface.set_hovered(self.away < HOVER_GRACE) {
            self.animate();
        }
    }

    fn animate(&mut self) {
        if self.animation.is_some() {
            return;
        }
        self.last_step = Instant::now();
        let block = RcBlock::new(|_: std::ptr::NonNull<NSTimer>| crate::app::with(|app| app.overlay.tick()));
        self.animation = Some(repeating(1.0 / 60.0, &block));
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_step).as_secs_f64().min(1.0 / 15.0);
        self.last_step = now;
        let settled = self.surface.step(dt);
        self.surface.render();
        if settled {
            if let Some(timer) = self.animation.take() {
                timer.invalidate();
            }
            if self.surface.is_hidden() {
                self.panel.orderOut(None);
            }
        }
    }
}

/// A repeating timer that keeps going while a menu is open.
pub fn repeating(interval: f64, block: &block2::DynBlock<dyn Fn(std::ptr::NonNull<NSTimer>)>) -> Retained<NSTimer> {
    // SAFETY: the timer copies the block, and it runs on this thread's run loop.
    unsafe {
        let timer = NSTimer::timerWithTimeInterval_repeats_block(interval, true, block);
        NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        timer
    }
}

/// Where a panel of `size` goes: `saved`, if the body would still be mostly on one of the screens,
/// and otherwise low in the middle of the main one.
fn placement(saved: Option<(f64, f64)>, size: CGSize, screens: &[CGRect], main: Option<CGRect>) -> CGPoint {
    if let Some((x, y)) = saved {
        let on_screen = screens.iter().any(|screen| {
            let middle = x + size.width / 2.0;
            middle >= screen.origin.x
                && middle <= screen.origin.x + screen.size.width
                && y + size.height / 2.0 >= screen.origin.y
                && y <= screen.origin.y + screen.size.height
        });
        if on_screen {
            return CGPoint::new(x, y);
        }
    }
    let screen = main.or(screens.first().copied()).unwrap_or(CGRect::new(CGPoint::ZERO, CGSize::new(1440.0, 900.0)));
    CGPoint::new(screen.origin.x + (screen.size.width - size.width) / 2.0, screen.origin.y + 24.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: CGSize = CGSize::new(700.0, 150.0);

    fn screen(x: f64, y: f64, width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
    }

    #[test]
    fn a_saved_spot_on_a_screen_is_kept() {
        let screens = [screen(0.0, 0.0, 1440.0, 900.0), screen(1440.0, 0.0, 1920.0, 1080.0)];
        let origin = placement(Some((2000.0, 500.0)), SIZE, &screens, Some(screens[0]));
        assert_eq!((origin.x, origin.y), (2000.0, 500.0));
    }

    #[test]
    fn a_spot_on_an_unplugged_screen_comes_back_to_the_main_one() {
        let screens = [screen(0.0, 0.0, 1440.0, 900.0)];
        let origin = placement(Some((2000.0, 500.0)), SIZE, &screens, Some(screens[0]));
        assert_eq!((origin.x, origin.y), (370.0, 24.0));
    }

    #[test]
    fn with_nothing_saved_it_sits_low_in_the_middle() {
        let screens = [screen(100.0, 40.0, 1000.0, 800.0)];
        let origin = placement(None, SIZE, &screens, Some(screens[0]));
        assert_eq!((origin.x, origin.y), (250.0, 64.0));
    }
}
