//! The daemon's age identity, the break-glass recipients, and where both
//! come from.
//!
//! Key lookup, first hit wins: `ISB_AGE_KEY` (the key itself), the systemd
//! credential `$CREDENTIALS_DIRECTORY/isb-age-key`, `ISB_AGE_KEY_FILE`, then
//! `~/.config/isb/age.txt`. With none of them, a new X25519 identity is
//! generated at that last path.

use std::fmt;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use age::secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The systemd credential name (`LoadCredential=`/`SetCredentialEncrypted=`).
pub const CREDENTIAL_NAME: &str = "isb-age-key";

/// Something a value is encrypted to: an age X25519 key (`age1…`) or an SSH
/// public key (`ssh-ed25519 …`, `ssh-rsa …`).
#[derive(Clone)]
pub enum Recipient {
    X25519(age::x25519::Recipient),
    Ssh(age::ssh::Recipient),
}

impl Recipient {
    /// Parse one recipient. An SSH key's trailing comment is ignored.
    pub fn parse(s: &str) -> Result<Recipient> {
        let s = s.trim();
        if s.starts_with("age1") {
            return age::x25519::Recipient::from_str(s)
                .map(Recipient::X25519)
                .map_err(|e| Error::invalid(format!("recipient {s:?}: {e}")));
        }
        if s.starts_with("ssh-") {
            // `type base64 [comment]`: the comment is not part of the key.
            let key: Vec<&str> = s.split_whitespace().take(2).collect();
            let key = key.join(" ");
            return age::ssh::Recipient::from_str(&key)
                .map(Recipient::Ssh)
                .map_err(|e| Error::invalid(format!("recipient {}: {e:?}", brief(&key))));
        }
        Err(Error::invalid(format!(
            "recipient {}: expected an age key (age1...) or an ssh-ed25519/ssh-rsa public key",
            brief(s)
        )))
    }

    pub fn as_age(&self) -> &dyn age::Recipient {
        match self {
            Recipient::X25519(r) => r,
            Recipient::Ssh(r) => r,
        }
    }
}

impl fmt::Display for Recipient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Recipient::X25519(r) => write!(f, "{r}"),
            Recipient::Ssh(r) => write!(f, "{r}"),
        }
    }
}

impl fmt::Debug for Recipient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Recipient({self})")
    }
}

impl FromStr for Recipient {
    type Err = Error;
    fn from_str(s: &str) -> Result<Recipient> {
        Recipient::parse(s)
    }
}

/// The first 24 characters, for error messages about keys.
fn brief(s: &str) -> String {
    let t: String = s.chars().take(24).collect();
    if t.len() < s.len() {
        format!("{t:?}...")
    } else {
        format!("{t:?}")
    }
}

/// Parse an identity: the first `AGE-SECRET-KEY-1…` line of an age key file
/// (`age-keygen` output, comments allowed) or the bare key.
pub fn parse_identity(text: &str) -> Result<age::x25519::Identity> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .ok_or_else(|| Error::invalid("no AGE-SECRET-KEY-1... line in the age key"))?;
    age::x25519::Identity::from_str(line)
        .map_err(|e| Error::invalid(format!("invalid age secret key: {e}")))
}

/// `age-keygen`'s file format, so `age -d -i` can use the file directly.
pub fn identity_file_text(id: &age::x25519::Identity) -> String {
    format!(
        "# isb serve's secrets key. Keep it out of unencrypted backups.\n# public key: {}\n{}\n",
        id.to_public(),
        id.to_string().expose_secret()
    )
}

/// `$XDG_CONFIG_HOME/isb`, else `~/.config/isb`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/etc"))
        .join("isb")
}

/// Where the key is looked for, in order. Injectable for tests.
#[derive(Clone, Default)]
pub struct KeySources {
    /// `ISB_AGE_KEY`: the key itself.
    pub key: Option<String>,
    /// `$CREDENTIALS_DIRECTORY`, where systemd puts `isb-age-key`.
    pub credentials_dir: Option<PathBuf>,
    /// `ISB_AGE_KEY_FILE`: must exist when set.
    pub key_file: Option<PathBuf>,
    /// `~/.config/isb/age.txt`: read when present, else generated.
    pub default_file: PathBuf,
}

impl fmt::Debug for KeySources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeySources")
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("credentials_dir", &self.credentials_dir)
            .field("key_file", &self.key_file)
            .field("default_file", &self.default_file)
            .finish()
    }
}

impl KeySources {
    pub fn from_env() -> KeySources {
        let var = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        KeySources {
            key: var("ISB_AGE_KEY"),
            credentials_dir: var("CREDENTIALS_DIRECTORY").map(PathBuf::from),
            key_file: var("ISB_AGE_KEY_FILE").map(PathBuf::from),
            default_file: config_dir().join("age.txt"),
        }
    }
}

/// Where the key came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOrigin {
    Env,
    Credential(PathBuf),
    KeyFile(PathBuf),
    DefaultFile(PathBuf),
    /// Just generated, at this path.
    Generated(PathBuf),
}

impl fmt::Display for KeyOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyOrigin::Env => f.write_str("$ISB_AGE_KEY"),
            KeyOrigin::Credential(p) => write!(f, "systemd credential {}", p.display()),
            KeyOrigin::KeyFile(p) => write!(f, "$ISB_AGE_KEY_FILE {}", p.display()),
            KeyOrigin::DefaultFile(p) => write!(f, "{}", p.display()),
            KeyOrigin::Generated(p) => write!(f, "{} (generated)", p.display()),
        }
    }
}

/// The daemon's identity, where it came from, and anything worth logging.
pub struct LoadedKey {
    pub identity: age::x25519::Identity,
    pub origin: KeyOrigin,
    pub notes: Vec<String>,
}

/// Find the daemon's key, or generate one at `default_file`.
pub fn load_identity(src: &KeySources) -> Result<LoadedKey> {
    lookup_identity(src, true)
}

/// Find the daemon's key; never generate one (a client reading the store
/// must not mint a key the daemon would then use).
pub fn find_identity(src: &KeySources) -> Result<LoadedKey> {
    lookup_identity(src, false)
}

fn lookup_identity(src: &KeySources, generate: bool) -> Result<LoadedKey> {
    let loaded = |identity, origin| LoadedKey {
        identity,
        origin,
        notes: Vec::new(),
    };
    if let Some(k) = src.key.as_deref().filter(|k| !k.trim().is_empty()) {
        let id = parse_identity(k).map_err(|e| Error::invalid(format!("ISB_AGE_KEY: {e}")))?;
        return Ok(loaded(id, KeyOrigin::Env));
    }
    if let Some(dir) = &src.credentials_dir {
        // The unit may carry other credentials; only ours counts.
        let p = dir.join(CREDENTIAL_NAME);
        if p.exists() {
            return Ok(loaded(read_identity(&p)?, KeyOrigin::Credential(p)));
        }
    }
    if let Some(p) = &src.key_file {
        // Named explicitly: missing is an error, not a reason to generate.
        return Ok(loaded(read_identity(p)?, KeyOrigin::KeyFile(p.clone())));
    }
    let p = &src.default_file;
    if p.exists() {
        let mut k = loaded(read_identity(p)?, KeyOrigin::DefaultFile(p.clone()));
        if let Ok(m) = std::fs::metadata(p) {
            if m.permissions().mode() & 0o077 != 0 {
                k.notes.push(format!(
                    "WARNING: {} is readable by others (mode {:o}); chmod 600 it",
                    p.display(),
                    m.permissions().mode() & 0o777
                ));
            }
        }
        return Ok(k);
    }
    if !generate {
        return Err(Error::invalid(format!(
            "no secrets key: not in $ISB_AGE_KEY, $CREDENTIALS_DIRECTORY/{CREDENTIAL_NAME}, $ISB_AGE_KEY_FILE or {}",
            p.display()
        )));
    }
    let id = generate_identity_file(p)?;
    let mut k = loaded(id, KeyOrigin::Generated(p.clone()));
    k.notes.push(format!(
        "generated a new secrets key at {}: exclude it from unencrypted backups, and add a break-glass recipient (recipients = [...] in {}) so secrets survive losing it",
        p.display(),
        SecretsConfig::default_path().display()
    ));
    Ok(k)
}

fn read_identity(p: &Path) -> Result<age::x25519::Identity> {
    let text = std::fs::read_to_string(p)
        .map_err(|e| Error::invalid(format!("age key {}: {e}", p.display())))?;
    parse_identity(&text).map_err(|e| Error::invalid(format!("age key {}: {e}", p.display())))
}

/// Write a new identity to `path` (0600, its directory 0700) without ever
/// replacing an existing file: written to a temp file, then hard-linked in.
pub fn generate_identity_file(path: &Path) -> Result<age::x25519::Identity> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::invalid(format!("{}: no parent directory", path.display())))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let id = age::x25519::Identity::generate();
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let r = (|| -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(identity_file_text(&id).as_bytes())?;
        f.sync_all()?;
        std::fs::hard_link(&tmp, path).map_err(|e| {
            Error::invalid(format!("cannot create age key {}: {e}", path.display()))
        })?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&tmp);
    r?;
    super::local::fsync_dir(dir);
    Ok(id)
}

/// `~/.config/isb/secrets.toml`: break-glass recipients.
///
/// ```toml
/// recipients = [
///   "age1...",
///   "ssh-ed25519 AAAA... ops@example.com",
/// ]
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsConfig {
    /// Every value is also encrypted to these, so it can be recovered
    /// without the daemon's key.
    #[serde(default)]
    pub recipients: Vec<String>,
}

impl SecretsConfig {
    /// `$ISB_SECRETS_CONFIG`, else `~/.config/isb/secrets.toml`.
    pub fn default_path() -> PathBuf {
        std::env::var_os("ISB_SECRETS_CONFIG")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| config_dir().join("secrets.toml"))
    }

    /// Load the file; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<SecretsConfig> {
        match std::fs::read_to_string(path) {
            Ok(text) => SecretsConfig::parse(&text).map_err(|e| Error::Parse {
                path: path.display().to_string(),
                message: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SecretsConfig::default()),
            Err(e) => Err(Error::invalid(format!("{}: {e}", path.display()))),
        }
    }

    pub fn parse(text: &str) -> Result<SecretsConfig> {
        let c: SecretsConfig = toml::from_str(text).map_err(|e| Error::invalid(e.to_string()))?;
        c.parsed_recipients()?;
        Ok(c)
    }

    pub fn parsed_recipients(&self) -> Result<Vec<Recipient>> {
        self.recipients
            .iter()
            .map(|r| Recipient::parse(r))
            .collect()
    }
}

/// The daemon's identity and the full recipient set: its own public key
/// first, then the break-glass recipients.
pub struct Keyring {
    identity: age::x25519::Identity,
    recipients: Vec<Recipient>,
}

impl Keyring {
    pub fn new(identity: age::x25519::Identity, break_glass: Vec<Recipient>) -> Keyring {
        let mut recipients = vec![Recipient::X25519(identity.to_public())];
        for r in break_glass {
            if !recipients.iter().any(|x| x.to_string() == r.to_string()) {
                recipients.push(r);
            }
        }
        Keyring {
            identity,
            recipients,
        }
    }

    /// The daemon's public key (`age1…`).
    pub fn public_key(&self) -> String {
        self.identity.to_public().to_string()
    }

    /// Everything a value is encrypted to.
    pub fn recipients(&self) -> &[Recipient] {
        &self.recipients
    }

    pub fn break_glass(&self) -> &[Recipient] {
        &self.recipients[1..]
    }

    pub fn identity(&self) -> &dyn age::Identity {
        &self.identity
    }

    /// Binary age ciphertext to every recipient.
    pub fn encrypt(&self, value: &[u8]) -> Result<Vec<u8>> {
        super::inline::encrypt(value, &self.recipients)
    }

    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        super::inline::decrypt(ciphertext, &[&self.identity])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const SSH_PUB: &str = include_str!("testdata/break_glass_ed25519.pub");

    fn sources(dir: &Path) -> KeySources {
        KeySources {
            default_file: dir.join("cfg/isb/age.txt"),
            ..Default::default()
        }
    }

    #[test]
    fn recipients_parse() {
        let id = age::x25519::Identity::generate();
        let pk = id.to_public().to_string();
        assert_eq!(Recipient::parse(&pk).unwrap().to_string(), pk);
        let r = Recipient::parse(SSH_PUB.trim()).unwrap();
        // The comment is dropped.
        assert!(r.to_string().starts_with("ssh-ed25519 AAAA"));
        assert!(!r.to_string().contains("isb-test"));
        assert!(Recipient::parse("age1nope").is_err());
        assert!(Recipient::parse("pgp:xyz").is_err());
        assert!(Recipient::parse("ssh-ed25519 AAAAnotbase64!").is_err());
    }

    #[test]
    fn config_parses_and_rejects() {
        let pk = age::x25519::Identity::generate().to_public().to_string();
        let c = SecretsConfig::parse(&format!(
            "recipients = [\"{pk}\", \"{}\"]\n",
            SSH_PUB.trim()
        ))
        .unwrap();
        assert_eq!(c.parsed_recipients().unwrap().len(), 2);
        assert!(SecretsConfig::parse("recipients = [\"bogus\"]").is_err());
        assert!(SecretsConfig::parse("recipient = []").is_err());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            SecretsConfig::load(&dir.path().join("none.toml")).unwrap(),
            SecretsConfig::default()
        );
    }

    #[test]
    fn lookup_order() {
        let dir = tempfile::tempdir().unwrap();
        let ids: Vec<_> = (0..4).map(|_| age::x25519::Identity::generate()).collect();
        let text = |i: usize| identity_file_text(&ids[i]);
        let pk = |i: usize| ids[i].to_public().to_string();
        let creds = dir.path().join("creds");
        std::fs::create_dir_all(&creds).unwrap();
        std::fs::write(creds.join(CREDENTIAL_NAME), text(1)).unwrap();
        let kf = dir.path().join("key.txt");
        std::fs::write(&kf, text(2)).unwrap();
        let mut src = sources(dir.path());
        std::fs::create_dir_all(src.default_file.parent().unwrap()).unwrap();
        std::fs::write(&src.default_file, text(3)).unwrap();
        src.key = Some(text(0));
        src.credentials_dir = Some(creds.clone());
        src.key_file = Some(kf.clone());

        let k = load_identity(&src).unwrap();
        assert_eq!(
            (k.identity.to_public().to_string(), k.origin),
            (pk(0), KeyOrigin::Env)
        );
        src.key = None;
        let k = load_identity(&src).unwrap();
        assert_eq!(k.identity.to_public().to_string(), pk(1));
        assert_eq!(k.origin, KeyOrigin::Credential(creds.join(CREDENTIAL_NAME)));
        // A credentials directory without our credential falls through.
        std::fs::remove_file(creds.join(CREDENTIAL_NAME)).unwrap();
        let k = load_identity(&src).unwrap();
        assert_eq!(k.identity.to_public().to_string(), pk(2));
        assert_eq!(k.origin, KeyOrigin::KeyFile(kf.clone()));
        src.key_file = None;
        let k = load_identity(&src).unwrap();
        assert_eq!(k.identity.to_public().to_string(), pk(3));
        assert_eq!(k.origin, KeyOrigin::DefaultFile(src.default_file.clone()));
        // A named key file that is missing is an error, never a new key.
        src.key_file = Some(dir.path().join("missing.txt"));
        assert!(load_identity(&src).is_err());
        // So is a bad ISB_AGE_KEY.
        src.key = Some("AGE-SECRET-KEY-1NOPE".into());
        assert!(
            load_identity(&src)
                .err()
                .unwrap()
                .to_string()
                .contains("ISB_AGE_KEY")
        );
        // The bare key works as well as the file format.
        let bare = ids[0].to_string().expose_secret().to_string();
        src.key = Some(bare);
        assert_eq!(
            load_identity(&src)
                .unwrap()
                .identity
                .to_public()
                .to_string(),
            pk(0)
        );
    }

    #[test]
    fn find_never_generates() {
        let dir = tempfile::tempdir().unwrap();
        let src = sources(dir.path());
        let e = find_identity(&src).err().unwrap().to_string();
        assert!(e.contains("no secrets key"), "{e}");
        assert!(!src.default_file.exists());
        let k = load_identity(&src).unwrap();
        assert_eq!(
            find_identity(&src)
                .unwrap()
                .identity
                .to_public()
                .to_string(),
            k.identity.to_public().to_string()
        );
    }

    #[test]
    fn generates_once_with_private_modes() {
        let dir = tempfile::tempdir().unwrap();
        let src = sources(dir.path());
        let k = load_identity(&src).unwrap();
        assert_eq!(k.origin, KeyOrigin::Generated(src.default_file.clone()));
        assert!(k.notes[0].contains("break-glass"));
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&src.default_file), 0o600);
        assert_eq!(mode(src.default_file.parent().unwrap()), 0o700);
        let text = std::fs::read_to_string(&src.default_file).unwrap();
        assert!(text.contains(&format!("# public key: {}", k.identity.to_public())));
        // The second start reads it back; nothing is regenerated.
        let k2 = load_identity(&src).unwrap();
        assert_eq!(k2.origin, KeyOrigin::DefaultFile(src.default_file.clone()));
        assert_eq!(
            k2.identity.to_public().to_string(),
            k.identity.to_public().to_string()
        );
        // And generating over an existing file refuses.
        assert!(generate_identity_file(&src.default_file).is_err());
        // Group-readable gets a warning.
        std::fs::set_permissions(&src.default_file, std::fs::Permissions::from_mode(0o640))
            .unwrap();
        assert!(load_identity(&src).unwrap().notes[0].contains("WARNING"));
    }

    #[test]
    fn keyring_dedupes_and_round_trips() {
        let id = age::x25519::Identity::generate();
        let own = Recipient::X25519(id.to_public());
        let ssh = Recipient::parse(SSH_PUB).unwrap();
        let k = Keyring::new(id, vec![own, ssh.clone(), ssh]);
        assert_eq!(k.recipients().len(), 2);
        assert_eq!(k.break_glass().len(), 1);
        let ct = k.encrypt(b"hunter2").unwrap();
        assert_eq!(k.decrypt(&ct).unwrap(), b"hunter2");
    }
}
