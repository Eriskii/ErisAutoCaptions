//! The app: the menu bar item and its menu, and keeping the capture, the engine and the overlay in
//! step with the settings.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::mpsc::Sender;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuDelegate, NSMenuItem, NSRunningApplication,
    NSStatusBar, NSStatusItem, NSVariableStatusItemLength, NSView,
};
use objc2_core_audio::AudioObjectID;
use objc2_foundation::{NSObject, NSObjectProtocol, NSString, NSTimer, ns_string};

use crate::audio::{self, Capture};
use crate::config::{self, Config, Source};
use crate::engine::{self, Command, Event};
use crate::models::{LANGUAGES, Model, Tier};
use crate::overlay::{Overlay, repeating};
use crate::sources;
use crate::surface::{Content, Control};

const LISTENING: &str = "Listening…";

/// What the capture is, or ought to be, listening to right now.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Device(AudioObjectID),
    /// Processes, or everything with none, and the output device that clocks them.
    Tap(Vec<AudioObjectID>, String),
    /// Nothing for now, and why not.
    Waiting(String),
}

#[derive(Clone)]
enum Action {
    Toggle,
    Source(Source),
    Tier(Tier),
    Language(&'static str),
    ResetPosition,
    Quit,
}

pub struct App {
    mtm: MainThreadMarker,
    config: Config,
    pub overlay: Overlay,
    engine: Sender<Command>,
    capture: Option<Capture>,
    target: Option<Target>,
    /// Asked for the microphone, waiting for the answer.
    microphone_asked: bool,
    /// The answer.
    microphone: Option<bool>,
    /// The latest word about loading or listening; a caption goes straight to the overlay.
    status: String,
    /// The last thing that went wrong, for the menu.
    problem: Option<String>,
    item: Retained<NSStatusItem>,
    handler: Retained<MenuHandler>,
    actions: Vec<Action>,
    poll: Option<Retained<NSTimer>>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` with the app, once it's up.
pub fn with(f: impl FnOnce(&mut App)) {
    APP.with(|app| {
        if let Some(app) = app.borrow_mut().as_mut() {
            f(app);
        }
    });
}

/// A control on the overlay was pressed: opens its menu under it, or turns captions off.
pub fn press(control: Control, view: &NSView) {
    let mut menu = None;
    with(|app| {
        if control == Control::TurnOff {
            app.perform(Action::Toggle);
            return;
        }
        let new = NSMenu::new(app.mtm);
        new.setAutoenablesItems(false);
        app.actions.clear();
        match control {
            Control::Source => app.fill_sources(&new),
            Control::Model => app.fill_models(&new),
            Control::Language => app.fill_languages(&new),
            Control::TurnOff => {}
        }
        app.overlay.menu_open = true;
        menu = Some((new, app.overlay.surface.control_rect(control)));
    });
    let Some((menu, at)) = menu else { return };
    // Outside `with`: this runs the event loop until the menu closes, timers and all.
    menu.popUpMenuPositioningItem_atLocation_inView(None, at, Some(view));
    with(|app| app.overlay.menu_open = false);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct MenuHandler;

    unsafe impl NSObjectProtocol for MenuHandler {}

    unsafe impl NSMenuDelegate for MenuHandler {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            with(|app| app.fill_menu(menu));
        }
    }

    impl MenuHandler {
        #[unsafe(method(pick:))]
        fn pick(&self, item: &NSMenuItem) {
            let tag = item.tag() as usize;
            with(|app| {
                if let Some(action) = app.actions.get(tag).cloned() {
                    app.perform(action);
                }
            });
        }
    }
);

impl App {
    fn model(&self) -> Model {
        Model::choose(self.config.model, &self.config.language)
    }

    /// Keeps where the overlay is and how big, after it's been moved or resized.
    pub fn overlay_changed(&mut self) {
        let (width, lines) = self.overlay.surface.body_size();
        self.config.position = Some(self.overlay.origin());
        (self.config.width, self.config.lines) = (width, lines);
        config::save(&self.config);
    }

    /// Tells the overlay's controls what the settings are.
    fn update_controls(&mut self) {
        let source = match &self.config.source {
            Source::System => "System Audio".to_owned(),
            Source::Device { name, .. } | Source::App { name, .. } => clip(name),
        };
        let model = match self.config.model {
            Tier::Small => "Small Model",
            Tier::Big => "Big Model",
        };
        let chosen = self.model().language;
        let language = LANGUAGES.iter().find(|language| language.code == chosen).map_or("", |language| language.name);
        self.overlay.surface.set_controls([
            &format!("{source} ⌄"),
            &format!("{model} ⌄"),
            &format!("{language} ⌄"),
            "Turn Off",
        ]);
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::Toggle => self.config.enabled = !self.config.enabled,
            Action::Source(source) => {
                self.config.source = source;
                self.target = None;
            }
            Action::Tier(tier) => self.config.model = tier,
            Action::Language(language) => self.config.language = language.to_owned(),
            Action::ResetPosition => {
                self.config.position = None;
                self.overlay.place(None, self.mtm);
            }
            Action::Quit => {
                self.capture = None;
                NSApplication::sharedApplication(self.mtm).terminate(None);
                return;
            }
        }
        config::save(&self.config);
        self.apply();
    }

    /// Brings everything into line with the settings.
    fn apply(&mut self) {
        self.update_icon();
        self.update_controls();
        if !self.config.enabled {
            self.capture = None;
            self.target = None;
            let _ = self.engine.send(Command::Stop);
            self.overlay.hide();
            if let Some(poll) = self.poll.take() {
                poll.invalidate();
            }
            return;
        }
        self.problem = None;
        self.overlay.show();
        let _ = self.engine.send(Command::Load(self.model()));
        self.sync_capture();
        if self.poll.is_none() {
            // Apps start and stop making sound, and devices come and go.
            let block = RcBlock::new(|_: NonNull<NSTimer>| with(App::sync_capture));
            self.poll = Some(repeating(2.0, &block));
        }
    }

    fn wanted_target(&self) -> Target {
        let output = audio::output_uid().unwrap_or_default();
        match &self.config.source {
            Source::System => Target::Tap(Vec::new(), output),
            Source::Device { uid, name } => {
                match audio::input_devices().into_iter().find(|device| device.uid == *uid) {
                    Some(device) => Target::Device(device.id),
                    None => Target::Waiting(format!("{name} isn't connected")),
                }
            }
            Source::App { bundle, name } => {
                let pids: Vec<i32> =
                    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle))
                        .iter()
                        .map(|app| app.processIdentifier())
                        .collect();
                let processes = audio::app_processes(bundle, &pids);
                if pids.is_empty() {
                    Target::Waiting(format!("Waiting for {name} to open"))
                } else if processes.is_empty() {
                    Target::Waiting(format!("Waiting for sound from {name}"))
                } else {
                    Target::Tap(processes, output)
                }
            }
        }
    }

    /// Starts capturing anew if what should be heard has changed.
    fn sync_capture(&mut self) {
        let target = self.wanted_target();
        if self.target.as_ref() == Some(&target) {
            return;
        }
        if matches!(target, Target::Device(_)) && !self.microphone_allowed() {
            return;
        }
        self.capture = None;
        let started = match &target {
            Target::Waiting(why) => {
                self.status = why.clone();
                self.overlay.set_content(Content::Status(why.clone()));
                None
            }
            Target::Device(device) => Some(audio::capture_device(*device)),
            Target::Tap(processes, _) => Some(audio::capture_tap(processes)),
        };
        self.target = Some(target);
        match started {
            Some(Ok((capture, consumer, rate))) => {
                self.capture = Some(capture);
                let _ = self.engine.send(Command::Listen(consumer, rate));
            }
            Some(Err(error)) => self.fail(error),
            None => {}
        }
    }

    /// Whether the microphone may be used, asking if that's not known; the answer starts the capture.
    fn microphone_allowed(&mut self) -> bool {
        match self.microphone {
            Some(true) => true,
            Some(false) => {
                let note = "Allow the microphone in System Settings, Privacy & Security".to_owned();
                self.overlay.set_content(Content::Status(note));
                false
            }
            None => {
                if !self.microphone_asked {
                    self.microphone_asked = true;
                    request_microphone();
                }
                false
            }
        }
    }

    fn fail(&mut self, error: String) {
        self.overlay.set_content(Content::Status(error.clone()));
        self.problem = Some(error);
    }

    fn on_event(&mut self, event: Event) {
        if !self.config.enabled {
            return;
        }
        let waiting = matches!(self.target, Some(Target::Waiting(_)));
        match event {
            Event::Downloading(fraction) => {
                self.status = format!("Downloading the model… {:.0}%", fraction * 100.0);
                self.overlay.set_content(Content::Status(self.status.clone()));
            }
            Event::Loading => {
                self.status = "Loading the model…".to_owned();
                self.overlay.set_content(Content::Status(self.status.clone()));
            }
            Event::Ready if waiting => {}
            Event::Ready => {
                self.status = LISTENING.to_owned();
                self.overlay.set_content(Content::Status(self.status.clone()));
            }
            Event::Text(text) if text.is_empty() => {
                if !waiting {
                    self.status = LISTENING.to_owned();
                    self.overlay.set_content(Content::Status(self.status.clone()));
                }
            }
            Event::Text(text) => self.overlay.set_content(Content::Caption(text)),
            Event::Failed(error) => self.fail(error),
        }
    }

    fn update_icon(&self) {
        let Some(button) = self.item.button(self.mtm) else { return };
        let name = if self.config.enabled { ns_string!("captions.bubble.fill") } else { ns_string!("captions.bubble") };
        match NSImage::imageWithSystemSymbolName_accessibilityDescription(name, Some(ns_string!("Captions"))) {
            Some(image) => button.setImage(Some(&image)),
            None => button.setTitle(ns_string!("CC")),
        }
    }

    fn add(&mut self, menu: &NSMenu, title: &str, action: Option<Action>, checked: bool) {
        let item = NSMenuItem::new(self.mtm);
        item.setTitle(&NSString::from_str(title));
        if let Some(action) = action {
            let target: &AnyObject = &self.handler;
            // SAFETY: the handler has `pick:`, and outlives the menu.
            unsafe {
                item.setTarget(Some(target));
                item.setAction(Some(sel!(pick:)));
            }
            item.setTag(self.actions.len() as isize);
            self.actions.push(action);
        } else {
            item.setEnabled(false);
        }
        item.setState(checked.into());
        menu.addItem(&item);
    }

    fn submenu(&self, menu: &NSMenu, title: &str) -> Retained<NSMenu> {
        let item = NSMenuItem::new(self.mtm);
        item.setTitle(&NSString::from_str(title));
        let sub = NSMenu::new(self.mtm);
        sub.setAutoenablesItems(false);
        item.setSubmenu(Some(&sub));
        menu.addItem(&item);
        sub
    }

    /// Fills the menu each time it opens, so the devices and windows in it are up to date.
    fn fill_menu(&mut self, menu: &NSMenu) {
        menu.removeAllItems();
        self.actions.clear();
        let on = self.config.enabled;
        menu.setAutoenablesItems(false);
        if let Some(problem) = self.problem.clone() {
            self.add(menu, &problem, None, false);
            menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        }
        self.add(menu, if on { "Turn Captions Off" } else { "Turn Captions On" }, Some(Action::Toggle), false);
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));

        let sources = self.submenu(menu, "Source");
        self.fill_sources(&sources);
        let models = self.submenu(menu, "Model");
        self.fill_models(&models);
        let languages = self.submenu(menu, "Language");
        self.fill_languages(&languages);

        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.add(menu, "Move Captions Back to the Middle", Some(Action::ResetPosition), false);
        self.add(menu, "Quit ErisAutoCaptions", Some(Action::Quit), false);
    }

    fn fill_sources(&mut self, menu: &NSMenu) {
        let current = self.config.source.clone();
        self.add(menu, "System Audio", Some(Action::Source(Source::System)), current == Source::System);
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.add(menu, "Microphones", None, false);
        let devices = audio::input_devices();
        for device in &devices {
            let source = Source::Device { uid: device.uid.clone(), name: device.name.clone() };
            let checked = current == source;
            self.add(menu, &device.name, Some(Action::Source(source)), checked);
        }
        if let Source::Device { uid, name } = &current
            && !devices.iter().any(|device| device.uid == *uid)
        {
            self.add(menu, &format!("{name} (not connected)"), Some(Action::Source(current.clone())), true);
        }
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        self.add(menu, "Windows", None, false);
        let windows = sources::windows();
        for window in &windows {
            let source = Source::App { bundle: window.bundle.clone(), name: window.app.clone() };
            let checked = matches!(&current, Source::App { bundle, .. } if *bundle == window.bundle);
            self.add(menu, &clip(&window.label()), Some(Action::Source(source)), checked);
        }
        if let Source::App { bundle, name } = &current
            && !windows.iter().any(|window| window.bundle == *bundle)
        {
            self.add(menu, &format!("{name} (no windows)"), Some(Action::Source(current.clone())), true);
        }
    }

    fn fill_models(&mut self, menu: &NSMenu) {
        let tier = self.config.model;

        self.add(menu, "Small (fast, about 45 MB)", Some(Action::Tier(Tier::Small)), tier == Tier::Small);
        self.add(menu, "Big (most accurate, about 270 MB)", Some(Action::Tier(Tier::Big)), tier == Tier::Big);
    }

    fn fill_languages(&mut self, menu: &NSMenu) {
        let chosen = self.model().language;

        for language in &LANGUAGES {
            let action = Action::Language(language.code);
            self.add(menu, language.name, Some(action), chosen == language.code);
        }
    }
}

/// Long window titles would make for a very wide menu.
fn clip(label: &str) -> String {
    const MOST: usize = 60;
    if label.chars().count() <= MOST {
        return label.to_owned();
    }
    let kept: String = label.chars().take(MOST - 1).collect();
    format!("{}…", kept.trim_end())
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}

fn request_microphone() {
    let block = RcBlock::new(|granted: Bool| {
        let granted = granted.as_bool();
        DispatchQueue::main().exec_async(move || {
            with(|app| {
                app.microphone = Some(granted);
                app.target = None;
                app.sync_capture();
            });
        });
    });
    // SAFETY: "soun" is AVMediaTypeAudio, and the system copies the block and calls it once.
    unsafe {
        let class = objc2::class!(AVCaptureDevice);
        let _: () = msg_send![class, requestAccessForMediaType: ns_string!("soun"), completionHandler: &*block];
    }
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("the app runs on the main thread");
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let config = config::load();
    let engine = engine::spawn(|event| {
        DispatchQueue::main().exec_async(move || with(|app| app.on_event(event)));
    });
    let overlay = Overlay::new(mtm, config.position, config.width, config.lines);
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    let handler: Retained<MenuHandler> = unsafe { msg_send![MenuHandler::alloc(mtm), init] };
    let menu = NSMenu::new(mtm);
    menu.setDelegate(Some(ProtocolObject::from_ref(&*handler)));
    item.setMenu(Some(&menu));
    let app = App {
        mtm,
        config,
        overlay,
        engine,
        capture: None,
        target: None,
        microphone_asked: false,
        microphone: None,
        status: String::new(),
        problem: None,
        item,
        handler,
        actions: Vec::new(),
        poll: None,
    };
    APP.with(|slot| *slot.borrow_mut() = Some(app));
    with(App::apply);
    application.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_labels_are_clipped_on_characters() {
        assert_eq!(clip("short"), "short");
        let clipped = clip(&"あ".repeat(100));
        assert_eq!(clipped.chars().count(), 60);
        assert!(clipped.ends_with('…'));
    }
}
