//! Where the binary output goes, with curl's rule for a terminal: the
//! stream goes to stdout when no file is named, but not onto an
//! interactive terminal, where it would only garble the screen — that is
//! refused with a hint, and `-o -` asks for stdout explicitly (curl's
//! `--output -`).

use std::io::{IsTerminal, Write};
use std::path::Path;

/// The output for `path`: a file, stdout for `-` (forced), or stdout for
/// none — refused when stdout is a terminal.
///
/// # Errors
///
/// The file's creation, or the refusal.
pub fn open(path: Option<&Path>, what: &str) -> anyhow::Result<Box<dyn Write + Send>> {
    match path {
        Some(p) if p.as_os_str() == "-" => Ok(stdout()),
        Some(p) => Ok(Box::new(std::io::BufWriter::new(
            std::fs::File::create(p)
                .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", p.display()))?,
        ))),
        None => {
            refuse_terminal(what)?;
            Ok(stdout())
        }
    }
}

fn stdout() -> Box<dyn Write + Send> {
    Box::new(std::io::BufWriter::new(std::io::stdout()))
}

/// Refuse to write `what` (binary) to stdout when it is a terminal.
///
/// # Errors
///
/// The refusal, naming the ways out.
pub fn refuse_terminal(what: &str) -> anyhow::Result<()> {
    if std::io::stdout().is_terminal() {
        anyhow::bail!(
            "not writing {what} to the terminal (binary output would garble it): \
             give -o FILE, redirect or pipe stdout, or use -o - to write it here anyway"
        );
    }
    Ok(())
}

/// Whether `path` names stdout (`-`).
#[must_use]
pub fn is_stdout(path: Option<&Path>) -> bool {
    path.is_none_or(|p| p.as_os_str() == "-")
}
