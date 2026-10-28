//! The backend registry: which radios this build supports.

use crate::{Backend, RadioError, Result};

#[cfg(feature = "hackrf")]
mod hackrf;
#[cfg(feature = "pluto")]
mod maia;
#[cfg(feature = "pluto")]
mod pluto;

/// Every backend this crate knows, compiled in or not: (name, `isdb-radio`
/// cargo feature enabling it). `isdb-cli` forwards each of these features
/// as its own feature of the same name (its `Cargo.toml`), which the error
/// of [`backend`] says.
const KNOWN: &[(&str, &str)] = &[("hackrf", "hackrf"), ("pluto", "pluto")];

/// The backends compiled into this build.
const COMPILED: &[&dyn Backend] = &[
    #[cfg(feature = "hackrf")]
    &hackrf::HackRf,
    #[cfg(feature = "pluto")]
    &pluto::Pluto,
];

/// The backends compiled into this build.
pub fn backends() -> Vec<&'static dyn Backend> {
    COMPILED.to_vec()
}

/// The compiled-in backend called `name`.
///
/// Fails with [`RadioError::UnknownBackend`] for an unknown name or a
/// backend left out of this build; its message lists the compiled-in
/// backends and the cargo feature enabling each known one.
pub fn backend(name: &str) -> Result<&'static dyn Backend> {
    if let Some(b) = COMPILED.iter().find(|b| b.name() == name) {
        return Ok(*b);
    }
    Err(RadioError::UnknownBackend {
        name: name.to_string(),
        feature: KNOWN.iter().find(|(k, _)| *k == name).map(|&(_, f)| f),
    })
}

/// The message of [`RadioError::UnknownBackend`] for `name`, a known
/// backend enabled by `feature` or (`None`) an unknown one.
pub(crate) fn unknown_backend_message(name: &str, feature: Option<&str>) -> String {
    let compiled = backends();
    let feature_text =
        |f: &str| format!("isdb-radio feature `{f}`, forwarded by isdb-cli feature `{f}`");
    let head = match feature {
        Some(f) => format!(
            "radio backend {name:?} is not compiled in ({})",
            feature_text(f)
        ),
        None => format!("unknown radio backend {name:?}"),
    };
    let list = |it: &mut dyn Iterator<Item = &(&str, &str)>| {
        it.map(|(n, f)| format!("{n} ({})", feature_text(f)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if compiled.is_empty() {
        return format!(
            "{head}; this build has no radio backends; known: {}",
            list(&mut KNOWN.iter())
        );
    }
    let is_compiled = |n: &str| compiled.iter().any(|b| b.name() == n);
    let mut msg = format!(
        "{head}; compiled in: {}",
        list(&mut KNOWN.iter().filter(|(n, _)| is_compiled(n)))
    );
    let missing = list(&mut KNOWN.iter().filter(|(n, _)| !is_compiled(n)));
    if !missing.is_empty() {
        msg += &format!("; not compiled in: {missing}");
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_compiled_backend_is_known_and_found() {
        for b in backends() {
            assert!(KNOWN.iter().any(|(n, _)| *n == b.name()), "{}", b.name());
            assert_eq!(backend(b.name()).unwrap().name(), b.name());
        }
    }

    const HACKRF: &str =
        "hackrf (isdb-radio feature `hackrf`, forwarded by isdb-cli feature `hackrf`)";
    const PLUTO: &str = "pluto (isdb-radio feature `pluto`, forwarded by isdb-cli feature `pluto`)";

    #[test]
    fn unknown_backend_names_the_features() {
        assert!(matches!(
            backend("sdr"),
            Err(RadioError::UnknownBackend { name, feature: None }) if name == "sdr"
        ));
        for (name, f) in KNOWN {
            if backend(name).is_err() {
                assert!(matches!(
                    backend(name),
                    Err(RadioError::UnknownBackend { feature: Some(g), .. }) if g == *f
                ));
            }
        }
        let e = backend("sdr").unwrap_err().to_string();
        assert!(e.starts_with("unknown radio backend \"sdr\"; "), "{e}");
        if backends().is_empty() {
            assert!(
                e.ends_with(&format!(
                    "this build has no radio backends; known: {HACKRF}, {PLUTO}"
                )),
                "{e}"
            );
        }
    }

    #[cfg(not(feature = "hackrf"))]
    #[test]
    fn hackrf_left_out() {
        let e = backend("hackrf").unwrap_err().to_string();
        assert!(
            e.starts_with(
                "radio backend \"hackrf\" is not compiled in (isdb-radio feature `hackrf`, \
                 forwarded by isdb-cli feature `hackrf`)"
            ),
            "{e}"
        );
    }

    #[cfg(not(feature = "pluto"))]
    #[test]
    fn pluto_left_out() {
        let e = backend("pluto").unwrap_err().to_string();
        assert!(
            e.starts_with(
                "radio backend \"pluto\" is not compiled in (isdb-radio feature `pluto`, \
                 forwarded by isdb-cli feature `pluto`)"
            ),
            "{e}"
        );
    }

    #[test]
    fn compiled_and_missing_are_listed_apart() {
        let e = backend("sdr").unwrap_err().to_string();
        match (backend("hackrf").is_ok(), backend("pluto").is_ok()) {
            (true, true) => assert!(
                e.ends_with(&format!("compiled in: {HACKRF}, {PLUTO}")),
                "{e}"
            ),
            (true, false) => assert!(
                e.ends_with(&format!("compiled in: {HACKRF}; not compiled in: {PLUTO}")),
                "{e}"
            ),
            (false, true) => assert!(
                e.ends_with(&format!("compiled in: {PLUTO}; not compiled in: {HACKRF}")),
                "{e}"
            ),
            (false, false) => assert!(e.contains("this build has no radio backends"), "{e}"),
        }
    }
}
