//! The windows on the Mac, for the Source menu.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGWindowListCopyWindowInfo, CGWindowListOption};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSString};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub bundle: String,
    pub app: String,
    /// Empty without Screen Recording permission, which macOS wants for window titles.
    pub title: String,
}

impl Window {
    pub fn label(&self) -> String {
        if self.title.is_empty() { self.app.clone() } else { format!("{} – {}", self.app, self.title) }
    }
}

/// Windows of ordinary apps, on any space, sorted by app. Those without a title collapse into one
/// entry per app, as an app's untitled windows can't be told apart, and all share its sound anyway.
pub fn windows() -> Vec<Window> {
    let Some(list) = CGWindowListCopyWindowInfo(CGWindowListOption::ExcludeDesktopElements, 0) else {
        return Vec::new();
    };
    // SAFETY: CFArray is toll-free bridged with NSArray, and this one holds dictionaries.
    let list: &NSArray<NSDictionary<NSString, NSObject>> = unsafe { &*CFRetained::as_ptr(&list).as_ptr().cast() };
    let own = std::process::id() as i32;
    let mut windows = Vec::new();
    for info in list.iter() {
        let number =
            |key: &str| info.objectForKey(&NSString::from_str(key)).and_then(|value| value.downcast::<NSNumber>().ok());
        let (Some(layer), Some(pid)) = (number("kCGWindowLayer"), number("kCGWindowOwnerPID")) else { continue };
        let pid = pid.intValue();
        if layer.intValue() != 0 || pid == own || too_small(&info) {
            continue;
        }
        let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else { continue };
        if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
            continue;
        }
        let (Some(bundle), Some(name)) = (app.bundleIdentifier(), app.localizedName()) else { continue };
        let title = info
            .objectForKey(&NSString::from_str("kCGWindowName"))
            .and_then(|value| value.downcast::<NSString>().ok())
            .map(|title| title.to_string())
            .unwrap_or_default();
        windows.push(Window { bundle: bundle.to_string(), app: name.to_string(), title });
    }
    tidy(windows)
}

fn too_small(info: &NSDictionary<NSString, NSObject>) -> bool {
    let Some(bounds) = info
        .objectForKey(&NSString::from_str("kCGWindowBounds"))
        .and_then(|value| value.downcast::<NSDictionary>().ok())
    else {
        return true;
    };
    let side = |key: &str| {
        // SAFETY: only compared as an object; the values of this dictionary are numbers.
        let bounds: &NSDictionary<NSString, NSObject> =
            unsafe { &*(Retained::as_ptr(&bounds).cast::<c_void>().cast()) };
        bounds
            .objectForKey(&NSString::from_str(key))
            .and_then(|value| value.downcast::<NSNumber>().ok())
            .map_or(0.0, |value| value.doubleValue())
    };
    side("Width") < 120.0 || side("Height") < 80.0
}

/// Sorts, drops duplicates, and drops an app's untitled windows if it has titled ones.
fn tidy(mut windows: Vec<Window>) -> Vec<Window> {
    windows
        .sort_by(|a, b| (a.app.to_lowercase(), &a.bundle, &a.title).cmp(&(b.app.to_lowercase(), &b.bundle, &b.title)));
    windows.dedup();
    let titled: Vec<String> =
        windows.iter().filter(|window| !window.title.is_empty()).map(|window| window.bundle.clone()).collect();
    windows.retain(|window| !window.title.is_empty() || !titled.contains(&window.bundle));
    windows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(app: &str, title: &str) -> Window {
        Window { bundle: format!("b.{app}"), app: app.to_owned(), title: title.to_owned() }
    }

    #[test]
    fn untitled_windows_collapse_and_give_way_to_titled_ones() {
        let tidied = tidy(vec![window("zed", ""), window("Arc", "Docs"), window("Arc", ""), window("zed", "")]);
        assert_eq!(tidied, [window("Arc", "Docs"), window("zed", "")]);
    }

    #[test]
    fn labels_show_the_title_when_there_is_one() {
        assert_eq!(window("Arc", "Docs").label(), "Arc – Docs");
        assert_eq!(window("Arc", "").label(), "Arc");
    }
}
