use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::ConfigError;

/// Secret values available to `${secret:NAME}`: the installation's secrets file first, then the
/// process environment under the same name.
#[derive(Debug, Default, Clone)]
pub struct Secrets {
    file: BTreeMap<String, String>,
}

impl Secrets {
    /// Reads a `KEY=VALUE` file; a missing file means "no file secrets", not an error.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Self {
                file: parse_env_file(&text),
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            file: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    pub fn get(&self, name: &str) -> Option<String> {
        self.file
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
    }
}

fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            (key.trim().to_owned(), value.to_owned())
        })
        .collect()
}

/// Replaces `${secret:NAME}`, `${env:NAME}` and `${env:NAME:-default}` in every string of the
/// document. Returns the names of the secrets that were used.
pub(crate) fn interpolate(
    value: &mut serde_yaml_ng::Value,
    secrets: &Secrets,
) -> Result<BTreeSet<String>, ConfigError> {
    let mut used = BTreeSet::new();
    let mut problems = Vec::new();
    walk(value, secrets, &mut used, &mut problems);
    if problems.is_empty() {
        Ok(used)
    } else {
        Err(ConfigError::Invalid(problems))
    }
}

fn walk(
    value: &mut serde_yaml_ng::Value,
    secrets: &Secrets,
    used: &mut BTreeSet<String>,
    problems: &mut Vec<String>,
) {
    use serde_yaml_ng::Value;
    match value {
        Value::String(s) if s.contains("${") => match substitute(s, secrets, used) {
            Ok(replaced) => *s = replaced,
            Err(problem) => problems.push(problem),
        },
        Value::Sequence(items) => items
            .iter_mut()
            .for_each(|v| walk(v, secrets, used, problems)),
        Value::Mapping(map) => map
            .iter_mut()
            .for_each(|(_, v)| walk(v, secrets, used, problems)),
        Value::Tagged(tagged) => walk(&mut tagged.value, secrets, used, problems),
        _ => {}
    }
}

fn substitute(
    input: &str,
    secrets: &Secrets,
    used: &mut BTreeSet<String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("unclosed reference in {input:?}"))?;
        let reference = &after[..end];
        if is_run_variable(reference) {
            // `${source}` and the like are filled per run by the dispatcher.
            out.push_str(&rest[start..start + 2 + end + 1]);
        } else {
            out.push_str(&resolve(reference, secrets, used)?);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A run variable (`${source}`): a plain name, no `kind:` prefix. Only these exist.
fn is_run_variable(reference: &str) -> bool {
    matches!(reference, "source")
}

fn resolve(
    reference: &str,
    secrets: &Secrets,
    used: &mut BTreeSet<String>,
) -> Result<String, String> {
    let (kind, spec) = reference
        .split_once(':')
        .ok_or_else(|| format!("reference ${{{reference}}} must be secret:NAME or env:NAME"))?;
    match kind {
        "secret" => {
            used.insert(spec.to_owned());
            secrets
                .get(spec)
                .ok_or_else(|| format!("secret {spec} is not set (secrets file or environment)"))
        }
        "env" => {
            let (name, default) = match spec.split_once(":-") {
                Some((name, default)) => (name, Some(default)),
                None => (spec, None),
            };
            std::env::var(name)
                .ok()
                .or_else(|| default.map(str::to_owned))
                .ok_or_else(|| format!("environment variable {name} is not set and has no default"))
        }
        other => Err(format!(
            "unknown reference kind {other:?} in ${{{reference}}}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_parsing_skips_comments_and_strips_quotes() {
        let map = parse_env_file("# comment\nA=1\n\nB = \"two words\"\nnot a pair\n");
        assert_eq!(map.get("A").map(String::as_str), Some("1"));
        assert_eq!(map.get("B").map(String::as_str), Some("two words"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn substitutes_secrets_and_env_defaults() {
        let secrets = Secrets::from_pairs([("PW", "s3cret")]);
        let mut used = BTreeSet::new();
        let out = substitute(
            "postgres://u:${secret:PW}@${env:FROOD_TEST_UNSET_HOST:-localhost}/db",
            &secrets,
            &mut used,
        )
        .unwrap();
        assert_eq!(out, "postgres://u:s3cret@localhost/db");
        assert!(used.contains("PW"));
    }

    #[test]
    fn reports_missing_secret_and_unknown_kind() {
        let secrets = Secrets::default();
        let mut used = BTreeSet::new();
        assert!(
            substitute("${secret:FROOD_TEST_MISSING}", &secrets, &mut used)
                .unwrap_err()
                .contains("FROOD_TEST_MISSING")
        );
        assert!(
            substitute("${vault:x}", &secrets, &mut used)
                .unwrap_err()
                .contains("unknown reference kind")
        );
        assert!(
            substitute("${secret:x", &secrets, &mut used)
                .unwrap_err()
                .contains("unclosed")
        );
    }
}
