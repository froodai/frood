//! Spheres of life (docs/26 §3): built-in ones named in the bot's language, an installation's
//! own by the names its config gives them (`GET /v1/spheres`).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::lang::{SPHERES, Words};
use crate::notify::esc;

/// The installation's spheres in their order, with the names of its own ones.
#[derive(Clone, Default)]
pub(crate) struct Spheres {
    order: Vec<String>,
    names: BTreeMap<String, String>,
}

impl Spheres {
    /// From `GET /v1/spheres`.
    pub(crate) fn from_api(list: &Value) -> Self {
        let mut spheres = Self::default();
        for sphere in list.as_array().into_iter().flatten() {
            let Some(id) = sphere["id"].as_str() else {
                continue;
            };
            spheres.order.push(id.to_owned());
            if let Some(name) = sphere["name"].as_str() {
                spheres.names.insert(id.to_owned(), name.to_owned());
            }
        }
        spheres
    }

    pub(crate) fn name(&self, id: &str, w: &Words) -> String {
        if let Some(name) = self.names.get(id) {
            return name.clone();
        }
        SPHERES
            .iter()
            .position(|s| *s == id)
            .map_or_else(|| id.to_owned(), |i| w.spheres[i].to_owned())
    }

    /// A sign and the name: «💼 Работа · бизнес».
    pub(crate) fn label(&self, id: &str, w: &Words) -> String {
        let sign = match id {
            "tech" => "🛠",
            "business" => "💼",
            "personal" => "🏠",
            _ => "🏷",
        };
        format!("{sign} {}", self.name(id, w))
    }

    fn rank(&self, sphere: Option<&str>) -> usize {
        match sphere {
            None => usize::MAX,
            Some(id) => self
                .order
                .iter()
                .position(|o| o == id)
                .or_else(|| {
                    self.order
                        .is_empty()
                        .then(|| SPHERES.iter().position(|s| *s == id))
                        .flatten()
                })
                .unwrap_or(usize::MAX - 1),
        }
    }

    /// Bulleted lines, under a heading for each sphere when they come from more than one: the
    /// installation's order, then any others, then those without a sphere.
    pub(crate) fn grouped(
        &self,
        mut items: Vec<(Option<String>, String)>,
        w: &Words,
    ) -> Vec<String> {
        let distinct: BTreeSet<Option<&str>> = items.iter().map(|(s, _)| s.as_deref()).collect();
        if distinct.len() < 2 {
            return items
                .into_iter()
                .map(|(_, line)| format!("• {line}"))
                .collect();
        }
        items.sort_by_key(|(sphere, _)| self.rank(sphere.as_deref()));
        let mut lines = Vec::new();
        let mut heading: Option<Option<String>> = None;
        for (sphere, line) in items {
            if heading.as_ref() != Some(&sphere) {
                let name = match &sphere {
                    Some(id) => self.label(id, w),
                    None => w.no_sphere.to_owned(),
                };
                lines.push(format!("<i>{}</i>", esc(&name)));
                heading = Some(sphere);
            }
            lines.push(format!("• {line}"));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::lang::Lang;

    #[test]
    fn built_in_spheres_speak_the_bots_language_and_own_ones_their_names() {
        let spheres = Spheres::from_api(&json!([
            { "id": "tech", "name": null, "built_in": true },
            { "id": "family", "name": "Семья", "built_in": false }
        ]));
        let w = Lang::Ru.words();
        assert_eq!(spheres.name("tech", w), "Работа · техника");
        assert_eq!(spheres.label("family", w), "🏷 Семья");
        assert_eq!(spheres.name("gone", w), "gone");
    }

    #[test]
    fn lines_group_by_sphere_only_when_there_are_several() {
        let w = Lang::En.words();
        let spheres = Spheres::default();
        let one = spheres.grouped(
            vec![
                (Some("tech".into()), "a".into()),
                (Some("tech".into()), "b".into()),
            ],
            w,
        );
        assert_eq!(one, ["• a", "• b"]);
        let several = spheres.grouped(
            vec![
                (None, "loose".into()),
                (Some("personal".into()), "flights".into()),
                (Some("tech".into()), "deploy".into()),
            ],
            w,
        );
        assert_eq!(
            several,
            [
                "<i>🛠 Work · tech</i>",
                "• deploy",
                "<i>🏠 Personal</i>",
                "• flights",
                "<i>No sphere</i>",
                "• loose"
            ]
        );
    }
}
