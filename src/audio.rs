//! Capturing audio with Core Audio: an input device directly, or apps and the whole system through a
//! process tap on a private aggregate device. Taps need only the "system audio recording" permission,
//! not Screen Recording.

use std::ffi::{CStr, c_void};
use std::mem::{self, MaybeUninit};
use std::ptr::{self, NonNull};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectPropertyScope, AudioObjectPropertySelector, CATapDescription,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyScopeInput, kAudioDevicePropertyStreamConfiguration,
    kAudioHardwarePropertyDefaultSystemOutputDevice, kAudioHardwarePropertyDevices,
    kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain, kAudioObjectPropertyName,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioProcessPropertyBundleID, kAudioProcessPropertyPID,
    kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSString, NSUUID};
use rtrb::{Consumer, Producer, RingBuffer};

const SECONDS_BUFFERED: f64 = 30.0;

fn address(selector: AudioObjectPropertySelector, scope: AudioObjectPropertyScope) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain }
}

fn property_size(object: AudioObjectID, address: &AudioObjectPropertyAddress) -> Option<u32> {
    let mut size = 0;
    // SAFETY: the pointers are good for the call, and there's no qualifier.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(object, NonNull::from(address), 0, ptr::null(), NonNull::from(&mut size))
    };
    (status == 0).then_some(size)
}

/// A property that's a single plain value.
fn property<T: Copy>(object: AudioObjectID, selector: AudioObjectPropertySelector) -> Option<T> {
    let address = address(selector, kAudioObjectPropertyScopeGlobal);
    let mut value = MaybeUninit::<T>::uninit();
    let mut size = mem::size_of::<T>() as u32;
    // SAFETY: the buffer holds `size` bytes, and T is the plain type the property holds.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(value.as_mut_ptr().cast()),
        )
    };
    (status == 0 && size as usize == mem::size_of::<T>()).then(|| unsafe { value.assume_init() })
}

fn object_list(selector: AudioObjectPropertySelector) -> Vec<AudioObjectID> {
    let address = address(selector, kAudioObjectPropertyScopeGlobal);
    let Some(mut size) = property_size(kAudioObjectSystemObject as AudioObjectID, &address) else {
        return Vec::new();
    };
    let mut list = vec![0 as AudioObjectID; size as usize / mem::size_of::<AudioObjectID>()];
    if list.is_empty() {
        return list;
    }
    // SAFETY: the list holds `size` bytes of object IDs.
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(list.as_mut_ptr().cast()),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    list.truncate(size as usize / mem::size_of::<AudioObjectID>());
    list
}

fn string_property(object: AudioObjectID, selector: AudioObjectPropertySelector) -> Option<String> {
    let string: *const CFString = property(object, selector)?;
    // SAFETY: Core Audio hands over a retained CFString.
    let string = unsafe { CFRetained::from_raw(NonNull::new(string.cast_mut())?) };
    Some(string.to_string())
}

/// How many channels of input a device has.
fn input_channels(device: AudioObjectID) -> u32 {
    let address = address(kAudioDevicePropertyStreamConfiguration, kAudioDevicePropertyScopeInput);
    let Some(mut size) = property_size(device, &address) else { return 0 };
    // u64s, to align the variable-length list its pointers need.
    let mut storage = vec![0u64; (size as usize).div_ceil(8)];
    // SAFETY: the storage holds `size` bytes and is aligned for an AudioBufferList.
    unsafe {
        let status = AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(storage.as_mut_ptr().cast()),
        );
        if status != 0 {
            return 0;
        }
        buffers(storage.as_ptr().cast()).iter().map(|buffer| buffer.mNumberChannels).sum()
    }
}

/// # Safety
/// `list` must point to a whole AudioBufferList, for as long as the slice is used.
unsafe fn buffers<'a>(list: *const AudioBufferList) -> &'a [AudioBuffer] {
    // SAFETY: the buffers run on past the struct's one declared element.
    unsafe { std::slice::from_raw_parts(ptr::addr_of!((*list).mBuffers).cast(), (*list).mNumberBuffers as usize) }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDevice {
    pub id: AudioObjectID,
    pub uid: String,
    pub name: String,
}

/// The devices that can be listened to.
pub fn input_devices() -> Vec<InputDevice> {
    object_list(kAudioHardwarePropertyDevices)
        .into_iter()
        .filter(|&id| input_channels(id) > 0)
        .filter_map(|id| {
            Some(InputDevice {
                id,
                uid: string_property(id, kAudioDevicePropertyDeviceUID)?,
                name: string_property(id, kAudioObjectPropertyName)?,
            })
        })
        .collect()
}

/// Whether a process with these details counts as the app: its own process, a helper named after it
/// (browsers and Electron play sound from those), or one macOS holds it responsible for.
pub fn belongs_to_app(bundle: &str, responsible_pid: i32, process_pid: i32, app: &str, app_pids: &[i32]) -> bool {
    bundle == app
        || bundle.strip_prefix(app).is_some_and(|rest| rest.starts_with('.'))
        || app_pids.contains(&process_pid)
        || app_pids.contains(&responsible_pid)
}

unsafe extern "C" {
    /// In libSystem. Not public API, but stable for years, and the only way to tie WebKit's sound
    /// process to Safari. It answers -1 for anything it can't.
    fn responsibility_get_pid_responsible_for_pid(pid: i32) -> i32;
}

/// The audio objects of the processes that make up an app, sorted so they compare. A process only
/// has one once it has used audio.
pub fn app_processes(app: &str, app_pids: &[i32]) -> Vec<AudioObjectID> {
    let mut processes: Vec<_> = object_list(kAudioHardwarePropertyProcessObjectList)
        .into_iter()
        .filter(|&process| {
            let Some(pid) = property::<i32>(process, kAudioProcessPropertyPID) else { return false };
            let bundle = string_property(process, kAudioProcessPropertyBundleID).unwrap_or_default();
            // SAFETY: it takes any pid.
            let responsible = unsafe { responsibility_get_pid_responsible_for_pid(pid) };
            belongs_to_app(&bundle, responsible, pid, app, app_pids)
        })
        .collect();
    processes.sort_unstable();
    processes
}

/// This process's audio object, left out of whole-system captures.
fn own_process() -> Option<AudioObjectID> {
    let own = std::process::id() as i32;
    object_list(kAudioHardwarePropertyProcessObjectList)
        .into_iter()
        .find(|&process| property::<i32>(process, kAudioProcessPropertyPID) == Some(own))
}

/// The default output device's UID. Taps hang off an aggregate device clocked by it, so it changing
/// means capturing anew.
pub fn output_uid() -> Option<String> {
    let device: AudioObjectID =
        property(kAudioObjectSystemObject as AudioObjectID, kAudioHardwarePropertyDefaultSystemOutputDevice)?;
    string_property(device, kAudioDevicePropertyDeviceUID)
}

/// What the IO thread needs.
struct Feed {
    producer: Producer<f32>,
    /// An aggregate lists the output device's inputs ahead of the tap's, so only the last buffer is
    /// wanted; on an input device, all of them.
    last_buffer_only: bool,
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    feed: *mut c_void,
) -> i32 {
    // SAFETY: the client data is the Feed the Capture owns, only touched here while running, and
    // the buffer list is good for the call.
    let (feed, all) = unsafe { (&mut *feed.cast::<Feed>(), buffers(input.as_ptr())) };
    let wanted = if feed.last_buffer_only { &all[all.len().saturating_sub(1)..] } else { all };
    // SAFETY: each buffer holds floats, as HAL devices' do.
    mix_into(&mut feed.producer, wanted.iter().map(|buffer| unsafe { samples(buffer) }));
    0
}

/// # Safety
/// The buffer must hold what it says it does, as 32-bit floats.
unsafe fn samples<'a>(buffer: &AudioBuffer) -> (&'a [f32], usize) {
    let channels = buffer.mNumberChannels.max(1) as usize;
    if buffer.mData.is_null() {
        return (&[], channels);
    }
    // SAFETY: the caller's word.
    let samples =
        unsafe { std::slice::from_raw_parts(buffer.mData.cast(), buffer.mDataByteSize as usize / size_of::<f32>()) };
    (samples, channels)
}

/// Averages interleaved buffers down to mono and pushes it. Real-time safe: no locks, no allocation,
/// and when the reader falls behind the excess is dropped.
fn mix_into<'a>(producer: &mut Producer<f32>, buffers: impl Iterator<Item = (&'a [f32], usize)> + Clone) {
    let frames = buffers.clone().map(|(samples, channels)| samples.len() / channels).max().unwrap_or(0);
    let total: usize = buffers.clone().map(|(_, channels)| channels).sum();
    if total == 0 {
        return;
    }
    for frame in 0..frames {
        let mut sum = 0.0;
        for (samples, channels) in buffers.clone() {
            for channel in 0..channels {
                sum += samples.get(frame * channels + channel).copied().unwrap_or(0.0);
            }
        }
        if producer.push(sum / total as f32).is_err() {
            return;
        }
    }
}

/// A running capture; it stops when dropped. Its audio comes out of the consumer `start` returned.
pub struct Capture {
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    aggregate: bool,
    tap: Option<AudioObjectID>,
    feed: *mut Feed,
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: these are ours. Stopping is synchronous, so the IO thread is done with the feed.
        unsafe {
            AudioDeviceStop(self.device, self.proc_id);
            AudioDeviceDestroyIOProcID(self.device, self.proc_id);
            if self.aggregate {
                AudioHardwareDestroyAggregateDevice(self.device);
            }
            if let Some(tap) = self.tap {
                AudioHardwareDestroyProcessTap(tap);
            }
            drop(Box::from_raw(self.feed));
        }
    }
}

fn check(what: &str, status: i32) -> Result<(), String> {
    if status == 0 { Ok(()) } else { Err(format!("{what} failed ({status})")) }
}

/// Runs the IO proc on a device, returning the audio and its sample rate.
fn run(
    device: AudioObjectID,
    aggregate: bool,
    tap: Option<AudioObjectID>,
) -> Result<(Capture, Consumer<f32>, f64), String> {
    let rate: f64 = property(device, kAudioDevicePropertyNominalSampleRate).unwrap_or(48000.0);
    let (producer, consumer) = RingBuffer::new((rate * SECONDS_BUFFERED) as usize);
    let feed = Box::into_raw(Box::new(Feed { producer, last_buffer_only: aggregate }));
    let mut proc_id: AudioDeviceIOProcID = None;
    let mut capture = Capture { device, proc_id: None, aggregate, tap, feed };
    // SAFETY: the feed lives until the capture drops, after the proc has stopped.
    unsafe {
        check(
            "Starting audio",
            AudioDeviceCreateIOProcID(device, Some(io_proc), feed.cast(), NonNull::from(&mut proc_id)),
        )?;
        capture.proc_id = proc_id;
        check("Starting audio", AudioDeviceStart(device, proc_id))?;
    }
    Ok((capture, consumer, rate))
}

/// Listens to an input device.
pub fn capture_device(device: AudioObjectID) -> Result<(Capture, Consumer<f32>, f64), String> {
    run(device, false, None)
}

/// Listens to these processes, or with none, to everything but ourselves.
pub fn capture_tap(processes: &[AudioObjectID]) -> Result<(Capture, Consumer<f32>, f64), String> {
    let output = output_uid().ok_or("There's no audio output device to follow")?;
    let numbers = |ids: &[AudioObjectID]| {
        NSArray::from_retained_slice(&ids.iter().map(|&id| NSNumber::new_u32(id)).collect::<Vec<_>>())
    };
    let description = if processes.is_empty() {
        let own: Vec<_> = own_process().into_iter().collect();
        // SAFETY: the array holds audio object IDs, as this takes.
        unsafe { CATapDescription::initStereoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &numbers(&own)) }
    } else {
        // SAFETY: as above.
        unsafe { CATapDescription::initStereoMixdownOfProcesses(CATapDescription::alloc(), &numbers(processes)) }
    };
    // SAFETY: setting a plain property.
    unsafe { description.setPrivate(true) };
    let mut tap: AudioObjectID = 0;
    // SAFETY: the description is good for the call, and the out pointer for an ID.
    check("Making the audio tap", unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut tap) })?;
    // SAFETY: reading a property.
    let tap_uuid = unsafe { description.UUID() }.UUIDString();

    let key = |key: &CStr| NSString::from_str(key.to_str().unwrap_or_default());
    let dictionary = |pairs: Vec<(Retained<NSString>, Retained<NSObject>)>| {
        let (keys, values): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
        let keys: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
        NSDictionary::from_retained_objects(&keys, &values)
    };
    let object = |string: Retained<NSString>| -> Retained<NSObject> { string.into_super() };
    let flag = |value: bool| -> Retained<NSObject> { NSNumber::new_bool(value).into_super().into_super() };
    let list = |entry: Retained<NSDictionary<NSString, NSObject>>| -> Retained<NSObject> {
        NSArray::from_retained_slice(&[entry]).into_super()
    };
    let output = NSString::from_str(&output);
    let description = dictionary(vec![
        (key(kAudioAggregateDeviceNameKey), object(NSString::from_str("ErisAutoCaptions"))),
        (key(kAudioAggregateDeviceUIDKey), object(NSUUID::UUID().UUIDString())),
        (key(kAudioAggregateDeviceMainSubDeviceKey), object(output.clone())),
        (key(kAudioAggregateDeviceIsPrivateKey), flag(true)),
        (key(kAudioAggregateDeviceIsStackedKey), flag(false)),
        (key(kAudioAggregateDeviceTapAutoStartKey), flag(true)),
        (
            key(kAudioAggregateDeviceSubDeviceListKey),
            list(dictionary(vec![(key(kAudioSubDeviceUIDKey), object(output))])),
        ),
        (
            key(kAudioAggregateDeviceTapListKey),
            list(dictionary(vec![
                (key(kAudioSubTapDriftCompensationKey), flag(true)),
                (key(kAudioSubTapUIDKey), object(tap_uuid)),
            ])),
        ),
    ]);
    let mut device: AudioObjectID = 0;
    // SAFETY: NSDictionary is toll-free bridged with CFDictionary, and the out pointer is good.
    let status = unsafe {
        AudioHardwareCreateAggregateDevice(
            &*(&*description as *const NSDictionary<NSString, NSObject>).cast(),
            NonNull::from(&mut device),
        )
    };
    if status != 0 {
        // SAFETY: the tap is ours.
        unsafe { AudioHardwareDestroyProcessTap(tap) };
        return Err(format!("Making the capture device failed ({status})"));
    }
    run(device, true, Some(tap))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pushed(buffers: &[(&[f32], usize)]) -> Vec<f32> {
        let (mut producer, mut consumer) = RingBuffer::new(64);
        mix_into(&mut producer, buffers.iter().copied());
        std::iter::from_fn(|| consumer.pop().ok()).collect()
    }

    #[test]
    fn interleaved_stereo_averages_to_mono() {
        assert_eq!(pushed(&[(&[1.0, 0.0, 0.5, 0.5], 2)]), [0.5, 0.5]);
    }

    #[test]
    fn separate_buffers_average_to_mono() {
        assert_eq!(pushed(&[(&[1.0, 1.0], 1), (&[0.0, 0.5], 1)]), [0.5, 0.75]);
    }

    #[test]
    fn a_full_buffer_drops_the_excess_instead_of_blocking() {
        let (mut producer, mut consumer) = RingBuffer::new(2);
        mix_into(&mut producer, [(&[1.0f32, 2.0, 3.0][..], 1)].into_iter());
        assert_eq!(consumer.slots(), 2);
    }

    #[test]
    fn an_app_owns_its_helpers_and_nothing_else() {
        let chrome = "com.google.Chrome";
        assert!(belongs_to_app(chrome, 9, 9, chrome, &[]));
        assert!(belongs_to_app("com.google.Chrome.helper", 9, 9, chrome, &[]));
        assert!(!belongs_to_app("com.google.Chromebook", 9, 9, chrome, &[]));
        assert!(belongs_to_app("com.apple.WebKit.GPU", 500, 9, "com.apple.Safari", &[500]));
        assert!(belongs_to_app("", 9, 500, "x.y", &[500]));
        assert!(!belongs_to_app("com.apple.Music", 9, 9, "com.apple.Safari", &[500]));
    }
}
