//! Host-side music bypass for Geometry Dash 2.11.
//!
//! GD's music (menu loop and level tracks) goes through FMOD Ex's streaming
//! path: the game loads a whole MP3 into a guest buffer, hands it to FMOD and
//! frees it, expecting FMOD to keep decoding from memory on a worker thread
//! fed through Mach semaphores. Inside the emulator that pipeline livelocks
//! (the FMOD worker threads spin in a semaphore/usleep handshake and never
//! advance their ring buffer), so the music channel contributes silence while
//! one-shot sound effects — decoded before the buffer is freed — play fine.
//!
//! Rather than teach the emulator FMOD's whole threading contract, this
//! module watches guest `fopen()`s for MP3 tracks and plays them directly
//! with Symphonia-decoded PCM on a dedicated host thread with its own OpenAL
//! device. The guest's FMOD path keeps running (it still paces the game
//! logic); the bypass just fills the silence with the same audio the game
//! asked to play.



use crate::audio::AudioFile;
use crate::environment::Environment;
use crate::fs::GuestPath;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use touchHLE_openal_soft_wrapper::{al_defines as aldef, al_types as altypes};
use touchHLE_openal_soft_wrapper as al;

const AL_LOOPING: altypes::ALenum = 0x1007;
const AL_GAIN: altypes::ALenum = 0x100A;
const AL_SOURCE_RELATIVE: altypes::ALenum = 0x0202;
const AL_TRUE: altypes::ALint = 1;

/// Track extensions the bypass should intercept. GD streams its music as
/// `<bundle>/*.mp3`; one-shot SFX (decoded before their buffers are freed)
/// already work through the guest's own audio stack.
const BYPASS_SUFFIXES: [&str; 1] = [".mp3"];

enum PlayerCommand {
    Play {
        pcm: std::sync::Arc<Vec<u8>>,
        sample_rate: u32,
        channels: u32,
        name: String,
    },
}

static PLAYER_SENDER: Mutex<Option<Sender<PlayerCommand>>> = Mutex::new(None);

/// Last decoded track. GD re-opens the same MP3 (especially the menu loop)
/// many times per session; re-decoding megabytes of PCM on the guest main
/// thread each time would stall emulation, so the most recent track is kept
/// alive here and re-shared through an `Arc`.
static PCM_CACHE: Mutex<Option<(String, std::sync::Arc<Vec<u8>>, u32, u32)>> = Mutex::new(None);

/// Called from `fopen()` for every guest-opened audio file while running GD.
pub fn on_music_file_open(env: &mut Environment, filename: &GuestPath) {
    // Cheap suffix check first, so unrelated callers only pay a string
    // compare.
    let path_str = filename.as_str().to_ascii_lowercase();
    if !BYPASS_SUFFIXES
        .iter()
        .any(|suffix| path_str.ends_with(suffix))
    {
        return;
    }

    let name = filename
        .as_str()
        .rsplit('/')
        .next()
        .unwrap_or(filename.as_str())
        .to_string();

    let cached = {
        let guard = PCM_CACHE.lock().unwrap();
        guard
            .as_ref()
            .filter(|(cached_name, _, _, _)| cached_name == &name)
            .map(|(_, pcm, rate, channels)| {
                (std::sync::Arc::clone(pcm), *rate, *channels)
            })
    };

    let (pcm, sample_rate, channels) = match cached {
        Some(data) => data,
        None => {
            let Some((pcm, sample_rate, channels)) = decode_track(env, filename, &name) else {
                return;
            };
            let pcm = std::sync::Arc::new(pcm);
            *PCM_CACHE.lock().unwrap() =
                Some((name.clone(), std::sync::Arc::clone(&pcm), sample_rate, channels));
            (pcm, sample_rate, channels)
        }
    };

    let sender = ensure_player_thread();
    if let Err(err) = sender.send(PlayerCommand::Play {
        pcm,
        sample_rate,
        channels,
        name,
    }) {
        log!("music bypass: player thread died: {:?}", err);
    }
}

fn decode_track(
    env: &mut Environment,
    filename: &GuestPath,
    name: &str,
) -> Option<(Vec<u8>, u32, u32)> {
    let audio_file = match AudioFile::open_for_reading(filename, &env.fs) {
        Ok(file) => file,
        Err(err) => {
            log!("music bypass: could not decode {}: {:?}", name, err);
            return None;
        }
    };
    match audio_file.into_decoded_pcm() {
        Some((pcm, sample_rate, channels)) => Some((pcm, sample_rate, channels)),
        None => {
            log!("music bypass: unsupported container for {}", name);
            None
        }
    }
}

fn ensure_player_thread() -> Sender<PlayerCommand> {
    let mut guard = PLAYER_SENDER.lock().unwrap();
    if let Some(sender) = guard.as_ref() {
        return sender.clone();
    }
    let (sender, receiver) = channel::<PlayerCommand>();
    std::thread::Builder::new()
        .name("GD music bypass".to_string())
        .spawn(move || {
            player_thread(receiver);
        })
        .expect("music bypass: failed to spawn player thread");
    *guard = Some(sender.clone());
    sender
}

fn player_thread(receiver: Receiver<PlayerCommand>) {
    const AL_BUFFER: altypes::ALenum = 0x1009;
    const AL_LOOPING: altypes::ALenum = 0x1007;
    const AL_SOURCE_RELATIVE: altypes::ALenum = 0x0202;
    const AL_TRUE: altypes::ALint = 1;
    const AL_STOPPED: altypes::ALint = 0x1014;
    const AL_BUFFERS_QUEUED: altypes::ALenum = 0x1015;
    const AL_BUFFERS_PROCESSED: altypes::ALenum = 0x1016;
    const CHUNK_BYTES: usize = 1024 * 1024;

    unsafe {
        let device = al::alcOpenDevice(std::ptr::null());
        if device.is_null() {
            log!("music bypass: alcOpenDevice failed");
            return;
        }
        let context = al::alcCreateContext(device, std::ptr::null());
        if context.is_null() {
            log!("music bypass: alcCreateContext failed");
            al::alcCloseDevice(device);
            return;
        }
        if al::alcMakeContextCurrent(context) == 0 {
            log!("music bypass: alcMakeContextCurrent failed");
            return;
        }

        let mut source: altypes::ALuint = 0;
        let mut buffers: Vec<altypes::ALuint> = Vec::new();
        let mut current_name = String::new();

        loop {
            match receiver.recv_timeout(std::time::Duration::from_millis(25)) {
                Ok(PlayerCommand::Play { pcm, sample_rate, channels, name }) => {
                    if name == current_name && source != 0 {
                        let mut state = 0;
                        al::alGetSourcei(source, aldef::AL_SOURCE_STATE, &mut state);
                        if state != AL_STOPPED {
                            continue;
                        }
                    }
                    if source != 0 {
                        al::alSourceStop(source);
                        // Unqueue everything still attached before deleting:
                        // OpenAL refuses to delete buffers that are still
                        // queued on a source (AL_INVALID_VALUE).
                        let mut detach = 0;
                        al::alGetSourcei(source, AL_BUFFERS_QUEUED, &mut detach);
                        while detach > 0 {
                            let mut done = detach;
                            if done > 64 {
                                done = 64;
                            }
                            let mut scratch = vec![0u32; done as usize];
                            al::alSourceUnqueueBuffers(source, done, scratch.as_mut_ptr());
                            if al::alGetError() != aldef::AL_NO_ERROR {
                                break;
                            }
                            detach -= done;
                        }
                        al::alSourcei(source, AL_BUFFER, 0);
                        al::alDeleteSources(1, &source);
                        if !buffers.is_empty() {
                            al::alDeleteBuffers(buffers.len() as altypes::ALsizei, buffers.as_ptr());
                            buffers.clear();
                        }
                    }
                    // Flush any sticky error left over from earlier calls so
                    // the per-chunk checks below only report fresh failures.
                    al::alGetError();
                    al::alGenSources(1, &mut source);
                    al::alSourcei(source, AL_LOOPING, 0);
                    al::alSourcef(source, AL_GAIN, 1.0);
                    al::alSourcei(source, AL_SOURCE_RELATIVE, AL_TRUE);

                    let format = match channels {
                        1 => aldef::AL_FORMAT_MONO16,
                        2 => aldef::AL_FORMAT_STEREO16,
                        other => {
                            log!("music bypass: {} has {} channels, unsupported", name, other);
                            continue;
                        }
                    };
                    let frame_bytes = channels as usize * 2;
                    let chunk_bytes = (CHUNK_BYTES / frame_bytes) * frame_bytes;
                    let mut offset = 0usize;
                    while offset < pcm.len() {
                        let end = (offset + chunk_bytes).min(pcm.len());
                        let mut buffer = 0;
                        al::alGenBuffers(1, &mut buffer);
                        al::alBufferData(
                            buffer,
                            format,
                            pcm[offset..end].as_ptr().cast(),
                            (end - offset) as altypes::ALsizei,
                            sample_rate as altypes::ALsizei,
                        );
                        let error = al::alGetError();
                        if error != aldef::AL_NO_ERROR {
                            log!(
                                "music bypass: alBufferData {} chunk {} error {:#x}",
                                name,
                                buffers.len(),
                                error
                            );
                            al::alDeleteBuffers(1, &buffer);
                            break;
                        }
                        al::alSourceQueueBuffers(source, 1, &buffer);
                        buffers.push(buffer);
                        offset = end;
                    }
                    if buffers.is_empty() {
                        continue;
                    }
                    al::alSourcePlay(source);
                    current_name = name.clone();
                    log!(
                        "music bypass: playing {} ({} Hz, {}ch, {} KiB PCM, {} chunks)",
                        name,
                        sample_rate,
                        channels,
                        pcm.len() / 1024,
                        buffers.len()
                    );
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if source == 0 || buffers.is_empty() {
                        continue;
                    }
                    let mut state = 0;
                    let mut queued = 0;
                    let mut processed = 0;
                    al::alGetSourcei(source, aldef::AL_SOURCE_STATE, &mut state);
                    if state != AL_STOPPED {
                        continue;
                    }
                    al::alGetSourcei(source, AL_BUFFERS_QUEUED, &mut queued);
                    al::alGetSourcei(source, AL_BUFFERS_PROCESSED, &mut processed);
                    if queued > 0 && processed == queued {
                        let mut drained = vec![0; queued as usize];
                        al::alSourceUnqueueBuffers(source, queued, drained.as_mut_ptr());
                        al::alSourceQueueBuffers(source, buffers.len() as altypes::ALsizei, buffers.as_ptr());
                        al::alSourcePlay(source);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        al::alSourceStop(source);
        al::alDeleteSources(1, &source);
        if !buffers.is_empty() {
            al::alDeleteBuffers(buffers.len() as altypes::ALsizei, buffers.as_ptr());
        }
        al::alcMakeContextCurrent(std::ptr::null_mut());
        al::alcDestroyContext(context);
        al::alcCloseDevice(device);
    }
}
