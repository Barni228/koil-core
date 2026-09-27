//! A session kept in files, so it can be continued by a different process
//!
//! The listing file is what user edits, and the state file next to it holds the saved [`Koil`]

use crate::{Koil, KoilError, Warning};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("`{0}` is not a file name")]
    NotAFileName(PathBuf),

    #[error("`{0}` already exists, another session may be running, end it or remove it first")]
    AlreadyRunning(PathBuf),

    #[error("No session for `{0}`, start one first")]
    NoSession(PathBuf, #[source] io::Error),

    #[error("The session file `{0}` is corrupted")]
    Corrupted(PathBuf, #[source] serde_json::Error),

    #[error("Can not access `{0}`")]
    Io(PathBuf, #[source] io::Error),

    #[error(transparent)]
    Koil(#[from] KoilError),
}

/// Files that belong to one session
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The listing file that user edits
    listing: PathBuf,
    /// The saved [`Koil`], so the session can be continued with [`Session::load`]
    state: PathBuf,
}

impl Session {
    /// A session with the listing file at `listing`, and the state file next to it
    pub fn new(listing: &Path) -> Result<Self, SessionError> {
        let name = listing
            .file_name()
            .ok_or_else(|| SessionError::NotAFileName(listing.to_path_buf()))?;
        let parent = match listing.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        // canonical, so koil can recognize and hide these files in the listing
        let dir = fs::canonicalize(parent).map_err(|e| SessionError::Io(parent.into(), e))?;
        let mut state_name = name.to_os_string();
        state_name.push(".session");

        Ok(Session {
            listing: dir.join(name),
            state: dir.join(state_name),
        })
    }

    /// The listing file that user edits
    pub fn listing(&self) -> &Path {
        &self.listing
    }

    /// Start a new session in `dir`, and write its listing
    /// The state is not saved, use [`Session::save`] if it should be continued by another process
    pub fn start(&self, dir: &Path) -> Result<Koil, SessionError> {
        if self.listing.exists() || self.state.exists() {
            return Err(SessionError::AlreadyRunning(self.listing.clone()));
        }
        let mut koil = Koil::builder()
            .ignore(&self.listing)
            .ignore(&self.state)
            .build();
        koil.open(dir)
            .map_err(|e| SessionError::Io(dir.to_path_buf(), e))?;
        self.write_listing(&koil)?;
        Ok(koil)
    }

    /// Continue the session saved with [`Session::save`]
    pub fn load(&self) -> Result<Koil, SessionError> {
        let state = fs::read_to_string(&self.state)
            .map_err(|e| SessionError::NoSession(self.listing.clone(), e))?;
        Koil::load_state(&state).map_err(|e| SessionError::Corrupted(self.state.clone(), e))
    }

    /// Read the edited listing file, and update `koil` with it
    /// If the listing is invalid, `koil` is left as it was
    pub fn update(&self, koil: &mut Koil) -> Result<Option<Warning>, SessionError> {
        let content = fs::read_to_string(&self.listing)
            .map_err(|e| SessionError::Io(self.listing.clone(), e))?;
        Ok(koil.update(&content)?)
    }

    /// Write the current listing of `koil` to the listing file
    pub fn write_listing(&self, koil: &Koil) -> Result<(), SessionError> {
        fs::write(&self.listing, koil.listing() + "\n")
            .map_err(|e| SessionError::Io(self.listing.clone(), e))
    }

    /// Write the listing, and save `koil`, so [`Session::load`] can continue it
    pub fn save(&self, koil: &Koil) -> Result<(), SessionError> {
        self.write_listing(koil)?;
        fs::write(&self.state, koil.save_state())
            .map_err(|e| SessionError::Io(self.state.clone(), e))
    }

    /// Remove every file of this session
    pub fn remove(&self) {
        let _ = fs::remove_file(&self.listing);
        let _ = fs::remove_file(&self.state);
    }
}
