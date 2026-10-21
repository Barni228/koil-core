//! How koil keeps paths in its JSON ([`Koil::save_state`](crate::Koil::save_state), the undo
//! history a frontend keeps, and [`Conflict`](crate::Conflict), which a frontend may hand back):
//! a path that is Unicode as a string, as serde keeps one, and one that is not (only on Linux,
//! or a malformed name on Windows) as serde keeps an `OsString` (`{"Unix": [bytes]}`, or
//! `{"Windows": [UTF-16 units]}`), where serde's own fails
//! A field has it with `#[serde(with = "serde_path")]`, an `Option` of one with
//! `serde_path::option`, and a `Vec` or a `HashSet` of them with `serde_path::many`

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// A path as koil keeps it
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum Repr<'a> {
    Unicode(Cow<'a, str>),
    Os(Cow<'a, OsStr>),
}

impl<'a> From<&'a Path> for Repr<'a> {
    fn from(path: &'a Path) -> Self {
        match path.to_str() {
            Some(path) => Repr::Unicode(Cow::Borrowed(path)),
            None => Repr::Os(Cow::Borrowed(path.as_os_str())),
        }
    }
}

impl From<Repr<'_>> for PathBuf {
    fn from(repr: Repr<'_>) -> Self {
        match repr {
            Repr::Unicode(path) => path.into_owned().into(),
            Repr::Os(path) => path.into_owned().into(),
        }
    }
}

pub(crate) fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    Repr::from(path).serialize(serializer)
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
    Repr::deserialize(deserializer).map(PathBuf::from)
}

pub(crate) mod option {
    use super::*;

    pub(crate) fn serialize<S: Serializer>(
        path: &Option<PathBuf>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        path.as_deref().map(Repr::from).serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<PathBuf>, D::Error> {
        Ok(Option::<Repr>::deserialize(deserializer)?.map(PathBuf::from))
    }
}

pub(crate) mod many {
    use super::*;

    pub(crate) fn serialize<'a, S: Serializer>(
        paths: impl IntoIterator<Item = &'a PathBuf>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(paths.into_iter().map(|path| Repr::from(path.as_path())))
    }

    pub(crate) fn deserialize<'de, P, D>(deserializer: D) -> Result<P, D::Error>
    where
        P: FromIterator<PathBuf>,
        D: Deserializer<'de>,
    {
        let paths = Vec::<Repr>::deserialize(deserializer)?;
        Ok(paths.into_iter().map(PathBuf::from).collect())
    }
}
