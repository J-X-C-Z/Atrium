//! Templates that come with Atrium. Users can copy and change them; their
//! own templates live in `~/.config/Atrium/workflows/*.toml` and in a
//! folder's `.atrium/workflows/*.toml`.

use crate::Template;

/// The built-in templates' sources, by id.
pub const SOURCES: &[(&str, &str)] = &[("mathmodel", include_str!("builtin/mathmodel.toml"))];

pub fn all() -> Vec<Template> {
    SOURCES
        .iter()
        .filter_map(|(_, text)| Template::parse(text).ok())
        .collect()
}

pub fn get(id: &str) -> Option<Template> {
    SOURCES
        .iter()
        .find(|(i, _)| *i == id)
        .and_then(|(_, text)| Template::parse(text).ok())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{
        render::{Lang, prompt},
        run::{Run, StepStatus},
    };

    #[test]
    fn every_builtin_template_is_valid() {
        for (id, text) in SOURCES {
            let t = Template::parse(text).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(&t.id, id);
        }
    }

    #[test]
    fn the_math_modeling_run_follows_the_contest() {
        let t = get("mathmodel").unwrap();
        let inputs: HashMap<String, String> = [
            ("problem".to_string(), "inputs/A题.pdf".to_string()),
            ("contest".to_string(), "MCM/ICM".to_string()),
            ("identity".to_string(), "张三".to_string()),
        ]
        .into();
        let run = Run::new(t.clone(), inputs, &["diagrams".into()], 0).unwrap();
        assert!(!run.inputs.contains_key("status") || run.inputs["status"] == "草稿");
        assert_eq!(run.step("diagrams").unwrap().status, StepStatus::Skipped);
        assert_eq!(run.ready(), vec!["analysis"]);

        let writing = run.checks_of("writing");
        assert!(
            writing
                .iter()
                .any(|c| matches!(c, crate::Check::PdfPages { max: Some(25), .. }))
        );
        assert!(
            !writing
                .iter()
                .any(|c| matches!(c, crate::Check::Absent { pattern: Some(p), .. } if p.contains("tableofcontents"))),
            "the CUMCM-only rule is left out for MCM"
        );
        let p = prompt(&run, "analysis", true, Lang::Zh).unwrap();
        assert!(p.contains("竞赛（MCM/ICM）"));
        assert!(p.contains("`inputs/A题.pdf`"));
        assert!(!p.contains("张三"));
        assert!(p.contains("/mathmodel-contest-suite:2analysis-modeling"));

        // The writer waits for the optional diagrams only while they run.
        let needs = t.needs_of(t.steps.iter().position(|s| s.id == "writing").unwrap());
        assert_eq!(needs, vec!["coding", "diagrams"]);
    }
}
