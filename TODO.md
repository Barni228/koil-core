## Features

- [ ] Allow commands to be entered in the settings box
- [ ] Allow creating many files with `file{1,2,3}` syntax
- [ ] Allow creating many files with `file{1..3}` syntax
- [ ] Allow different sorting
- [ ] Allow updating listing on save
- [ ] Add a way to open files from koil
- [ ] Add file icons
- [ ] Parse file with `chumsky`
- [ ] Publish to crates.io

## Not Planned

- [ ] Allow having BOTH glob AND regex in one expression

## Done

- [x] Allow rejecting some actions while keeping others (in confirm dialog)
- [x] Handle cases where file system changes while this is still running (open koil, then create new file)
- [x] Warn user about creating files with weird names (like `:^&<here>`)
- [x] Allow entering a regex pattern, so like glob but its regex (`regex=true` in the CLI header)
- [x] Optionally respect `.gitignore` (`gitignore=true` in the CLI header), so `**/*.rs` skips `target/`
- [x] Allow entering a glob pattern, to see every file with matching path (`**/*.rs`)
- [x] Allow showing hidden paths (including `../`), with `hidden=true` in the CLI header
- [x] Separate cli and library code into 2 different projects (`koil-cli`)
- [x] Create `koil undo`, which undoes the last apply (deletes go to the trash)
- [x] Generate random looking IDs with `sqids`
- [x] Allow nested paths in the listing (`dir/A`)
- [x] Allow entering into not yet created directories (`>dir/`)
- [x] Dont always rename to `tmp` when resolving renames
- [x] Make it so when line starts with `::`, it means "open this" (like pressing enter)
- [x] Make a `KoilBuilder`
- [x] Allow cross-directory edits
- [x] Don't panic when user created a random id for some reason
