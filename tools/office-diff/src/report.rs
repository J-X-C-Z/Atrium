//! The HTML and JSON report: a score per file, side-by-side pages and a
//! difference image for each page.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::Serialize;

use crate::diff::FileCompare;

/// One page's images, as paths relative to the report directory.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PageImages {
    pub atrium: Option<String>,
    pub libreoffice: Option<String>,
    pub diff: Option<String>,
}

/// One file in the report.
#[derive(Clone, Debug, Serialize)]
pub struct FileReport {
    pub name: String,
    pub kind: String,
    pub compare: FileCompare,
    pub images: Vec<PageImages>,
    /// Set when Atrium could not lay the file out at all.
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunMeta {
    pub generated: String,
    pub soffice: Option<String>,
    pub width_px: u32,
    pub files: usize,
}

const CSS: &str = r#"
:root { color-scheme: light dark; --ink:#1b1b1f; --muted:#67676f; --line:#d9d9df; --bg:#fbfbfd; --card:#fff; --bad:#c0392b; --warn:#b7791f; --good:#1e7d4f; }
@media (prefers-color-scheme: dark) { :root { --ink:#eeeeF2; --muted:#a0a0aa; --line:#33333b; --bg:#141418; --card:#1c1c22; } }
body { margin:0; font:15px/1.5 -apple-system, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif; color:var(--ink); background:var(--bg); }
main { max-width:1200px; margin:0 auto; padding:32px 24px 80px; }
h1 { font-size:26px; margin:0 0 4px; }
h2 { font-size:20px; margin:40px 0 8px; }
.muted { color:var(--muted); }
table { border-collapse:collapse; width:100%; background:var(--card); border:1px solid var(--line); }
th, td { text-align:left; padding:7px 10px; border-bottom:1px solid var(--line); vertical-align:top; }
th { font-weight:600; }
.score { font-variant-numeric:tabular-nums; font-weight:600; }
.s-good { color:var(--good); } .s-warn { color:var(--warn); } .s-bad { color:var(--bad); }
.pages { display:grid; grid-template-columns:1fr 1fr 1fr; gap:14px; align-items:start; }
figure { margin:0; background:var(--card); border:1px solid var(--line); padding:8px; }
figure img { width:100%; height:auto; display:block; }
figcaption { font-size:13px; color:var(--muted); padding:4px 2px 0; }
.error { background:var(--card); border:1px solid var(--bad); color:var(--bad); padding:10px 12px; }
details { margin:10px 0; }
summary { cursor:pointer; }
code { background:rgba(127,127,127,.15); padding:1px 4px; border-radius:4px; }
"#;

/// Writes `index.html` and `report.json` into `dir`.
pub fn write(dir: &Path, files: &[FileReport], meta: &RunMeta) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let json = serde_json::to_string_pretty(&serde_json::json!({
        "meta": meta,
        "files": files,
    }))?;
    std::fs::write(dir.join("report.json"), json)?;

    let mut html = String::new();
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    html.push_str("<title>Office vs LibreOffice</title><style>");
    html.push_str(CSS);
    html.push_str("</style></head><body><main>");
    let _ = writeln!(html, "<h1>Office vs LibreOffice</h1>");
    let _ = writeln!(
        html,
        "<p class=\"muted\">Generated {} &middot; {} files &middot; {} px pages",
        escape(&meta.generated),
        meta.files,
        meta.width_px
    );
    match &meta.soffice {
        Some(path) => {
            let _ = writeln!(
                html,
                " &middot; LibreOffice: <code>{}</code></p>",
                escape(path)
            );
        }
        None => {
            let _ = writeln!(
                html,
                " &middot; <strong>LibreOffice was not available</strong>: only Atrium pages are shown.</p>"
            );
        }
    }
    if files.is_empty() {
        html.push_str("<p class=\"error\">No files were rendered.</p>");
    }
    // Summary table.
    html.push_str("<table><thead><tr><th>File</th><th>Kind</th><th>Score</th><th>Pages (Atrium / LibreOffice)</th><th>Pages compared</th></tr></thead><tbody>");
    for file in files {
        let class = if file.compare.compared {
            score_class(file.compare.score)
        } else {
            "muted"
        };
        let score = if file.compare.compared {
            format!("{:.1}", file.compare.score)
        } else {
            "not compared".into()
        };
        let compared = file.compare.pages.len();
        let _ = writeln!(
            html,
            "<tr><td><a href=\"#{}\">{}</a></td><td>{}</td><td class=\"score {}\">{}</td><td>{} / {}</td><td>{}</td></tr>",
            escape(&file.name),
            escape(&file.name),
            escape(&file.kind),
            class,
            score,
            file.compare.atrium_pages,
            file.compare.lo_pages,
            compared
        );
    }
    html.push_str("</tbody></table>");

    for file in files {
        let _ = writeln!(
            html,
            "<h2 id=\"{}\">{}</h2>",
            escape(&file.name),
            escape(&file.name)
        );
        if let Some(error) = &file.error {
            let _ = writeln!(html, "<p class=\"error\">{}</p>", escape(error));
        }
        if file.compare.compared {
            let _ = writeln!(
                html,
                "<p class=\"muted\">kind <code>{}</code> &middot; score <span class=\"score {}\">{:.1}</span> &middot; page-count penalty {:.2} &middot; mean perceptual page difference {:.3}</p>",
                escape(&file.kind),
                score_class(file.compare.score),
                file.compare.score,
                file.compare.page_penalty,
                mean_perceptual(&file.compare)
            );
        } else {
            let _ = writeln!(
                html,
                "<p class=\"muted\">kind <code>{}</code> &middot; <strong>not compared</strong> (LibreOffice produced no pages)</p>",
                escape(&file.kind)
            );
        }
        html.push_str("<details><summary>Per-page numbers</summary><table><thead><tr><th>Page</th><th>Score</th><th>Words A / L</th><th>Content F1</th><th>Match</th><th>Mean distance</th><th>Image</th></tr></thead><tbody>");
        for page in &file.compare.pages {
            let distance = page
                .mean_distance
                .map(|d| format!("{d:.3}"))
                .unwrap_or_else(|| "—".into());
            let _ = writeln!(
                html,
                "<tr><td>{}</td><td class=\"score {}\">{:.1}</td><td>{} / {}</td><td>{:.2}</td><td>{:.2}</td><td>{}</td><td>{:.3}</td></tr>",
                page.index + 1,
                score_class(page.score),
                page.score,
                page.atrium_words,
                page.lo_words,
                page.content_f1,
                page.token_match,
                distance,
                page.image_score
            );
        }
        html.push_str("</tbody></table></details>");

        html.push_str("<div class=\"pages\">");
        for (index, images) in file.images.iter().enumerate() {
            let _ = writeln!(html, "<figure>");
            match (&images.atrium, &images.libreoffice) {
                (Some(atrium), Some(lo)) => {
                    let _ = writeln!(
                        html,
                        "<div style=\"display:grid;grid-template-columns:1fr 1fr;gap:6px\"><div><img src=\"{}\" alt=\"Atrium page {}\"><figcaption>Atrium</figcaption></div><div><img src=\"{}\" alt=\"LibreOffice page {}\"><figcaption>LibreOffice</figcaption></div></div>",
                        escape(atrium),
                        index + 1,
                        escape(lo),
                        index + 1
                    );
                    if let Some(diff) = &images.diff {
                        let _ = writeln!(
                            html,
                            "<img style=\"margin-top:6px\" src=\"{}\" alt=\"difference\"><figcaption>Difference (red = disagreement)</figcaption>",
                            escape(diff)
                        );
                    }
                }
                (Some(atrium), None) => {
                    let _ = writeln!(
                        html,
                        "<img src=\"{}\" alt=\"Atrium page {}\"><figcaption>Atrium page {} (no LibreOffice page)</figcaption>",
                        escape(atrium),
                        index + 1,
                        index + 1
                    );
                }
                (None, Some(lo)) => {
                    let _ = writeln!(
                        html,
                        "<img src=\"{}\" alt=\"LibreOffice page {}\"><figcaption>LibreOffice page {} (no Atrium page)</figcaption>",
                        escape(lo),
                        index + 1,
                        index + 1
                    );
                }
                (None, None) => {}
            }
            let _ = writeln!(html, "<figcaption>Page {}</figcaption></figure>", index + 1);
        }
        html.push_str("</div>");
    }
    html.push_str("</main></body></html>");
    let path = dir.join("index.html");
    std::fs::write(&path, html)?;
    Ok(path)
}

fn mean_perceptual(compare: &FileCompare) -> f32 {
    if compare.pages.is_empty() {
        return 1.0;
    }
    compare.pages.iter().map(|p| p.perceptual_diff).sum::<f32>() / compare.pages.len() as f32
}

fn score_class(score: f32) -> &'static str {
    if score >= 85.0 {
        "s-good"
    } else if score >= 60.0 {
        "s-warn"
    } else {
        "s-bad"
    }
}

/// HTML text escaping. Enough for names and numbers; the tool writes no
/// user-supplied markup.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// A filesystem-safe name for one file's assets.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('_');
        } else {
            out.push('-');
        }
    }
    if out.is_empty() { "file".into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_is_complete() {
        assert_eq!(
            escape("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }

    #[test]
    fn slugs_keep_ascii_names() {
        assert_eq!(slug("a b/预算.docx"), "a_b---.docx");
    }
}
