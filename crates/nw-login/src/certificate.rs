// SPDX-License-Identifier: AGPL-3.0-or-later
//! Self-signed certificates and PEM files: the edge-http mirror certificate and the admin relay
//! keys use them. A self-signed certificate here is never a CA; a peer trusts exactly it.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use openssl::asn1::Asn1Time;
use openssl::bn::{BigNum, MsbOption};
use openssl::error::ErrorStack;
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::rsa::Rsa;
use openssl::x509::extension::{
    BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAlternativeName,
};
use openssl::x509::{X509, X509NameBuilder};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// RSA key size of the self-signed certificates.
const KEY_BITS: u32 = 2048;
/// Random bits of the serial number, so a new certificate never repeats an old serial.
const SERIAL_BITS: i32 = 128;
const X509_VERSION_3: i32 = 2;
const COMMON_NAME: &str = "CN";
/// File mode of a written private key on Unix: owner read and write only.
#[cfg(unix)]
pub const OWNER_ONLY_MODE: u32 = 0o600;

/// A certificate file or key could not be made, read or used.
#[derive(Debug, thiserror::Error)]
pub enum CertificateError {
    /// A file cannot be read or written.
    #[error("cannot access {}: {source}", .path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A PEM file holds no usable certificate or key.
    #[error("{} holds no valid {what}", .path.display())]
    Pem {
        /// The file.
        path: PathBuf,
        /// `certificate` or `private key`.
        what: &'static str,
    },
    /// The key does not fit the certificate, or the TLS library refused them.
    #[error("TLS setup failed: {0}")]
    Rustls(#[from] rustls::Error),
    /// OpenSSL could not make the certificate.
    #[error("certificate generation failed: {0}")]
    OpenSsl(#[from] ErrorStack),
    /// No process crypto provider is installed.
    #[error("no TLS crypto provider is installed")]
    NoProvider,
}

/// What a self-signed certificate is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateUse {
    /// A TLS server for the certificate's DNS names.
    Server,
    /// A TLS client (client authentication).
    Client,
}

/// A self-signed RSA certificate named `common_name` for exactly `dns_names`: not a CA, for
/// `usage` only, valid `valid_days` days. Returns the certificate and PKCS#8 key, as PEM.
///
/// # Errors
///
/// Returns the OpenSSL error when the key or the certificate cannot be made.
pub fn self_signed_certificate(
    common_name: &str,
    dns_names: &[String],
    usage: CertificateUse,
    valid_days: u32,
) -> Result<(Vec<u8>, Vec<u8>), ErrorStack> {
    let key = PKey::from_rsa(Rsa::generate(KEY_BITS)?)?;
    let mut name = X509NameBuilder::new()?;
    name.append_entry_by_text(COMMON_NAME, common_name)?;
    let name = name.build();
    let mut builder = X509::builder()?;
    builder.set_version(X509_VERSION_3)?;
    let mut serial = BigNum::new()?;
    serial.rand(SERIAL_BITS, MsbOption::MAYBE_ZERO, false)?;
    let serial = serial.to_asn1_integer()?;
    builder.set_serial_number(&serial)?;
    builder.set_subject_name(&name)?;
    builder.set_issuer_name(&name)?;
    builder.set_pubkey(&key)?;
    let not_before = Asn1Time::days_from_now(0)?;
    let not_after = Asn1Time::days_from_now(valid_days)?;
    builder.set_not_before(&not_before)?;
    builder.set_not_after(&not_after)?;
    builder.append_extension(BasicConstraints::new().critical().build()?)?;
    let (key_usage, extended_usage) = match usage {
        CertificateUse::Server => (
            KeyUsage::new()
                .critical()
                .digital_signature()
                .key_encipherment()
                .build()?,
            ExtendedKeyUsage::new().server_auth().build()?,
        ),
        CertificateUse::Client => (
            KeyUsage::new().critical().digital_signature().build()?,
            ExtendedKeyUsage::new().client_auth().build()?,
        ),
    };
    builder.append_extension(key_usage)?;
    builder.append_extension(extended_usage)?;
    let mut names = SubjectAlternativeName::new();
    for dns_name in dns_names {
        names.dns(dns_name);
    }
    let names = names.build(&builder.x509v3_context(None, None))?;
    builder.append_extension(names)?;
    builder.sign(&key, MessageDigest::sha256())?;
    Ok((builder.build().to_pem()?, key.private_key_to_pem_pkcs8()?))
}

/// The bytes of `path`.
///
/// # Errors
///
/// Returns [`CertificateError::Io`] when the file cannot be read.
pub fn read_file(path: &Path) -> Result<Vec<u8>, CertificateError> {
    std::fs::read(path).map_err(|source| CertificateError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// The certificate chain in the PEM file `path` (at least one certificate).
///
/// # Errors
///
/// Returns [`CertificateError`] when the file cannot be read or holds no certificate.
pub fn read_chain(path: &Path) -> Result<Vec<CertificateDer<'static>>, CertificateError> {
    CertificateDer::pem_slice_iter(&read_file(path)?)
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .filter(|chain| !chain.is_empty())
        .ok_or_else(|| CertificateError::Pem {
            path: path.to_path_buf(),
            what: "certificate",
        })
}

/// The private key in the PEM file `path`.
///
/// # Errors
///
/// Returns [`CertificateError`] when the file cannot be read or holds no key.
pub fn read_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, CertificateError> {
    PrivateKeyDer::from_pem_slice(&read_file(path)?).map_err(|_| CertificateError::Pem {
        path: path.to_path_buf(),
        what: "private key",
    })
}

/// Writes `bytes` to a new file that only the owner can read (on Unix). An existing file is an
/// error, so no certificate or key is replaced.
///
/// # Errors
///
/// Returns [`CertificateError::Io`] when the file exists or cannot be written.
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<(), CertificateError> {
    let io_error = |source| CertificateError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(OWNER_ONLY_MODE);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(bytes))
        .map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_DAYS: u32 = 1;

    fn names_and_text(usage: CertificateUse) -> (Vec<String>, String) {
        let dns_names = vec!["relay.example.test".to_owned()];
        let (certificate, key) =
            self_signed_certificate("Test", &dns_names, usage, VALID_DAYS).expect("certificate");
        let certificate = X509::from_pem(&certificate).expect("parse");
        assert!(PKey::private_key_from_pem(&key).is_ok());
        assert!(
            certificate
                .verify(&certificate.public_key().expect("key"))
                .expect("verify")
        );
        let names = certificate
            .subject_alt_names()
            .expect("names")
            .iter()
            .filter_map(|name| name.dnsname().map(str::to_owned))
            .collect();
        let text = String::from_utf8(certificate.to_text().expect("text")).expect("utf-8");
        assert!(text.contains("CA:FALSE"), "{text}");
        (names, text)
    }

    #[test]
    fn a_server_certificate_is_for_server_use_only() {
        let (names, text) = names_and_text(CertificateUse::Server);
        assert_eq!(names, ["relay.example.test"]);
        assert!(text.contains("TLS Web Server Authentication"), "{text}");
        assert!(!text.contains("TLS Web Client Authentication"), "{text}");
    }

    #[test]
    fn a_client_certificate_is_for_client_use_only() {
        let (_, text) = names_and_text(CertificateUse::Client);
        assert!(text.contains("TLS Web Client Authentication"), "{text}");
        assert!(!text.contains("TLS Web Server Authentication"), "{text}");
    }

    #[test]
    fn written_files_are_never_replaced_and_read_back() {
        let dir = tempfile::tempdir().expect("dir");
        let (certificate, key) =
            self_signed_certificate("Test", &[], CertificateUse::Client, VALID_DAYS)
                .expect("certificate");
        let certificate_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        write_new(&certificate_path, &certificate).expect("certificate");
        write_new(&key_path, &key).expect("key");
        assert!(matches!(
            write_new(&key_path, b"other"),
            Err(CertificateError::Io { .. })
        ));
        assert_eq!(read_chain(&certificate_path).expect("chain").len(), 1);
        assert!(read_private_key(&key_path).is_ok());
        assert!(matches!(
            read_chain(&key_path),
            Err(CertificateError::Pem { .. })
        ));
        assert!(matches!(
            read_private_key(&certificate_path),
            Err(CertificateError::Pem { .. })
        ));
    }
}
