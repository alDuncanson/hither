//! Named inboxes, so `hither to laptop photos/` works after saving the
//! laptop's inbox link once. Stored as JSON beside the identity file.

use std::{collections::BTreeMap, path::PathBuf, str::FromStr};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{identity, inbox::InboxTicket, link};

/// Where the address book lives.
pub fn friends_path() -> Result<PathBuf> {
    Ok(identity::default_path()?.with_file_name("friends.json"))
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Friends {
    /// name -> inbox ticket string
    pub inboxes: BTreeMap<String, String>,
}

impl Friends {
    pub fn load() -> Result<Self> {
        let path = friends_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("{} is not valid", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = friends_path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text + "\n")
            .with_context(|| format!("could not write {}", path.display()))?;
        Ok(())
    }

    /// Remember `ticket` as `name`. Names are lowercase letters, digits, `-`
    /// and `_`, so they can never be mistaken for a path or a ticket.
    pub fn add(&mut self, name: &str, ticket: &InboxTicket) -> Result<()> {
        validate_name(name)?;
        self.inboxes.insert(name.to_string(), ticket.to_string());
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.inboxes.remove(name).is_some()
    }

    pub fn get(&self, name: &str) -> Option<InboxTicket> {
        self.inboxes
            .get(name)
            .and_then(|t| InboxTicket::from_str(t).ok())
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 32
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'),
        "a friend's name is 1-32 lowercase letters, digits, '-' or '_'"
    );
    ensure!(
        !name.starts_with("blob") && !name.starts_with("inbox"),
        "that name looks too much like a ticket"
    );
    Ok(())
}

/// A saved name, or an inbox ticket / link.
pub fn resolve_inbox(input: &str) -> Result<InboxTicket> {
    if validate_name(input).is_ok() {
        if let Some(t) = Friends::load()?.get(input) {
            return Ok(t);
        }
        if link::parse_any(input).is_err() {
            bail!(
                "no friend named {input:?}. Save one with `hither friends add {input} <inbox link>`"
            );
        }
    }
    link::parse_inbox(input)
}

/// True if `input` is a saved friend's name.
pub fn is_friend(input: &str) -> bool {
    validate_name(input).is_ok()
        && Friends::load()
            .map(|f| f.get(input).is_some())
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_strict() {
        assert!(validate_name("laptop").is_ok());
        assert!(validate_name("sam-2").is_ok());
        assert!(validate_name("Sam").is_err());
        assert!(validate_name("photos/").is_err());
        assert!(validate_name("inboxabc").is_err());
        assert!(validate_name("").is_err());
    }
}
