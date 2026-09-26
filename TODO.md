## Features

- [ ] Generate unique IDs using HashId (convert number to random looking string)
- [ ] create `koil undo`, which undoes whatever it just did
- [ ] Allow updating listing on save
- [ ] Allow entering a glob pattern, to see every file with matching path
- [ ] Allow entering a regex pattern, so like glob but its regex
- [ ] Allow having BOTH glob AND regex in one expression
- [ ] Allow creating many files with `file{1,2,3}` syntax
- [ ] Allow creating many files with `file{1..3}` syntax
- [ ] Add a way to open files from koil
- [ ] Add file icons
- [ ] Allow different sorting
- [ ] Warn user about creating files with weird names (like `:^&<here>`)
- [ ] Parse file with `chumsky`
- [ ] Handle cases where file system changes while this is still running (open koil, then create new file)
- [ ] Publish to crates.io

## Done

- [x] Allow nested paths in the listing (`dir/A`)
- [x] Allow entering into not yet created directories (`>dir/`)
- [x] Dont always rename to `tmp` when resolving renames
- [x] Make it so when line starts with `::`, it means "open this" (like pressing enter)
- [x] Make a `KoilBuilder`
- [x] Allow cross-directory edits
- [x] Don't panic when user created a random id for some reason
