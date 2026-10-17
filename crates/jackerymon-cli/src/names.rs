//! Nicknames for units, in a `names` file beside the keys: one
//! `<serial> <name>` per line, the name being the rest of the line.
//! Blank lines and lines starting with `#` are skipped.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;

/// The file's name in a key directory.
const NAMES_FILE: &str = "names";
const COMMENT: char = '#';

/// Each unit's name, by serial, in file order.
#[derive(Debug, Default)]
pub(crate) struct Names(Vec<(String, String)>);

impl Names {
    /// The names in `directory`; none when it has no names file.
    pub(crate) fn load(directory: &Path) -> Result<Self> {
        let path = directory.join(NAMES_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| path.display().to_string()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn parse(text: &str) -> Result<Self> {
        let mut names: Vec<(String, String)> = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(COMMENT) {
                continue;
            }
            let number = index + 1;
            let Some((serial, name)) = line.split_once(char::is_whitespace) else {
                bail!("line {number}: no name after {line}");
            };
            let name = name.trim();
            if names.iter().any(|(other, _)| other == serial) {
                bail!("line {number}: unit {serial} named again");
            }
            if names.iter().any(|(_, other)| other == name) {
                bail!("line {number}: name {name:?} given twice");
            }
            names.push((serial.to_owned(), name.to_owned()));
        }
        Ok(Self(names))
    }

    /// The name of the unit `serial`, if it has one.
    pub(crate) fn name(&self, serial: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(other, _)| other == serial)
            .map(|(_, name)| name.as_str())
    }

    /// The serial of the unit called `name`, if any is.
    pub(crate) fn serial(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(_, other)| other == name)
            .map(|(serial, _)| serial.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::Names;

    #[test]
    fn reads_serials_and_names() {
        let names =
            Names::parse("# the units\n\n855125071405677 garage\n855225011707296   back shed  \n")
                .expect("names");
        assert_eq!(names.name("855125071405677"), Some("garage"));
        assert_eq!(names.name("855225011707296"), Some("back shed"));
        assert_eq!(names.serial("back shed"), Some("855225011707296"));
        assert_eq!(names.name("1"), None);
    }

    #[test]
    fn refuses_a_missing_or_repeated_name() {
        for text in ["123", "1 a\n1 b", "1 a\n2 a"] {
            assert!(Names::parse(text).is_err(), "{text:?}");
        }
    }
}
