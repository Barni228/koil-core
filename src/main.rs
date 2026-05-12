use io::Write;
use koil::Koil;
use std::{fs, io};

const FORCE: bool = true;

fn main() -> anyhow::Result<()> {
    let dir = std::env::current_dir()?;

    let listing_path = dir.join(".koil_listing");

    if !FORCE && listing_path.exists() {
        eprintln!(
            "Error: {} already exists.\nAnother instance may be running. Remove it and try again.",
            listing_path.display()
        );
        return Err(
            io::Error::new(io::ErrorKind::AlreadyExists, "listing file already exists").into(),
        );
    }

    let mut k = Koil::builder().ignore(&listing_path).build();
    // TODO: maybe do the fancy stdin.lock stuff
    println!("Reading: {}", dir.display());
    k.open(&dir)?;

    loop {
        fs::write(&listing_path, k.listing())?;

        // println!(
        //     "Opened {}.\n\
        //     Edit it, then press Enter...",
        //     listing_path.display()
        // );

        let new = input("Press Enter to update: ");
        // {
        //     let stdin = io::stdin();
        //     let mut buf = String::new();
        //     io::BufRead::read_line(&mut stdin.lock(), &mut buf)?;
        // }

        let content = fs::read_to_string(&listing_path)?;
        k.update(&content)?;
        if !new.is_empty() {
            break;
        }
    }

    let actions = k.compute_actions();
    for action in actions {
        println!("{:?}", action);
    }

    let _ = fs::remove_file(&listing_path);
    println!("Finished.");
    Ok(())
}

fn input(prompt: &str) -> String {
    let mut line = String::new();
    print!("{}", prompt);
    io::stdout().flush().unwrap();
    io::stdin()
        .read_line(&mut line)
        .expect("Failed to read the line");
    line.trim_end().to_string()
}
