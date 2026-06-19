//! UI-agnostic rest-timer engine, shared by every GymBuddy client.
//!
//! The engine knows nothing about Telegram, the TUI, or Android: it takes a rest
//! duration and the name of the exercise the user is resting between, then emits a
//! sequence of timed [`Cue`]s onto a channel. Each frontend drains the channel and
//! renders the cues however it likes — Telegram sends chat messages, the TUI plays
//! tones and prints lines, Android does the same as the TUI with its own sound
//! backend.
//!
//! Running the countdown is the consumer's job: spawn [`run_timer`] as a task and
//! abort that task to cancel (re-arming = abort the old task, spawn a new one). The
//! engine carries no cancellation state of its own.

use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

/// A single timed event in a rest countdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cue {
    /// The 10-seconds-to-go warning: "get ready for your next set of {exercise}".
    Ready { exercise: String },
    /// One of the final ticks: `3`, then `2`, then `1`.
    Countdown(u8),
    /// Rest is over — go.
    Go,
}

/// Run a rest countdown of `duration_secs`, sending each [`Cue`] on `cues` at its
/// scheduled moment. Returns when the final [`Cue::Go`] has been sent (or earlier if
/// the receiver was dropped). Abort the spawned task to cancel.
pub async fn run_timer(duration_secs: u32, exercise: String, cues: UnboundedSender<Cue>) {
    let mut elapsed = 0u32;
    for (at, cue) in schedule(duration_secs, &exercise) {
        let wait = at.saturating_sub(elapsed);
        if wait > 0 {
            tokio::time::sleep(Duration::from_secs(u64::from(wait))).await;
            elapsed = at;
        }
        if cues.send(cue).is_err() {
            return; // consumer gone — stop counting down.
        }
    }
}

/// Build the ordered `(offset_secs_from_start, cue)` schedule for a countdown.
///
/// Cues land at fixed offsets from the *end* of the rest: the ready warning at
/// `T-10s` and the ticks at `T-3 / T-2 / T-1`, with `Go` at `T-0`. Cues whose offset
/// would be negative are dropped, so short rests degrade gracefully (a 5s rest skips
/// the warning, a 2s rest skips the warning and the "3" tick, and so on).
fn schedule(duration_secs: u32, exercise: &str) -> Vec<(u32, Cue)> {
    let candidates = [
        (10u32, Cue::Ready { exercise: exercise.to_string() }),
        (3, Cue::Countdown(3)),
        (2, Cue::Countdown(2)),
        (1, Cue::Countdown(1)),
        (0, Cue::Go),
    ];
    let mut points: Vec<(u32, Cue)> = candidates
        .into_iter()
        .filter(|(before_end, _)| duration_secs >= *before_end)
        .map(|(before_end, cue)| (duration_secs - before_end, cue))
        .collect();
    points.sort_by_key(|(at, _)| *at);
    points
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn cues_of(duration: u32) -> Vec<Cue> {
        schedule(duration, "Bench Press").into_iter().map(|(_, c)| c).collect()
    }

    #[test]
    fn long_rest_has_full_sequence() {
        assert_eq!(
            schedule(180, "Bench Press"),
            vec![
                (170, Cue::Ready { exercise: "Bench Press".into() }),
                (177, Cue::Countdown(3)),
                (178, Cue::Countdown(2)),
                (179, Cue::Countdown(1)),
                (180, Cue::Go),
            ]
        );
    }

    #[test]
    fn exactly_ten_seconds_emits_ready_at_start() {
        assert_eq!(schedule(10, "Bench Press")[0], (0, Cue::Ready { exercise: "Bench Press".into() }));
    }

    #[test]
    fn short_rests_drop_unreachable_cues() {
        assert_eq!(cues_of(8), vec![Cue::Countdown(3), Cue::Countdown(2), Cue::Countdown(1), Cue::Go]);
        assert_eq!(cues_of(2), vec![Cue::Countdown(2), Cue::Countdown(1), Cue::Go]);
        assert_eq!(cues_of(1), vec![Cue::Countdown(1), Cue::Go]);
        assert_eq!(cues_of(0), vec![Cue::Go]);
    }

    #[tokio::test(start_paused = true)]
    async fn run_timer_emits_every_cue_in_order() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::spawn(run_timer(15, "Squat".into(), tx));
        // Paused-clock auto-advance resolves each sleep as soon as the task idles,
        // so draining the channel walks the whole schedule without real waiting.
        let mut got = Vec::new();
        while let Some(cue) = rx.recv().await {
            got.push(cue);
        }
        assert_eq!(
            got,
            vec![Cue::Ready { exercise: "Squat".into() }, Cue::Countdown(3), Cue::Countdown(2), Cue::Countdown(1), Cue::Go]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_receiver_stops_the_timer() {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(run_timer(180, "Deadlift".into(), tx));
        drop(rx);
        // With no receiver, the first send fails and the task returns promptly.
        handle.await.expect("timer task should finish cleanly");
    }
}
