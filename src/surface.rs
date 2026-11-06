//! The overlay's layers: the dock's black body, with the captions rising inside it, and the controls
//! and resize handle that appear when the pointer is over it. Everything moves on springs, stepped
//! once a frame and then drawn by `render`.
//!
//! Each line of text is its own layer, wrapped here rather than by Core Animation, so a new word only
//! redraws the line it's on, and old lines can leave without the rest re-wrapping.

use std::ptr;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSColor, NSFont, NSFontAttributeName, NSFontWeightMedium, NSForegroundColorAttributeName};
use objc2_core_foundation::{CFAttributedString, CFRange, CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGMutablePath, CGPath};
use objc2_core_text::{CTFramesetter, CTTypesetter};
use objc2_foundation::{NSAttributedString, NSAttributedStringKey, NSDictionary, NSString};
use objc2_quartz_core::{CALayer, CAShapeLayer, CATextLayer, CATransaction, CATransform3D, kCALineCapRound};

use crate::shape::smooth_rect;
use crate::spring::Spring;

pub const MIN_WIDTH: f64 = 480.0;
pub const MAX_WIDTH: f64 = 1600.0;
pub const MIN_LINES: u32 = 1;
pub const MAX_LINES: u32 = 8;

const PAD_X: f64 = 24.0;
const PAD_Y: f64 = 14.0;
const FONT_SIZE: f64 = 20.0;
const CONTROL_FONT_SIZE: f64 = 13.0;
/// Lines kept above the visible ones, for the scroll to carry out of sight.
const SPARE_LINES: usize = 2;
/// What the controls add to the body, when they're showing.
const CONTROLS_HEIGHT: f64 = 42.0;
const PILL_HEIGHT: f64 = 28.0;
const PILL_PAD: f64 = 12.0;
const PILL_GAP: f64 = 8.0;
/// The dock's default corners.
const CORNER_RADIUS: f64 = 22.0;
const SMOOTHING: f64 = 0.6;
/// Room around the body for its shadow.
const MARGIN: f64 = 28.0;
/// How far from the body's bottom right corner a press takes the handle.
const HANDLE_REACH: f64 = 30.0;
const HANDLE_RADIUS: f64 = 15.0;
const HANDLE_INSET: f64 = 7.0;
/// The dock's "expressive" springs: quick and direct, with a little give at the end.
const RESIZE: (f64, f64) = (0.34, 0.72);
/// No overshoot: the text glides up a line and stops.
const SCROLL: (f64, f64) = (0.42, 1.0);
const APPEAR: (f64, f64) = (0.3, 0.68);
const HOVER: (f64, f64) = (0.3, 0.8);

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    /// A note about what's going on: dim, one line, in a body just big enough.
    Status(String),
    /// Captions: bright, in a body of the chosen size, scrolling to show the end.
    Caption(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Source,
    Model,
    Language,
    TurnOff,
}

struct Pill {
    control: Control,
    layer: Retained<CALayer>,
    text: Retained<CATextLayer>,
    rect: CGRect,
}

struct Row {
    layer: Retained<CATextLayer>,
    drawn: (String, u8),
}

pub struct Surface {
    pub root: Retained<CALayer>,
    pub size: CGSize,
    body: Retained<CAShapeLayer>,
    clip: Retained<CALayer>,
    /// Holds the controls and the handle, clipped to the body so they can't show outside it.
    reveal: Retained<CALayer>,
    reveal_mask: Retained<CAShapeLayer>,
    controls: Retained<CALayer>,
    pills: Vec<Pill>,
    handle: Retained<CAShapeLayer>,
    font: Retained<NSFont>,
    control_font: Retained<NSFont>,
    scale: f64,
    line_height: f64,
    /// The body's size for captions, as chosen with the handle.
    full_width: f64,
    lines: u32,
    content: Option<Content>,
    status_width: f64,
    hovered: bool,
    /// Where in the caption's text the first row starts, and what it says, to find it again.
    start: usize,
    anchor: String,
    rows: Vec<Row>,
    width: Spring,
    height: Spring,
    /// How far the text has risen past the top of the clip.
    scroll: Spring,
    appear: Spring,
    hover: Spring,
}

impl Surface {
    pub fn new(scale: f64, width: f64, lines: u32) -> Self {
        let font = NSFont::systemFontOfSize_weight(FONT_SIZE, unsafe { NSFontWeightMedium });
        let control_font = NSFont::systemFontOfSize_weight(CONTROL_FONT_SIZE, unsafe { NSFontWeightMedium });
        let line_height = measure(&attributed("Ag", &font, 1.0), f64::MAX).height.ceil();

        let root = CALayer::new();
        root.setOpacity(0.0);

        let black = CGColor::new_generic_gray(0.0, 1.0);
        let body = CAShapeLayer::new();
        body.setFillColor(Some(&black));
        body.setShadowColor(Some(&black));
        body.setShadowOpacity(0.35);
        body.setShadowRadius(12.0);
        body.setShadowOffset(CGSize::ZERO);
        root.addSublayer(&body);

        let clip = CALayer::new();
        clip.setMasksToBounds(true);
        clip.setAnchorPoint(CGPoint::ZERO);
        root.addSublayer(&clip);

        let controls = CALayer::new();
        controls.setAnchorPoint(CGPoint::ZERO);
        let reveal = CALayer::new();
        reveal.setAnchorPoint(CGPoint::ZERO);
        reveal.setAllowsGroupOpacity(true);
        reveal.setOpacity(0.0);
        let reveal_mask = CAShapeLayer::new();
        reveal_mask.setFillColor(Some(&black));
        // SAFETY: a layer may be masked by a layer nothing else holds as a sublayer.
        unsafe { reveal.setMask(Some(&reveal_mask)) };
        reveal.addSublayer(&controls);
        root.addSublayer(&reveal);

        let handle = CAShapeLayer::new();
        handle.setFillColor(None);
        handle.setStrokeColor(Some(&CGColor::new_generic_gray(1.0, 0.6)));
        handle.setLineWidth(2.5);
        // SAFETY: a CAShapeLayer takes this constant.
        handle.setLineCap(unsafe { kCALineCapRound });
        reveal.addSublayer(&handle);

        let pills = [Control::Source, Control::Model, Control::Language, Control::TurnOff]
            .into_iter()
            .map(|control| {
                let layer = CALayer::new();
                layer.setBackgroundColor(Some(&CGColor::new_generic_gray(1.0, 0.16)));
                layer.setCornerRadius(PILL_HEIGHT / 2.0);
                layer.setAnchorPoint(CGPoint::ZERO);
                let text = CATextLayer::new();
                text.setContentsScale(scale);
                text.setAnchorPoint(CGPoint::ZERO);
                layer.addSublayer(&text);
                controls.addSublayer(&layer);
                Pill { control, layer, text, rect: CGRect::ZERO }
            })
            .collect();

        let resting = 2.0 * PAD_Y + line_height;
        let spring = |value, (response, damping): (f64, f64)| Spring::new(value, response, damping);
        let mut surface = Self {
            root,
            size: CGSize::ZERO,
            body,
            clip,
            reveal,
            reveal_mask,
            controls,
            pills,
            handle,
            font,
            control_font,
            scale,
            line_height,
            full_width: width,
            lines,
            content: None,
            status_width: resting,
            hovered: false,
            start: 0,
            anchor: String::new(),
            rows: Vec::new(),
            width: spring(resting, RESIZE).with_epsilon(0.05),
            height: spring(resting, RESIZE).with_epsilon(0.05),
            scroll: spring(0.0, SCROLL).with_epsilon(0.05),
            appear: spring(0.0, APPEAR),
            hover: spring(0.0, HOVER),
        };
        surface.set_size(width, lines);
        surface
    }

    pub fn line_height(&self) -> f64 {
        self.line_height
    }

    pub fn body_size(&self) -> (f64, u32) {
        (self.full_width, self.lines)
    }

    /// Sets the body's size for captions, and the room around it. The panel has to follow `size`.
    pub fn set_size(&mut self, width: f64, lines: u32) {
        self.full_width = width.clamp(MIN_WIDTH, MAX_WIDTH).round();
        self.lines = lines.clamp(MIN_LINES, MAX_LINES);
        self.size = CGSize::new(
            self.full_width + 2.0 * MARGIN,
            2.0 * PAD_Y + f64::from(self.lines) * self.line_height + CONTROLS_HEIGHT + 2.0 * MARGIN,
        );
        without_animation(|| {
            self.root.setBounds(CGRect::new(CGPoint::ZERO, self.size));
            // It scales in from the middle of the body's foot.
            self.root.setAnchorPoint(CGPoint::new(0.5, MARGIN / self.size.height));
            self.root.setPosition(CGPoint::new(self.size.width / 2.0, MARGIN));
            let mask: &CALayer = &self.reveal_mask;
            for layer in [&*self.reveal, mask] {
                layer.setBounds(CGRect::new(CGPoint::ZERO, self.size));
                layer.setAnchorPoint(CGPoint::ZERO);
                layer.setPosition(CGPoint::ZERO);
            }
        });
        self.lay_out_text();
        self.aim();
        // The pointer's dragging this directly, so nothing lags.
        self.width.settle();
        self.height.settle();
        self.scroll.settle();
        self.lay_out_pills();
    }

    /// Text is drawn for this many pixels to the point: the screen the overlay's on.
    pub fn set_scale(&mut self, scale: f64) {
        if self.scale == scale {
            return;
        }
        self.scale = scale;
        without_animation(|| {
            let rows = self.rows.iter().map(|row| &row.layer);
            for layer in rows.chain(self.pills.iter().map(|pill| &pill.text)) {
                layer.setContentsScale(scale);
                layer.setNeedsDisplay();
            }
        });
    }

    /// The nearest whole pixel, where text stays sharp.
    fn snap(&self, value: f64) -> f64 {
        (value * self.scale).round() / self.scale
    }

    /// The body as it is now, in the root layer.
    pub fn body_rect(&self) -> CGRect {
        CGRect::new(
            CGPoint::new((self.size.width - self.width.value) / 2.0, MARGIN),
            CGSize::new(self.width.value, self.height.value + self.hover.value.max(0.0) * CONTROLS_HEIGHT),
        )
    }

    pub fn set_shown(&mut self, shown: bool) {
        self.appear.animate(if shown { 1.0 } else { 0.0 });
    }

    pub fn is_hidden(&self) -> bool {
        self.appear.is_resting() && self.appear.target == 0.0
    }

    /// Shows or hides the controls. Returns whether that's a change.
    pub fn set_hovered(&mut self, hovered: bool) -> bool {
        if self.hovered == hovered {
            return false;
        }
        self.hovered = hovered;
        self.hover.animate(if hovered { 1.0 } else { 0.0 });
        self.aim();
        true
    }

    /// What the source, model and language buttons say, and the last one, which turns captions off.
    pub fn set_controls(&mut self, labels: [&str; 4]) {
        without_animation(|| {
            for (pill, label) in self.pills.iter().zip(labels) {
                let string = attributed(label, &self.control_font, 0.92);
                // SAFETY: a CATextLayer takes an attributed string.
                unsafe { pill.text.setString(Some(as_object(&*string))) };
            }
        });
        self.lay_out_pills();
    }

    /// How far the controls sit below where they're laid out, as the body may be shorter than full.
    fn controls_drop(&self) -> f64 {
        self.height.value - (2.0 * PAD_Y + f64::from(self.lines) * self.line_height)
    }

    fn lay_out_pills(&mut self) {
        let mut x = MARGIN + PAD_X - PILL_PAD;
        let y = MARGIN + 2.0 * PAD_Y + f64::from(self.lines) * self.line_height + 4.0;
        let right = MARGIN + self.full_width - PAD_X + PILL_PAD;
        let room = right - x;
        let widths: Vec<f64> = self.pills.iter().map(pill_width).collect();
        // The first, the source, gives way if its name is too long for them all.
        let others: f64 = widths[1..].iter().sum::<f64>() + 3.0 * PILL_GAP;
        let source = widths[0].min(room - others).max(PILL_PAD * 2.0);
        without_animation(|| {
            for (i, pill) in self.pills.iter_mut().enumerate() {
                let width = if i == 0 { source } else { widths[i] };
                // The last sits apart, at the far end.
                let left = if pill.control == Control::TurnOff { right - width } else { x };
                pill.rect = CGRect::new(CGPoint::new(left, y), CGSize::new(width, PILL_HEIGHT));
                pill.layer.setBounds(CGRect::new(CGPoint::ZERO, pill.rect.size));
                pill.layer.setPosition(pill.rect.origin);
                pill.layer.setMasksToBounds(true);
                pill.text
                    .setBounds(CGRect::new(CGPoint::ZERO, CGSize::new((width - 2.0 * PILL_PAD).max(0.0), PILL_HEIGHT)));
                pill.text.setPosition(CGPoint::new(PILL_PAD, -6.0));
                x += width + PILL_GAP;
            }
        });
    }

    /// The control at a point in the root layer, if they're showing.
    pub fn control_at(&self, point: CGPoint) -> Option<Control> {
        if !self.hovered {
            return None;
        }
        let point = CGPoint::new(point.x, point.y - self.controls_drop());
        self.pills.iter().find(|pill| contains(pill.rect, point)).map(|pill| pill.control)
    }

    /// Where a control's menu opens: the control's corner, in the root layer.
    pub fn control_rect(&self, control: Control) -> CGPoint {
        let rect = self.pills.iter().find(|pill| pill.control == control).map_or(CGRect::ZERO, |pill| pill.rect);
        CGPoint::new(rect.origin.x, rect.origin.y + self.controls_drop())
    }

    /// Whether a point in the root layer is on the resize handle, if it's showing.
    pub fn handle_at(&self, point: CGPoint) -> bool {
        let body = self.body_rect();
        self.hovered
            && (body.origin.x + body.size.width - point.x).abs() < HANDLE_REACH
            && (point.y - body.origin.y).abs() < HANDLE_REACH
    }

    pub fn set_content(&mut self, content: Content) {
        if self.content.as_ref() == Some(&content) {
            return;
        }
        if matches!(content, Content::Status(_)) {
            self.start = 0;
            self.anchor.clear();
            self.scroll.set(0.0);
        }
        self.content = Some(content);
        self.lay_out_text();
        self.aim();
    }

    /// Breaks the text into rows and updates the rows' layers, leaving alone those still the same.
    fn lay_out_text(&mut self) {
        let inner = self.full_width - 2.0 * PAD_X;
        let visible = self.lines as usize;
        let (mut rows, brightness): (Vec<(String, usize)>, f64) = match &self.content {
            None => return,
            Some(Content::Status(text)) => {
                self.status_width = (measure(&attributed(text, &self.font, 1.0), f64::MAX).width.ceil() + 2.0 * PAD_X)
                    .min(self.full_width);
                (vec![(text.clone(), text.len())], 0.6)
            }
            Some(Content::Caption(text)) => {
                self.start = find_start(text, self.start, &self.anchor);
                (wrap(&text[self.start..], &self.font, inner), 1.0)
            }
        };

        // Rows that have scrolled out of sight leave, with the scroll adjusted so nothing moves.
        let excess = rows.len().saturating_sub(visible + SPARE_LINES);
        if excess > 0 {
            self.start += rows[..excess].iter().map(|(_, bytes)| bytes).sum::<usize>();
            rows.drain(..excess);
            let moved = excess as f64 * self.line_height;
            self.scroll.value -= moved;
            self.scroll.target -= moved;
            self.scroll.value = self.scroll.value.max(0.0);
            self.scroll.target = self.scroll.target.max(0.0);
            // Those rows' layers leave too, so the ones left keep what they've drawn.
            for row in self.rows.drain(..excess.min(self.rows.len())) {
                row.layer.removeFromSuperlayer();
            }
        }
        self.anchor = rows.first().map_or(String::new(), |(text, _)| text.clone());

        let level = (brightness * 100.0) as u8;
        let (font, scale, clip) = (&self.font, self.scale, &self.clip);
        without_animation(|| {
            while self.rows.len() > rows.len() {
                if let Some(row) = self.rows.pop() {
                    row.layer.removeFromSuperlayer();
                }
            }
            while self.rows.len() < rows.len() {
                let layer = CATextLayer::new();
                layer.setContentsScale(scale);
                layer.setAnchorPoint(CGPoint::new(0.0, 1.0));
                clip.addSublayer(&layer);
                self.rows.push(Row { layer, drawn: (String::new(), 0) });
            }
            for (row, (text, _)) in self.rows.iter_mut().zip(&rows) {
                if row.drawn.0 != *text || row.drawn.1 != level {
                    let string = attributed(text, font, brightness);
                    // SAFETY: a CATextLayer takes an attributed string.
                    unsafe { row.layer.setString(Some(as_object(&*string))) };
                    row.drawn = (text.clone(), level);
                }
            }
        });
        let scrolled = rows.len().saturating_sub(visible) as f64 * self.line_height;
        self.scroll.animate(if matches!(self.content, Some(Content::Status(_))) { 0.0 } else { scrolled });
    }

    /// Sets the body's springs going to the size it should be now.
    fn aim(&mut self) {
        let caption = matches!(self.content, Some(Content::Caption(_)));
        // With the pointer over it, it's the size it captions at, as that's what the handle resizes.
        let tall = if caption || self.hovered { f64::from(self.lines) } else { 1.0 };
        self.width.animate(if caption || self.hovered { self.full_width } else { self.status_width });
        self.height.animate(2.0 * PAD_Y + tall * self.line_height);
    }

    /// Moves everything on by `dt` seconds. Returns whether it's come to rest.
    pub fn step(&mut self, dt: f64) -> bool {
        let springs = [&mut self.width, &mut self.height, &mut self.scroll, &mut self.appear, &mut self.hover];
        springs.into_iter().fold(true, |settled, spring| {
            spring.step(dt);
            settled && spring.is_resting()
        })
    }

    /// Jumps to where everything is heading.
    pub fn settle(&mut self) {
        for spring in [&mut self.width, &mut self.height, &mut self.scroll, &mut self.appear, &mut self.hover] {
            spring.settle();
        }
    }

    pub fn render(&self) {
        without_animation(|| {
            let rect = self.body_rect();
            let path =
                smooth_rect(rect.origin.x, rect.origin.y, rect.size.width, rect.size.height, CORNER_RADIUS, SMOOTHING);
            let path: &CGPath = &path;
            self.body.setPath(Some(path));
            self.body.setShadowPath(Some(path));

            // The text keeps to the body's foot; the controls open above it.
            let clip = CGSize::new(
                self.snap((rect.size.width - 2.0 * PAD_X).max(0.0)),
                self.snap((self.height.value - 2.0 * PAD_Y).max(0.0)),
            );
            self.clip.setBounds(CGRect::new(CGPoint::ZERO, clip));
            self.clip.setPosition(CGPoint::new(self.snap(rect.origin.x + PAD_X), self.snap(rect.origin.y + PAD_Y)));
            // Whole sizes and places: otherwise the text's bitmap gets resampled, and goes soft.
            let row_size = CGSize::new(self.full_width - 2.0 * PAD_X, (self.line_height * 1.3).ceil());
            for (i, row) in self.rows.iter().enumerate() {
                row.layer.setBounds(CGRect::new(CGPoint::ZERO, row_size));
                let top = clip.height + self.scroll.value - i as f64 * self.line_height;
                row.layer.setPosition(CGPoint::new(0.0, self.snap(top)));
            }

            // They come in once the body has made room, and go first when it closes.
            let opened = ((self.hover.value - 0.6) / 0.4).clamp(0.0, 1.0);
            let widened = ((self.width.value - (self.full_width - 40.0)) / 40.0).clamp(0.0, 1.0);
            self.reveal.setOpacity(opened.min(widened) as f32);
            self.reveal_mask.setPath(Some(path));
            self.controls.setPosition(CGPoint::new(0.0, self.snap(self.controls_drop())));
            let (right, bottom) = (rect.origin.x + rect.size.width, rect.origin.y);
            self.handle.setPath(Some(&*handle_path(right, bottom)));

            let appear = self.appear.value;
            self.root.setOpacity(appear.clamp(0.0, 1.0) as f32);
            // Exactly the identity at rest, so nothing's resampled.
            let scale = if self.appear.is_resting() { 1.0 } else { 0.9 + 0.1 * appear };
            self.root.setTransform(CATransform3D::new_scale(scale, scale, 1.0));
        });
    }
}

fn without_animation<R>(f: impl FnOnce() -> R) -> R {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    let result = f();
    CATransaction::commit();
    result
}

fn contains(rect: CGRect, point: CGPoint) -> bool {
    point.x >= rect.origin.x
        && point.x <= rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y <= rect.origin.y + rect.size.height
}

/// A curve following the inside of the body's bottom right corner.
fn handle_path(right: f64, bottom: f64) -> objc2_core_foundation::CFRetained<CGMutablePath> {
    let path = CGMutablePath::new();
    let target = Some(&*path);
    let (x, y) = (right - HANDLE_INSET, bottom + HANDLE_INSET);
    // SAFETY: a null transform is allowed, and means none.
    unsafe {
        CGMutablePath::move_to_point(target, ptr::null(), x, y + HANDLE_RADIUS);
        CGMutablePath::add_curve_to_point(
            target,
            ptr::null(),
            x,
            y + HANDLE_RADIUS * 0.45,
            x - HANDLE_RADIUS * 0.55,
            y,
            x - HANDLE_RADIUS,
            y,
        );
    }
    path
}

fn pill_width(pill: &Pill) -> f64 {
    let string = pill.text.string();
    let width = string
        .and_then(|string| string.downcast::<NSAttributedString>().ok())
        .map_or(0.0, |string| measure(&string, f64::MAX).width.ceil());
    width + 2.0 * PILL_PAD
}

/// Where the caption's rows now start: the same place if that's still what it says, or wherever those
/// words have moved to as older text was cut off, or the beginning.
fn find_start(text: &str, start: usize, anchor: &str) -> usize {
    if anchor.is_empty() {
        return 0;
    }
    if text.get(start..).is_some_and(|rest| rest.starts_with(anchor)) {
        return start;
    }
    text.rfind(anchor).unwrap_or(0)
}

/// Breaks text into rows that fit `width`, as what to draw and how many bytes of `text` each took.
fn wrap(text: &str, font: &NSFont, width: f64) -> Vec<(String, usize)> {
    if text.is_empty() {
        return Vec::new();
    }
    let string = attributed(text, font, 1.0);
    let units: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: NSAttributedString is toll-free bridged with CFAttributedString.
    let typesetter = unsafe {
        let string: &CFAttributedString = &*ptr::from_ref(&*string).cast();
        CTTypesetter::with_attributed_string(string)
    };
    let mut rows = Vec::new();
    let mut at = 0;
    while at < units.len() {
        // SAFETY: the index is in the string, and it returns how many units fit.
        let count = unsafe { typesetter.suggest_line_break(at as isize, width) }.max(1) as usize;
        let end = (at + count).min(units.len());
        let row = String::from_utf16_lossy(&units[at..end]);
        rows.push((row.trim_end().to_owned(), row.len()));
        at = end;
    }
    rows
}

fn attributed(text: &str, font: &NSFont, brightness: f64) -> Retained<NSAttributedString> {
    let color = NSColor::colorWithWhite_alpha(brightness, 1.0);
    // SAFETY: the keys are AppKit's statics, with the types they ask for.
    unsafe {
        let keys: [&NSAttributedStringKey; 2] = [NSFontAttributeName, NSForegroundColorAttributeName];
        let values: [&AnyObject; 2] = [as_object(font), as_object(&*color)];
        let attributes = NSDictionary::from_slices(&keys, &values);
        NSAttributedString::new_with_attributes(&NSString::from_str(text), &attributes)
    }
}

fn as_object<T: objc2::Message>(object: &T) -> &AnyObject {
    // SAFETY: every Objective-C object is an AnyObject.
    unsafe { &*ptr::from_ref(object).cast() }
}

/// The size Core Text lays this out in.
fn measure(text: &NSAttributedString, width: CGFloat) -> CGSize {
    // SAFETY: NSAttributedString is toll-free bridged with CFAttributedString, and a zero-length
    // range means all of it.
    unsafe {
        let text: &CFAttributedString = &*ptr::from_ref(text).cast();
        let framesetter = CTFramesetter::with_attributed_string(text);
        framesetter.suggest_frame_size_with_constraints(
            CFRange { location: 0, length: 0 },
            None,
            CGSize::new(width, CGFloat::MAX),
            ptr::null_mut(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface() -> Surface {
        let mut surface = Surface::new(2.0, 680.0, 2);
        surface.set_shown(true);
        surface
    }

    /// `count` different words, long enough to wrap many times.
    fn speech(count: usize) -> String {
        (0..count).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" ")
    }

    fn drawn(surface: &Surface) -> Vec<String> {
        surface.rows.iter().map(|row| row.drawn.0.clone()).collect()
    }

    /// Where a row's top sits relative to the clip's top, at rest.
    fn resting_tops(surface: &Surface) -> Vec<f64> {
        (0..surface.rows.len()).map(|i| surface.scroll.target - i as f64 * surface.line_height).collect()
    }

    #[test]
    fn wrapping_keeps_every_byte_and_fits_the_width() {
        let font = NSFont::systemFontOfSize_weight(FONT_SIZE, unsafe { NSFontWeightMedium });
        let text = speech(120);
        let rows = wrap(&text, &font, 400.0);
        assert!(rows.len() > 3);
        assert_eq!(rows.iter().map(|(_, bytes)| bytes).sum::<usize>(), text.len());
        for (row, _) in &rows {
            assert!(measure(&attributed(row, &font, 1.0), f64::MAX).width <= 401.0, "{row}");
        }
    }

    #[test]
    fn wrapping_handles_text_without_spaces() {
        let font = NSFont::systemFontOfSize_weight(FONT_SIZE, unsafe { NSFontWeightMedium });
        let text = "これは字幕です。".repeat(30);
        let rows = wrap(&text, &font, 300.0);
        assert!(rows.len() > 1);
        assert_eq!(rows.iter().map(|(_, bytes)| bytes).sum::<usize>(), text.len());
    }

    #[test]
    fn the_end_of_the_text_rests_at_the_bottom_of_the_body() {
        let mut surface = surface();
        surface.set_content(Content::Caption(speech(200)));
        let last = *resting_tops(&surface).last().unwrap();
        // With two lines showing, the last row's top is one line down from the clip's top.
        assert_eq!(last, -surface.line_height);
    }

    #[test]
    fn old_rows_leave_without_moving_or_rewrapping_the_rest() {
        let mut surface = surface();
        let words = 400;
        surface.set_content(Content::Caption(speech(words)));
        surface.scroll.settle();
        for extra in 1..200 {
            let height = surface.line_height;
            let before: Vec<(String, f64)> = drawn(&surface).into_iter().zip(resting_tops(&surface)).collect();
            // Where the scroll is on its way from, so that a row that leaves doesn't jolt the others.
            let scrolled = surface.scroll.value;
            surface.set_content(Content::Caption(speech(words + extra)));
            assert!(surface.rows.len() <= surface.lines as usize + SPARE_LINES);
            let dropped = before.iter().position(|(row, _)| *row == surface.rows[0].drawn.0).expect("rows carry over");
            assert_eq!(surface.scroll.value, scrolled - dropped as f64 * height);
            // Every row that stayed reads the same, except perhaps the one being added to.
            let after = drawn(&surface);
            for (i, (row, _)) in before[dropped..before.len() - 1].iter().enumerate() {
                assert_eq!(after[i], *row);
            }
            surface.scroll.settle();
        }
    }

    #[test]
    fn cutting_the_oldest_text_does_not_disturb_the_rows() {
        let mut surface = surface();
        let text = speech(300);
        surface.set_content(Content::Caption(text.clone()));
        let before = drawn(&surface);
        // The same words, with a stretch gone from the front, as `Captions` does.
        let cut = text.find("word50 ").unwrap();
        surface.set_content(Content::Caption(text[cut..].to_owned()));
        assert_eq!(drawn(&surface), before);
    }

    #[test]
    fn a_status_clears_the_scroll_and_shrinks_the_body() {
        let mut surface = surface();
        surface.set_content(Content::Caption(speech(200)));
        surface.set_content(Content::Status("Listening…".to_owned()));
        surface.settle();
        assert_eq!(surface.scroll.value, 0.0);
        assert_eq!(drawn(&surface), ["Listening…"]);
        assert!(surface.body_rect().size.width < 300.0);
    }

    #[test]
    fn hovering_grows_the_body_upward_only() {
        let mut surface = surface();
        surface.set_content(Content::Caption(speech(5)));
        surface.settle();
        let rest = surface.body_rect();
        assert!(surface.set_hovered(true));
        assert!(!surface.set_hovered(true));
        surface.settle();
        let open = surface.body_rect();
        assert_eq!(open.origin.y, rest.origin.y);
        assert_eq!(open.size.height, rest.size.height + CONTROLS_HEIGHT);
    }

    #[test]
    fn the_controls_only_answer_while_showing() {
        let mut surface = surface();
        surface.set_controls(["System Audio", "Big", "English", "Turn Off"]);
        surface.set_content(Content::Caption(speech(5)));
        surface.settle();
        let middle = |rect: CGRect| CGPoint::new(rect.origin.x + 5.0, rect.origin.y + 5.0);
        let point = middle(surface.pills[1].rect);
        assert_eq!(surface.control_at(point), None);
        surface.set_hovered(true);
        surface.settle();
        assert_eq!(surface.control_at(point), Some(Control::Model));
        assert_eq!(surface.control_at(middle(surface.pills[3].rect)), Some(Control::TurnOff));
    }

    #[test]
    fn hovering_a_status_opens_the_body_to_its_full_size() {
        let mut surface = surface();
        surface.set_controls(["System Audio", "Big", "English", "Turn Off"]);
        surface.set_content(Content::Status("Listening…".to_owned()));
        surface.settle();
        let small = surface.body_rect();
        assert_eq!(small.size.height, 2.0 * PAD_Y + surface.line_height);
        surface.set_hovered(true);
        surface.settle();
        let open = surface.body_rect();
        assert_eq!(open.size.width, 680.0);
        assert_eq!(open.size.height, 2.0 * PAD_Y + 2.0 * surface.line_height + CONTROLS_HEIGHT);
        assert_eq!(surface.controls_drop(), 0.0);
        for pill in &surface.pills {
            assert!(pill.rect.origin.y + pill.rect.size.height <= open.origin.y + open.size.height);
        }
        let rect = surface.pills[2].rect;
        assert_eq!(surface.control_at(CGPoint::new(rect.origin.x + 5.0, rect.origin.y + 5.0)), Some(Control::Language));
    }

    #[test]
    fn resizing_changes_the_height_with_or_without_captions() {
        for content in [Content::Status("Listening…".to_owned()), Content::Caption(speech(5))] {
            let mut surface = surface();
            surface.set_content(content);
            surface.set_hovered(true);
            surface.settle();
            let before = surface.body_rect().size.height;
            surface.set_size(680.0, 5);
            assert_eq!(surface.body_rect().size.height, before + 3.0 * surface.line_height);
            // Its foot stays at the same place in the panel, which grows downward to match.
            assert_eq!(surface.body_rect().origin.y, MARGIN);
        }
    }

    #[test]
    fn the_controls_wait_for_the_body_to_make_room() {
        let mut surface = surface();
        surface.set_controls(["System Audio", "Big", "English", "Turn Off"]);
        surface.set_content(Content::Status("Listening…".to_owned()));
        surface.settle();
        surface.set_hovered(true);
        surface.render();
        assert_eq!(surface.reveal.opacity(), 0.0);
        // Partway through opening they're still hidden, with the body short of holding them.
        surface.step(0.03);
        surface.render();
        assert_eq!(surface.reveal.opacity(), 0.0, "hover {}", surface.hover.value);
        surface.settle();
        surface.render();
        assert_eq!(surface.reveal.opacity(), 1.0);
    }

    #[test]
    fn text_sits_on_whole_pixels_at_rest() {
        for scale in [1.0, 2.0] {
            let mut surface = Surface::new(scale, 683.4, 3);
            surface.set_content(Content::Caption(speech(120)));
            surface.settle();
            surface.render();
            assert_eq!(surface.body_size().0, 683.0);
            let on_pixel = |value: f64| (value * scale).fract() == 0.0;
            for row in &surface.rows {
                let (bounds, position) = (row.layer.bounds(), row.layer.position());
                assert!(on_pixel(bounds.size.width) && on_pixel(bounds.size.height), "{bounds:?}");
                assert!(on_pixel(position.x) && on_pixel(position.y), "{position:?}");
            }
            let clip = surface.clip.position();
            assert!(on_pixel(clip.x) && on_pixel(clip.y), "{clip:?}");
            assert_eq!(surface.root.transform().m11, 1.0);
        }
    }

    #[test]
    fn moving_to_another_screen_redraws_the_text_for_it() {
        let mut surface = surface();
        surface.set_content(Content::Caption(speech(5)));
        surface.set_scale(1.0);
        assert_eq!(surface.rows[0].layer.contentsScale(), 1.0);
        assert_eq!(surface.pills[0].text.contentsScale(), 1.0);
        surface.set_content(Content::Caption(speech(60)));
        assert!(surface.rows.iter().all(|row| row.layer.contentsScale() == 1.0));
    }

    #[test]
    fn the_pills_stay_inside_the_body_with_a_long_source_name() {
        let mut surface = Surface::new(2.0, MIN_WIDTH, 2);
        surface.set_controls([
            "A very long window title that goes on and on and on and on",
            "Big",
            "Vietnamese",
            "Turn Off",
        ]);
        surface.set_hovered(true);
        surface.set_content(Content::Caption(speech(5)));
        surface.settle();
        let body = surface.body_rect();
        for pill in &surface.pills {
            assert!(pill.rect.origin.x >= body.origin.x);
            assert!(pill.rect.origin.x + pill.rect.size.width <= body.origin.x + body.size.width);
        }
        for pair in surface.pills.windows(2) {
            assert!(pair[0].rect.origin.x + pair[0].rect.size.width <= pair[1].rect.origin.x);
        }
    }

    #[test]
    fn the_handle_is_at_the_bottom_right() {
        let mut surface = surface();
        surface.set_content(Content::Caption(speech(5)));
        surface.set_hovered(true);
        surface.settle();
        let body = surface.body_rect();
        let corner = CGPoint::new(body.origin.x + body.size.width - 8.0, body.origin.y + 8.0);
        assert!(surface.handle_at(corner));
        assert!(!surface.handle_at(CGPoint::new(body.origin.x + 8.0, body.origin.y + 8.0)));
    }

    #[test]
    fn sizes_are_kept_in_bounds() {
        let mut surface = surface();
        surface.set_size(10.0, 99);
        assert_eq!(surface.body_size(), (MIN_WIDTH, MAX_LINES));
        surface.set_size(9999.0, 0);
        assert_eq!(surface.body_size(), (MAX_WIDTH, MIN_LINES));
    }

    #[test]
    fn changing_a_word_only_touches_the_last_row() {
        let mut surface = surface();
        surface.set_content(Content::Caption(speech(60)));
        let before = drawn(&surface);
        surface.set_content(Content::Caption(format!("{} again", speech(60))));
        let after = drawn(&surface);
        assert_eq!(before[..before.len() - 1], after[..before.len() - 1]);
    }
}
