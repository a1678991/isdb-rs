//! Device selection strings: `NAME[:key=value[,key=value...]]`.

use anyhow::{Error, Result, bail};
use std::fmt;
use std::str::FromStr;

/// A device selection: a backend name and its device arguments, written
/// `NAME[:key=value[,key=value...]]`, e.g. `sdr` or `sdr:serial=0000abcd`.
///
/// Parsing checks only the syntax (non-empty name and keys, no repeated
/// key); the backend checks the argument names and values. The syntax
/// cannot express every string: the name cannot contain `:` (it is
/// `[A-Za-z0-9_-]+`), keys cannot contain `,` or `=`, values cannot
/// contain `,` (they may contain `=` and `:`, e.g. `uri=ip:192.168.2.1`),
/// and whitespace around names, keys and values is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSpec {
    /// Backend name ([`crate::backend`]).
    pub backend: String,
    /// Device arguments in the order given.
    pub args: Vec<(String, String)>,
}

impl DeviceSpec {
    /// The value of argument `key`, if given.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.args
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Parse `key=value` (key non-empty, without surrounding whitespace; the
/// value may be empty and may contain `=`).
pub fn parse_key_value(s: &str) -> Result<(String, String)> {
    let Some((k, v)) = s.split_once('=') else {
        bail!("{s:?}: want NAME=VALUE");
    };
    let k = k.trim();
    if k.is_empty() {
        bail!("{s:?}: empty name before '='");
    }
    Ok((k.to_string(), v.trim().to_string()))
}

impl FromStr for DeviceSpec {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let (name, rest) = match s.split_once(':') {
            Some((n, r)) => (n.trim(), Some(r)),
            None => (s.trim(), None),
        };
        if name.is_empty() {
            bail!("device {s:?}: empty backend name (want NAME[:key=value,...])");
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("device {s:?}: backend name {name:?} is not [A-Za-z0-9_-]+");
        }
        let mut args: Vec<(String, String)> = Vec::new();
        if let Some(rest) = rest {
            if rest.trim().is_empty() {
                bail!("device {s:?}: nothing after ':' (want NAME:key=value,...)");
            }
            for item in rest.split(',') {
                let (k, v) =
                    parse_key_value(item).map_err(|e| anyhow::anyhow!("device {s:?}: {e}"))?;
                if args.iter().any(|(a, _)| *a == k) {
                    bail!("device {s:?}: argument {k:?} given twice");
                }
                args.push((k, v));
            }
        }
        Ok(Self {
            backend: name.to_string(),
            args,
        })
    }
}

impl fmt::Display for DeviceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.backend)?;
        for (i, (k, v)) in self.args.iter().enumerate() {
            write!(f, "{}{k}={v}", if i == 0 { ':' } else { ',' })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<DeviceSpec> {
        s.parse()
    }

    #[test]
    fn name_only_and_with_args() {
        let d = p("sdr").unwrap();
        assert_eq!((d.backend.as_str(), d.args.len()), ("sdr", 0));
        let d = p("sdr:serial=0000abcd").unwrap();
        assert_eq!(d.backend, "sdr");
        assert_eq!(d.get("serial"), Some("0000abcd"));
        assert_eq!(d.get("other"), None);
        let d = p("pluto:uri=ip:192.168.2.1,buffer=65536").unwrap();
        assert_eq!(
            d.args,
            vec![
                ("uri".to_string(), "ip:192.168.2.1".to_string()),
                ("buffer".to_string(), "65536".to_string())
            ]
        );
        // Whitespace around names and values is ignored; values may be
        // empty or contain '='.
        let d = p(" x : a = 1 , b= , c=d=e").unwrap();
        assert_eq!(d.backend, "x");
        assert_eq!(d.get("a"), Some("1"));
        assert_eq!(d.get("b"), Some(""));
        assert_eq!(d.get("c"), Some("d=e"));
    }

    #[test]
    fn syntax_errors() {
        for bad in [
            "",
            ":serial=1",
            "sdr:",
            "sdr: ",
            "sdr:serial",
            "sdr:=1",
            "sdr:serial=1,",
            "sdr:serial=1,serial=2",
            "hack rf",
            "sdr/1",
        ] {
            assert!(p(bad).is_err(), "{bad:?}");
        }
        let e = p("sdr:serial").unwrap_err().to_string();
        assert!(e.contains("NAME=VALUE"), "{e}");
    }

    #[test]
    fn display_round_trips() {
        for s in ["sdr", "sdr:serial=0000abcd", "x:a=1,b=,c=d=e"] {
            let d = p(s).unwrap();
            assert_eq!(d.to_string(), s);
            assert_eq!(p(&d.to_string()).unwrap(), d);
        }
    }

    #[test]
    fn key_values() {
        assert_eq!(
            parse_key_value("gain=20").unwrap(),
            ("gain".into(), "20".into())
        );
        assert!(parse_key_value("gain").is_err());
        assert!(parse_key_value("=20").is_err());
    }
}
