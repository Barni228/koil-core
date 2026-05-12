## Features

- [ ] Dont always rename to `tmp` when resolving renames
- [ ] Generate unique IDs using HashId (convert number to random looking string)
- [ ] create `koil undo`, which undoes whatever it just did
- [ ] Make it so when line starts with `::`, it means "open this" (like pressing enter)
- [ ] Allow updating listing on save
- [ ] Allow entering a glob pattern, to see every file with matching path
- [ ] Allow entering a regex pattern, so like glob but its regex
- [ ] Allow having BOTH glob AND regex in one expression
- [ ] Allow creating many files with `file{1,2,3}` syntax
- [ ] Allow creating many files with `file{1..3}` syntax
- [ ] Add a way to open files from koil
- [ ] Add file icons
- [ ] Allow different sorting
- [ ] Parse file with `chumsky`
- [ ] Publish to crates.io

## Done

- [x] Make a `KoilBuilder`
- [x] Allow cross-directory edits
- [x] Don't panic when user created a random id for some reason
