//! The agent's persistent identity.
//!
//! One Ed25519 key per agent, which is also the key of its Iroh endpoint. Its
//! public half is the [`AgentId`]; its canonical text is the same 64 lowercase
//! hex characters as Iroh's `EndpointId`.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::str::FromStr;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An agent's public identity: its Ed25519 public key.
///
/// Displayed, parsed and serialized as 64 lowercase hex characters; parsing
/// accepts only that form, and only a valid curve point.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentId([u8; 32]);

impl AgentId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Whether `sig` is this agent's signature of `msg`, under the strict
    /// Ed25519 rules (no small-order keys, no malleable signatures).
    pub fn verify(&self, msg: &[u8], sig: &[u8; 64]) -> bool {
        VerifyingKey::from_bytes(&self.0)
            .is_ok_and(|key| key.verify_strict(msg, &Signature::from_bytes(sig)).is_ok())
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl fmt::Debug for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AgentId({self})")
    }
}

/// The text is not an agent id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("an agent id is the 64 lowercase hex characters of an Ed25519 public key")]
pub struct InvalidAgentId;

impl FromStr for AgentId {
    type Err = InvalidAgentId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let text = s.as_bytes();
        if text.len() != 64 {
            return Err(InvalidAgentId);
        }
        let mut bytes = [0u8; 32];
        let (pairs, _) = text.as_chunks::<2>();
        for (byte, [high, low]) in bytes.iter_mut().zip(pairs) {
            *byte = (lower_hex_digit(*high)? << 4) | lower_hex_digit(*low)?;
        }
        VerifyingKey::from_bytes(&bytes).map_err(|_| InvalidAgentId)?;
        Ok(Self(bytes))
    }
}

fn lower_hex_digit(c: u8) -> Result<u8, InvalidAgentId> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(InvalidAgentId),
    }
}

impl Serialize for AgentId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for AgentId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// The agent's Ed25519 signing key.
///
/// Its `Debug` shows only the [`AgentId`].
pub struct AgentKey(SigningKey);

impl AgentKey {
    /// A new key from the operating system's random number generator.
    ///
    /// # Panics
    ///
    /// If the operating system cannot provide random bytes.
    pub fn generate() -> Self {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).expect("the OS random number generator failed");
        Self(SigningKey::from_bytes(&secret))
    }

    /// Loads the key at `path`, or creates it there when the file does not
    /// exist.
    ///
    /// The file holds the 32 raw bytes of the secret key and is created with
    /// mode `0600`. An existing file readable or writable by anyone but its
    /// owner is refused with an error whose message contains
    /// `agent.key permissions too open`.
    pub fn load_or_create(path: &Path) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(path) {
            Ok(file) => {
                let key = Self::generate();
                Self::write_new(file, &key).inspect_err(|_| {
                    // Never leave a partial key behind.
                    let _ = fs::remove_file(path);
                })?;
                Ok(key)
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Self::load(path),
            Err(err) => Err(err),
        }
    }

    fn write_new(mut file: File, key: &Self) -> io::Result<()> {
        // The umask may have cleared bits of the creation mode.
        #[cfg(unix)]
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(&key.to_bytes())?;
        file.sync_all()
    }

    fn load(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Checked on the open file, so it is the file that is read.
            let mode = file.metadata()?.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "agent.key permissions too open: {} has mode {mode:03o}, expected 600",
                        path.display()
                    ),
                ));
            }
        }
        let mut content = Vec::with_capacity(32);
        file.take(33).read_to_end(&mut content)?;
        let secret: [u8; 32] = content.as_slice().try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} must hold exactly 32 bytes", path.display()),
            )
        })?;
        Ok(Self(SigningKey::from_bytes(&secret)))
    }

    pub fn id(&self) -> AgentId {
        AgentId(self.0.verifying_key().to_bytes())
    }

    /// The 32 raw bytes of the secret key, as Iroh's `SecretKey::from_bytes`
    /// takes them.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.0.sign(msg).to_bytes()
    }
}

impl fmt::Debug for AgentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AgentKey").field(&self.id()).finish()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn key_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.key");
        let first = AgentKey::load_or_create(&path).unwrap();
        let second = AgentKey::load_or_create(&path).unwrap();
        assert_eq!(first.id(), second.id());
        assert_eq!(first.to_bytes(), second.to_bytes());
    }

    #[test]
    fn key_file_is_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.key");
        let key = AgentKey::load_or_create(&path).unwrap();
        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read(&path).unwrap(), key.to_bytes());
    }

    #[test]
    fn wide_permissions_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.key");
        AgentKey::load_or_create(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let err = AgentKey::load_or_create(&path).unwrap_err();
        assert!(err.to_string().contains("permissions too open"), "{err}");
    }

    #[test]
    fn key_of_the_wrong_length_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.key");
        fs::write(&path, [7u8; 31]).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(AgentKey::load_or_create(&path).is_err());
    }

    #[test]
    fn agent_id_hex_roundtrip() {
        let id = AgentKey::generate().id();
        assert_eq!(id.to_string().len(), 64);
        assert_eq!(id, id.to_string().parse().unwrap());
        assert!(
            id.to_string()
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(
            serde_json::to_value(id).unwrap(),
            serde_json::Value::String(id.to_string())
        );
        let back: AgentId = serde_json::from_value(serde_json::to_value(id).unwrap()).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn agent_id_parse_rejects_non_canonical_text() {
        let hex = AgentKey::generate().id().to_string();
        assert!(hex.to_uppercase().parse::<AgentId>().is_err());
        assert!(hex[..63].parse::<AgentId>().is_err());
        assert!(format!("{hex}00").parse::<AgentId>().is_err());
        assert!(format!("+{}", &hex[1..]).parse::<AgentId>().is_err());
        assert!(format!("g{}", &hex[1..]).parse::<AgentId>().is_err());
        // 32 bytes that are not a point on the curve.
        assert!(NOT_A_POINT.parse::<AgentId>().is_err());
    }

    #[test]
    fn agent_id_equals_iroh_endpoint_id() {
        let k = AgentKey::generate();
        assert_eq!(
            iroh::SecretKey::from_bytes(&k.to_bytes())
                .public()
                .to_string(),
            k.id().to_string()
        );
    }

    #[test]
    fn signature_verifies_and_tamper_fails() {
        let key = AgentKey::generate();
        let other = AgentKey::generate();
        let msg = b"agentnet test message";
        let sig = key.sign(msg);
        assert!(key.id().verify(msg, &sig));
        assert!(!key.id().verify(b"agentnet test messagE", &sig));
        let mut bad = sig;
        bad[0] ^= 1;
        assert!(!key.id().verify(msg, &bad));
        assert!(!other.id().verify(msg, &sig));
    }

    /// Lowercase hex of a 32-byte string that does not decompress to an
    /// Ed25519 point.
    const NOT_A_POINT: &str = "0200000000000000000000000000000000000000000000000000000000000000";
}
