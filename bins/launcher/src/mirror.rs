// SPDX-License-Identifier: GPL-3.0-or-later
//! The game process's private network view (study notes
//! `docs/re/2026-09-18-remote-config-layers.md`, `docs/re/2026-11-02-cms-news-hosts.md` and
//! `docs/re/2026-11-02-region-ping.md`). The client calls the remote-config bucket, its analytics
//! stream, STS, its region ping host and its news hosts by fixed Amazon names. The launcher runs
//! Proton inside `bwrap` with two private files bound over the system ones, for the game process
//! alone:
//!
//! - `/etc/hosts`: the server's `[aws_mirror]` names lead to the server's own address;
//! - Wine's first trust file: the system roots plus the server's mirror certificate.
//!
//! Nothing on the system changes: no system hosts file, no DNS, no Wine prefix setting.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Write;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nw_protocol::aws_mirror::{AwsMirror, MIRROR_HOST_KINDS, is_aws_mirror_host};
use rustls::RootCertStore;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

/// The system hosts file, read by glibc and so by Wine's name lookups.
pub const SYSTEM_HOSTS_FILE: &str = "/etc/hosts";
/// The first root file Wine's crypt32 reads (`CRYPT_knownLocations`); Wine stops at the first
/// file it can read.
pub const SYSTEM_TRUST_FILE: &str = "/etc/ssl/certs/ca-certificates.crt";
/// The sandbox program.
pub const BWRAP_PROGRAM: &str = "bwrap";
/// Folder of the private files, under the launcher settings folder.
pub const NETWORK_DIR: &str = "game-network";
const HOSTS_FILE_NAME: &str = "hosts";
const TRUST_FILE_NAME: &str = "ca-certificates.crt";
/// HTTPS port, used only to look up the server's addresses.
const HTTPS_PORT: u16 = 443;
/// First line of the private hosts file.
const HOSTS_HEADER: &str =
    "# Blighted launcher: the game process only. Amazon names below lead to the server.";
/// `bwrap`: keep the whole file system and devices, then bind one file read-only over another.
const BWRAP_KEEP_ROOT: [&str; 3] = ["--dev-bind", "/", "/"];
const BWRAP_READ_ONLY_BIND: &str = "--ro-bind";
const BWRAP_END_OF_OPTIONS: &str = "--";

/// The server's mirror cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum MirrorError {
    /// The server asked to map a name that is not one of the [`MIRROR_HOST_KINDS`].
    #[error("the server asked to redirect {0}, which is not a {MIRROR_HOST_KINDS}")]
    BadHost(String),
    /// The server named no host.
    #[error("the server named no host to redirect")]
    NoHosts,
    /// The certificate is not one valid server certificate for exactly the named hosts.
    #[error("the server's mirror certificate is not valid for {host}: {reason}")]
    BadCertificate {
        /// The host it was checked for.
        host: String,
        /// Why it was refused.
        reason: String,
    },
    /// The server's address could not be found.
    #[error("cannot find the address of {host}: {source}")]
    Lookup {
        /// The server host.
        host: String,
        /// The lookup error.
        #[source]
        source: std::io::Error,
    },
    /// A file could not be read or written.
    #[error("cannot access {}: {source}", .path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Checks the server's mirror: every host passes [`is_aws_mirror_host`], and the certificate
/// is one self-signed server certificate, not a CA, valid now for each host. A CA would let the
/// server vouch for other names, so it is refused.
pub fn check_mirror(mirror: &AwsMirror) -> Result<(), MirrorError> {
    if mirror.hosts.is_empty() {
        return Err(MirrorError::NoHosts);
    }
    if let Some(host) = mirror
        .hosts
        .iter()
        .find(|host| host.to_ascii_lowercase() != **host || !is_aws_mirror_host(host))
    {
        return Err(MirrorError::BadHost(host.clone()));
    }
    let refuse = |host: &str, reason: String| MirrorError::BadCertificate {
        host: host.to_owned(),
        reason,
    };
    let first = &mirror.hosts[0];
    let certificates = CertificateDer::pem_slice_iter(mirror.certificate_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| refuse(first, error.to_string()))?;
    let [certificate] = certificates.as_slice() else {
        return Err(refuse(
            first,
            "it must be exactly one certificate".to_owned(),
        ));
    };
    let mut roots = RootCertStore::empty();
    roots
        .add(certificate.clone())
        .map_err(|error| refuse(first, error.to_string()))?;
    let verifier = WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|error| refuse(first, error.to_string()))?;
    for host in &mirror.hosts {
        let name =
            ServerName::try_from(host.as_str()).map_err(|error| refuse(host, error.to_string()))?;
        verifier
            .verify_server_cert(certificate, &[], &name, &[], UnixTime::now())
            .map_err(|error| refuse(host, error.to_string()))?;
    }
    Ok(())
}

/// The addresses of the server host, without repeats.
pub fn server_addresses(server_host: &str) -> Result<Vec<IpAddr>, MirrorError> {
    let addresses: BTreeSet<IpAddr> = (server_host, HTTPS_PORT)
        .to_socket_addrs()
        .map_err(|source| MirrorError::Lookup {
            host: server_host.to_owned(),
            source,
        })?
        .map(|address| address.ip())
        .collect();
    Ok(addresses.into_iter().collect())
}

/// The private hosts file: a header, one line per address naming every mirror host, then the
/// system hosts file unchanged. glibc takes the first line that names a host, so the mirror lines
/// win.
pub fn hosts_file(system_hosts: &str, addresses: &[IpAddr], hosts: &[String]) -> String {
    let names = hosts.join(" ");
    let mut text = format!("{HOSTS_HEADER}\n");
    for address in addresses {
        text.push_str(&format!("{address} {names}\n"));
    }
    text.push_str(system_hosts);
    text
}

/// The private trust file: the system roots, then the mirror certificate.
pub fn trust_file(system_roots: &str, certificate_pem: &str) -> String {
    let mut text = system_roots.to_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(certificate_pem);
    text
}

/// The two private files, as written for one game start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkFiles {
    /// Bound over [`SYSTEM_HOSTS_FILE`].
    pub hosts: PathBuf,
    /// Bound over [`SYSTEM_TRUST_FILE`].
    pub trust: PathBuf,
}

fn read_text(path: &Path) -> Result<String, MirrorError> {
    std::fs::read_to_string(path).map_err(|source| MirrorError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_text(path: &Path, text: &str) -> Result<(), MirrorError> {
    std::fs::File::create(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(|source| MirrorError::Io {
            path: path.to_path_buf(),
            source,
        })
}

/// Checks `mirror`, looks up `server_host`, and writes the private files into `dir`, made from
/// [`SYSTEM_HOSTS_FILE`] and [`SYSTEM_TRUST_FILE`].
pub fn write_network_files(
    dir: &Path,
    mirror: &AwsMirror,
    server_host: &str,
) -> Result<NetworkFiles, MirrorError> {
    check_mirror(mirror)?;
    let addresses = server_addresses(server_host)?;
    write_files_from(
        dir,
        mirror,
        &addresses,
        Path::new(SYSTEM_HOSTS_FILE),
        Path::new(SYSTEM_TRUST_FILE),
    )
}

/// Writes the private files into `dir`, made from the given system files.
fn write_files_from(
    dir: &Path,
    mirror: &AwsMirror,
    addresses: &[IpAddr],
    system_hosts_path: &Path,
    system_trust_path: &Path,
) -> Result<NetworkFiles, MirrorError> {
    std::fs::create_dir_all(dir).map_err(|source| MirrorError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let files = NetworkFiles {
        hosts: dir.join(HOSTS_FILE_NAME),
        trust: dir.join(TRUST_FILE_NAME),
    };
    let system_hosts = read_text(system_hosts_path)?;
    write_text(
        &files.hosts,
        &hosts_file(&system_hosts, addresses, &mirror.hosts),
    )?;
    let system_roots = read_text(system_trust_path)?;
    write_text(
        &files.trust,
        &trust_file(&system_roots, &mirror.certificate_pem),
    )?;
    Ok(files)
}

/// The `bwrap` arguments that run `program` with the private files in place.
pub fn bwrap_arguments(files: &NetworkFiles) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = BWRAP_KEEP_ROOT.iter().map(OsString::from).collect();
    for (private, system) in [
        (&files.hosts, SYSTEM_HOSTS_FILE),
        (&files.trust, SYSTEM_TRUST_FILE),
    ] {
        arguments.push(BWRAP_READ_ONLY_BIND.into());
        arguments.push(private.into());
        arguments.push(system.into());
    }
    arguments.push(BWRAP_END_OF_OPTIONS.into());
    arguments
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use nw_protocol::aws_mirror::CLIENT_PING_PORT;

    use super::*;

    /// A self-signed server certificate (not a CA) for three mirror hosts.
    const MIRROR_PEM: &str = include_str!("../testdata/aws-mirror.crt");
    /// The same names in a CA certificate.
    const CA_PEM: &str = include_str!("../testdata/aws-mirror-ca.crt");
    const HOSTS: [&str; 3] = [
        "ags-javelin-remote-config.s3.amazonaws.com",
        "kinesis.us-east-1.amazonaws.com",
        "sts.us-east-1.amazonaws.com",
    ];

    fn mirror(hosts: &[&str], pem: &str) -> AwsMirror {
        AwsMirror {
            hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
            certificate_pem: pem.to_owned(),
            ping_port: CLIENT_PING_PORT,
        }
    }

    #[test]
    fn a_server_certificate_for_the_named_hosts_is_accepted() {
        crate::install_crypto_provider();
        check_mirror(&mirror(&HOSTS, MIRROR_PEM)).expect("valid mirror");
        check_mirror(&mirror(&HOSTS[..1], MIRROR_PEM)).expect("one host");
    }

    #[test]
    fn a_ca_or_a_certificate_for_other_names_is_refused() {
        crate::install_crypto_provider();
        assert!(matches!(
            check_mirror(&mirror(&HOSTS, CA_PEM)),
            Err(MirrorError::BadCertificate { .. })
        ));
        assert!(matches!(
            check_mirror(&mirror(&["sts.eu-west-1.amazonaws.com"], MIRROR_PEM)),
            Err(MirrorError::BadCertificate { host, .. }) if host == "sts.eu-west-1.amazonaws.com"
        ));
        let two = format!("{MIRROR_PEM}{MIRROR_PEM}");
        assert!(matches!(
            check_mirror(&mirror(&HOSTS, &two)),
            Err(MirrorError::BadCertificate { .. })
        ));
        assert!(matches!(
            check_mirror(&mirror(&HOSTS, "not a certificate")),
            Err(MirrorError::BadCertificate { .. })
        ));
    }

    #[test]
    fn only_remote_config_kinesis_and_sts_hosts_are_redirected() {
        for host in [
            "2mfrik7h83.execute-api.us-east-1.amazonaws.com",
            "steamcommunity.com",
            "STS.us-east-1.amazonaws.com",
        ] {
            assert!(matches!(
                check_mirror(&mirror(&[host], MIRROR_PEM)),
                Err(MirrorError::BadHost(found)) if found == host
            ));
        }
        assert!(matches!(
            check_mirror(&mirror(&[], MIRROR_PEM)),
            Err(MirrorError::NoHosts)
        ));
    }

    #[test]
    fn the_hosts_file_puts_the_mirror_first_and_keeps_the_system_lines() {
        let system = "127.0.0.1 localhost\n127.0.0.1 login.example.org\n";
        let addresses = [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ];
        let hosts: Vec<String> = HOSTS.iter().map(|host| (*host).to_owned()).collect();
        let text = hosts_file(system, &addresses, &hosts);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], HOSTS_HEADER);
        assert_eq!(lines[1], format!("127.0.0.1 {}", HOSTS.join(" ")));
        assert_eq!(lines[2], format!("::1 {}", HOSTS.join(" ")));
        assert!(text.ends_with(system));
    }

    #[test]
    fn the_trust_file_adds_the_mirror_after_the_system_roots() {
        assert_eq!(trust_file("A", "B\n"), "A\nB\n");
        assert_eq!(trust_file("A\n", "B\n"), "A\nB\n");
        assert_eq!(trust_file("", "B\n"), "B\n");
    }

    #[test]
    fn the_server_address_is_looked_up() {
        assert_eq!(
            server_addresses("127.0.0.1").expect("address"),
            [IpAddr::V4(Ipv4Addr::LOCALHOST)]
        );
    }

    #[test]
    fn the_files_are_written_and_bound_over_the_system_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let system_hosts = dir.path().join("system-hosts");
        let system_trust = dir.path().join("system-trust");
        std::fs::write(&system_hosts, "127.0.0.1 localhost\n").expect("hosts");
        std::fs::write(&system_trust, "ROOTS\n").expect("trust");
        let files = write_files_from(
            &dir.path().join(NETWORK_DIR),
            &mirror(&HOSTS, MIRROR_PEM),
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            &system_hosts,
            &system_trust,
        )
        .expect("files");
        let hosts = std::fs::read_to_string(&files.hosts).expect("hosts");
        assert!(hosts.contains(&format!("127.0.0.1 {}", HOSTS.join(" "))));
        assert!(hosts.ends_with("127.0.0.1 localhost\n"));
        let trust = std::fs::read_to_string(&files.trust).expect("trust");
        assert_eq!(trust, format!("ROOTS\n{MIRROR_PEM}"));
        let arguments: Vec<String> = bwrap_arguments(&files)
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            arguments,
            [
                "--dev-bind".to_owned(),
                "/".to_owned(),
                "/".to_owned(),
                "--ro-bind".to_owned(),
                files.hosts.display().to_string(),
                SYSTEM_HOSTS_FILE.to_owned(),
                "--ro-bind".to_owned(),
                files.trust.display().to_string(),
                SYSTEM_TRUST_FILE.to_owned(),
                "--".to_owned(),
            ]
        );
    }
}
