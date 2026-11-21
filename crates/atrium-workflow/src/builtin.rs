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
        let mut run = Run::new(t.clone(), inputs, &["diagrams".into()], 0).unwrap();
        assert_eq!(
            run.step("select").unwrap().status,
            StepStatus::Skipped,
            "one problem given: no choosing"
        );
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

        // Analysis lists the sub-questions; the route is the user's pick;
        // each sub-question is solved on its own.
        let pass = || vec![crate::run::CheckResult::pass("ok")];
        for id in ["analysis", "models"] {
            run.begin(id, None, 1).unwrap();
            run.turn_finished(id, 2).unwrap();
            run.checked(id, pass(), 3).unwrap();
        }
        let p = prompt(&run, "models", true, Lang::Zh);
        assert!(p.is_some());
        let route = crate::read_choices(r#"[{"id":"A","title":"排队论 + 整数规划"}]"#);
        run.approve("models", Some(route[0].clone()), "Q2 先用启发式", 4)
            .unwrap();
        let p = prompt(&run, "foundation", true, Lang::Zh).unwrap();
        assert!(p.contains("选定 排队论 + 整数规划；补充：Q2 先用启发式"));
        run.begin("foundation", None, 5).unwrap();
        run.turn_finished("foundation", 6).unwrap();
        run.checked("foundation", pass(), 7).unwrap();
        run.approve("foundation", None, "", 8).unwrap();
        assert_eq!(run.to_expand().len(), 1);
        run.expand(
            "solve",
            crate::read_choices(r#"[{"id":"Q1","title":"问题一"},{"id":"Q2","title":"问题二"}]"#),
            9,
        )
        .unwrap();
        let q2 = prompt(&run, "solve:Q2", false, Lang::Zh).unwrap();
        assert!(q2.contains("这一步只做 问题二（编号 Q2）"));
        assert!(q2.contains("`code/Q2.py`"));
        assert!(
            run.checks_of("solve:Q2").iter().any(
                |c| matches!(c, crate::Check::Contains { path, .. } if path == "reports/Q2.md")
            )
        );
    }
}
