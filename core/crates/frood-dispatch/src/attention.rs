//! Attention changed from the console (docs/26 §4, QY1): how urgent each kind of situation is,
//! what an agent looks into before anyone is told, the daily push fuse, due-date reminders,
//! the stall detectors, and each person's own settings. A layer over `frood.yaml`'s
//! `attention`, kept with the console's sources and jobs; what `frood.yaml` sets wins and is
//! changed only there.

use std::collections::BTreeMap;

use frood_config::{
    Attention, AttentionPerson, AttentionRoute, BeforeNotify, Detectors, Reminder, SituationMatch,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::Dispatch;
use crate::connections::{ConnectError, checked, record};

/// The situations the console shows, in its order: proposals by risk, then the rest.
const SITUATIONS: &[(&str, Option<&str>)] = &[
    ("proposal", Some("new")),
    ("proposal", Some("change")),
    ("proposal", Some("conflict")),
    ("proposal", Some("stale_knowledge")),
    ("proposal", Some("entity_merge")),
    ("question", None),
    ("reminder", None),
    ("alert", None),
];

/// The stall detectors (C3): name, what its value is, and the built-in value.
const DETECTORS: &[(&str, &str, &str)] = &[
    ("dispatcher_lag", "duration", "10m"),
    ("queued", "duration", "30m"),
    ("overrun", "duration", "10m"),
    ("failures_in_a_row", "count", "3"),
    ("source_stale", "duration", "48h"),
    ("budget_share", "share", "0.8"),
    ("module_down", "duration", "5m"),
    ("backups_missed", "count", "2"),
];

pub(crate) fn is_unset(attention: &Attention) -> bool {
    *attention == Attention::default()
}

fn matches(when: &SituationMatch, kind: &str, risk: Option<&str>) -> bool {
    when.kind == kind && when.risk.as_deref().is_none_or(|r| Some(r) == risk)
}

/// `frood.yaml`'s attention with the console's, setting by setting: what the file sets wins.
pub(crate) fn merge(file: &Attention, console: &Attention) -> Attention {
    let mut out = file.clone();
    // The first rule that matches a situation decides it, and the file's come first.
    out.routes.extend(console.routes.iter().cloned());
    out.before_notify
        .extend(console.before_notify.iter().cloned());
    out.max_push_per_day = file.max_push_per_day.or(console.max_push_per_day);
    if file.reminders.is_empty() {
        out.reminders = console.reminders.clone();
    }
    out.detectors = merge_detectors(&file.detectors, &console.detectors);
    for (email, mine) in &console.people {
        let person = out.people.entry(email.clone()).or_default();
        person.timezone = person.timezone.take().or_else(|| mine.timezone.clone());
        person.quiet_hours = person
            .quiet_hours
            .take()
            .or_else(|| mine.quiet_hours.clone());
        person.max_push_per_day = person.max_push_per_day.or(mine.max_push_per_day);
        person.reminders = person.reminders.take().or_else(|| mine.reminders.clone());
    }
    out
}

fn detector_values(detectors: &Detectors) -> Map<String, Value> {
    match serde_json::to_value(detectors) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn merge_detectors(file: &Detectors, console: &Detectors) -> Detectors {
    let mut values = detector_values(console);
    values.extend(detector_values(file));
    serde_json::from_value(Value::Object(values)).unwrap_or_else(|_| file.clone())
}

/// The agent job that looks into a situation first, and how long people wait for it at most.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Before {
    pub run: String,
    pub max_wait: String,
}

impl From<&BeforeNotify> for Before {
    fn from(rule: &BeforeNotify) -> Self {
        Self {
            run: rule.run.clone(),
            max_wait: rule.max_wait.clone(),
        }
    }
}

/// One kind of situation, as the console shows it for changing.
#[derive(Debug, Clone, Serialize)]
pub struct SituationSetting {
    pub kind: String,
    pub risk: Option<String>,
    /// now (push), digest or silent (Inbox only).
    pub urgency: String,
    pub default_urgency: String,
    /// `config` (frood.yaml changes it, and only there), `console` or `default`.
    pub urgency_from: &'static str,
    pub before: Option<Before>,
    pub before_from: &'static str,
}

/// A setting: what it is now, what it is without a change, and who set it.
#[derive(Debug, Clone, Serialize)]
pub struct Setting {
    pub value: Value,
    pub default: Value,
    /// `config`, `console` or `default`.
    pub from: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct DetectorSetting {
    pub name: &'static str,
    /// duration (like 10m), count or share (0 to 1).
    pub kind: &'static str,
    #[serde(flatten)]
    pub setting: Setting,
}

/// The installation's attention, as the console changes it.
#[derive(Debug, Clone, Serialize)]
pub struct AttentionSettings {
    pub situations: Vec<SituationSetting>,
    pub max_push_per_day: Setting,
    pub reminders: Setting,
    pub detectors: Vec<DetectorSetting>,
    /// Agent jobs that can look into a situation before anyone is told.
    pub investigators: Vec<String>,
}

/// The console's attention settings, whole: what is left out is as `frood.yaml` and the
/// built-in values make it. A setting `frood.yaml` makes may be sent back as it is.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AttentionChange {
    #[serde(default)]
    pub situations: Vec<SituationChange>,
    #[serde(default)]
    pub max_push_per_day: Option<u32>,
    /// None or empty: no reminders.
    #[serde(default)]
    pub reminders: Option<Vec<Reminder>>,
    /// By name; a value left out or null is the built-in one.
    #[serde(default)]
    pub detectors: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SituationChange {
    pub kind: String,
    #[serde(default)]
    pub risk: Option<String>,
    pub urgency: String,
    #[serde(default)]
    pub before: Option<Before>,
}

/// A person's own attention settings, as the console shows them.
#[derive(Debug, Clone, Serialize)]
pub struct PersonalSettings {
    pub email: String,
    pub timezone: Setting,
    pub quiet_hours: Setting,
    pub max_push_per_day: Setting,
    /// Null: the installation's reminders (the default).
    pub reminders: Setting,
}

/// A person's own settings, whole; a setting `frood.yaml` makes may be sent back as it is.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PersonalChange {
    #[serde(default)]
    pub timezone: Option<String>,
    /// Like "23:00-08:00".
    #[serde(default)]
    pub quiet_hours: Option<String>,
    #[serde(default)]
    pub max_push_per_day: Option<u32>,
    /// None: the installation's reminders; empty: none.
    #[serde(default)]
    pub reminders: Option<Vec<Reminder>>,
}

fn situation_name(kind: &str, risk: Option<&str>) -> String {
    match risk {
        Some(risk) => format!("{kind} ({risk})"),
        None => kind.to_owned(),
    }
}

fn from(in_file: bool, in_console: bool) -> &'static str {
    if in_file {
        "config"
    } else if in_console {
        "console"
    } else {
        "default"
    }
}

fn json_of<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// A detector's value as its kind has it: a duration stays text, a count or share a number.
fn detector_value(kind: &str, name: &str, value: &Value) -> Result<Value, ConnectError> {
    let text = match value {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    let bad = || ConnectError::Invalid(format!("{name}: {value} is not a {kind}"));
    Ok(match kind {
        "duration" if !text.is_empty() => json!(text),
        "count" => json!(text.parse::<u32>().map_err(|_| bad())?),
        "share" => json!(text.parse::<f64>().map_err(|_| bad())?),
        _ => return Err(bad()),
    })
}

impl Dispatch {
    /// Attention from `frood.yaml` and the console's layer, made again.
    pub async fn reload_attention(&self) -> sqlx::Result<()> {
        let (_, overlay) = self.overlay().await?;
        self.ctx
            .attention
            .set(merge(&self.ctx.config_attention, &overlay.attention));
        Ok(())
    }

    /// The installation's attention, with what set each part.
    pub async fn attention_settings(&self) -> sqlx::Result<AttentionSettings> {
        let (_, overlay) = self.overlay().await?;
        let file = &*self.ctx.config_attention;
        let console = &overlay.attention;
        let now = merge(file, console);
        let situations = SITUATIONS
            .iter()
            .map(|&(kind, risk)| {
                let in_file = file.routes.iter().any(|r| matches(&r.when, kind, risk));
                let in_console = console.routes.iter().any(|r| matches(&r.when, kind, risk));
                let before_in_file = file
                    .before_notify
                    .iter()
                    .any(|r| matches(&r.when, kind, risk));
                let before_in_console = console
                    .before_notify
                    .iter()
                    .any(|r| matches(&r.when, kind, risk));
                SituationSetting {
                    kind: kind.to_owned(),
                    risk: risk.map(str::to_owned),
                    urgency: crate::inbox::urgency(&now, kind, risk),
                    default_urgency: crate::inbox::urgency(&Attention::default(), kind, risk),
                    urgency_from: from(in_file, in_console),
                    before: now
                        .before_notify
                        .iter()
                        .find(|r| matches(&r.when, kind, risk))
                        .map(Before::from),
                    before_from: from(before_in_file, before_in_console),
                }
            })
            .collect();
        let (file_detectors, console_detectors, now_detectors) = (
            detector_values(&file.detectors),
            detector_values(&console.detectors),
            detector_values(&now.detectors),
        );
        let detectors = DETECTORS
            .iter()
            .map(|&(name, kind, default)| DetectorSetting {
                name,
                kind,
                setting: Setting {
                    value: now_detectors.get(name).cloned().unwrap_or(Value::Null),
                    default: detector_value(kind, name, &json!(default)).unwrap_or(Value::Null),
                    from: from(
                        file_detectors.contains_key(name),
                        console_detectors.contains_key(name),
                    ),
                },
            })
            .collect();
        let mut investigators = vec!["investigate".to_owned()];
        if let Some(base) = &self.job_base {
            let config = base.merged(&overlay);
            investigators.extend(
                config
                    .jobs
                    .iter()
                    .filter(|(name, job)| {
                        *name != "investigate" && job.uses.as_deref() == Some("agents.execute")
                    })
                    .map(|(name, _)| name.clone()),
            );
        }
        Ok(AttentionSettings {
            situations,
            max_push_per_day: Setting {
                value: json_of(&now.max_push_per_day),
                default: json!(crate::inbox::DEFAULT_MAX_PUSH_PER_DAY),
                from: from(
                    file.max_push_per_day.is_some(),
                    console.max_push_per_day.is_some(),
                ),
            },
            reminders: Setting {
                value: json_of(&now.reminders),
                default: json!([]),
                from: from(!file.reminders.is_empty(), !console.reminders.is_empty()),
            },
            detectors,
            investigators,
        })
    }

    /// Changes the installation's attention from the console, checked as `frood.yaml` would
    /// be. Returns the revision.
    pub async fn change_attention(
        &self,
        actor: &str,
        change: AttentionChange,
    ) -> Result<i64, ConnectError> {
        let base = self
            .job_base
            .clone()
            .ok_or_else(|| ConnectError::Invalid("attention cannot be changed here".to_owned()))?;
        let file = &*self.ctx.config_attention;
        let (_, mut overlay) = self.overlay().await?;
        let mut layer = Attention {
            people: overlay.attention.people.clone(),
            ..Attention::default()
        };
        for situation in change.situations {
            let (kind, risk) = (situation.kind.as_str(), situation.risk.as_deref());
            let name = situation_name(kind, risk);
            if !SITUATIONS.contains(&(kind, risk)) {
                return Err(ConnectError::Invalid(format!(
                    "there is no situation {name}"
                )));
            }
            let when = SituationMatch {
                kind: kind.to_owned(),
                risk: risk.map(str::to_owned),
            };
            match file.routes.iter().find(|r| matches(&r.when, kind, risk)) {
                Some(route) if route.urgency != situation.urgency => {
                    return Err(ConnectError::InConfig(format!("how urgent {name} is")));
                }
                Some(_) => {}
                None if situation.urgency
                    == crate::inbox::urgency(&Attention::default(), kind, risk) => {}
                None => layer.routes.push(AttentionRoute {
                    when: when.clone(),
                    urgency: situation.urgency,
                }),
            }
            match file
                .before_notify
                .iter()
                .find(|r| matches(&r.when, kind, risk))
            {
                Some(rule) if situation.before.as_ref() != Some(&Before::from(rule)) => {
                    return Err(ConnectError::InConfig(format!(
                        "what is looked into before {name}"
                    )));
                }
                Some(_) => {}
                None => {
                    if let Some(before) = situation.before {
                        layer.before_notify.push(BeforeNotify {
                            when,
                            run: before.run.trim().to_owned(),
                            max_wait: before.max_wait.trim().to_owned(),
                        });
                    }
                }
            }
        }
        match (file.max_push_per_day, change.max_push_per_day) {
            (Some(set), Some(wanted)) if set != wanted => {
                return Err(ConnectError::InConfig(
                    "attention.max_push_per_day".to_owned(),
                ));
            }
            (Some(_), _) => {}
            (None, wanted) => layer.max_push_per_day = wanted,
        }
        let reminders = change.reminders.unwrap_or_default();
        if file.reminders.is_empty() {
            layer.reminders = reminders;
        } else if !reminders.is_empty() && reminders != file.reminders {
            return Err(ConnectError::InConfig("attention.reminders".to_owned()));
        }
        let file_detectors = detector_values(&file.detectors);
        let mut detectors = Map::new();
        for (name, value) in change.detectors {
            let Some(&(name, kind, _)) = DETECTORS.iter().find(|(n, _, _)| *n == name) else {
                return Err(ConnectError::Invalid(format!(
                    "there is no detector {name}"
                )));
            };
            if value.is_null() || value == json!("") {
                continue;
            }
            let value = detector_value(kind, name, &value)?;
            match file_detectors.get(name) {
                Some(set) if *set != value => {
                    return Err(ConnectError::InConfig(format!(
                        "attention.detectors.{name}"
                    )));
                }
                Some(_) => {}
                None => {
                    detectors.insert(name.to_owned(), value);
                }
            }
        }
        layer.detectors = serde_json::from_value(Value::Object(detectors))
            .map_err(|e| ConnectError::Invalid(e.to_string()))?;
        overlay.attention = layer;
        checked(&base.merged(&overlay))?;
        let mut tx = self.ctx.pool.begin().await?;
        let revision = record(&mut tx, actor, &overlay, "changed attention").await?;
        tx.commit().await?;
        self.reload_attention().await?;
        Ok(revision)
    }

    /// A person's own attention settings.
    pub async fn personal_settings(&self, email: &str) -> sqlx::Result<PersonalSettings> {
        let (_, overlay) = self.overlay().await?;
        let file = &*self.ctx.config_attention;
        let now = merge(file, &overlay.attention);
        let none = AttentionPerson::default();
        let in_file = file.people.get(email).unwrap_or(&none);
        let in_console = overlay.attention.people.get(email).unwrap_or(&none);
        let mine = now.people.get(email).unwrap_or(&none);
        Ok(PersonalSettings {
            email: email.to_owned(),
            timezone: Setting {
                value: json_of(&mine.timezone),
                default: Value::Null,
                from: from(in_file.timezone.is_some(), in_console.timezone.is_some()),
            },
            quiet_hours: Setting {
                value: json_of(&mine.quiet_hours),
                default: Value::Null,
                from: from(
                    in_file.quiet_hours.is_some(),
                    in_console.quiet_hours.is_some(),
                ),
            },
            max_push_per_day: Setting {
                value: json_of(&mine.max_push_per_day),
                default: json!(
                    now.max_push_per_day
                        .unwrap_or(crate::inbox::DEFAULT_MAX_PUSH_PER_DAY)
                ),
                from: from(
                    in_file.max_push_per_day.is_some(),
                    in_console.max_push_per_day.is_some(),
                ),
            },
            reminders: Setting {
                value: json_of(&mine.reminders),
                default: json_of(&now.reminders),
                from: from(in_file.reminders.is_some(), in_console.reminders.is_some()),
            },
        })
    }

    /// Changes a person's own attention settings. Returns the revision.
    pub async fn change_personal(
        &self,
        actor: &str,
        email: &str,
        change: PersonalChange,
    ) -> Result<i64, ConnectError> {
        let base = self.job_base.clone().ok_or_else(|| {
            ConnectError::Invalid("these settings cannot be changed here".to_owned())
        })?;
        let none = AttentionPerson::default();
        let file = self.ctx.config_attention.people.get(email).unwrap_or(&none);
        let (_, mut overlay) = self.overlay().await?;
        let text = |v: Option<String>| v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
        let place = |field: &str| format!("attention.people.{email}.{field}");
        let mut mine = AttentionPerson::default();
        match (&file.timezone, text(change.timezone)) {
            (Some(set), Some(wanted)) if *set != wanted => {
                return Err(ConnectError::InConfig(place("timezone")));
            }
            (Some(_), _) => {}
            (None, wanted) => mine.timezone = wanted,
        }
        match (&file.quiet_hours, text(change.quiet_hours)) {
            (Some(set), Some(wanted)) if *set != wanted => {
                return Err(ConnectError::InConfig(place("quiet_hours")));
            }
            (Some(_), _) => {}
            (None, wanted) => mine.quiet_hours = wanted,
        }
        match (file.max_push_per_day, change.max_push_per_day) {
            (Some(set), Some(wanted)) if set != wanted => {
                return Err(ConnectError::InConfig(place("max_push_per_day")));
            }
            (Some(_), _) => {}
            (None, wanted) => mine.max_push_per_day = wanted,
        }
        match (&file.reminders, change.reminders) {
            (Some(set), Some(wanted)) if *set != wanted => {
                return Err(ConnectError::InConfig(place("reminders")));
            }
            (Some(_), _) => {}
            (None, wanted) => mine.reminders = wanted,
        }
        if mine == none {
            overlay.attention.people.remove(email);
        } else {
            overlay.attention.people.insert(email.to_owned(), mine);
        }
        checked(&base.merged(&overlay))?;
        let mut tx = self.ctx.pool.begin().await?;
        let revision = record(
            &mut tx,
            actor,
            &overlay,
            &format!("{email} changed their settings"),
        )
        .await?;
        tx.commit().await?;
        self.reload_attention().await?;
        Ok(revision)
    }

    /// The channel modules people can link accounts of (`notify.send@1`).
    pub fn channels(&self) -> Vec<String> {
        self.ctx.modules.providers("notify.send")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(kind: &str, risk: Option<&str>, urgency: &str) -> AttentionRoute {
        AttentionRoute {
            when: SituationMatch {
                kind: kind.to_owned(),
                risk: risk.map(str::to_owned),
            },
            urgency: urgency.to_owned(),
        }
    }

    #[test]
    fn the_file_wins_setting_by_setting() {
        let mut file = Attention::default();
        file.routes.push(route("proposal", None, "silent"));
        file.max_push_per_day = Some(10);
        file.detectors.queued = Some("1h".into());
        file.people.insert(
            "a@example.com".into(),
            AttentionPerson {
                timezone: Some("Europe/Amsterdam".into()),
                ..Default::default()
            },
        );
        let mut console = Attention::default();
        console.routes.push(route("proposal", Some("new"), "now"));
        console.routes.push(route("question", None, "digest"));
        console.max_push_per_day = Some(50);
        console.detectors.queued = Some("5m".into());
        console.detectors.module_down = Some("1m".into());
        console.people.insert(
            "a@example.com".into(),
            AttentionPerson {
                timezone: Some("Europe/Moscow".into()),
                quiet_hours: Some("23:00-08:00".into()),
                ..Default::default()
            },
        );
        let now = merge(&file, &console);
        assert_eq!(
            crate::inbox::urgency(&now, "proposal", Some("new")),
            "silent"
        );
        assert_eq!(crate::inbox::urgency(&now, "question", None), "digest");
        assert_eq!(now.max_push_per_day, Some(10));
        assert_eq!(now.detectors.queued.as_deref(), Some("1h"));
        assert_eq!(now.detectors.module_down.as_deref(), Some("1m"));
        let person = &now.people["a@example.com"];
        assert_eq!(person.timezone.as_deref(), Some("Europe/Amsterdam"));
        assert_eq!(person.quiet_hours.as_deref(), Some("23:00-08:00"));
    }

    #[test]
    fn detector_values_follow_their_kind() {
        assert_eq!(detector_value("count", "x", &json!("4")).unwrap(), json!(4));
        assert_eq!(
            detector_value("share", "x", &json!(0.5)).unwrap(),
            json!(0.5)
        );
        assert_eq!(
            detector_value("duration", "x", &json!(" 2h ")).unwrap(),
            json!("2h")
        );
        assert!(detector_value("count", "x", &json!("many")).is_err());
    }
}
