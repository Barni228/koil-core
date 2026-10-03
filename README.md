# koil-core

Edit a directory as a list of entries, like [`oil.nvim`](https://github.com/stevearc/oil.nvim).

`koil-core` is the library behind koil. It has no UI. A frontend gets the entries of a
directory and lets the user edit them. Then it hands them back. koil compares the edit with
the filesystem and returns the actions that would make it real (create, delete, rename,
copy). The frontend shows these actions to the user and applies them once the user
confirms. Deleted paths go to the system trash, and every apply can be undone.

The library takes and returns plain Rust data. It has no text format, so any frontend can
use it: a CLI, a TUI, a GUI, or an editor plugin.

## Example

```rust,no_run
use koil_core::{Entry, Koil};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut koil = Koil::builder().build();
    koil.open("/home/me/notes")?;

    // Show the entries to the user, and let them edit
    let mut entries = koil.listing();
    // Rename the first entry
    entries[0].name = "renamed.md".into();
    // Create a file inside a dir that does not exist yet
    entries.push(Entry {
        id: None,
        name: "drafts/idea.md".into(),
        is_dir: false,
    });

    // Nothing changes if any entry is invalid
    // Each problem points at an entry, so a frontend can highlight that line
    let warnings = koil.update(&entries)?;
    for warning in warnings {
        println!("warning: {warning}");
    }

    // Preview what will happen
    for action in koil.compute_actions() {
        println!("{action}"); // like `move a.md -> renamed.md`
    }

    // Run it, then change your mind
    koil.apply()?;
    koil.undo()?;
    Ok(())
}
```

## How editing works

Each existing entry has an `Id`. It is a stable handle that always points to the same path.
The edit is read by what happened to each ID:

- A new name means rename.
- An ID that shows up twice means copy.
- A missing ID means delete.
- An entry with no ID means create. A name can be nested, like `dir/file`, and missing
  parent dirs are created too.
- An ID written in the listing of a different directory means move.

Edits add up across directories. You can open one dir, remove an entry, open another dir,
paste the entry there, and koil sees it as a move.

The actions come in a safe order. Swaps and rename cycles (`a -> b`, `b -> a`) go through a
temporary name.

## Features

- **Two phases.** `update`, `check`, `compute_actions` and `undo_steps` only preview.
  `apply` and `undo` change the filesystem.
- **Errors per entry.** `update` returns every problem at once, with the index of the entry
  it is about: unknown IDs, duplicates, invalid names, a name that is too long or has
  control characters, and a create or rename onto something that already exists.
- **Name warnings.** Names that work but may cause trouble later give a warning, not an
  error. These include characters Windows does not allow, characters a shell needs quoted,
  reserved names like `CON`, emoji, invisible or look-alike characters, spaces at the edges,
  a trailing `.`, a leading `-`, and names that are not valid UTF-8.
- **Patterns.** Open a glob like `src/**/*.rs`, or a regex, to edit every matching file and
  dir at once. Names are shown relative to the base dir. Dirs that nothing inside can match
  are never searched, and a pattern that matches or has to search too much (`Limits`) stops
  early, instead of reading every path.
- **Paths as a shell reads them.** A path can be quoted (`"my dir"`, `'my dir'`) or escaped
  (`my\ dir`, except on Windows, where `\` is a separator), like one copied from a terminal.
  What is quoted is never special in a pattern, so `"[draft]"*.md` is every file starting
  with `[draft]`.
- **Hidden and ignored files.** Hidden entries and entries ignored by git can be hidden or
  shown (`Settings`). A hidden entry is still shown while it has pending changes, so a
  change is never lost.
- **Undo.** Deleted paths are moved to the system trash, not removed. `undo` brings them
  back and moves created paths to the trash.
- **Sessions.** `save_state` and `load_state` save the whole session as JSON, so another
  process can continue it.

## Platforms

koil works on Linux, macOS and Windows. It uses the
[`trash`](https://crates.io/crates/trash) crate for the trash on Linux and Windows. On
macOS it calls `NSFileManager` itself, because it has to know where the trashed item went
to restore it.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.
