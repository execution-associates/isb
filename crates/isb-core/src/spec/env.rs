//! A service's `environment`: plain values and secret variables.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{OnChange, SecretAs};
use crate::flex;

/// A service's environment: plain values, and variables whose value is a
/// top-level secret (`KEY: {secret: NAME}`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Environment {
    /// `KEY: VALUE`: instance config (`environment.KEY`).
    pub vars: BTreeMap<String, String>,
    /// `KEY: {secret: NAME}`: variable to top-level secret key.
    pub secrets: BTreeMap<String, String>,
    /// `KEY: {secret: NAME, as: file}`: variable to top-level secret key.
    /// The value is the file [`Environment::file_path`] and `KEY_FILE` holds
    /// that path; neither the value nor `KEY` is instance config.
    pub files: BTreeMap<String, String>,
    /// `KEY: {secret: NAME, on_change: ...}`: the variable's own setting.
    pub on_change: BTreeMap<String, OnChange>,
}

impl Environment {
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty() && self.secrets.is_empty() && self.files.is_empty()
    }

    /// Where an `as: file` secret's value is written.
    pub fn file_path(key: &str) -> String {
        format!("/run/secrets/{key}")
    }

    /// The `KEY_FILE` variables of `as: file` secrets, with their paths.
    pub fn file_vars(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.files
            .iter()
            .map(|(var, key)| (format!("{var}_FILE"), Self::file_path(key)))
    }
}

/// The plain values, so `spec.env` reads as the map it mostly is.
impl std::ops::Deref for Environment {
    type Target = BTreeMap<String, String>;
    fn deref(&self) -> &Self::Target {
        &self.vars
    }
}

impl std::ops::DerefMut for Environment {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.vars
    }
}

impl<'a> IntoIterator for &'a Environment {
    type Item = (&'a String, &'a String);
    type IntoIter = std::collections::btree_map::Iter<'a, String, String>;
    fn into_iter(self) -> Self::IntoIter {
        self.vars.iter()
    }
}

impl From<BTreeMap<String, String>> for Environment {
    fn from(vars: BTreeMap<String, String>) -> Self {
        Environment {
            vars,
            secrets: BTreeMap::new(),
            files: BTreeMap::new(),
            on_change: BTreeMap::new(),
        }
    }
}

impl Serialize for Environment {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        let mut keys: Vec<&String> = self
            .vars
            .keys()
            .chain(self.secrets.keys())
            .chain(self.files.keys())
            .collect();
        keys.sort();
        keys.dedup();
        for k in keys {
            let (sec, file) = match (self.vars.get(k), self.secrets.get(k), self.files.get(k)) {
                (Some(v), _, _) => {
                    m.serialize_entry(k, v)?;
                    continue;
                }
                (None, Some(sec), _) => (sec, false),
                (None, None, Some(sec)) => (sec, true),
                (None, None, None) => continue,
            };
            let mut v = BTreeMap::from([("secret", sec.as_str())]);
            if let Some(o) = self.on_change.get(k) {
                v.insert("on_change", o.as_str());
            }
            // Only when set, so an `as: env` variable serializes (and
            // hashes) exactly as before the field existed.
            if file {
                v.insert("as", "file");
            }
            m.serialize_entry(k, &v)?
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for Environment {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let mut env = Environment::default();
        match flex::EnvMapOrList::deserialize(d)? {
            flex::EnvMapOrList::Map(m) => {
                for (k, v) in m {
                    match v {
                        flex::EnvValue::Scalar(v) => {
                            env.vars.insert(k, v.into_string());
                        }
                        flex::EnvValue::Secret { secret, .. } if secret.is_empty() => {
                            return Err(D::Error::custom(format!(
                                "environment {k}: secret needs a top-level secret's name"
                            )));
                        }
                        flex::EnvValue::Secret {
                            secret,
                            on_change,
                            delivery,
                        } => {
                            if let Some(o) = on_change {
                                env.on_change.insert(k.clone(), o);
                            }
                            match delivery.unwrap_or_default() {
                                SecretAs::Env => env.secrets.insert(k, secret),
                                SecretAs::File => env.files.insert(k, secret),
                            };
                        }
                    }
                }
            }
            flex::EnvMapOrList::List(l) => {
                for item in l {
                    let Some((k, v)) = item.split_once('=') else {
                        return Err(D::Error::custom(format!(
                            "environment entry {item:?} has no value: write {item}=VALUE"
                        )));
                    };
                    env.vars.insert(k.to_string(), v.to_string());
                }
            }
        }
        Ok(env)
    }
}
