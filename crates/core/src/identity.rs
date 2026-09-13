//! A persisted keypair, so an endpoint id (and therefore an inbox link) can
//! stay the same across runs.
//!
//! By default `hither <paths>` uses a fresh key every time; nothing needs to
//! know who you are to pull a share. An inbox is different: its link must
//! keep working, so it needs the same key tomorrow. `hither id` creates and
//! prints that identity; `--identity` makes a share use it too.
//!
//! Storage: 64 lowercase hex characters in a file only the user can read,
//! at the platform data directory (`~/Library/Application Support/hither/
//! identity` on macOS, `$XDG_DATA_HOME/hither/identity` elsewhere). The
//! `HITHER_SECRET` environment variable overrides the file, which is handy
//! for tests and servers.
//!
//! Export and import move that secret between machines. The secret *is* the
//! identity: whoever holds it is you, so it is shown once, on request, and
//! never written anywhere but the identity file. Two forms are accepted:
//! the 64 hex characters, or 24 words (the 32 key bytes plus one checksum
//! byte, eleven bits per word) for reading aloud or typing by hand.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use data_encoding::HEXLOWER;
use iroh::{EndpointId, SecretKey};

use crate::words;

/// Environment variable holding a hex secret key that overrides the file.
pub const ENV_SECRET: &str = "HITHER_SECRET";
/// Environment variable overriding where the identity file lives.
pub const ENV_FILE: &str = "HITHER_IDENTITY_FILE";

/// A keypair and where it came from.
#[derive(Debug, Clone)]
pub struct Identity {
    secret: SecretKey,
    source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Read from `HITHER_SECRET`.
    Environment,
    /// Read from an existing file.
    File(PathBuf),
    /// Generated during this call and written to the file.
    Created(PathBuf),
}

/// Where the identity file lives unless overridden.
pub fn default_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os(ENV_FILE) {
        return Ok(PathBuf::from(p));
    }
    let base = dirs::data_dir().context("could not determine the user data directory")?;
    Ok(base.join("hither").join("identity"))
}

impl Identity {
    /// Environment override, else load or create the file at `default_path`.
    pub fn load_default() -> Result<Self> {
        if let Some(id) = Self::from_env()? {
            return Ok(id);
        }
        Self::load_or_create(default_path()?)
    }

    /// `Some` if `HITHER_SECRET` is set.
    pub fn from_env() -> Result<Option<Self>> {
        match std::env::var(ENV_SECRET) {
            Ok(hex) => Ok(Some(Self {
                secret: parse_hex(hex.trim()).context("HITHER_SECRET is not a valid key")?,
                source: Source::Environment,
            })),
            Err(_) => Ok(None),
        }
    }

    /// Read the key at `path`, or generate one and write it there.
    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("could not read {}", path.display()))?;
            let secret = parse_hex(text.trim())
                .with_context(|| format!("{} does not contain a valid key", path.display()))?;
            return Ok(Self {
                secret,
                source: Source::File(path.to_path_buf()),
            });
        }
        let secret = SecretKey::generate();
        write_secret(path, &secret)?;
        Ok(Self {
            secret,
            source: Source::Created(path.to_path_buf()),
        })
    }

    pub fn secret_key(&self) -> &SecretKey {
        &self.secret
    }

    /// The public half: what other people dial.
    pub fn endpoint_id(&self) -> EndpointId {
        self.secret.public()
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    /// True if this call generated the key.
    pub fn was_created(&self) -> bool {
        matches!(self.source, Source::Created(_))
    }

    /// The file path, if the identity lives in a file.
    pub fn path(&self) -> Option<&Path> {
        match &self.source {
            Source::File(p) | Source::Created(p) => Some(p),
            Source::Environment => None,
        }
    }

    // ------------------------------------------------------ export / import

    /// The secret as 64 hex characters.
    pub fn to_hex(&self) -> String {
        HEXLOWER.encode(&self.secret.to_bytes())
    }

    /// The secret as 24 words: 32 key bytes then one checksum byte, so a
    /// mistyped word is caught instead of silently producing a different
    /// identity.
    pub fn to_words(&self) -> String {
        let key = self.secret.to_bytes();
        let mut bytes = key.to_vec();
        bytes.push(checksum(&key));
        words::encode(&bytes).join(" ")
    }

    /// Parse either form produced by [`to_hex`](Self::to_hex) or
    /// [`to_words`](Self::to_words).
    pub fn parse_secret(text: &str) -> Result<SecretKey> {
        let trimmed = text.trim();
        let parts = words::split(trimmed);
        if parts.len() == 24 {
            let bytes = words::decode(&parts, 33).context("those are not identity words")?;
            let (key, check) = bytes.split_at(32);
            let key: [u8; 32] = key.try_into().expect("32 bytes");
            ensure!(
                check[0] == checksum(&key),
                "the words do not check out; one of them is probably wrong"
            );
            return Ok(SecretKey::from_bytes(&key));
        }
        if parts.len() == 1 {
            return parse_hex(parts[0]).context("expected 64 hex characters or 24 words");
        }
        bail!(
            "expected 64 hex characters or 24 words, got {} words",
            parts.len()
        )
    }

    /// Write `secret` to `path`. Refuses to replace a *different* existing
    /// identity unless `force`, because the old one would be gone for good.
    pub fn import(path: impl AsRef<Path>, secret: SecretKey, force: bool) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            let existing = Self::load_or_create(path)?;
            if existing.endpoint_id() == secret.public() {
                return Ok(existing);
            }
            ensure!(
                force,
                "{} already holds a different identity ({}). Export it first if you want to keep it, then import with --force",
                path.display(),
                existing.endpoint_id().fmt_short()
            );
            std::fs::remove_file(path)?;
        }
        write_secret(path, &secret)?;
        Ok(Self {
            secret,
            source: Source::Created(path.to_path_buf()),
        })
    }
}

/// One byte of BLAKE3 over the key, enough to catch a wrong word.
fn checksum(key: &[u8; 32]) -> u8 {
    blake3::hash(key).as_bytes()[0]
}

fn parse_hex(text: &str) -> Result<SecretKey> {
    let bytes = HEXLOWER
        .decode(text.to_ascii_lowercase().as_bytes())
        .context("expected 64 hex characters")?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected exactly 32 bytes of key"))?;
    Ok(SecretKey::from_bytes(&bytes))
}

fn write_secret(path: &Path, secret: &SecretKey) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let text = format!("{}\n", HEXLOWER.encode(&secret.to_bytes()));
    // Write to a sibling and rename so a crash never leaves a half-written key.
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("could not create {}", tmp.display()))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("could not move the key into {}", path.display()))?;
    if path.exists() && std::fs::read_to_string(path)?.trim().is_empty() {
        bail!("identity file {} is empty after writing", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_loads_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("identity");
        let a = Identity::load_or_create(&path).unwrap();
        assert!(a.was_created());
        let b = Identity::load_or_create(&path).unwrap();
        assert!(!b.was_created());
        assert_eq!(a.endpoint_id(), b.endpoint_id());
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim().len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn words_round_trip_and_catch_a_typo() {
        let dir = tempfile::tempdir().unwrap();
        let id = Identity::load_or_create(dir.path().join("identity")).unwrap();
        let words = id.to_words();
        assert_eq!(words.split(' ').count(), 24);
        let back = Identity::parse_secret(&words).unwrap();
        assert_eq!(back.public(), id.endpoint_id());
        assert_eq!(
            Identity::parse_secret(&id.to_hex()).unwrap().public(),
            id.endpoint_id()
        );
        // Swap the first word for another valid word: checksum must fail.
        let mut parts: Vec<&str> = words.split(' ').collect();
        parts[0] = if parts[0] == "zoo" { "abandon" } else { "zoo" };
        assert!(Identity::parse_secret(&parts.join(" ")).is_err());
    }

    #[test]
    fn import_refuses_to_replace_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity");
        let a = Identity::load_or_create(&path).unwrap();
        let other = SecretKey::generate();
        assert!(Identity::import(&path, other.clone(), false).is_err());
        assert_eq!(
            Identity::load_or_create(&path).unwrap().endpoint_id(),
            a.endpoint_id()
        );
        let b = Identity::import(&path, other.clone(), true).unwrap();
        assert_eq!(b.endpoint_id(), other.public());
    }

    #[test]
    fn rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity");
        std::fs::write(&path, "not a key\n").unwrap();
        assert!(Identity::load_or_create(&path).is_err());
    }
}
