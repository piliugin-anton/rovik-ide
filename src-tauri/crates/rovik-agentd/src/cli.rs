//! The commands and their arguments. Exit codes: 0 when the command did
//! what it does, 1 when it couldn't (the reason goes to stderr), 2 for bad
//! arguments.

use crate::config::Config;
use crate::home::Home;
use crate::installer::{self, System};
use crate::{attach, doctor, serve, status};
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
  doctor [--socket PATH] [--data DIR] [--json]
      Check this account and server for the daemon, and say what Rovik's
      folders take and its limits here. --json prints it for the app.
  status [--data DIR] [--json]
      This user's chats on the server: each one's latest Autopilot task,
      its stage, its outcome and what it spent, and the run going now.
      --json prints it for scripts.
  version
      Print the daemon's version and protocol.
  open URL...
      What a command's browser runs on a team server (D20): asks the daemon
      to open each page, one of the command's servers', in the browser on
      the user's computer, and says what came of it.
  token add COMPUTER [--data DIR]
      Let COMPUTER connect: the SHA-256 of its token, in hex, comes on
      stdin and goes in config.toml. A running daemon reads it when it
      next starts.

  install --admin USER [--binary FILE] [--no-browser] [--root DIR]
      As root: install Rovik for everyone on this server, with USER its
      administrator, and start USER's daemon. FILE is the build to install,
      this one unless told. --no-browser leaves the browser tools' browser
      out (a WSL distribution on a developer's computer).
  user start|stop|restart|status [--root DIR]
      Through sudo: your own daemon.
  user toolchain rust|node VERSION [-c COMPONENT] [-t TARGET] [--root DIR]
      Through sudo: the Rust or Node version a project needs, installed
      once for everyone on the server (rust: stable, beta, nightly,
      nightly-YYYY-MM-DD, 1.N or 1.N.P; node: lts, a major version or
      X.Y.Z). An administrator can turn this off in agentd.toml.
  user image REFERENCE [--root DIR]
      Through sudo: an image of a public registry that a project's service
      runs, fetched once into the server's image cache, from which each
      daemon's Podman copies it. Off where the shared caches are.
  admin [limits [NAME=VALUE ...] | add USER | remove USER | browser |
         bubblewrap | pasta | containers | claude-code | toolchain list |
         toolchain add KIND VERSION | toolchain remove KIND VERSION |
         toolchain prune DAYS | toolchains user-install|shared-caches on|off |
         toolchains setup | images list | images prune DAYS |
         upgrade FILE [--allow-downgrade] | uninstall] [--root DIR]
      Through sudo, for Rovik's administrators: the limits on each user's
      daemon (none takes one away), the administrators, the browser for
      the browser tools (the Chrome for Testing Rovik pins, which install
      and upgrade put in place too), bubblewrap, which confines the chats'
      commands (install and upgrade put it in place too, from the distro's
      package, with pasta), pasta, which gives each user's commands a network
      of their own (install and upgrade require it, from the passt package),
      the container engine that runs each user's project services (Podman,
      the Docker Compose Rovik pins and the users' subordinate ids, which
      install and upgrade put in place too), Claude Code for the connector
      of that name (Anthropic's
      package, installed once for every user's chats, or updated), the
      shared Rust and Node versions and the package caches
      the daemons share (toolchain add works whether or not users may
      install them; prune takes the versions no chat used for DAYS), the
      image cache (prune takes the images no daemon asked for in DAYS),
      an upgrade for everyone (a release's
      build, signed in FILE.sig, or a build of one's own where agentd.toml
      allows unsigned ones; an older build only with --allow-downgrade), or
      Rovik's removal from the server.
  --root DIR puts / at DIR and prints the commands that would change the
  system instead of running them.";

#[derive(Debug, PartialEq)]
pub enum Command {
    Serve(Options),
    Attach {
        socket: Option<PathBuf>,
    },
    Doctor(Options),
    Status(Options),
    /// `open URL...`, as a command's browser runs it.
    Open {
        urls: Vec<String>,
    },
    Version,
    Help,
    AddToken {
        computer: String,
        data: Option<PathBuf>,
    },
    Install {
        admin: String,
        binary: Option<PathBuf>,
        root: Option<PathBuf>,
        /// The browser tools' browser is installed too (`--no-browser`
        /// leaves it out).
        browser: bool,
    },
    User {
        args: Vec<String>,
        root: Option<PathBuf>,
    },
    Admin {
        args: Vec<String>,
        root: Option<PathBuf>,
        /// `upgrade` may install an older build.
        allow_downgrade: bool,
    },
}

#[derive(Debug, Default, PartialEq)]
pub struct Options {
    pub socket: Option<PathBuf>,
    pub data: Option<PathBuf>,
    pub same_machine: bool,
    pub no_virtual_files: bool,
    /// `doctor`'s report as JSON.
    pub json: bool,
}

pub fn parse(argv: &[String]) -> Result<Command, String> {
    let Some((command, rest)) = argv.split_first() else {
        return Ok(Command::Help);
    };
    if matches!(command.as_str(), "install" | "user" | "admin") {
        return parse_system(command, rest);
    }
    if command == "token" {
        return parse_token(rest);
    }
    if command == "open" {
        return match rest {
            [] => Err("open takes the address of the page to open".into()),
            urls => Ok(Command::Open {
                urls: urls.to_vec(),
            }),
        };
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
            "--json" if matches!(command.as_str(), "doctor" | "status") => options.json = true,
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
        "status" => Ok(Command::Status(options)),
        "version" | "--version" => Ok(Command::Version),
        "help" | "--help" | "-h" => Ok(Command::Help),
        other => Err(format!("unknown command {other}")),
    }
}

/// `token add COMPUTER [--data DIR]`.
fn parse_token(rest: &[String]) -> Result<Command, String> {
    let mut data = None;
    let mut words = Vec::new();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--data" => {
                let dir = it.next().ok_or("--data needs a value")?;
                data = Some(PathBuf::from(dir));
            }
            "--help" | "-h" => return Ok(Command::Help),
            flag if flag.starts_with('-') => {
                return Err(format!("unknown argument {flag} for token"))
            }
            word => words.push(word.to_string()),
        }
    }
    match words.as_slice() {
        [add, computer] if add == "add" => Ok(Command::AddToken {
            computer: computer.clone(),
            data,
        }),
        _ => Err("token takes: add COMPUTER".into()),
    }
}

/// `install`, `user` and `admin`: words and a few options.
fn parse_system(command: &str, rest: &[String]) -> Result<Command, String> {
    let (mut root, mut admin, mut binary) = (None, None, None);
    let mut allow_downgrade = false;
    let mut browser = true;
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
            "--no-browser" if command == "install" => browser = false,
            "--allow-downgrade" if command == "admin" => allow_downgrade = true,
            // `user toolchain rust 1.82 -c rustfmt`: rustup's flags, with
            // the words the installer checks them among.
            "-c" | "-t" if matches!(command, "user" | "admin") => {
                let name = value()?;
                words.push(arg.clone());
                words.push(name);
            }
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
            browser,
        }),
        "user" => Ok(Command::User { args: words, root }),
        _ => Ok(Command::Admin {
            args: words,
            root,
            allow_downgrade,
        }),
    }
}

/// The system a command changes: `/`, or, in development, a folder in its
/// place (`--root DIR`). Never for the installed binary: the sudoers rule
/// runs that one as root for everyone (`user`) and the administrators
/// (`admin`), and a folder of the caller's (a link in it to
/// `/opt/rovik/bin`) would have root write where the caller says, past the
/// release's signature. A development build takes the folder whoever runs
/// it: the tests under `tests/install.rs` do, as root on some runners.
fn system(root: Option<PathBuf>) -> Result<System, String> {
    if root.is_some() && installer::runs_installed() {
        return Err(format!(
            "--root is for development builds: {} changes the system itself.",
            installer::INSTALLED
        ));
    }
    Ok(System::new(root))
}

/// `open URL...`: each page asked of the daemon, as a command's browser on
/// a team server asks (D20). What was done goes to stdout; why a page
/// wasn't opened, to stderr, and the command fails.
fn open(urls: &[String]) -> Result<(), String> {
    use rovik_core::overlay::confine::pages;
    let socket = std::env::var_os(pages::SOCKET_VAR).ok_or(
        "Rovik's browser opens only the pages of its chats' commands on a team server, and this \
         program isn't one of them: it opened nothing.",
    )?;
    let chat = std::env::var(pages::CHAT_VAR).unwrap_or_default();
    let refused: Vec<String> = urls
        .iter()
        .filter_map(
            |url| match pages::ask(std::path::Path::new(&socket), &chat, url) {
                Ok(done) => {
                    println!("{done}");
                    None
                }
                Err(why) => Some(why),
            },
        )
        .collect();
    match refused.is_empty() {
        true => Ok(()),
        false => Err(refused.join("\n")),
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
        Command::Status(options) => status::status(options),
        Command::Open { urls } => open(&urls),
        Command::Version => {
            println!("rovik-agentd {VERSION} (protocol {PROTOCOL})");
            Ok(())
        }
        Command::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Command::AddToken { computer, data } => add_token(&computer, data),
        Command::Install {
            admin,
            binary,
            root,
            browser,
        } => system(root).and_then(|sys| installer::install(&sys, &admin, binary, browser)),
        Command::User { args, root } => system(root).and_then(|sys| installer::user(&sys, &args)),
        Command::Admin {
            args,
            root,
            allow_downgrade,
        } => system(root).and_then(|sys| installer::admin(&sys, &args, allow_downgrade)),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("rovik-agentd: {e}");
            1
        }
    }
}

/// `token add`: the hash from stdin into config.toml, the folders made
/// as the daemon makes them.
fn add_token(computer: &str, data: Option<PathBuf>) -> Result<(), String> {
    let home = Home::locate(data)?;
    home.prepare()?;
    let mut hash = String::new();
    std::io::stdin()
        .read_line(&mut hash)
        .map_err(|e| e.to_string())?;
    Config::add_token(&home.config_file(), computer, hash.trim())?;
    println!("{computer} can connect once the daemon next starts.");
    Ok(())
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
                json: false,
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
        assert_eq!(
            parsed(&["doctor", "--json"]),
            Ok(Command::Doctor(Options {
                json: true,
                ..Default::default()
            }))
        );
        assert_eq!(parsed(&["version"]), Ok(Command::Version));
        // The browser's arguments as they come, whatever they look like.
        assert_eq!(
            parsed(&["open", "http://localhost:5173/", "--new-window"]),
            Ok(Command::Open {
                urls: vec!["http://localhost:5173/".into(), "--new-window".into()]
            })
        );
        assert_eq!(parsed(&[]), Ok(Command::Help));
        assert_eq!(parsed(&["serve", "--help"]), Ok(Command::Help));
    }

    #[test]
    fn wrong_arguments_say_what_is_wrong() {
        assert!(parsed(&["attach"]).unwrap_err().contains("--stdio"));
        assert!(parsed(&["attach", "--stdio", "--data", "/d"]).is_err());
        assert!(parsed(&["doctor", "--same-machine"]).is_err());
        assert!(parsed(&["serve", "--json"]).is_err());
        assert!(parsed(&["serve", "--socket"])
            .unwrap_err()
            .contains("needs a value"));
        assert!(parsed(&["launch"]).unwrap_err().contains("unknown command"));
        assert!(parsed(&["install"]).unwrap_err().contains("--admin"));
        assert!(parsed(&["open"]).unwrap_err().contains("address"));
        assert!(parsed(&["install", "--admin", "ann", "now"]).is_err());
        assert!(parsed(&["user", "start", "--as", "bo"]).is_err());
        assert!(parsed(&["token", "add"]).is_err());
        assert!(parsed(&["token", "remove", "laptop"]).is_err());
    }

    #[test]
    fn the_system_commands_parse() {
        assert_eq!(
            parsed(&["install", "--admin", "ann", "--root", "/r"]),
            Ok(Command::Install {
                admin: "ann".into(),
                binary: None,
                root: Some("/r".into()),
                browser: true,
            })
        );
        assert_eq!(
            parsed(&["install", "--admin", "ann", "--no-browser"]),
            Ok(Command::Install {
                admin: "ann".into(),
                binary: None,
                root: None,
                browser: false,
            })
        );
        assert_eq!(
            parsed(&["token", "add", "laptop", "--data", "/d"]),
            Ok(Command::AddToken {
                computer: "laptop".into(),
                data: Some("/d".into()),
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
                allow_downgrade: false,
            })
        );
        assert_eq!(
            parsed(&["admin", "upgrade", "/tmp/b", "--allow-downgrade"]),
            Ok(Command::Admin {
                args: vec!["upgrade".into(), "/tmp/b".into()],
                root: None,
                allow_downgrade: true,
            })
        );
        assert!(parsed(&["user", "start", "--allow-downgrade"]).is_err());
    }

    /// `--root` is a development build's, whoever runs it: the install
    /// tests run this crate's binary with it as root on some runners.
    #[test]
    fn a_development_build_takes_root_whoever_runs_it() {
        let dir = rovik_core::testing::temp_dir();
        assert!(system(Some(dir.0.clone())).is_ok());
        assert!(system(None).is_ok());
    }
}
