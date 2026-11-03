// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Amazon host names the game client calls with no endpoint setting: the remote-config
//! bucket (S3), the analytics stream (Kinesis) and its credentials (STS) (study note
//! `docs/re/2026-09-18-remote-config-layers.md`), the news service (CMS) with its image host
//! (study note `docs/re/2026-11-02-cms-news-hosts.md`), and the DynamoDB host of the region ping
//! (study note `docs/re/2026-11-02-region-ping.md`). A Blighted server answers these names
//! itself. Our launcher maps only these names to the server, and only for the game process.

use serde::{Deserialize, Serialize};

/// The remote-config buckets of the client. The client picks one by its environment
/// (`FUN_1410376e0`: 3 staging, 4 prod, else dev; character-select note). Our client logs
/// `ags-javelin-remote-config`.
pub const REMOTE_CONFIG_BUCKETS: [&str; 3] = [
    "ags-javelin-remote-config",
    "ags-javelin-staging-remote-config",
    "ags-javelin-dev-remote-config",
];
/// The S3 host of region `us-east-1`; the client puts the bucket in front of it (virtual-host
/// style, `FUN_140853a80`).
pub const S3_GLOBAL_HOST: &str = "s3.amazonaws.com";
/// The parent domain of every AWS service host.
pub const AWS_DOMAIN: &str = "amazonaws.com";
const HOST_SEPARATOR: char = '.';
const KINESIS_SERVICE: &str = "kinesis";
const STS_SERVICE: &str = "sts";
/// The service the region ping calls: `GET /ping` on `dynamodb.<region>.amazonaws.com`, port 80
/// (`FUN_1474bafd0` builds the host).
const DYNAMODB_SERVICE: &str = "dynamodb";
/// The port the client's region pinger calls. The launcher's proxy sends the ping on to the
/// server's [`AwsMirror::ping_port`].
pub const CLIENT_PING_PORT: u16 = 80;
/// The news service (CMS) host: message of the day, news stories and menu tiles. Fixed in the
/// client's default-host table (`FUN_14616ae90`).
pub const CMS_HOST: &str = "d1hkbwzm1bktgo.cloudfront.net";
/// The host of the news images. The CMS documents name it in their image URLs.
pub const CMS_IMAGE_HOST: &str = "client.content-service.amazongames.com";
/// The kinds of host [`is_aws_mirror_host`] accepts, for error messages.
pub const MIRROR_HOST_KINDS: &str =
    "remote-config, Kinesis, STS, region ping, news or news image host";

/// `<bucket>.s3.amazonaws.com`.
pub fn remote_config_host(bucket: &str) -> String {
    format!("{bucket}{HOST_SEPARATOR}{S3_GLOBAL_HOST}")
}

/// Whether `host` (lower case) is the S3 host of one of the [`REMOTE_CONFIG_BUCKETS`].
pub fn is_remote_config_host(host: &str) -> bool {
    REMOTE_CONFIG_BUCKETS
        .iter()
        .any(|bucket| host == remote_config_host(bucket))
}

/// Whether `host` is `<service>.amazonaws.com` or `<service>.<region>.amazonaws.com`.
fn is_service_host(host: &str, service: &str) -> bool {
    let Some(rest) = host
        .strip_prefix(service)
        .and_then(|rest| rest.strip_prefix(HOST_SEPARATOR))
    else {
        return false;
    };
    rest.eq_ignore_ascii_case(AWS_DOMAIN)
        || rest
            .split_once(HOST_SEPARATOR)
            .is_some_and(|(region, domain)| {
                !region.is_empty() && domain.eq_ignore_ascii_case(AWS_DOMAIN)
            })
}

/// Whether `host` (lower case) is a Kinesis host, `kinesis.<region>.amazonaws.com`.
pub fn is_kinesis_host(host: &str) -> bool {
    is_service_host(host, KINESIS_SERVICE)
}

/// Whether `host` (lower case) is an STS host, `sts.amazonaws.com` or
/// `sts.<region>.amazonaws.com`.
pub fn is_sts_host(host: &str) -> bool {
    is_service_host(host, STS_SERVICE)
}

/// `dynamodb.<region>.amazonaws.com`, the host the client pings for `region`.
pub fn dynamodb_host(region: &str) -> String {
    format!("{DYNAMODB_SERVICE}{HOST_SEPARATOR}{region}{HOST_SEPARATOR}{AWS_DOMAIN}")
}

/// Whether `host` (lower case) is a DynamoDB host, the target of the region ping.
pub fn is_dynamodb_host(host: &str) -> bool {
    is_service_host(host, DYNAMODB_SERVICE)
}

/// Whether `host` (lower case) is the news host [`CMS_HOST`] or its image host
/// [`CMS_IMAGE_HOST`].
pub fn is_cms_host(host: &str) -> bool {
    host == CMS_HOST || host == CMS_IMAGE_HOST
}

/// Whether a server may ask the launcher to map `host` to it: the remote-config bucket host, a
/// Kinesis host, an STS host, a DynamoDB host or a news host ([`is_cms_host`]). No other Amazon
/// name is ever mapped.
pub fn is_aws_mirror_host(host: &str) -> bool {
    is_remote_config_host(host)
        || is_kinesis_host(host)
        || is_sts_host(host)
        || is_dynamodb_host(host)
        || is_cms_host(host)
}

/// Part of the launcher info: the Amazon names this server answers, the certificate it shows for
/// them, and the port of its region ping. The names are answered on the launcher API's own HTTPS
/// port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AwsMirror {
    /// Host names, each passing [`is_aws_mirror_host`].
    pub hosts: Vec<String>,
    /// The server's self-signed certificate for exactly these names, as PEM. It is not a CA.
    pub certificate_pem: String,
    /// The server's plain HTTP port of the region ping (`public_ping_port` in `edge.toml`).
    pub ping_port: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_hosts_are_virtual_host_style() {
        assert_eq!(
            remote_config_host("ags-javelin-remote-config"),
            "ags-javelin-remote-config.s3.amazonaws.com"
        );
        for bucket in REMOTE_CONFIG_BUCKETS {
            assert!(is_remote_config_host(&remote_config_host(bucket)));
        }
        assert!(!is_remote_config_host("other-bucket.s3.amazonaws.com"));
        assert!(!is_remote_config_host(S3_GLOBAL_HOST));
    }

    #[test]
    fn service_hosts_are_recognized() {
        assert!(is_kinesis_host("kinesis.us-east-1.amazonaws.com"));
        assert!(is_sts_host("sts.us-east-1.amazonaws.com"));
        assert!(is_sts_host("sts.amazonaws.com"));
        for other in [
            "kinesis.amazonaws.com.evil.net",
            "kinesis..amazonaws.com",
            "kinesisx.us-east-1.amazonaws.com",
            "s3.amazonaws.com",
            "game.example.org",
        ] {
            assert!(!is_kinesis_host(other) && !is_sts_host(other), "{other}");
        }
    }

    #[test]
    fn ping_hosts_are_recognized() {
        assert_eq!(
            dynamodb_host("us-east-1"),
            "dynamodb.us-east-1.amazonaws.com"
        );
        assert!(is_dynamodb_host(&dynamodb_host("ap-southeast-2")));
        for other in [
            "dynamodb.us-east-1.amazonaws.com.evil.net",
            "dynamodbx.us-east-1.amazonaws.com",
            "kinesis.us-east-1.amazonaws.com",
        ] {
            assert!(!is_dynamodb_host(other), "{other}");
        }
    }

    #[test]
    fn news_hosts_are_recognized() {
        assert!(is_cms_host(CMS_HOST));
        assert!(is_cms_host(CMS_IMAGE_HOST));
        for other in [
            "d2c74t4zimux3r.cloudfront.net",
            "x.d1hkbwzm1bktgo.cloudfront.net",
            "client.catalogservice.amazongames.com",
            "content-service.amazongames.com",
        ] {
            assert!(!is_cms_host(other), "{other}");
        }
    }

    #[test]
    fn only_the_listed_services_may_be_mirrored() {
        for host in [
            "ags-javelin-remote-config.s3.amazonaws.com",
            "kinesis.us-east-1.amazonaws.com",
            "sts.us-east-1.amazonaws.com",
            "dynamodb.us-east-1.amazonaws.com",
            CMS_HOST,
            CMS_IMAGE_HOST,
        ] {
            assert!(is_aws_mirror_host(host), "{host}");
        }
        for host in [
            "s3.amazonaws.com",
            "2mfrik7h83.execute-api.us-east-1.amazonaws.com",
            "us-east-1.client.stats-service.amazongames.com",
            "client.entitlementservice.amazongames.com",
            "cloudfront.net",
            "login.example.org",
        ] {
            assert!(!is_aws_mirror_host(host), "{host}");
        }
    }
}
