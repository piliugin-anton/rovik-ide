//! The commands and their arguments. Exit codes: 0 when the command did
//! what it does, 1 when it couldn't (the reason goes to stderr), 2 for bad
//! arguments.

use crate::{attach, doctor, serve};
use rovik_protocol::PROTOCOL;
use std::path::PathBuf;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const USAGE: &str = "\
usage: rovik-agentd <command>

  serve [--socket PATH] [--data DIR] [--same-machine] [--no-virtual-files]
      Run the daemon. --data keeps its files, and the agent's work, in DIR
      instead of ~/.rovik and ~/.local/share/rovik. --same-machine runs a
      chat in the folder its start names (development, until projects are
      carried to the server). --no-virtual-files doesn't mount them, and
      CLM chats fall back to trimming.
  attach --stdio [--socket PATH]
      Bridge stdin and stdout to the daemon's socket (an SSH exec channel).
  doctor [--socket PATH] [--data DIR]
      Check this account and server for the daemon.
  version
      Print the daemon's version and protocol.";

#[derive(Debug, PartialEq)]
pub enum Command {
    Serve(Options),
    Attach { socket: Option<PathBuf> },
    Doctor(Options),
    Version,
    Help,
}

#[derive(Debug, Default, PartialEq)]
pub struct Options {
    pub socket: Option<PathBuf>,
    pub data: Option<PathBuf>,
    pub same_machine: bool,
    pub no_virtual_files: bool,
}

pub fn parse(argv: &[String]) -> Result<Command, String> {
    let Some((command, rest)) = argv.split_first() else {
        return Ok(Command::Help);
    };
    let mut options = Options::default();
    let mut stdio = false;
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--socket" => options.socket = Some(value()?),
            "--data" if command != "attach" => options.data = Some(value()?),
            "--same-machine" if command == "serve" => options.same_machine = true,
            "--no-virtual-files" if command == "serve" => options.no_virtual_files = true,
            "--stdio" if command == "attach" => stdio = true,
            "--help" | "-h" => return Ok(Command::Help),
            other => return Err(format!("unknown argument {other} for {command}")),
        }
    }
    match command.as_str() {
        "serve" => Ok(Command::Serve(options)),
        "attach" if stdio => Ok(Command::Attach {
            socket: options.socket,
        }),
        "attach" => Err("attach needs --stdio".into()),
        "doctor" => Ok(Command::Doctor(options)),
        "version" | "--version" => Ok(Command::Version),
        "help" | "--help" | "-h" => Ok(Command::Help),
        other => Err(format!("unknown command {other}")),
    }
}

pub fn run(argv: &[String]) -> i32 {
    let command = match parse(argv) {
        Ok(command) => command,
        Err(e) => {
            eprintln!("rovik-agentd: {e}\n\n{USAGE}");
            return 2;
        }
    };
    let result = match command {
        Command::Serve(options) => serve::serve(options),
        Command::Attach { socket } => attach::attach(socket),
        Command::Doctor(options) => doctor::doctor(options),
        Command::Version => {
            println!("rovik-agentd {VERSION} (protocol {PROTOCOL})");
            Ok(())
        }
        Command::Help => {
            println!("{USAGE}");
            Ok(())
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("rovik-agentd: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Command, String> {
        parse(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn commands_and_their_options_parse() {
        assert_eq!(
            parsed(&["serve", "--socket", "/s", "--data", "/d", "--same-machine"]),
            Ok(Command::Serve(Options {
                socket: Some("/s".into()),
                data: Some("/d".into()),
                same_machine: true,
                no_virtual_files: false,
            }))
        );
        assert_eq!(
            parsed(&["serve", "--no-virtual-files"]),
            Ok(Command::Serve(Options {
                no_virtual_files: true,
                ..Default::default()
            }))
        );
        assert_eq!(parsed(&["serve"]), Ok(Command::Serve(Options::default())));
        assert_eq!(
            parsed(&["attach", "--stdio", "--socket", "/s"]),
            Ok(Command::Attach {
                socket: Some("/s".into())
            })
        );
        assert_eq!(
            parsed(&["doctor", "--data", "/d"]),
            Ok(Command::Doctor(Options {
                data: Some("/d".into()),
                ..Default::default()
            }))
        );
        assert_eq!(parsed(&["version"]), Ok(Command::Version));
        assert_eq!(parsed(&[]), Ok(Command::Help));
        assert_eq!(parsed(&["serve", "--help"]), Ok(Command::Help));
    }

    #[test]
    fn wrong_arguments_say_what_is_wrong() {
        assert!(parsed(&["attach"]).unwrap_err().contains("--stdio"));
        assert!(parsed(&["attach", "--stdio", "--data", "/d"]).is_err());
        assert!(parsed(&["doctor", "--same-machine"]).is_err());
        assert!(parsed(&["serve", "--socket"])
            .unwrap_err()
            .contains("needs a value"));
        assert!(parsed(&["launch"]).unwrap_err().contains("unknown command"));
    }
}
