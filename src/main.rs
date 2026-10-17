//! rtop: a small, fast system monitor for the terminal.
//!
//! Three threads talk over channels:
//!
//! ```text
//!  sampler thread ──Metrics, KillDone──┐
//!                                      ├──► mpsc channel ──► UI thread (owns App, draws)
//!  input thread ────Key, Paste, Resize─┘                         │
//!  sampler thread ◄──────────── KillRequest ─────────────────────┘
//! ```

mod app;
mod config;
mod format;
mod help;
mod layout;
mod metrics;
mod net;
mod order;
mod panel;
mod process;
mod sensors;
mod storage;
mod ui;
mod zfs;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result, bail};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent,
};
use ratatui::crossterm::execute;

use app::{App, Event, MAX_INTERVAL_MS, MIN_INTERVAL_MS};
use config::ConfigSource;

const DEFAULT_INTERVAL_MS: u64 = 1_000;

const USAGE_HEAD: &str = "\
rtop - a small, fast system monitor

USAGE:
    rtop [OPTIONS]

OPTIONS:
    -i, --interval <ms>   Refresh interval, 250 to 8000 ms [default: 1000]
    -c, --config <path>   Read this config file, not the default one
        --no-config       Read no config file
    -t, --tree            Start in the tree view
    -h, --help            Show this help
    -V, --version         Show the version";

/// The `--help` text. The key list comes from the same table as the help overlay.
fn usage() -> String {
    let keys: Vec<String> = help::usage_keys()
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("    {line}")
            }
        })
        .collect();
    format!("{USAGE_HEAD}\n\nKEYS:\n{}", keys.join("\n"))
}

/// What the command line asks for.
#[derive(Debug, PartialEq)]
struct Cli {
    /// Some when -i was given.
    interval_ms: Option<u64>,
    config: ConfigSource,
    tree: bool,
}

#[derive(Debug, PartialEq)]
enum Parsed {
    Run(Cli),
    Help,
    Version,
}

fn main() -> Result<()> {
    let cli = match parse_args(std::env::args().skip(1))? {
        Parsed::Run(cli) => cli,
        Parsed::Help => {
            println!("{}", usage());
            return Ok(());
        }
        Parsed::Version => {
            println!("rtop {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
    };

    // The only file the UI thread reads, and it reads it before the TUI starts.
    let config = match config::load(&cli.config, |key| std::env::var_os(key)) {
        Ok((config, _path)) => config,
        Err(error) => {
            eprintln!("rtop: {error:#}");
            std::process::exit(1);
        }
    };

    let interval_ms = Arc::new(AtomicU64::new(DEFAULT_INTERVAL_MS));
    let (tx, rx) = mpsc::channel();
    let (kill_tx, kill_rx) = mpsc::channel();
    let mut app =
        App::new(metrics::HostInfo::detect(), Arc::clone(&interval_ms)).with_kill_sender(kill_tx);
    // Precedence: the command line, then the config file, then the built-in default.
    app.apply_config(&config);
    if let Some(ms) = cli.interval_ms {
        interval_ms.store(ms, Ordering::Relaxed);
    }
    if cli.tree {
        app.tree = true;
    }
    metrics::spawn_sampler(tx.clone(), interval_ms, kill_rx).context("start sampler thread")?;

    // `ratatui::run` enters raw mode and the alternate screen, and it puts the
    // terminal back afterwards, even when the closure panics.
    ratatui::run(|terminal| {
        let _paste = BracketedPaste::enable();
        spawn_input(tx).context("start input thread")?;
        run(terminal, app, &rx)
    })
}

/// With bracketed paste on, pasted text arrives as one `Paste` event, not as keys, so a
/// paste cannot type `x` and then `y` or `f` into the kill prompt. Dropping the guard turns
/// it off again, also when the run panics. Best effort: a terminal without it still works.
struct BracketedPaste(bool);

impl BracketedPaste {
    fn enable() -> Self {
        Self(execute!(io::stdout(), EnableBracketedPaste).is_ok())
    }
}

impl Drop for BracketedPaste {
    fn drop(&mut self) {
        if self.0 {
            // Nothing to do if this fails: the terminal is being put back anyway.
            execute!(io::stdout(), DisableBracketedPaste).ok();
        }
    }
}

fn run(terminal: &mut DefaultTerminal, mut app: App, rx: &Receiver<Event>) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        // Block until something happens, then drain the backlog, so a burst of keys costs one redraw.
        app.handle(rx.recv()?);
        for event in rx.try_iter() {
            app.handle(event);
        }
    }
    Ok(())
}

fn spawn_input(tx: Sender<Event>) -> io::Result<()> {
    thread::Builder::new().name("input".into()).spawn(move || {
        while let Ok(event) = event::read() {
            let event = match event {
                TermEvent::Key(key) => Event::Key(key),
                TermEvent::Paste(text) => Event::Paste(text),
                TermEvent::Resize(..) => Event::Resize,
                _ => continue,
            };
            if tx.send(event).is_err() {
                break;
            }
        }
    })?;
    Ok(())
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Parsed> {
    let mut cli = Cli {
        interval_ms: None,
        config: ConfigSource::Default,
        tree: false,
    };
    let mut no_config = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "-i" | "--interval" => {
                let value = args
                    .next()
                    .context("--interval needs a value in milliseconds")?;
                let ms: u64 = value.parse().with_context(|| {
                    format!("invalid interval {value:?}: expected milliseconds")
                })?;
                cli.interval_ms = Some(ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS));
            }
            "-c" | "--config" => {
                let path = args.next().context("--config needs a file path")?;
                cli.config = ConfigSource::Path(PathBuf::from(path));
            }
            "--no-config" => no_config = true,
            "-t" | "--tree" => cli.tree = true,
            other => bail!("unknown argument {other:?}\n\n{}", usage()),
        }
    }
    if no_config {
        if matches!(cli.config, ConfigSource::Path(_)) {
            bail!("--config and --no-config cannot be used together");
        }
        cli.config = ConfigSource::Disabled;
    }
    Ok(Parsed::Run(cli))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Parsed> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    fn run_cli(args: &[&str]) -> Cli {
        match parse(args).unwrap() {
            Parsed::Run(cli) => cli,
            other => panic!("{args:?} gave {other:?}"),
        }
    }

    #[test]
    fn parses_interval_and_clamps_it() {
        assert_eq!(run_cli(&[]).interval_ms, None);
        assert_eq!(run_cli(&["-i", "500"]).interval_ms, Some(500));
        assert_eq!(
            run_cli(&["--interval", "1"]).interval_ms,
            Some(MIN_INTERVAL_MS)
        );
        assert_eq!(run_cli(&["-i", "99999"]).interval_ms, Some(MAX_INTERVAL_MS));
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse(&["--help"]).unwrap(), Parsed::Help);
        assert_eq!(parse(&["-h"]).unwrap(), Parsed::Help);
        assert_eq!(parse(&["-V"]).unwrap(), Parsed::Version);
        assert_eq!(parse(&["--version"]).unwrap(), Parsed::Version);
    }

    #[test]
    fn config_flags() {
        assert_eq!(run_cli(&[]).config, ConfigSource::Default);
        assert_eq!(
            run_cli(&["--config", "x"]).config,
            ConfigSource::Path(PathBuf::from("x"))
        );
        assert_eq!(
            run_cli(&["-c", "y.toml"]).config,
            ConfigSource::Path(PathBuf::from("y.toml"))
        );
        assert_eq!(run_cli(&["--no-config"]).config, ConfigSource::Disabled);
        assert!(parse(&["--config", "x", "--no-config"]).is_err());
        assert!(parse(&["--no-config", "-c", "x"]).is_err());
        assert!(parse(&["--config"]).is_err());
    }

    #[test]
    fn tree_flag() {
        assert!(!run_cli(&[]).tree);
        assert!(run_cli(&["-t"]).tree);
        assert!(run_cli(&["--tree"]).tree);
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse(&["-i"]).is_err());
        assert!(parse(&["-i", "fast"]).is_err());
        assert!(parse(&["--nope"]).is_err());
    }

    #[test]
    fn usage_lists_the_keys() {
        let text = usage();
        assert!(text.contains("KEYS:"), "{text}");
        assert!(text.contains("kill"), "{text}");
        assert!(text.contains("--no-config"), "{text}");
        for section in help::KEYMAP {
            assert!(text.contains(section.title), "{}", section.title);
        }
    }
}
