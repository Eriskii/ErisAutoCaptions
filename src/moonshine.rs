//! Moonshine's C API, as far as captions need it. The structs follow `moonshine-c-api.h` in the
//! release `build.rs` links.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::ptr;

const HEADER_VERSION: i32 = 30000;

pub const ARCH_TINY_STREAMING: u32 = 2;
pub const ARCH_SMALL_STREAMING: u32 = 4;
pub const ARCH_MEDIUM_STREAMING: u32 = 5;

#[repr(C)]
struct RawOption {
    name: *const c_char,
    value: *const c_char,
}

#[repr(C)]
struct RawLine {
    text: *const c_char,
    audio_data: *const f32,
    audio_data_count: usize,
    start_time: f32,
    duration: f32,
    id: u64,
    is_complete: i8,
    is_updated: i8,
    is_new: i8,
    has_text_changed: i8,
    have_speakers_changed: i8,
    speaker_spans: *const c_void,
    speaker_span_count: u64,
    last_transcription_latency_ms: u32,
    words: *const c_void,
    word_count: u64,
}

#[repr(C)]
struct RawTranscript {
    lines: *const RawLine,
    line_count: u64,
}

unsafe extern "C" {
    fn moonshine_error_to_string(error: i32) -> *const c_char;
    fn moonshine_free_buffer(buffer: *mut c_void);
    fn moonshine_load_transcriber_from_files(
        path: *const c_char,
        arch: u32,
        options: *const RawOption,
        options_count: u64,
        version: i32,
    ) -> i32;
    fn moonshine_free_transcriber(transcriber: i32);
    fn moonshine_create_stream(transcriber: i32, flags: u32) -> i32;
    fn moonshine_free_stream(transcriber: i32, stream: i32) -> i32;
    fn moonshine_start_stream(transcriber: i32, stream: i32) -> i32;
    fn moonshine_stop_stream(transcriber: i32, stream: i32) -> i32;
    fn moonshine_transcribe_add_audio_to_stream(
        transcriber: i32,
        stream: i32,
        audio: *const f32,
        length: u64,
        sample_rate: i32,
        flags: u32,
    ) -> i32;
    fn moonshine_transcribe_stream(
        transcriber: i32,
        stream: i32,
        flags: u32,
        transcript: *mut *const RawTranscript,
    ) -> i32;
    fn moonshine_get_stt_dependencies(
        language: *const c_char,
        options: *const RawOption,
        options_count: u64,
        json: *mut *mut c_char,
    ) -> i32;
}

fn check(code: i32) -> Result<i32, String> {
    if code >= 0 {
        return Ok(code);
    }
    // SAFETY: it returns a static string for any code.
    Err(unsafe { CStr::from_ptr(moonshine_error_to_string(code)) }.to_string_lossy().into_owned())
}

/// The JSON list of files a model is made of.
pub fn manifest(language: &str, arch: u32) -> Result<String, String> {
    let language = CString::new(language).map_err(|error| error.to_string())?;
    let (name, value) = (c"model_arch", CString::new(arch.to_string()).unwrap());
    let option = RawOption { name: name.as_ptr(), value: value.as_ptr() };
    let mut json = ptr::null_mut();
    // SAFETY: the strings outlive the call, and on success it hands us a buffer to free.
    unsafe {
        check(moonshine_get_stt_dependencies(language.as_ptr(), &option, 1, &mut json))?;
        let text = CStr::from_ptr(json).to_string_lossy().into_owned();
        moonshine_free_buffer(json.cast());
        Ok(text)
    }
}

/// One line of the transcript: roughly a sentence, the last of them possibly still being spoken.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: String,
    pub complete: bool,
}

pub struct Transcriber(i32);

// SAFETY: the C API is documented as thread-safe, and handles are plain integers.
unsafe impl Send for Transcriber {}

impl Transcriber {
    pub fn load(path: &Path, arch: u32) -> Result<Self, String> {
        let path = CString::new(path.to_string_lossy().as_bytes()).map_err(|error| error.to_string())?;
        // SAFETY: the path outlives the call, and no options is allowed.
        let handle =
            unsafe { moonshine_load_transcriber_from_files(path.as_ptr(), arch, ptr::null(), 0, HEADER_VERSION) };
        check(handle).map(Self)
    }

    pub fn start_stream(&self) -> Result<Stream<'_>, String> {
        // SAFETY: the handle is ours.
        unsafe {
            let handle = check(moonshine_create_stream(self.0, 0))?;
            check(moonshine_start_stream(self.0, handle))?;
            Ok(Stream { transcriber: self, handle })
        }
    }
}

impl Drop for Transcriber {
    fn drop(&mut self) {
        // SAFETY: the handle is ours, and streams borrow the transcriber so are gone by now.
        unsafe { moonshine_free_transcriber(self.0) };
    }
}

pub struct Stream<'a> {
    transcriber: &'a Transcriber,
    handle: i32,
}

impl Stream<'_> {
    /// Audio is mono floats at any rate; Moonshine resamples it.
    pub fn add_audio(&self, audio: &[f32], sample_rate: i32) -> Result<(), String> {
        // SAFETY: the slice outlives the call, which copies it.
        check(unsafe {
            moonshine_transcribe_add_audio_to_stream(
                self.transcriber.0,
                self.handle,
                audio.as_ptr(),
                audio.len() as u64,
                sample_rate,
                0,
            )
        })
        .map(drop)
    }

    pub fn lines(&self) -> Result<Vec<Line>, String> {
        let mut transcript = ptr::null();
        // SAFETY: the transcript stays valid until the next call on this transcriber, and we copy
        // what we need out of it before then.
        unsafe {
            check(moonshine_transcribe_stream(self.transcriber.0, self.handle, 0, &mut transcript))?;
            let Some(transcript) = transcript.as_ref() else { return Ok(Vec::new()) };
            if transcript.lines.is_null() {
                return Ok(Vec::new());
            }
            let lines = std::slice::from_raw_parts(transcript.lines, transcript.line_count as usize);
            Ok(lines
                .iter()
                .map(|line| Line {
                    text: if line.text.is_null() {
                        String::new()
                    } else {
                        CStr::from_ptr(line.text).to_string_lossy().trim().to_owned()
                    },
                    complete: line.is_complete != 0,
                })
                .collect())
        }
    }

    /// Finishes off the audio already added, marking every line complete.
    pub fn finish(&self) -> Result<Vec<Line>, String> {
        // SAFETY: the handle is ours.
        check(unsafe { moonshine_stop_stream(self.transcriber.0, self.handle) })?;
        self.lines()
    }
}

impl Drop for Stream<'_> {
    fn drop(&mut self) {
        // SAFETY: the handle is ours.
        unsafe { moonshine_free_stream(self.transcriber.0, self.handle) };
    }
}
