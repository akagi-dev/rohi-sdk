///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025-2026 Akagi Engineering <admin@akagi.dev>
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
//
///////////////////////////////////////////////////////////////////////////////
//! The words of `rohi SERVICE [ACTION [ARG]...]`, before any device is
//! contacted: where the global options are, and which word is what.
//!
//! Nothing here knows a service, an action or an argument: the names come from
//! the device, and which positional word belongs to which argument is decided
//! by the schema it reports (see `action`).

use std::ffi::OsString;

use clap::Command;

use super::diagnostic::CliError;

/// What `rohi SERVICE ...` asks for.
#[derive(Debug, PartialEq, Eq)]
pub struct Words {
    pub service: String,
    pub action: Option<String>,
    pub args: Vec<String>,
    pub help: bool,
}

/// Splits the words after `rohi` into the service, the action and the
/// positional arguments. `-h` and `--help` ask for help; `--` ends the options,
/// everything after it is an argument, also when it starts with `-`.
pub fn words(tail: &[String]) -> Result<Words, CliError> {
    let Some((service, rest)) = tail.split_first() else {
        return Err(CliError::Usage("no service given".into()));
    };
    let mut words = Words {
        service: service.clone(),
        action: None,
        args: Vec::new(),
        help: false,
    };
    let mut options = true;
    for word in rest {
        if options && word == "--" {
            options = false;
        } else if options && (word == "-h" || word == "--help") {
            words.help = true;
        } else if options && is_option(word) {
            return Err(CliError::Usage(
                "unknown option; put `--` before a value that starts with `-`".into(),
            ));
        } else if words.action.is_none() && options {
            words.action = Some(word.clone());
        } else {
            words.args.push(word.clone());
        }
    }
    if !options && words.action.is_none() {
        return Err(CliError::Usage(format!(
            "`--` must come after the action: rohi {service} ACTION -- ARG..."
        )));
    }
    Ok(words)
}

/// Whether `word` looks like an option. `-` alone (a value read from stdin)
/// and negative numbers (`-5`, `-.5`) are values.
fn is_option(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next() == Some('-')
        && chars
            .next()
            .is_some_and(|second| second != '.' && !second.is_ascii_digit())
}

/// The global options of `command`: `(short, long)` of those that take a value.
fn globals(command: &Command) -> Vec<(Option<char>, Option<String>)> {
    command
        .get_arguments()
        .filter(|arg| arg.is_global_set() && arg.get_action().takes_values())
        .map(|arg| (arg.get_short(), arg.get_long().map(str::to_owned)))
        .collect()
}

/// How many words of an option `word` takes: 0 when it is not a global option
/// of `globals`, 1 when its value is attached (`--output=json`, `-ojson`), 2
/// when the value is the next word.
fn option_words(globals: &[(Option<char>, Option<String>)], word: &str) -> usize {
    if let Some(long) = word.strip_prefix("--") {
        let (name, attached) = match long.split_once('=') {
            Some((name, _)) => (name, true),
            None => (long, false),
        };
        let known = globals
            .iter()
            .any(|(_, long)| long.as_deref() == Some(name));
        return match (known, attached) {
            (false, _) => 0,
            (true, true) => 1,
            (true, false) => 2,
        };
    }
    let mut chars = word.chars();
    if chars.next() != Some('-') {
        return 0;
    }
    let Some(short) = chars.next() else { return 0 };
    if !globals.iter().any(|(known, _)| *known == Some(short)) {
        return 0;
    }
    if chars.next().is_some() { 1 } else { 2 }
}

/// Moves the global options that follow the name of a service in front of it,
/// so that clap sees them where it expects them. A command line that names one
/// of the commands of `command` (or none) is returned unchanged.
pub fn lift_globals(command: &Command, args: Vec<OsString>) -> Vec<OsString> {
    let globals = globals(command);
    let words: Vec<Option<&str>> = args.iter().map(|arg| arg.to_str()).collect();
    // The first word that is not a global option or its value.
    let mut index = 1;
    let service = loop {
        let Some(Some(word)) = words.get(index) else {
            return args;
        };
        match option_words(&globals, word) {
            0 => break index,
            taken => index += taken,
        }
    };
    let word = words[service].unwrap_or_default();
    let is_command = word == "help"
        || word.starts_with('-')
        || command
            .get_subcommands()
            .any(|sub| sub.get_name() == word || sub.get_all_aliases().any(|alias| alias == word));
    if is_command {
        return args;
    }

    let mut lifted = Vec::new();
    let mut kept = Vec::new();
    let mut index = service + 1;
    while index < args.len() {
        let Some(word) = words[index] else {
            kept.extend_from_slice(&args[index..]);
            break;
        };
        if word == "--" {
            kept.extend_from_slice(&args[index..]);
            break;
        }
        let taken = option_words(&globals, word).min(args.len() - index);
        if taken == 0 {
            kept.push(args[index].clone());
            index += 1;
        } else {
            lifted.extend_from_slice(&args[index..index + taken]);
            index += taken;
        }
    }
    let mut result = args[..service].to_vec();
    result.extend(lifted);
    result.push(args[service].clone());
    result.extend(kept);
    result
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;
    use crate::cli::Cli;

    fn words_of(line: &str) -> Result<Words, CliError> {
        let tail: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        words(&tail)
    }

    fn lifted(line: &str) -> Vec<String> {
        let args = line.split_whitespace().map(OsString::from).collect();
        lift_globals(&Cli::command(), args)
            .into_iter()
            .map(|word| word.into_string().unwrap())
            .collect()
    }

    #[test]
    fn the_words_are_service_action_and_arguments() {
        let words = words_of("example configure https://example.org true 5000").unwrap();
        assert_eq!(words.service, "example");
        assert_eq!(words.action.as_deref(), Some("configure"));
        assert_eq!(words.args, ["https://example.org", "true", "5000"]);
        assert!(!words.help);

        let words = words_of("system").unwrap();
        assert_eq!(
            (words.action, words.args.len(), words.help),
            (None, 0, false)
        );
        assert!(words_of("system reboot --help").unwrap().help);
        assert!(words_of("system -h").unwrap().help);
    }

    #[test]
    fn dashes_end_the_options() {
        let words = words_of("example set -- --odd -5 -").unwrap();
        assert_eq!(words.action.as_deref(), Some("set"));
        assert_eq!(words.args, ["--odd", "-5", "-"]);
        // After `--` the help flag is a value.
        let words = words_of("example set -- --help").unwrap();
        assert!(!words.help);
        assert_eq!(words.args, ["--help"]);
        assert!(words_of("example -- set").is_err());
    }

    #[test]
    fn negative_numbers_and_stdin_are_values_other_dashes_are_options() {
        let words = words_of("example set -5 -.5 -").unwrap();
        assert_eq!(words.args, ["-5", "-.5", "-"]);
        let error = words_of("example set --odd").unwrap_err().to_string();
        assert!(
            error.contains("unknown option") && error.contains("--"),
            "{error}"
        );
        assert!(!error.contains("--odd"), "{error}");
        assert!(words_of("example -x").is_err());
    }

    #[test]
    fn global_options_move_in_front_of_the_service() {
        assert_eq!(
            lifted("rohi wifi scan -o json"),
            ["rohi", "-o", "json", "wifi", "scan"]
        );
        assert_eq!(
            lifted("rohi system reboot --device=tcp://h:1 --color never"),
            [
                "rohi",
                "--device=tcp://h:1",
                "--color",
                "never",
                "system",
                "reboot"
            ]
        );
        assert_eq!(
            lifted("rohi -D tcp://h:1 system -ojson reboot"),
            ["rohi", "-D", "tcp://h:1", "-ojson", "system", "reboot"]
        );
        // Words after `--` are values.
        assert_eq!(
            lifted("rohi example set -o json -- -o x"),
            ["rohi", "-o", "json", "example", "set", "--", "-o", "x"]
        );
    }

    #[test]
    fn commands_and_service_help_are_left_alone() {
        for line in [
            "rohi status -o json",
            "rohi config wifi ssid=a -o json",
            "rohi --help",
            "rohi help",
            "rohi -o json",
            "rohi",
        ] {
            let expected: Vec<_> = line.split_whitespace().map(str::to_owned).collect();
            assert_eq!(lifted(line), expected, "{line}");
        }
        // Unknown options after a service stay where they are for the words.
        assert_eq!(
            lifted("rohi wifi scan --bogus"),
            ["rohi", "wifi", "scan", "--bogus"]
        );
    }
}
