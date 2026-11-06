//! `erisautocaptions`: live captions in an overlay, from the menu bar. With no command it's the app.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

mod app;
mod audio;
mod captions;
mod config;
mod engine;
mod models;
mod moonshine;
mod overlay;
mod shape;
mod snapshot;
mod sources;
mod spring;
mod surface;

#[derive(Parser)]
#[command(version, about = "Live captions for anything your Mac is playing or hearing, on-device")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Copy, ValueEnum)]
enum TierArg {
    Small,
    Big,
}

#[derive(Subcommand)]
enum Command {
    /// Caption a WAV file as if it were live, and print the transcript.
    Transcribe {
        file: PathBuf,
        #[arg(long, value_enum, default_value = "big")]
        model: TierArg,
        #[arg(long, default_value = "en")]
        language: String,
        /// Also report how much on-screen text would get rewritten, with and without steadying it.
        #[arg(long, hide = true)]
        stats: bool,
    },
    /// List what can be listened to.
    Sources,
    /// Draw the overlay's states into a PNG, without showing a window.
    Snapshot { path: PathBuf },
}

/// How many words of `before` a move to `after` takes back or changes.
fn rewritten(before: &str, after: &str) -> usize {
    let same = before
        .split_whitespace()
        .zip(after.split_whitespace())
        .take_while(|(a, b)| captions::plain(a) == captions::plain(b))
        .count();
    before.split_whitespace().count() - same
}

fn transcribe(file: &PathBuf, tier: models::Tier, language: &str, stats: bool) -> Result<(), String> {
    let mut reader = hound::WavReader::open(file).map_err(|error| format!("{}: {error}", file.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>(),
        hound::SampleFormat::Int => {
            let scale = 2f32.powi(spec.bits_per_sample as i32 - 1);
            reader.samples::<i32>().map(|sample| sample.map(|sample| sample as f32 / scale)).collect()
        }
    }
    .map_err(|error| error.to_string())?;
    let mono: Vec<f32> = samples.chunks(channels).map(|frame| frame.iter().sum::<f32>() / channels as f32).collect();

    let model = models::Model::choose(tier, language);
    let path = model.fetch(|fraction| eprint!("\rDownloading {:.0}%", fraction * 100.0))?;
    let transcriber = moonshine::Transcriber::load(&path, model.arch)?;
    let stream = transcriber.start_stream()?;
    let mut captions = captions::Captions::new(language);
    let (mut raw, mut steady, mut raw_rewrites, mut steady_rewrites) = (String::new(), String::new(), 0, 0);
    for (i, chunk) in mono.chunks(spec.sample_rate as usize / 10).enumerate() {
        stream.add_audio(chunk, spec.sample_rate as i32)?;
        let lines = stream.lines()?;
        let now = raw_text(&lines);
        raw_rewrites += rewritten(&raw, &now);
        raw = now;
        if let Some(text) = captions.update(&lines, Duration::from_millis(i as u64 * 100)) {
            steady_rewrites += rewritten(&steady, &text);
            steady = text;
        }
    }
    if stats {
        eprintln!("Words rewritten on screen: {raw_rewrites} as guessed, {steady_rewrites} steadied");
    }
    for line in stream.finish()? {
        println!("{}", line.text);
    }
    Ok(())
}

fn raw_text(lines: &[moonshine::Line]) -> String {
    lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join(" ")
}

fn list_sources() {
    println!("Input devices:");
    for device in audio::input_devices() {
        println!("  {} ({})", device.name, device.uid);
    }
    println!("Windows:");
    for window in sources::windows() {
        println!("  {} ({})", window.label(), window.bundle);
    }
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Some(Command::Transcribe { file, model, language, stats }) => {
            let tier = match model {
                TierArg::Small => models::Tier::Small,
                TierArg::Big => models::Tier::Big,
            };
            transcribe(&file, tier, &language, stats)
        }
        Some(Command::Sources) => {
            list_sources();
            Ok(())
        }
        Some(Command::Snapshot { path }) => snapshot::write(&path),
        None => {
            app::run();
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ErisAutoCaptions: {error}");
            ExitCode::FAILURE
        }
    }
}
