use anyhow::{Context, bail};
use clap::{Args, CommandFactory, Parser, Subcommand};
use io::Write;
use koil::session::Session;
use koil::{Koil, Warning};
use std::path::{Path, PathBuf};
use std::{env, io};

/// Edit directories as text
///
/// Without a subcommand, starts an interactive session:
/// - the listing file is opened in `$VISUAL` or `$EDITOR`
/// - edit it, and press Enter to update it
/// - use any commands you usually can in the shell, like `end` to end session
#[derive(Parser)]
#[command(version, verbatim_doc_comment)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path of the listing file
    #[arg(short, long, global = true, default_value = ".koil_listing")]
    path: PathBuf,

    /// Do not open the listing in an editor, in the interactive session
    #[arg(long)]
    no_editor: bool,

    #[command(flatten)]
    end: EndArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Start a session in the current directory, and write the listing file
    #[command(visible_alias = "s")]
    Start,
    /// Read the edited listing file, and write the updated one
    #[command(visible_alias = "u")]
    Update,
    /// Apply every change made so far, and keep the session going
    #[command(visible_alias = "a")]
    Apply(EndArgs),
    /// Revert the last apply of this session, deleted paths come back from the trash
    Undo(EndArgs),
    /// End the session, and apply every change made in it
    #[command(visible_alias = "e")]
    End(EndArgs),
}

/// Commands of the interactive session
#[derive(Parser)]
#[command(
    multicall = true,
    disable_help_subcommand = true,
    help_template = "Press Enter to update, or type a command:\n{subcommands}"
)]
enum Interactive {
    /// Read the edited listing file, and write the updated one (same as pressing Enter)
    #[command(visible_alias = "u")]
    Update,
    /// Apply every change made so far, and keep the session going
    #[command(visible_alias = "a")]
    Apply(EndArgs),
    /// Revert the last apply of this session, deleted paths come back from the trash
    Undo(EndArgs),
    /// End the session, and apply every change made in it
    #[command(visible_alias = "e")]
    End(EndArgs),
    /// Show this help, use `<command> --help` for help of a command
    #[command(visible_alias = "h")]
    Help,
}

#[derive(Args, Clone, Copy)]
struct EndArgs {
    /// Apply the changes without asking
    #[arg(short, long)]
    yes: bool,

    /// Only print the shell commands that would apply the changes
    #[arg(long, conflicts_with = "yes")]
    dry_run: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let session = Session::new(&cli.path)?;

    match cli.command {
        None => interactive(&session, cli.end, !cli.no_editor),
        Some(Command::Start) => {
            let koil = session.start(&env::current_dir()?)?;
            session.save(&koil)?;
            println!(
                "Edit `{}`, then run `koil update` or `koil end`",
                session.listing().display()
            );
            Ok(())
        }
        Some(Command::Update) => {
            let koil = load(&session)?;
            session.save(&koil)?;
            Ok(())
        }
        Some(Command::Apply(args)) => {
            let mut koil = load(&session)?;
            let result = apply(&mut koil, args);
            // a dry run changes nothing
            if !args.dry_run {
                session.save(&koil)?;
            }
            result
        }
        Some(Command::Undo(args)) => {
            let mut koil = load(&session)?;
            let result = undo(&mut koil, args);
            if !args.dry_run {
                session.save(&koil)?;
            }
            result
        }
        Some(Command::End(args)) => {
            let mut koil = load(&session)?;
            // a dry run changes nothing, so the session can go on
            if !args.dry_run {
                session.remove();
            }
            apply(&mut koil, args)
        }
    }
}

/// Continue the saved session, with the edits made in its listing
fn load(session: &Session) -> anyhow::Result<Koil> {
    let mut koil = session.load()?;
    warn(session.update(&mut koil)?);
    Ok(koil)
}

/// `defaults` are used by every `apply`, `undo` and `end`, on top of their own args
fn interactive(session: &Session, defaults: EndArgs, editor: bool) -> anyhow::Result<()> {
    let mut koil = session.start(&env::current_dir()?)?;
    println!("Editing `{}`", session.listing().display());
    if editor && let Err(err) = open_in_editor(session.listing()) {
        eprintln!("Warning: can not open an editor: {err:#}");
    }
    let with_defaults = |args: EndArgs| EndArgs {
        yes: args.yes || defaults.yes,
        dry_run: args.dry_run || defaults.dry_run,
    };

    loop {
        let Some(line) = input("Press Enter to update, or type a command ([h]elp): ")? else {
            // stdin was closed, so user can not confirm anything
            session.remove();
            println!("\nNothing was applied.");
            return Ok(());
        };
        let command = if line.trim().is_empty() {
            Interactive::Update
        } else {
            match Interactive::try_parse_from(line.split_whitespace()) {
                Ok(command) => command,
                // also prints `--help` of a command
                Err(err) => {
                    let _ = err.print();
                    continue;
                }
            }
        };
        if let Interactive::Help = command {
            println!("{}", Interactive::command().render_help());
            continue;
        }

        match session.update(&mut koil) {
            Ok(warning) => warn(warning),
            Err(err) => {
                eprintln!("Error: {:#}", anyhow::Error::from(err));
                continue;
            }
        }
        let result = match command {
            Interactive::Update | Interactive::Help => Ok(()),
            Interactive::Apply(args) => apply(&mut koil, with_defaults(args)),
            Interactive::Undo(args) => undo(&mut koil, with_defaults(args)),
            Interactive::End(args) => {
                session.remove();
                return apply(&mut koil, with_defaults(args));
            }
        };
        if let Err(err) = result {
            eprintln!("Error: {err:#}");
        }
        session.write_listing(&koil)?;
    }
}

/// Show the changes, and apply them if user agrees
fn apply(koil: &mut Koil, args: EndArgs) -> anyhow::Result<()> {
    let base = env::current_dir()?;
    let commands: Vec<String> = koil
        .compute_actions()
        .iter()
        .map(|a| a.command(&base))
        .collect();
    if confirm(&commands, args, "Apply these changes?")? {
        let report = koil.apply()?;
        println!("Applied {} changes.", report.changes);
        warn(report.warning);
    }
    Ok(())
}

/// Show how the last apply would be reverted, and revert it if user agrees
fn undo(koil: &mut Koil, args: EndArgs) -> anyhow::Result<()> {
    let base = env::current_dir()?;
    let Some(steps) = koil.undo_steps()? else {
        println!("Nothing to undo.");
        return Ok(());
    };
    let commands: Vec<String> = steps.iter().map(|s| s.command(&base)).collect();
    if confirm(&commands, args, "Undo these changes?")? {
        let report = koil.undo()?;
        println!("Undid {} changes.", report.changes);
        warn(report.warning);
    }
    Ok(())
}

fn warn(warning: Option<Warning>) {
    if let Some(warning) = warning {
        eprintln!("Warning: {warning}");
    }
}

/// Show the shell commands of the changes, and return true if user agrees to run them
/// On a dry run, only prints the commands, and never agrees
fn confirm(commands: &[String], args: EndArgs, question: &str) -> anyhow::Result<bool> {
    if args.dry_run {
        for command in commands {
            println!("{command}");
        }
        return Ok(false);
    }

    if commands.is_empty() {
        println!("No changes.");
        return Ok(false);
    }
    for command in commands {
        println!("  {command}");
    }
    if !args.yes {
        let answer = input(&format!("{question} [y/N] "))?.unwrap_or_default();
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Nothing was changed.");
            return Ok(false);
        }
    }
    Ok(true)
}

/// Open `path` in `$VISUAL` or `$EDITOR`, or the system default app if neither is set
/// Waits for the editor to exit, so terminal editors can take over the terminal
fn open_in_editor(path: &Path) -> anyhow::Result<()> {
    let editor = ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(|var| env::var(var).ok())
        .find(|e| !e.trim().is_empty())
        .unwrap_or_else(|| {
            let default = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            default.to_string()
        });
    // through the shell, so an editor with arguments (like `code -w`) works
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path)
        .status()
        .with_context(|| format!("Can not run `{editor}`"))?;
    if !status.success() {
        bail!("`{editor}` exited with {status}");
    }
    Ok(())
}

/// Read a line from stdin, `None` if stdin is closed
fn input(prompt: &str) -> io::Result<Option<String>> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end().to_string()))
}
