//! Audible rest-timer cues for the terminal client.
//!
//! The [`CuePlayer`] trait is the seam a future Android client reuses: it owns the
//! shared timer engine and the same [`Cue`] vocabulary, and only swaps this audio
//! backend for the platform's sound API. Here the backend is `rodio`, synthesizing
//! short sine "bip"s for the ready warning and 3-2-1 ticks and a longer "beeeep" at
//! "Go".

use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::Duration;

use gymbuddy_timer_core::Cue;

/// Plays the sound for a timer [`Cue`]. `Send` so the player can be held across the
/// event loop's `await` points on a multi-threaded runtime.
pub trait CuePlayer: Send {
    fn play(&self, cue: &Cue);
}

/// A single tone to synthesize.
#[derive(Clone, Copy)]
struct Tone {
    hz: f32,
    millis: u64,
}

impl Tone {
    /// Short, higher "bip" — the ready warning and each countdown tick.
    const BIP: Tone = Tone { hz: 880.0, millis: 140 };
    /// Longer, lower "beeeep" — the "Go" cue.
    const GO: Tone = Tone { hz: 587.0, millis: 650 };
}

fn tone_for(cue: &Cue) -> Tone {
    match cue {
        Cue::Ready { .. } | Cue::Countdown(_) => Tone::BIP,
        Cue::Go => Tone::GO,
    }
}

/// `rodio`-backed player. A dedicated thread owns the (non-`Send`) audio stream and
/// plays tones sequentially; the player just forwards tones to it over a channel.
pub struct RodioPlayer {
    tones: Sender<Tone>,
}

impl RodioPlayer {
    /// Start the playback thread. The (non-`Send`) audio stream is created inside
    /// the thread and never crosses a thread boundary; the thread reports whether
    /// the default device opened so the caller can fall back to silence.
    pub fn new() -> anyhow::Result<Self> {
        let (tones, rx) = channel::<Tone>();
        let (init, init_rx) = channel::<Result<(), String>>();
        thread::spawn(move || {
            let (_stream, handle) = match rodio::OutputStream::try_default() {
                // Keep `_stream` alive for the thread's lifetime; dropping it stops audio.
                Ok(pair) => {
                    let _ = init.send(Ok(()));
                    pair
                }
                Err(e) => {
                    let _ = init.send(Err(e.to_string()));
                    return;
                }
            };
            while let Ok(tone) = rx.recv() {
                if let Ok(sink) = rodio::Sink::try_new(&handle) {
                    use rodio::Source as _;
                    let source = rodio::source::SineWave::new(tone.hz).take_duration(Duration::from_millis(tone.millis)).amplify(0.18);
                    sink.append(source);
                    sink.sleep_until_end();
                }
            }
        });
        match init_rx.recv() {
            Ok(Ok(())) => Ok(Self { tones }),
            Ok(Err(e)) => anyhow::bail!("opening default audio output: {e}"),
            Err(_) => anyhow::bail!("audio thread exited before reporting readiness"),
        }
    }
}

impl CuePlayer for RodioPlayer {
    fn play(&self, cue: &Cue) {
        // A dead playback thread just means no sound; never break the UI for it.
        let _ = self.tones.send(tone_for(cue));
    }
}

/// No-op player used when no audio device is available — cues still appear as
/// transcript lines, they just make no sound.
pub struct SilentPlayer;

impl CuePlayer for SilentPlayer {
    fn play(&self, _cue: &Cue) {}
}

/// The best available player: real audio when a device opens, silence otherwise.
///
/// Failure is intentionally quiet — the TUI owns the alternate screen, so a stderr
/// warning would corrupt it; cues still show as transcript lines either way.
pub fn default_player() -> Box<dyn CuePlayer> {
    match RodioPlayer::new() {
        Ok(player) => Box::new(player),
        Err(_) => Box::new(SilentPlayer),
    }
}
