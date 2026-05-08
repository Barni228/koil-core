use std::{fs, io};

use koil::Koil;

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

    let mut k = Koil::new();
    println!("Reading: {}", dir.display());
    k.open(dir)?;
    fs::write(&listing_path, k.listing())?;

    println!(
        "Opened {}.\
        Edit it, then press Enter...",
        listing_path.display()
    );

    {
        let stdin = io::stdin();
        let mut buf = String::new();
        io::BufRead::read_line(&mut stdin.lock(), &mut buf)?;
    }

    let content = fs::read_to_string(&listing_path)?;
    let actions = k.compute_actions(content)?;
    for action in actions {
        println!("{}", action.command());
    }

    let _ = fs::remove_file(&listing_path);
    println!("Finished.");
    Ok(())
}

