//! Domain-layer benchmark suite (`revisa bench`, dev-tools only).
//!
//! Runs the diff pipeline stage by stage over a deterministic generated
//! corpus (or an external `--left/--right` pair) and reports wall time plus
//! machine-independent counters per stage. GUI/frame-path costs are out of
//! scope — this covers walk/rename, diff, highlight, compose, search, and
//! a few micros.

mod corpus;

use std::collections::HashSet;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use crate::app::{self, FileDiffData};
use crate::domain::diff::LineDiff;
use crate::domain::file_pair::{self, FileChangeKind, FilePair};
use crate::domain::fold::FoldState;
use crate::domain::search::{self, SearchableFileData};
use crate::domain::settings::Settings;
use crate::highlight::Highlighter;

pub struct Options {
    pub filter: Option<String>,
    pub left: Option<PathBuf>,
    pub right: Option<PathBuf>,
    pub scale: usize,
    pub iterations: usize,
    pub seed: u64,
    pub json: bool,
}

const SEARCH_QUERIES: &[&str] = &["fn", "config", "Data", "zzz_no_match", "übermatch"];
const FUZZY_QUERIES: &[&str] = &["src", "handler", "core/parse", "zz"];
const FOLD_REPS: usize = 8;
const FUZZY_REPS: usize = 64;

struct StageResult {
    name: String,
    wall_ms: f64,
    counters: Vec<(&'static str, f64)>,
}

/// Run `f` `iterations` times; return the last result and the median wall ms.
fn timed<T>(iterations: usize, mut f: impl FnMut() -> T) -> (T, f64) {
    let mut walls = Vec::with_capacity(iterations);
    let mut result = None;
    for _ in 0..iterations.max(1) {
        let start = Instant::now();
        result = Some(f());
        walls.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    walls.sort_by(f64::total_cmp);
    (
        result.expect("timed() runs at least one iteration"),
        walls[walls.len() / 2],
    )
}

pub fn run(opts: &Options) {
    let corpus = match (&opts.left, &opts.right) {
        (Some(l), Some(r)) => corpus::Corpus::external(l.clone(), r.clone()),
        _ => match corpus::generate(opts.scale, opts.seed) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error generating corpus: {e}");
                std::process::exit(1);
            }
        },
    };
    eprintln!(
        "corpus: {} (scale {}, seed {:#x})",
        corpus.left.parent().unwrap_or(&corpus.left).display(),
        opts.scale,
        opts.seed
    );

    let want = |s: &str| opts.filter.as_deref().is_none_or(|f| s.contains(f));
    let iters = |s: &str| if want(s) { opts.iterations } else { 1 };
    let want_read = ["read-diff", "highlight", "compose", "search", "fold"]
        .iter()
        .any(|s| want(s));
    let want_compose = ["compose", "search", "fold"].iter().any(|s| want(s));

    let settings = Settings::default();
    let mut results: Vec<StageResult> = Vec::new();

    // walk: directory scan + pairing + rename detection.
    file_pair::RENAME_DIFFS.store(0, std::sync::atomic::Ordering::Relaxed);
    let (pairs, wall) = timed(iters("walk"), || {
        file_pair::walk_and_pair(&corpus.left, &corpus.right, false).unwrap_or_else(|e| {
            eprintln!("Error scanning corpus: {e}");
            std::process::exit(1);
        })
    });
    let pairs_diffed =
        file_pair::RENAME_DIFFS.load(std::sync::atomic::Ordering::Relaxed) / iters("walk").max(1);
    if want("walk") {
        results.push(walk_result(
            &pairs,
            corpus.renames.as_deref(),
            pairs_diffed,
            wall,
        ));
    }

    // read-diff: read contents + Myers line diff, sequential (phase-1 cost
    // per file; the app runs this on rayon, sequential is stabler to compare).
    let mut read_data: Vec<(String, String, bool)> = Vec::new();
    if want_read {
        let (data, wall) = timed(iters("read-diff"), || {
            let mut lines = 0usize;
            let mut added = 0usize;
            let mut deleted = 0usize;
            let data: Vec<(String, String, bool)> = pairs
                .iter()
                .map(|p| {
                    let (stat, old, new, diff, is_binary) = app::read_and_diff(p);
                    black_box(&diff);
                    lines += count_lines(&old) + count_lines(&new);
                    added += stat.added;
                    deleted += stat.deleted;
                    (old, new, is_binary)
                })
                .collect();
            (data, lines, added, deleted)
        });
        let (data, lines, added, deleted) = data;
        read_data = data;
        if want("read-diff") {
            results.push(StageResult {
                name: "read-diff".to_string(),
                wall_ms: wall,
                counters: vec![
                    ("files", pairs.len() as f64),
                    ("lines", lines as f64),
                    ("lines_per_s", rate(lines, wall)),
                    ("added", added as f64),
                    ("deleted", deleted as f64),
                ],
            });
        }
    }

    // highlight-init: syntax dump load + theme.
    let (highlighter, wall) = timed(iters("highlight-init"), || Highlighter::new(None));
    if want("highlight-init") {
        results.push(StageResult {
            name: "highlight-init".to_string(),
            wall_ms: wall,
            counters: vec![],
        });
    }

    // highlight: raw syntect throughput over every text side, split per extension.
    if want("highlight") {
        let inputs: Vec<(&str, String)> = pairs
            .iter()
            .zip(&read_data)
            .filter(|(_, (_, _, is_binary))| !is_binary)
            .flat_map(|(p, (old, new, _))| {
                let name = p.relative_path.to_string_lossy().into_owned();
                [(old.as_str(), name.clone()), (new.as_str(), name)]
            })
            .filter(|(content, _)| !content.is_empty())
            .collect();
        let mut per_ext: Vec<(String, f64, usize)> = Vec::new();
        let (_, wall) = timed(iters("highlight"), || {
            per_ext.clear();
            let mut total_lines = 0usize;
            for (content, name) in &inputs {
                let start = Instant::now();
                black_box(highlighter.highlight_file(content, name));
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                let lines = count_lines(content);
                total_lines += lines;
                let ext = name.rsplit('.').next().unwrap_or("").to_string();
                match per_ext.iter_mut().find(|(e, _, _)| *e == ext) {
                    Some(slot) => {
                        slot.1 += ms;
                        slot.2 += lines;
                    }
                    None => per_ext.push((ext, ms, lines)),
                }
            }
            total_lines
        });
        let total_lines: usize = per_ext.iter().map(|(_, _, l)| l).sum();
        results.push(StageResult {
            name: "highlight".to_string(),
            wall_ms: wall,
            counters: vec![
                ("sides", inputs.len() as f64),
                ("lines", total_lines as f64),
                ("lines_per_s", rate(total_lines, wall)),
            ],
        });
        per_ext.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (ext, ms, lines) in per_ext {
            results.push(StageResult {
                name: format!("highlight[{ext}]"),
                wall_ms: ms,
                counters: vec![("lines", lines as f64), ("lines_per_s", rate(lines, ms))],
            });
        }
    }

    // compose: full FileDiffData construction (diff + highlight + inline +
    // styled spans) — the cost of opening one file, applied to every pair.
    let mut datas: Vec<FileDiffData> = Vec::new();
    if want_compose {
        let mut walls = Vec::new();
        for _ in 0..iters("compose") {
            // Clone inputs outside the timed region; compose consumes Strings.
            let inputs: Vec<(String, String, String, String, bool)> = pairs
                .iter()
                .zip(&read_data)
                .map(|(p, (old, new, is_binary))| {
                    let name = p.relative_path.to_string_lossy().into_owned();
                    let old_name = p
                        .old_relative_path
                        .as_ref()
                        .map_or_else(|| name.clone(), |op| op.to_string_lossy().into_owned());
                    (old.clone(), new.clone(), name, old_name, *is_binary)
                })
                .collect();
            let b = &settings.behavior;
            let start = Instant::now();
            datas = inputs
                .into_iter()
                .map(|(old, new, name, old_name, is_binary)| {
                    if is_binary {
                        FileDiffData::binary_placeholder(
                            b.fold_context,
                            b.fold_expand_step,
                            b.fold_row_height,
                        )
                    } else {
                        app::compute_diff_from_contents_with_diff(
                            old,
                            new,
                            None::<LineDiff>,
                            &name,
                            &old_name,
                            &highlighter,
                            &settings,
                            false,
                        )
                    }
                })
                .collect();
            walls.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        walls.sort_by(f64::total_cmp);
        if want("compose") {
            let rows: usize = datas.iter().map(|d| d.aligned_rows.len()).sum();
            let spans: usize = datas
                .iter()
                .flat_map(|d| d.left_styled.iter().chain(&d.right_styled))
                .map(Vec::len)
                .sum();
            let span_size = size_of::<crate::highlight::StyledSpan>();
            let vec_headers = datas
                .iter()
                .map(|d| d.left_styled.len() + d.right_styled.len())
                .sum::<usize>()
                * size_of::<Vec<()>>();
            results.push(StageResult {
                name: "compose".to_string(),
                wall_ms: walls[walls.len() / 2],
                counters: vec![
                    ("files", datas.len() as f64),
                    ("rows", rows as f64),
                    ("styled_spans", spans as f64),
                    (
                        "styled_mb",
                        (spans * span_size + vec_headers) as f64 / (1024.0 * 1024.0),
                    ),
                ],
            });
        }
    }

    // search-snapshot: the per-dispatch corpus snapshot (finding: deep clone).
    if want("search") {
        let (snapshots, wall) = timed(iters("search-snapshot"), || {
            datas
                .iter()
                .map(SearchableFileData::from_diff_data)
                .collect::<Vec<_>>()
        });
        if want("search-snapshot") {
            let bytes: usize = read_data.iter().map(|(o, n, _)| o.len() + n.len()).sum();
            results.push(StageResult {
                name: "search-snapshot".to_string(),
                wall_ms: wall,
                counters: vec![
                    ("files", snapshots.len() as f64),
                    ("content_mb", bytes as f64 / (1024.0 * 1024.0)),
                ],
            });
        }
        for query in SEARCH_QUERIES {
            let (matches, wall) = timed(iters("search"), || {
                snapshots
                    .iter()
                    .enumerate()
                    .map(|(i, snap)| search::compute_file_matches(i, snap, query).len())
                    .sum::<usize>()
            });
            results.push(StageResult {
                name: format!("search[{query}]"),
                wall_ms: wall,
                counters: vec![("matches", matches as f64)],
            });
        }
    }

    // fold: FoldState construction + unified prefix-sum for every file.
    if want("fold") {
        let b = &settings.behavior;
        let ((), wall) = timed(iters("fold"), || {
            for _ in 0..FOLD_REPS {
                for d in &datas {
                    let mut fs = FoldState::new(
                        d.aligned_rows.len(),
                        &d.hunks,
                        b.fold_context,
                        b.fold_expand_step,
                        b.fold_row_height,
                    );
                    black_box(fs.total_view_rows_unified(&d.aligned_rows));
                }
            }
        });
        let rows: usize = datas.iter().map(|d| d.aligned_rows.len()).sum();
        results.push(StageResult {
            name: "fold".to_string(),
            wall_ms: wall,
            counters: vec![
                ("files", datas.len() as f64),
                ("rows", rows as f64),
                ("reps", FOLD_REPS as f64),
            ],
        });
    }

    // fuzzy: quick-picker matching over all corpus paths.
    if want("fuzzy") {
        let paths: Vec<String> = pairs
            .iter()
            .map(|p| p.relative_path.to_string_lossy().into_owned())
            .collect();
        for query in FUZZY_QUERIES {
            let (matched, wall) = timed(iters("fuzzy"), || {
                let mut matched = 0usize;
                for _ in 0..FUZZY_REPS {
                    matched = paths
                        .iter()
                        .filter(|p| crate::ui::quick_picker::fuzzy_match(query, p).is_some())
                        .count();
                }
                matched
            });
            results.push(StageResult {
                name: format!("fuzzy[{query}]"),
                wall_ms: wall,
                counters: vec![
                    ("candidates", paths.len() as f64),
                    ("matched", matched as f64),
                    ("reps", FUZZY_REPS as f64),
                ],
            });
        }
    }

    report(&results, opts.json);
}

fn walk_result(
    pairs: &[FilePair],
    truth: Option<&[(PathBuf, PathBuf)]>,
    pairs_diffed: usize,
    wall: f64,
) -> StageResult {
    let renamed: Vec<(&PathBuf, &PathBuf)> = pairs
        .iter()
        .filter(|p| matches!(p.kind, FileChangeKind::Renamed { .. }))
        .filter_map(|p| p.old_relative_path.as_ref().map(|o| (o, &p.relative_path)))
        .collect();
    let deleted = pairs
        .iter()
        .filter(|p| p.kind == FileChangeKind::Deleted)
        .count();
    let added = pairs
        .iter()
        .filter(|p| p.kind == FileChangeKind::Added)
        .count();
    // Pre-detection candidate space: every rename started as deleted+added.
    let d0 = deleted + renamed.len();
    let a0 = added + renamed.len();

    let mut counters = vec![
        ("pairs", pairs.len() as f64),
        ("deleted", d0 as f64),
        ("added", a0 as f64),
        ("candidates", (d0 * a0) as f64),
        ("pairs_diffed", pairs_diffed as f64),
        ("renames", renamed.len() as f64),
    ];
    if let Some(truth) = truth {
        let truth_set: HashSet<(&PathBuf, &PathBuf)> = truth.iter().map(|(o, n)| (o, n)).collect();
        let tp = renamed.iter().filter(|r| truth_set.contains(r)).count();
        let precision = if renamed.is_empty() {
            1.0
        } else {
            tp as f64 / renamed.len() as f64
        };
        let recall = if truth.is_empty() {
            1.0
        } else {
            tp as f64 / truth.len() as f64
        };
        counters.push(("precision", precision));
        counters.push(("recall", recall));
    }
    StageResult {
        name: "walk".to_string(),
        wall_ms: wall,
        counters,
    }
}

fn count_lines(s: &str) -> usize {
    s.lines().count()
}

/// Items per second from a count and a wall time in ms.
fn rate(count: usize, wall_ms: f64) -> f64 {
    if wall_ms <= 0.0 {
        0.0
    } else {
        count as f64 / (wall_ms / 1000.0)
    }
}

fn fmt_value(v: f64) -> String {
    if v.fract().abs() < 1e-9 && v.abs() < 1e15 {
        format!("{v:.0}")
    } else {
        format!("{v:.2}")
    }
}

fn report(results: &[StageResult], json: bool) {
    if json {
        use std::fmt::Write;
        let mut out = String::from("[\n");
        for (i, r) in results.iter().enumerate() {
            let _ = write!(
                out,
                "  {{\"stage\": \"{}\", \"wall_ms\": {:.3}",
                r.name, r.wall_ms
            );
            for (k, v) in &r.counters {
                let _ = write!(out, ", \"{k}\": {}", fmt_value(*v));
            }
            out.push_str(if i + 1 == results.len() {
                "}\n"
            } else {
                "},\n"
            });
        }
        out.push(']');
        println!("{out}");
        return;
    }
    let name_w = results
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(5)
        .max(5);
    println!("{:<name_w$}  {:>10}  counters", "stage", "wall_ms");
    for r in results {
        let counters: Vec<String> = r
            .counters
            .iter()
            .map(|(k, v)| format!("{k}={}", fmt_value(*v)))
            .collect();
        println!(
            "{:<name_w$}  {:>10.2}  {}",
            r.name,
            r.wall_ms,
            counters.join(" ")
        );
    }
}
