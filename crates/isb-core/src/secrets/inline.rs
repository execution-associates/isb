//! age encryption, and the inline form compose files carry
//! (`secrets: {x: {age: ...}}`).
//!
//! Inline ciphertext is written ASCII-armored (`-----BEGIN AGE ENCRYPTED
//! FILE-----`), the format `age -a` writes and `age -d` reads, so it can be
//! made and checked with the age CLI too. Decryption also accepts the binary
//! format base64-encoded, on one line or several.

use std::io::{Read, Write};

use age::armor::{ArmoredReader, ArmoredWriter, Format};

use super::Recipient;
use crate::error::{Error, Result};

const ARMOR_BEGIN: &str = "-----BEGIN AGE ENCRYPTED FILE-----";

fn age_err(step: &str, e: impl std::fmt::Display) -> Error {
    Error::invalid(format!("age {step}: {e}"))
}

fn encryptor(recipients: &[Recipient]) -> Result<age::Encryptor> {
    if recipients.is_empty() {
        return Err(Error::invalid("age encrypt: no recipients"));
    }
    age::Encryptor::with_recipients(recipients.iter().map(Recipient::as_age))
        .map_err(|e| age_err("encrypt", e))
}

/// Binary age ciphertext of `value` to every recipient.
pub fn encrypt(value: &[u8], recipients: &[Recipient]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(value.len() + 256);
    let mut w = encryptor(recipients)?
        .wrap_output(&mut out)
        .map_err(|e| age_err("encrypt", e))?;
    w.write_all(value).map_err(|e| age_err("encrypt", e))?;
    w.finish().map_err(|e| age_err("encrypt", e))?;
    Ok(out)
}

/// Decrypt age ciphertext, binary or armored, with the first identity that
/// fits.
pub fn decrypt(ciphertext: &[u8], identities: &[&dyn age::Identity]) -> Result<Vec<u8>> {
    let d =
        age::Decryptor::new(ArmoredReader::new(ciphertext)).map_err(|e| age_err("decrypt", e))?;
    if d.is_scrypt() {
        return Err(Error::invalid(
            "age decrypt: passphrase-encrypted values are not supported; encrypt to a recipient",
        ));
    }
    let mut r = d
        .decrypt(identities.iter().copied())
        .map_err(|e| age_err("decrypt", e))?;
    let mut out = Vec::new();
    r.read_to_end(&mut out).map_err(|e| age_err("decrypt", e))?;
    Ok(out)
}

/// ASCII-armored ciphertext for a compose file's `age:` field.
pub fn encrypt_inline(value: &[u8], recipients: &[Recipient]) -> Result<String> {
    let mut out = Vec::new();
    let armor = ArmoredWriter::wrap_output(&mut out, Format::AsciiArmor)
        .map_err(|e| age_err("encrypt", e))?;
    let mut w = encryptor(recipients)?
        .wrap_output(armor)
        .map_err(|e| age_err("encrypt", e))?;
    w.write_all(value).map_err(|e| age_err("encrypt", e))?;
    w.finish()
        .and_then(|a| a.finish())
        .map_err(|e| age_err("encrypt", e))?;
    String::from_utf8(out).map_err(|e| age_err("encrypt", e))
}

/// Decrypt an `age:` field: armored, or base64 of the binary format.
pub fn decrypt_inline(text: &str, identities: &[&dyn age::Identity]) -> Result<Vec<u8>> {
    let t = text.trim();
    if t.starts_with(ARMOR_BEGIN) {
        // The armor parser is strict about line endings; YAML block
        // scalars may have added indentation-free trailing spaces.
        let norm: String = t.lines().map(|l| l.trim_end().to_string() + "\n").collect();
        return decrypt(norm.as_bytes(), identities);
    }
    let bin = crate::rpc::b64_decode(t).map_err(|_| {
        Error::invalid(
            "age: expected ASCII-armored ciphertext (age -a) or base64 of age's binary format",
        )
    })?;
    decrypt(&bin, identities)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh_identity() -> impl age::Identity {
        let key = include_bytes!("testdata/break_glass_ed25519");
        age::ssh::Identity::from_buffer(&key[..], None)
            .unwrap()
            .with_callbacks(age::NoCallbacks)
    }

    #[test]
    fn inline_round_trip_armored_and_base64() {
        let id = age::x25519::Identity::generate();
        let rs = vec![Recipient::X25519(id.to_public())];
        let a = encrypt_inline(b"s3cret\n", &rs).unwrap();
        assert!(a.starts_with(ARMOR_BEGIN));
        assert!(!a.contains("s3cret"));
        assert_eq!(decrypt_inline(&a, &[&id]).unwrap(), b"s3cret\n");
        // As a YAML block scalar would hand it back: indented lines trimmed,
        // trailing whitespace added.
        let messy: String = a.lines().map(|l| format!("{l}  \n")).collect();
        assert_eq!(decrypt_inline(&messy, &[&id]).unwrap(), b"s3cret\n");
        let b = crate::rpc::b64_encode(&encrypt(b"bin", &rs).unwrap());
        assert_eq!(decrypt_inline(&b, &[&id]).unwrap(), b"bin");
        // Wrapped base64 too.
        let wrapped: String = b
            .as_bytes()
            .chunks(40)
            .map(|c| String::from_utf8_lossy(c).into_owned() + "\n")
            .collect();
        assert_eq!(decrypt_inline(&wrapped, &[&id]).unwrap(), b"bin");
        // The wrong key fails, and so does garbage.
        let other = age::x25519::Identity::generate();
        assert!(decrypt_inline(&a, &[&other]).is_err());
        assert!(decrypt_inline("not age at all", &[&id]).is_err());
        assert!(encrypt_inline(b"x", &[]).is_err());
    }

    #[test]
    fn ssh_recipients_decrypt_with_the_ssh_key() {
        let r = Recipient::parse(include_str!("testdata/break_glass_ed25519.pub")).unwrap();
        let ct = encrypt(b"break glass", &[r]).unwrap();
        let ssh = ssh_identity();
        assert_eq!(decrypt(&ct, &[&ssh]).unwrap(), b"break glass");
    }
}
