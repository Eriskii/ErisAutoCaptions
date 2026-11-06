//! Draws the overlay's states into a PNG, without showing a window, for checking how it looks.

use std::path::Path;

use objc2::AnyThread;
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColor, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    kCGColorSpaceSRGB,
};
use objc2_foundation::{NSDictionary, NSString};

use crate::surface::{Content, Surface};

const SCALE: f64 = 2.0;
const LONG: &str = "But I may say that contributors to the growth to some extent are also related to the fact that we \
                    are able to keep production stable in terms of the cost wherever it's possible, and I will \
                    address the cost at greater length later.";

pub fn write(path: &Path) -> Result<(), String> {
    let mut surface = Surface::new(SCALE, 680.0, 2);
    surface.set_shown(true);
    surface.set_controls(["System Audio ⌄", "Big Model ⌄", "English ⌄", "Turn Off"]);
    let states = [
        Content::Status("Listening…".to_owned()),
        Content::Status("Downloading the model… 43%".to_owned()),
        Content::Caption("So these are two factors".to_owned()),
        Content::Caption(
            "So these are two factors that I think should be included in the design, and then we can talk about \
             the appearance."
                .to_owned(),
        ),
        Content::Caption(LONG.to_owned()),
    ];
    let mut frames = Vec::new();
    for state in states {
        surface.set_content(state);
        surface.settle();
        surface.render();
        frames.push(capture(&surface).ok_or("couldn't draw a frame")?);
    }
    // With the pointer over it, listening and captioning, and resized bigger.
    surface.set_hovered(true);
    surface.set_content(Content::Status("Listening…".to_owned()));
    surface.settle();
    surface.render();
    frames.push(capture(&surface).ok_or("couldn't draw a frame")?);
    surface.set_content(Content::Caption(LONG.to_owned()));
    surface.settle();
    surface.render();
    frames.push(capture(&surface).ok_or("couldn't draw a frame")?);
    surface.set_size(860.0, 4);
    surface.settle();
    surface.render();
    frames.push(capture(&surface).ok_or("couldn't draw a frame")?);
    let sheet = stack(&frames).ok_or("couldn't draw the sheet")?;
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &sheet);
    // SAFETY: an empty dictionary is valid properties.
    let png = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) };
    match png {
        Some(png) if png.writeToFile_atomically(&NSString::from_str(&path.to_string_lossy()), true) => Ok(()),
        _ => Err(format!("couldn't write {}", path.display())),
    }
}

fn bitmap(width: f64, height: f64) -> Option<CFRetained<CGContext>> {
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    // SAFETY: a null buffer asks it to allocate one.
    unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            (width * SCALE) as usize,
            (height * SCALE) as usize,
            8,
            0,
            Some(&space),
            CGImageAlphaInfo::PremultipliedLast.0,
        )
    }
}

/// One frame, over a mid grey so the black body shows.
fn capture(surface: &Surface) -> Option<CFRetained<CGImage>> {
    let size = surface.size;
    let context = bitmap(size.width, size.height)?;
    CGContext::scale_ctm(Some(&context), SCALE, SCALE);
    CGContext::set_fill_color_with_color(Some(&context), Some(&CGColor::new_generic_gray(0.62, 1.0)));
    CGContext::fill_rect(Some(&context), CGRect::new(CGPoint::ZERO, size));
    surface.root.renderInContext(&context);
    CGBitmapContextCreateImage(Some(&context))
}

fn stack(frames: &[CFRetained<CGImage>]) -> Option<CFRetained<CGImage>> {
    let size = |frame: &CFRetained<CGImage>| (CGImage::width(Some(frame)) as f64, CGImage::height(Some(frame)) as f64);
    let width = frames.iter().map(|frame| size(frame).0).fold(0.0, f64::max);
    let total: f64 = frames.iter().map(|frame| size(frame).1).sum();
    let context = bitmap(width / SCALE, total / SCALE)?;
    let mut y = total;
    for frame in frames {
        let (frame_width, height) = size(frame);
        y -= height;
        CGContext::draw_image(
            Some(&context),
            CGRect::new(CGPoint::new(0.0, y), CGSize::new(frame_width, height)),
            Some(frame),
        );
    }
    CGBitmapContextCreateImage(Some(&context))
}
