//! `office-diff`: renders every file in a corpus with LibreOffice and with
//! Atrium's layout engine, compares the pages, and writes an HTML report.
//!
//! Uses the system's fonts through `fontdb`/`rustybuzz`; no GPUI.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow};
use tiny_skia::Pixmap;

use office_diff::atrium;
use office_diff::diff::{FileCompare, compare_file, compare_page};
use office_diff::lo;
use office_diff::render;
use office_diff::report::{self, FileReport, PageImages, RunMeta};
use office_diff::shaper::SystemShaper;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("office-diff: {err:#}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    corpus: PathBuf,
    report: PathBuf,
    soffice: PathBuf,
    atrium_only: bool,
    width: u32,
    filter: Option<String>,
    fail_under: Option<f32>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        corpus: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpus/out"),
        report: PathBuf::from("target/office-diff-report"),
        soffice: std::env::var_os("SOFFICE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("soffice")),
        atrium_only: false,
        width: 900,
        filter: None,
        fail_under: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--corpus" => args.corpus = take(&mut it, "--corpus")?.into(),
            "--report" => args.report = take(&mut it, "--report")?.into(),
            "--soffice" => args.soffice = take(&mut it, "--soffice")?.into(),
            "--width" => {
                args.width = take(&mut it, "--width")?
                    .parse()
                    .map_err(|_| anyhow!("--width needs a number"))?
            }
            "--filter" => args.filter = Some(take(&mut it, "--filter")?),
            "--fail-under" => {
                args.fail_under = Some(
                    take(&mut it, "--fail-under")?
                        .parse()
                        .map_err(|_| anyhow!("--fail-under needs a number"))?,
                )
            }
            "--atrium-only" | "--no-libreoffice" => args.atrium_only = true,
            "--help" | "-h" => {
                println!(
                    "office-diff [--corpus DIR] [--report DIR] [--soffice PATH]\n\
                     \x20           [--width PX] [--filter TEXT] [--fail-under SCORE]\n\
                     \x20           [--atrium-only]\n\n\
                     Renders each .docx/.pptx/.xlsx in DIR with Atrium and LibreOffice,\n\
                     compares the pages and writes DIR/index.html and DIR/report.json."
                );
                std::process::exit(0);
            }
            other => return Err(anyhow!("unknown argument {other:?}")),
        }
    }
    Ok(args)
}

fn take(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    it.next().ok_or_else(|| anyhow!("{flag} needs a value"))
}

fn run() -> Result<ExitCode> {
    let args = parse_args()?;
    let files = corpus_files(&args.corpus, args.filter.as_deref())?;
    if files.is_empty() {
        return Err(anyhow!(
            "no .docx/.pptx/.xlsx files in {}; run tools/office-diff/corpus/generate.py first",
            args.corpus.display()
        ));
    }
    let shaper = SystemShaper::new();
    let assets = args.report.join("assets");
    let work = args.report.join("work");
    std::fs::create_dir_all(&assets)?;
    std::fs::create_dir_all(&work)?;
    let soffice =
        (!args.atrium_only && lo::available(&args.soffice)).then_some(args.soffice.clone());
    if soffice.is_none() {
        eprintln!(
            "office-diff: LibreOffice ({}) is not available; rendering Atrium pages only",
            args.soffice.display()
        );
    }
    let mut reports = Vec::new();
    for path in &files {
        reports.push(process(
            path,
            &shaper,
            &assets,
            &work,
            soffice.as_deref(),
            args.width,
        )?);
    }
    let meta = RunMeta {
        generated: jiff::Timestamp::now().to_string(),
        soffice: soffice.as_ref().map(|p| p.display().to_string()),
        width_px: args.width,
        files: reports.len(),
    };
    let html = report::write(&args.report, &reports, &meta)?;
    println!();
    println!("{:<40} {:>10}  pages", "file", "score");
    for file in &reports {
        let score = if file.compare.compared {
            format!("{:.1}", file.compare.score)
        } else {
            "not compared".into()
        };
        println!(
            "{:<40} {:>10}  {} / {}",
            file.name, score, file.compare.atrium_pages, file.compare.lo_pages
        );
        if let Some(error) = &file.error {
            println!("    {error}");
        }
    }
    println!("\nreport: {}", html.display());
    if let Some(minimum) = args.fail_under {
        let worst = reports
            .iter()
            .filter(|f| f.compare.compared)
            .map(|f| f.compare.score)
            .fold(f32::INFINITY, f32::min);
        if worst < minimum {
            eprintln!("office-diff: worst score {worst:.1} is below {minimum:.1}");
            return Ok(ExitCode::from(2));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn corpus_files(dir: &Path, filter: Option<&str>) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries {
        let path = entry?.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !matches!(ext.to_ascii_lowercase().as_str(), "docx" | "pptx" | "xlsx") {
            continue;
        }
        if let Some(filter) = filter
            && !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(filter))
        {
            continue;
        }
        out.push(path);
    }
    out.sort();
    Ok(out)
}

fn process(
    path: &Path,
    shaper: &SystemShaper,
    assets: &Path,
    work: &Path,
    soffice: Option<&Path>,
    width: u32,
) -> Result<FileReport> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    let slug = report::slug(&name);
    let asset_dir = assets.join(&slug);
    std::fs::create_dir_all(&asset_dir)?;

    let mut errors = Vec::new();
    let laid = match atrium::load(path, shaper) {
        Ok(laid) => Some(laid),
        Err(err) => {
            errors.push(format!("Atrium: {err:#}"));
            None
        }
    };
    let mut atrium_pixmaps: Vec<Option<Pixmap>> = Vec::new();
    let mut atrium_images: Vec<Option<String>> = Vec::new();
    if let Some(laid) = &laid {
        for (index, page) in laid.pages.iter().enumerate() {
            match render::render(shaper, page, width) {
                Ok(pixmap) => {
                    let file = format!("atrium-{}.png", index + 1);
                    pixmap.save_png(asset_dir.join(&file))?;
                    atrium_images.push(Some(format!("assets/{slug}/{file}")));
                    atrium_pixmaps.push(Some(pixmap));
                }
                Err(err) => {
                    errors.push(format!("Atrium page {}: {err:#}", index + 1));
                    atrium_images.push(None);
                    atrium_pixmaps.push(None);
                }
            }
        }
    }

    let mut lo_pages = Vec::new();
    if let Some(soffice) = soffice {
        match lo::convert(soffice, path, work) {
            Ok(pdf) => match lo::pages(&pdf, work, width) {
                Ok(pages) => lo_pages = pages,
                Err(err) => errors.push(format!("LibreOffice pages: {err:#}")),
            },
            Err(err) => errors.push(format!("LibreOffice: {err:#}")),
        }
    }
    let mut lo_pixmaps: Vec<Option<Pixmap>> = Vec::new();
    let mut lo_images: Vec<Option<String>> = Vec::new();
    for (index, page) in lo_pages.iter().enumerate() {
        let file = format!("lo-{}.png", index + 1);
        match std::fs::copy(&page.png, asset_dir.join(&file)) {
            Ok(_) => lo_images.push(Some(format!("assets/{slug}/{file}"))),
            Err(err) => {
                errors.push(format!("copying LibreOffice page {}: {err}", index + 1));
                lo_images.push(None);
            }
        }
        match std::fs::read(&page.png)
            .ok()
            .and_then(|bytes| Pixmap::decode_png(&bytes).ok())
        {
            Some(pixmap) => lo_pixmaps.push(Some(pixmap)),
            None => lo_pixmaps.push(None),
        }
    }

    let atrium_count = laid.as_ref().map(|l| l.pages.len()).unwrap_or(0);
    let lo_count = lo_pages.len();
    let pages = atrium_count.max(lo_count);
    let mut compares = Vec::new();
    let mut images = Vec::new();
    for index in 0..pages {
        let atrium_tokens = laid
            .as_ref()
            .and_then(|l| l.tokens.get(index))
            .map(|t| t.as_slice());
        let lo_tokens = lo_pages.get(index).map(|p| p.tokens.as_slice());
        let atrium_size = laid
            .as_ref()
            .and_then(|l| l.pages.get(index))
            .map(|p| (p.width_pt, p.height_pt))
            .unwrap_or((1.0, 1.0));
        let lo_size = lo_pages
            .get(index)
            .map(|p| (p.width_pt, p.height_pt))
            .unwrap_or((1.0, 1.0));
        let diff_file = format!("diff-{}.png", index + 1);
        let diff_rel = if atrium_pixmaps.get(index).is_some_and(Option::is_some)
            && lo_pixmaps.get(index).is_some_and(Option::is_some)
        {
            Some(format!("assets/{slug}/{diff_file}"))
        } else {
            None
        };
        let compare = compare_page(
            index,
            atrium_tokens,
            lo_tokens,
            atrium_size,
            lo_size,
            atrium_pixmaps.get(index).and_then(Option::as_ref),
            lo_pixmaps.get(index).and_then(Option::as_ref),
            &asset_dir.join(&diff_file),
        )?;
        compares.push(compare);
        images.push(PageImages {
            atrium: atrium_images.get(index).cloned().flatten(),
            libreoffice: lo_images.get(index).cloned().flatten(),
            diff: diff_rel,
        });
    }
    let compare: FileCompare = compare_file(compares, atrium_count, lo_count);
    Ok(FileReport {
        name,
        kind: laid.as_ref().map(|l| l.kind).unwrap_or("unknown").into(),
        compare,
        images,
        error: (!errors.is_empty()).then(|| errors.join("; ")),
    })
}
