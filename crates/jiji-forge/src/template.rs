//! The repo's pull-request template. GitHub reads
//! `PULL_REQUEST_TEMPLATE(.md|.txt)?` from `.github/`, the repo root, or
//! `docs/` on the default branch and pre-fills the web compose box with it
//! — but the API applies no template at all, so a PR created through it
//! gets exactly the body it is given. Jiji closes that gap: when the trunk
//! tree carries a template, new PR bodies fold it in below the managed
//! description.

use crate::reconcile::wrap_managed_body;

/// A template found on trunk: where it lives and what it says. The path
/// rides into [`crate::SubmitPlan`] so the panel can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrTemplate {
    pub path: String,
    pub text: String,
}

/// Candidate template paths in GitHub's own precedence order (`.github/`,
/// then the repo root, then `docs/`). GitHub matches the filename
/// case-insensitively; probing exact paths keeps the tree-read primitive
/// simple, so the two spellings that occur in practice are listed and a
/// MiXeD-case template would be missed — a documented approximation.
pub fn pr_template_candidates() -> Vec<String> {
    let mut candidates = Vec::new();
    for dir in [".github/", "", "docs/"] {
        for stem in ["PULL_REQUEST_TEMPLATE", "pull_request_template"] {
            for ext in [".md", ".txt", ""] {
                candidates.push(format!("{dir}{stem}{ext}"));
            }
        }
    }
    candidates
}

/// A new PR's full body: the commit-derived text inside Jiji's managed
/// sentinels, then the template below as ordinary user-space text. The
/// description leads so the PR opens with what actually changed; the
/// template follows for the author to fill in on GitHub — and because it
/// sits outside the managed section, later re-submits keep updating the
/// description without ever touching the filled-in template (the same
/// fingerprint rule that protects any hand-written text).
pub fn new_pr_body(commit_body: &str, title: &str, template: Option<&PrTemplate>) -> String {
    let managed = wrap_managed_body(commit_body, title);
    match template {
        Some(template) if !template.text.trim().is_empty() => {
            format!("{managed}\n\n{}", template.text.trim_end())
        }
        _ => managed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{extract_managed_body, plan_pr_text, stored_body_fp};

    #[test]
    fn candidates_probe_github_locations_in_precedence_order() {
        let candidates = pr_template_candidates();
        assert_eq!(candidates[0], ".github/PULL_REQUEST_TEMPLATE.md");
        assert!(candidates.contains(&"PULL_REQUEST_TEMPLATE.md".to_owned()));
        assert!(candidates.contains(&"docs/pull_request_template.txt".to_owned()));
        assert!(candidates.contains(&".github/pull_request_template".to_owned()));
        // `.github/` outranks the root, which outranks `docs/`.
        let pos = |path: &str| candidates.iter().position(|c| c == path).unwrap();
        assert!(pos(".github/pull_request_template.md") < pos("PULL_REQUEST_TEMPLATE.md"));
        assert!(pos("pull_request_template.md") < pos("docs/PULL_REQUEST_TEMPLATE.md"));
    }

    #[test]
    fn template_rides_below_the_managed_section() {
        let template = PrTemplate {
            path: ".github/PULL_REQUEST_TEMPLATE.md".into(),
            text: "## Checklist\n- [ ] tests\n".into(),
        };
        let body = new_pr_body("The commit body.", "feat: thing", Some(&template));
        assert_eq!(extract_managed_body(&body), Some("The commit body."));
        assert!(body.ends_with("## Checklist\n- [ ] tests"));
        // The fingerprint covers only the managed text, so the template is
        // user-space from birth.
        assert_eq!(
            stored_body_fp(&body),
            Some(crate::reconcile::fingerprint("The commit body.").as_str())
        );

        // No template (or a blank one) leaves the plain managed body.
        assert_eq!(
            new_pr_body("b", "t", None),
            wrap_managed_body("b", "t")
        );
        let blank = PrTemplate { path: "x".into(), text: "  \n".into() };
        assert_eq!(
            new_pr_body("b", "t", Some(&blank)),
            wrap_managed_body("b", "t")
        );
    }

    #[test]
    fn later_reconciles_never_touch_the_filled_in_template() {
        let template = PrTemplate {
            path: "PULL_REQUEST_TEMPLATE.md".into(),
            text: "## Checklist\n- [ ] tests".into(),
        };
        let body = new_pr_body("first draft", "feat: t", Some(&template));
        // The author fills the checklist in on GitHub.
        let on_github = body.replace("- [ ] tests", "- [x] tests");

        // The commit body moves; the managed section updates and the
        // filled-in checklist survives verbatim.
        let plan = plan_pr_text("feat: t", &on_github, "feat: t", "second draft");
        let new_body = plan.body.expect("commit moved, body updates");
        assert_eq!(extract_managed_body(&new_body), Some("second draft"));
        assert!(new_body.ends_with("## Checklist\n- [x] tests"));
        assert!(plan.warnings.is_empty());
    }
}
