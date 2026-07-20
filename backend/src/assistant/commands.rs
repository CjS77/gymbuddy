//! The slash-command table — one source of truth for every surface that names a
//! command.
//!
//! The set used to be written out three times (the dispatcher, `/help`, and
//! `/start`) and had already drifted: `/start` omitted `/cancel`. Everything now
//! derives from [`COMMANDS`] — the dispatcher matches the parsed [`Command`],
//! `/help` and `/start` render their lists from it, and
//! [`gymbuddy_proto::ClientRequest::ListCommands`] advertises it to clients.
//!
//! The dispatcher's match is exhaustive over [`Command`], so adding a row here
//! without handling it fails to compile rather than silently doing nothing.

use gymbuddy_proto::CommandInfo;

use crate::db::User;

/// A slash command the assistant understands.
///
/// The dispatcher matches on this rather than on the raw word, which is what
/// keeps the table and the handlers from drifting apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Start,
    Status,
    History,
    Exercises,
    Philosophy,
    NextWorkout,
    Programme,
    Progress,
    Cancel,
    Clear,
    Timers,
    Feedback,
    Help,
}

/// Who may see and run a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visibility {
    Everyone,
    /// Beta testers only, and invisible to everyone else. Advertising such a
    /// command to a non-tester would leak its existence just as loudly as a
    /// "permission denied" would — see `cmd_feedback`, whose handler stays silent
    /// for the same reason.
    BetaOnly,
}

/// One row of the command table.
pub struct CommandSpec {
    pub command: Command,
    /// The command word, leading slash included. This is what a client completes.
    pub name: &'static str,
    /// Argument placeholder for the help line, e.g. `<text>`. Never completed.
    pub args: Option<&'static str>,
    /// One-line description, shared by `/help`, `/start`, and the advertised set.
    pub description: &'static str,
    visibility: Visibility,
}

impl CommandSpec {
    /// The `/help` line for this command: `/feedback <text> -- File a bug report`.
    pub fn help_line(&self) -> String {
        match self.args {
            Some(args) => format!("{} {args} -- {}", self.name, self.description),
            None => format!("{} -- {}", self.name, self.description),
        }
    }
}

/// Every slash command, in the order the help lists them.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        command: Command::Start,
        name: "/start",
        args: None,
        description: "Introduction and registration",
        visibility: Visibility::Everyone,
    },
    CommandSpec {
        command: Command::Status,
        name: "/status",
        args: None,
        description: "Current session and today's stats",
        visibility: Visibility::Everyone,
    },
    CommandSpec {
        command: Command::History,
        name: "/history",
        args: None,
        description: "Last 5 workout summaries",
        visibility: Visibility::Everyone,
    },
    CommandSpec {
        command: Command::Exercises,
        name: "/exercises",
        args: None,
        description: "List available exercises by muscle group",
        visibility: Visibility::Everyone,
    },
    CommandSpec {
        command: Command::Philosophy,
        name: "/philosophy",
        args: None,
        description: "Build or refine your training philosophy (multi-turn)",
        visibility: Visibility::Everyone,
    },
    CommandSpec {
        command: Command::NextWorkout,
        name: "/nextworkout",
        args: Some("[adhoc]"),
        description: "Design a tailored workout for today (logs nothing; 'adhoc' leaves your programme untouched)",
        visibility: Visibility::Everyone,
    },
    CommandSpec {
        command: Command::Programme,
        name: "/programme",
        args: None,
        description: "Build a multi-week programme your sessions are designed against (multi-turn)",
        visibility: Visibility::Everyone,
    },
    CommandSpec {
        command: Command::Progress,
        name: "/progress",
        args: None,
        description: "How you're tracking against each of your goals",
        visibility: Visibility::Everyone,
    },
    CommandSpec {
        command: Command::Cancel,
        name: "/cancel",
        args: None,
        description: "Cancel an in-progress interview (e.g. /philosophy or /programme)",
        visibility: Visibility::Everyone,
    },
    CommandSpec {
        command: Command::Clear,
        name: "/clear",
        args: None,
        description: "Clear conversation context (fresh start)",
        visibility: Visibility::Everyone,
    },
    CommandSpec {
        command: Command::Timers,
        name: "/timers",
        args: None,
        description: "Toggle the rest timer between sets (on by default)",
        visibility: Visibility::Everyone,
    },
    CommandSpec {
        command: Command::Feedback,
        name: "/feedback",
        args: Some("<text>"),
        description: "File a bug report or feature request",
        visibility: Visibility::BetaOnly,
    },
    CommandSpec {
        command: Command::Help,
        name: "/help",
        args: None,
        description: "This message",
        visibility: Visibility::Everyone,
    },
];

impl Command {
    /// Parse the first word of `text` as a command; `None` when it names none.
    ///
    /// Case-insensitive, so `/STATUS` dispatches like `/status`.
    pub fn parse(text: &str) -> Option<Self> {
        let word = text.split_whitespace().next()?.to_lowercase();
        COMMANDS.iter().find(|spec| spec.name == word).map(|spec| spec.command)
    }
}

/// The commands `user` may see and run, in help order.
///
/// The single gate for both running and advertising a command, so the two can't
/// disagree about what a user is allowed to know exists.
pub fn visible_to(user: &User) -> impl Iterator<Item = &'static CommandSpec> + '_ {
    COMMANDS.iter().filter(|spec| match spec.visibility {
        Visibility::Everyone => true,
        Visibility::BetaOnly => user.beta_tester,
    })
}

/// The advertised set for `user`, ready for [`gymbuddy_proto::ServerResponse::Commands`].
pub fn advertised_to(user: &User) -> Vec<CommandInfo> {
    visible_to(user)
        .map(|spec| CommandInfo { name: spec.name.to_string(), description: spec.description.to_string() })
        .collect()
}

/// Every slash-word in `text` that does not name a real command.
///
/// User-facing copy that says "run /programme" is a promise the dispatcher has to
/// keep, and for a while it did not: [R1.7] shipped onboarding copy naming
/// `/programme` several tickets before [C4.2] registered the command, so following
/// the suggestion fell through to the LLM as ordinary chat. Tests over the strings
/// that name commands use this to make that class of gap fail here instead.
#[cfg(test)]
pub(crate) fn unknown_commands_in(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| word.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '"']).to_lowercase())
        .filter(|word| word.starts_with('/') && word.len() > 1)
        .filter(|word| Command::parse(word).is_none())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::User;

    fn user(beta_tester: bool) -> User {
        User {
            id: 1,
            name: "Alice".into(),
            telegram_id: None,
            pubkey: None,
            timezone: "UTC".into(),
            created_at: String::new(),
            updated_at: String::new(),
            beta_tester,
            timers_enabled: true,
        }
    }

    #[test]
    fn parse_reads_the_first_word_only() {
        assert_eq!(Command::parse("/status"), Some(Command::Status));
        assert_eq!(Command::parse("/feedback the timer never stops"), Some(Command::Feedback));
        assert_eq!(Command::parse("/STATUS"), Some(Command::Status));
        assert_eq!(Command::parse("  /status  "), Some(Command::Status));
    }

    /// The guard used by the copy tests has to actually catch a bad reference, or those
    /// tests pass by finding nothing rather than by the commands being real.
    #[test]
    fn unknown_commands_in_spots_a_command_that_does_not_exist() {
        assert_eq!(unknown_commands_in("Run /nosuchthing to begin."), ["/nosuchthing"]);
        // Real commands, trailing punctuation and mid-sentence use are all accepted.
        assert!(unknown_commands_in("Try /philosophy, then /programme. Or /nextworkout!").is_empty());
        assert!(unknown_commands_in("no commands here").is_empty());
    }

    #[test]
    fn parse_rejects_non_commands() {
        assert_eq!(Command::parse("3 sets of bench press"), None);
        assert_eq!(Command::parse("/nosuchcommand"), None);
        assert_eq!(Command::parse(""), None);
        // A prefix of a real command is not that command.
        assert_eq!(Command::parse("/stat"), None);
    }

    /// The whole point of the table: a command a user can't run is never named to
    /// them, so its existence can't be inferred from the advertised set.
    #[test]
    fn feedback_is_advertised_only_to_beta_testers() {
        let names = |beta| advertised_to(&user(beta)).into_iter().map(|c| c.name).collect::<Vec<_>>();
        assert!(!names(false).contains(&"/feedback".to_string()));
        assert!(names(true).contains(&"/feedback".to_string()));
    }

    #[test]
    fn beta_gating_is_the_only_difference_in_the_advertised_set() {
        let plain = advertised_to(&user(false));
        let beta = advertised_to(&user(true));
        assert_eq!(beta.len(), plain.len() + 1);
        assert_eq!(plain, beta.iter().filter(|c| c.name != "/feedback").cloned().collect::<Vec<_>>());
    }

    #[test]
    fn every_advertised_command_parses_back_to_itself() {
        visible_to(&user(true)).for_each(|spec| assert_eq!(Command::parse(spec.name), Some(spec.command)));
    }

    #[test]
    fn advertised_commands_carry_a_description_and_a_slash() {
        advertised_to(&user(true)).iter().for_each(|c| {
            assert!(c.name.starts_with('/'), "{} lacks a leading slash", c.name);
            assert!(!c.description.is_empty(), "{} has no description", c.name);
        });
    }

    /// Two rows sharing a name would make `parse` pick one and quietly strand the
    /// other's handler.
    #[test]
    fn command_names_are_unique() {
        let mut names: Vec<&str> = COMMANDS.iter().map(|spec| spec.name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate command name in the table");
    }

    #[test]
    fn help_line_includes_arguments_when_the_command_takes_them() {
        let spec = COMMANDS.iter().find(|s| s.command == Command::Feedback).unwrap();
        assert_eq!(spec.help_line(), "/feedback <text> -- File a bug report or feature request");
        let spec = COMMANDS.iter().find(|s| s.command == Command::Status).unwrap();
        assert_eq!(spec.help_line(), "/status -- Current session and today's stats");
    }
}
