//! The thread that runs the model: fetches and loads it, takes audio from the capture, and reports
//! what to show.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use rtrb::Consumer;

use crate::captions::Captions;
use crate::models::Model;
use crate::moonshine::{Line, Stream, Transcriber};

/// How often new audio goes to the model. It decodes at most every 200ms of new audio by itself.
const PACE: Duration = Duration::from_millis(100);

pub enum Command {
    /// Fetches and loads a model, unless it's the one loaded.
    Load(Model),
    /// Takes audio from here, in place of wherever it came from before, at this rate.
    Listen(Consumer<f32>, f64),
    /// Stops listening, and frees the model.
    Stop,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Downloading(f64),
    Loading,
    Ready,
    Text(String),
    Failed(String),
}

pub fn spawn(on_event: impl Fn(Event) + Send + 'static) -> Sender<Command> {
    let (sender, receiver) = channel();
    thread::Builder::new()
        .name("captions engine".to_owned())
        .spawn(move || run(&receiver, &on_event))
        .expect("couldn't start the engine thread");
    sender
}

struct Loaded {
    model: Model,
    transcriber: Transcriber,
}

fn run(commands: &Receiver<Command>, on_event: &dyn Fn(Event)) {
    let mut loaded: Option<Loaded> = None;
    let mut input: Option<(Consumer<f32>, f64)> = None;
    loop {
        // With a model and audio it listens, until a command calls for a fresh start.
        let command = match &loaded {
            Some(current) if input.is_some() => {
                match listen(&current.transcriber, current.model, &mut input, commands, on_event) {
                    Some(command) => command,
                    None => return,
                }
            }
            _ => match commands.recv() {
                Ok(command) => command,
                Err(_) => return,
            },
        };
        match command {
            Command::Load(model) => load(model, &mut loaded, on_event),
            Command::Listen(consumer, rate) => input = Some((consumer, rate)),
            Command::Stop => {
                input = None;
                loaded = None;
            }
        }
    }
}

fn load(model: Model, loaded: &mut Option<Loaded>, on_event: &dyn Fn(Event)) {
    if loaded.as_ref().is_some_and(|current| current.model == model) {
        on_event(Event::Ready);
        return;
    }
    *loaded = None;
    let result = model.fetch(|fraction| on_event(Event::Downloading(fraction))).and_then(|path| {
        on_event(Event::Loading);
        Transcriber::load(&path, model.arch)
    });
    match result {
        Ok(transcriber) => {
            *loaded = Some(Loaded { model, transcriber });
            on_event(Event::Ready);
        }
        Err(error) => on_event(Event::Failed(error)),
    }
}

/// Listens until the next command, which it returns, or until the app is going away.
fn listen(
    transcriber: &Transcriber,
    model: Model,
    input: &mut Option<(Consumer<f32>, f64)>,
    commands: &Receiver<Command>,
    on_event: &dyn Fn(Event),
) -> Option<Command> {
    let Some((consumer, rate)) = input else { unreachable!("listening with no input") };
    let started = Instant::now();
    let mut captions = Captions::new(model.language);
    let mut stream = start(transcriber, on_event);
    let mut audio = Vec::new();
    on_event(Event::Text(String::new()));
    loop {
        match commands.recv_timeout(PACE) {
            Ok(command) => return Some(command),
            Err(RecvTimeoutError::Disconnected) => return None,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let Some(current) = &stream else {
            stream = start(transcriber, on_event);
            continue;
        };
        audio.clear();
        if let Ok(chunk) = consumer.read_chunk(consumer.slots()) {
            let (first, second) = chunk.as_slices();
            audio.extend_from_slice(first);
            audio.extend_from_slice(second);
            chunk.commit_all();
        }
        let lines = if audio.is_empty() {
            current.lines()
        } else {
            current.add_audio(&audio, *rate as i32).and_then(|()| current.lines())
        };
        let lines: Vec<Line> = match lines {
            Ok(lines) => lines,
            Err(error) => {
                on_event(Event::Failed(error));
                continue;
            }
        };
        if let Some(text) = captions.update(&lines, started.elapsed()) {
            on_event(Event::Text(text));
        }
        if captions.wants_new_stream(&lines) {
            captions.stream_replaced(&lines);
            drop(stream.take());
            stream = start(transcriber, on_event);
        }
    }
}

fn start<'a>(transcriber: &'a Transcriber, on_event: &dyn Fn(Event)) -> Option<Stream<'a>> {
    match transcriber.start_stream() {
        Ok(stream) => Some(stream),
        Err(error) => {
            on_event(Event::Failed(error));
            None
        }
    }
}
