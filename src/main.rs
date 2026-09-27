use anyhow::{Context, bail};
use clap::{Args, CommandFactory, Parser, Subcommand};
use io::Write;
use koil::{Action, Koil};
use std::path::{Path, PathBuf};
use std::{env, fs, io};

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

/// Files that belong to one session
struct Session {
    /// The listing file that user edits
    listing: PathBuf,
    /// The saved [`Koil`], so `koil update` and `koil end` can continue the session
    state: PathBuf,
}

impl Session {
    fn new(listing: &Path) -> anyhow::Result<Self> {
        let name = listing
            .file_name()
            .with_context(|| format!("`{}` is not a file name", listing.display()))?;
        let parent = match listing.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        // canonical, so koil can recognize and hide these files in the listing
        let dir = fs::canonicalize(parent)
            .with_context(|| format!("Can not open `{}`", parent.display()))?;
        let mut state_name = name.to_os_string();
        state_name.push(".session");

        Ok(Session {
            listing: dir.join(name),
            state: dir.join(state_name),
        })
    }

    /// Open the current dir, and write its listing
    fn start(&self) -> anyhow::Result<Koil> {
        if self.listing.exists() || self.state.exists() {
            bail!(
                "`{}` already exists.\n\
                Another session may be running, end it with `koil end`, or remove it and try again.",
                self.listing.display()
            );
        }
        let mut koil = Koil::builder()
            .ignore(&self.listing)
            .ignore(&self.state)
            .build();
        koil.open(env::current_dir()?)?;
        self.write_listing(&koil)?;
        Ok(koil)
    }

    /// Continue the session started with `koil start`
    fn load(&self) -> anyhow::Result<Koil> {
        let state = fs::read_to_string(&self.state).with_context(|| {
            format!(
                "No session for `{}`, start one with `koil start`",
                self.listing.display()
            )
        })?;
        Koil::load_state(&state).context("The session file is corrupted")
    }

    fn write_listing(&self, koil: &Koil) -> io::Result<()> {
        fs::write(&self.listing, koil.listing() + "\n")
    }

    fn save(&self, koil: &Koil) -> io::Result<()> {
        fs::write(&self.state, koil.save_state())
    }

    /// Apply the edited listing to `koil`
    /// If the listing is invalid, `koil` is left as it was
    fn update(&self, koil: &mut Koil) -> anyhow::Result<()> {
        let content = fs::read_to_string(&self.listing)
            .with_context(|| format!("Can not read `{}`", self.listing.display()))?;
        let mut updated = koil.clone();
        if let Some(warning) = updated.update(&content)? {
            eprintln!("Warning: {warning}");
        }
        *koil = updated;
        Ok(())
    }

    /// Remove every file of this session
    fn remove(&self) {
        let _ = fs::remove_file(&self.listing);
        let _ = fs::remove_file(&self.state);
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let session = Session::new(&cli.path)?;

    match cli.command {
        None => interactive(&session, cli.end, !cli.no_editor),
        Some(Command::Start) => {
            let koil = session.start()?;
            session.save(&koil)?;
            println!(
                "Edit `{}`, then run `koil update` or `koil end`",
                session.listing.display()
            );
            Ok(())
        }
        Some(Command::Update) => {
            let mut koil = session.load()?;
            session.update(&mut koil)?;
            session.write_listing(&koil)?;
            session.save(&koil).context("Can not save the session")
        }
        Some(Command::Apply(args)) => {
            let mut koil = session.load()?;
            session.update(&mut koil)?;
            let result = apply(&mut koil, args);
            // a dry run changes nothing
            if !args.dry_run {
                session.write_listing(&koil)?;
                session.save(&koil).context("Can not save the session")?;
            }
            result
        }
        Some(Command::End(args)) => {
            let mut koil = session.load()?;
            session.update(&mut koil)?;
            // a dry run changes nothing, so the session can go on
            if !args.dry_run {
                session.remove();
            }
            end(koil, args)
        }
    }
}

/// `defaults` are used by every `apply` and `end`, on top of their own args
fn interactive(session: &Session, defaults: EndArgs, editor: bool) -> anyhow::Result<()> {
    let mut koil = session.start()?;
    println!("Editing `{}`", session.listing.display());
    if editor && let Err(err) = open_in_editor(&session.listing) {
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

        if let Err(err) = session.update(&mut koil) {
            eprintln!("Error: {err:#}");
            continue;
        }
        match command {
            Interactive::Update | Interactive::Help => {}
            Interactive::Apply(args) => {
                if let Err(err) = apply(&mut koil, with_defaults(args)) {
                    eprintln!("Error: {err:#}");
                }
            }
            Interactive::End(args) => {
                session.remove();
                return end(koil, with_defaults(args));
            }
        }
        session.write_listing(&koil)?;
    }
}

/// Show the actions, and run them if user agrees, `koil` keeps going with the new filesystem
fn apply(koil: &mut Koil, args: EndArgs) -> anyhow::Result<()> {
    let base = env::current_dir()?;
    // the diff is only consumed if the actions run
    let actions = koil.clone().compute_actions();
    if !confirm(&actions, args, &base)? {
        return Ok(());
    }
    let result = run(&actions, &base);
    // even if some action failed, others changed the filesystem
    if let Some(warning) = koil.refresh()? {
        eprintln!("Warning: {warning}");
    }
    result
}

/// Show the actions, and run them if user agrees
fn end(mut koil: Koil, args: EndArgs) -> anyhow::Result<()> {
    let base = env::current_dir()?;
    let actions = koil.compute_actions();
    if confirm(&actions, args, &base)? {
        run(&actions, &base)?;
    }
    Ok(())
}

/// Show the actions, and return true if user agrees to run them
/// On a dry run, only prints the shell commands, and never agrees
/// Paths in `base` are shown relative to it
fn confirm(actions: &[Action], args: EndArgs, base: &Path) -> anyhow::Result<bool> {
    if args.dry_run {
        for action in actions {
            println!("{}", action.command(base));
        }
        return Ok(false);
    }

    if actions.is_empty() {
        println!("No changes.");
        return Ok(false);
    }
    for action in actions {
        println!("  {}", action.command(base));
    }
    if !args.yes {
        let answer = input("Apply these changes? [y/N] ")?.unwrap_or_default();
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Nothing was applied.");
            return Ok(false);
        }
    }
    Ok(true)
}

fn run(actions: &[Action], base: &Path) -> anyhow::Result<()> {
    for (i, action) in actions.iter().enumerate() {
        action.run().with_context(|| {
            format!(
                "Failed to run `{}`, {} of {} changes were applied",
                action.command(base),
                i,
                actions.len()
            )
        })?;
    }
    println!("Applied {} changes.", actions.len());
    Ok(())
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
