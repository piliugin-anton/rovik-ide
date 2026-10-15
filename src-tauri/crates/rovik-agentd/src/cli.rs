//! The commands and their arguments. Exit codes: 0 when the command did
//! what it does, 1 when it couldn't (the reason goes to stderr), 2 for bad
//! arguments.

use crate::installer::{self, System};
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
      Print the daemon's version and protocol.

  install --admin USER [--binary FILE] [--root DIR]
      As root: install Rovik for everyone on this server, with USER its
      administrator, and start USER's daemon. FILE is the build to install,
      this one unless told.
  user start|stop|restart|status [--root DIR]
      Through sudo: your own daemon.
  admin [limits [NAME=VALUE ...] | add USER | remove USER | upgrade FILE |
         uninstall] [--root DIR]
      Through sudo, for Rovik's administrators: the limits on each user's
      daemon (none takes one away), the administrators, an upgrade for
      everyone, or Rovik's removal from the server.
  --root DIR puts / at DIR and prints the commands that would change the
  system instead of running them.";

#[derive(Debug, PartialEq)]
pub enum Command {
    Serve(Options),
    Attach {
        socket: Option<PathBuf>,
    },
    Doctor(Options),
    Version,
    Help,
    Install {
        admin: String,
        binary: Option<PathBuf>,
        root: Option<PathBuf>,
    },
    User {
        args: Vec<String>,
        root: Option<PathBuf>,
    },
    Admin {
        args: Vec<String>,
        root: Option<PathBuf>,
    },
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
    if matches!(command.as_str(), "install" | "user" | "admin") {
        return parse_system(command, rest);
    }
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

/// `install`, `user` and `admin`: words and a few options.
fn parse_system(command: &str, rest: &[String]) -> Result<Command, String> {
    let (mut root, mut admin, mut binary) = (None, None, None);
    let mut words = Vec::new();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(value()?)),
            "--admin" if command == "install" => admin = Some(value()?),
            "--binary" if command == "install" => binary = Some(PathBuf::from(value()?)),
            "--help" | "-h" => return Ok(Command::Help),
            flag if flag.starts_with('-') => {
                return Err(format!("unknown argument {flag} for {command}"))
            }
            word => words.push(word.to_string()),
        }
    }
    match command {
        "install" if !words.is_empty() => Err(format!("install takes no {}", words[0])),
        "install" => Ok(Command::Install {
            admin: admin.ok_or("install needs --admin USER")?,
            binary,
            root,
        }),
        "user" => Ok(Command::User { args: words, root }),
        _ => Ok(Command::Admin { args: words, root }),
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
        Command::Install {
            admin,
            binary,
            root,
        } => installer::install(&System::new(root), &admin, binary),
        Command::User { args, root } => installer::user(&System::new(root), &args),
        Command::Admin { args, root } => installer::admin(&System::new(root), &args),
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
        assert!(parsed(&["install"]).unwrap_err().contains("--admin"));
        assert!(parsed(&["install", "--admin", "ann", "now"]).is_err());
        assert!(parsed(&["user", "start", "--as", "bo"]).is_err());
    }

    #[test]
    fn the_system_commands_parse() {
        assert_eq!(
            parsed(&["install", "--admin", "ann", "--root", "/r"]),
            Ok(Command::Install {
                admin: "ann".into(),
                binary: None,
                root: Some("/r".into()),
            })
        );
        assert_eq!(
            parsed(&["user", "start"]),
            Ok(Command::User {
                args: vec!["start".into()],
                root: None,
            })
        );
        assert_eq!(
            parsed(&["admin", "limits", "max_slots=2", "--root", "/r"]),
            Ok(Command::Admin {
                args: vec!["limits".into(), "max_slots=2".into()],
                root: Some("/r".into()),
            })
        );
    }
}
