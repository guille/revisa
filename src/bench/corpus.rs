//! Deterministic PR-shaped corpus generator for the bench suite.
//!
//! Produces a left/right directory pair mixing languages, file sizes, and
//! change kinds, plus a ground-truth rename manifest so rename detection can
//! be scored for precision/recall, not just speed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

// Corpus shape at scale 1; counts multiply by `--scale`.
const MODIFIED: usize = 18;
const EXACT_RENAMES: usize = 6;
const NEAR_RENAMES: usize = 6;
const DELETED: usize = 8;
const ADDED: usize = 8;
/// Of the deleted/added files, this many of each share a line count
/// (adversarial for size-based rename pre-filters).
const SAME_SIZE: usize = 3;
const SAME_SIZE_LINES: usize = 300;
const BINARY: usize = 1;
/// Above the default `max_diff_lines`, to exercise the size-guard path.
const HUGE_LINES: usize = 4_500;
/// Edit percentage applied to modified files.
const MODIFIED_EDIT_PCT: usize = 12;

const SIZES: &[usize] = &[40, 100, 250, 600, 1_200];
const DIRS: &[&str] = &[
    "src",
    "src/core",
    "src/util",
    "pkg/api",
    "pkg/model",
    "docs",
    "config",
    "tests",
];
const WORDS: &[&str] = &[
    "config", "buffer", "index", "handler", "state", "parse", "cache", "token", "render", "widget",
    "stream", "worker", "result", "value", "entry", "batch", "layout", "cursor", "anchor",
    "segment",
];

pub struct Corpus {
    pub left: PathBuf,
    pub right: PathBuf,
    /// Ground-truth renames as (old, new) relative paths; `None` for
    /// external corpora passed via `--left/--right`.
    pub renames: Option<Vec<(PathBuf, PathBuf)>>,
}

impl Corpus {
    pub fn external(left: PathBuf, right: PathBuf) -> Self {
        Self {
            left,
            right,
            renames: None,
        }
    }
}

/// xorshift64* — deterministic, avoids a rand dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn word(&mut self) -> &'static str {
        WORDS[self.below(WORDS.len())]
    }

    /// Random identifier unique enough to keep unrelated files dissimilar.
    fn ident(&mut self) -> String {
        let w = self.word();
        let tag = self.next() & 0xffff;
        format!("{w}_{tag:x}")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Go,
    Markdown,
    Yaml,
    Json,
}

const LANG_CYCLE: &[Lang] = &[
    Lang::Rust,
    Lang::Go,
    Lang::Rust,
    Lang::Markdown,
    Lang::Yaml,
    Lang::Rust,
    Lang::Go,
    Lang::Json,
];

impl Lang {
    fn ext(self) -> &'static str {
        match self {
            Self::Rust => "rs",
            Self::Go => "go",
            Self::Markdown => "md",
            Self::Yaml => "yaml",
            Self::Json => "json",
        }
    }
}

fn type_name(rng: &mut Rng) -> String {
    let w = rng.word();
    let tag = rng.next() & 0xfff;
    let mut chars = w.chars();
    let head = chars.next().unwrap_or('X').to_ascii_uppercase();
    format!("{head}{}{tag:x}", chars.as_str())
}

/// One plausible statement line at the given indentation.
/// Brace-free so generated blocks stay balanced.
fn statement(lang: Lang, rng: &mut Rng, indent: &str) -> String {
    let a = rng.ident();
    let b = rng.ident();
    let c = rng.word();
    match lang {
        Lang::Rust => match rng.below(4) {
            0 => format!("{indent}let {a} = {b}.{c}();"),
            1 => format!("{indent}{a}.push({b});"),
            2 => format!("{indent}let {a} = {b} + {c}.len();"),
            _ => format!("{indent}{a}.insert({b}, {c});"),
        },
        Lang::Go => match rng.below(3) {
            0 => format!("{indent}{a} := {b}.{c}()"),
            1 => format!("{indent}{a} = append({a}, {b})"),
            _ => format!("{indent}return {a}.{c}({b})"),
        },
        Lang::Markdown => format!("{indent}The {a} maps each {b} onto the shared {c} table."),
        Lang::Yaml => format!("{indent}{a}: {b}"),
        Lang::Json => format!("{indent}\"{a}\": \"{b}\","),
    }
}

fn push_block(lang: Lang, rng: &mut Rng, out: &mut Vec<String>) {
    match lang {
        Lang::Rust => match rng.below(3) {
            0 => {
                out.push(format!("pub struct {} {{", type_name(rng)));
                for _ in 0..2 + rng.below(4) {
                    out.push(format!("    pub {}: u32,", rng.ident()));
                }
                out.push("}".to_string());
            }
            1 => {
                out.push(format!(
                    "pub fn {}({}: &{}) -> usize {{",
                    rng.ident(),
                    rng.word(),
                    type_name(rng)
                ));
                for _ in 0..3 + rng.below(6) {
                    out.push(statement(lang, rng, "    "));
                }
                out.push(format!("    {}", rng.ident()));
                out.push("}".to_string());
            }
            _ => {
                out.push(format!("pub enum {} {{", type_name(rng)));
                for _ in 0..2 + rng.below(3) {
                    out.push(format!("    {},", type_name(rng)));
                }
                out.push("}".to_string());
            }
        },
        Lang::Go => {
            if rng.below(2) == 0 {
                out.push(format!("type {} struct {{", type_name(rng)));
                for _ in 0..2 + rng.below(4) {
                    out.push(format!("\t{} int", rng.ident()));
                }
                out.push("}".to_string());
            } else {
                out.push(format!(
                    "func {}({} *{}) int {{",
                    rng.ident(),
                    rng.word(),
                    type_name(rng)
                ));
                for _ in 0..3 + rng.below(6) {
                    out.push(statement(lang, rng, "\t"));
                }
                out.push("}".to_string());
            }
        }
        Lang::Markdown => {
            out.push(format!("## {} {}", type_name(rng), rng.word()));
            out.push(String::new());
            for _ in 0..2 + rng.below(4) {
                out.push(statement(lang, rng, ""));
            }
            out.push(String::new());
            for _ in 0..=rng.below(3) {
                out.push(format!("- {} handles the {}", rng.ident(), rng.word()));
            }
        }
        Lang::Yaml => {
            out.push(format!("{}:", rng.ident()));
            for _ in 0..2 + rng.below(4) {
                out.push(statement(lang, rng, "  "));
            }
            for _ in 0..rng.below(3) {
                out.push(format!("  - {}", rng.ident()));
            }
        }
        Lang::Json => {
            out.push(format!("  \"{}\": {{", rng.ident()));
            for _ in 0..2 + rng.below(4) {
                out.push(statement(lang, rng, "    "));
            }
            out.push("  },".to_string());
        }
    }
    out.push(String::new());
}

fn gen_file(lang: Lang, rng: &mut Rng, target_lines: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(target_lines + 16);
    if lang == Lang::Json {
        out.push("{".to_string());
    }
    while out.len() < target_lines {
        push_block(lang, rng, &mut out);
    }
    if lang == Lang::Json {
        out.push("}".to_string());
    }
    out
}

/// Apply roughly `pct`% line edits in short runs of replace/insert/delete.
fn mutate(lines: &mut Vec<String>, lang: Lang, rng: &mut Rng, pct: usize) {
    let target = (lines.len() * pct / 100).max(1);
    let mut edited = 0;
    while edited < target && !lines.is_empty() {
        let pos = rng.below(lines.len());
        let run = (1 + rng.below(3))
            .min(lines.len() - pos)
            .min(target - edited + 1);
        match rng.below(3) {
            0 => {
                for line in lines.iter_mut().skip(pos).take(run) {
                    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                    *line = statement(lang, rng, &indent);
                }
            }
            1 => {
                lines.drain(pos..pos + run);
            }
            _ => {
                for _ in 0..run {
                    lines.insert(pos, statement(lang, rng, "    "));
                }
            }
        }
        edited += run;
    }
}

fn write_lines(path: &Path, lines: &[String]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut content = lines.join("\n");
    content.push('\n');
    fs::write(path, content)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)
}

struct Namer {
    file_no: usize,
}

impl Namer {
    fn path(&mut self, rng: &mut Rng, lang: Lang) -> PathBuf {
        let dir = DIRS[rng.below(DIRS.len())];
        let word = rng.word();
        let no = self.file_no;
        self.file_no += 1;
        PathBuf::from(dir).join(format!("{word}_{no:03}.{}", lang.ext()))
    }
}

pub fn generate(scale: usize, seed: u64) -> io::Result<Corpus> {
    let root = std::env::temp_dir().join(format!("revisa-bench-s{scale}-{seed:x}"));
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    let left = root.join("left");
    let right = root.join("right");
    let mut rng = Rng::new(seed);
    let mut namer = Namer { file_no: 0 };
    let mut renames = Vec::new();

    let lang_at = |i: usize| LANG_CYCLE[i % LANG_CYCLE.len()];
    let size_at = |i: usize| SIZES[i % SIZES.len()];

    for i in 0..MODIFIED * scale {
        let lang = lang_at(i);
        let path = namer.path(&mut rng, lang);
        let base = gen_file(lang, &mut rng, size_at(i));
        write_lines(&left.join(&path), &base)?;
        let mut edited = base.clone();
        mutate(&mut edited, lang, &mut rng, MODIFIED_EDIT_PCT);
        write_lines(&right.join(&path), &edited)?;
    }

    for i in 0..EXACT_RENAMES * scale {
        let lang = lang_at(i + 1);
        let old = namer.path(&mut rng, lang);
        let new = namer.path(&mut rng, lang);
        let base = gen_file(lang, &mut rng, size_at(i));
        write_lines(&left.join(&old), &base)?;
        write_lines(&right.join(&new), &base)?;
        renames.push((old, new));
    }

    for i in 0..NEAR_RENAMES * scale {
        let lang = lang_at(i + 2);
        let old = namer.path(&mut rng, lang);
        let new = namer.path(&mut rng, lang);
        let base = gen_file(lang, &mut rng, size_at(i));
        write_lines(&left.join(&old), &base)?;
        let mut edited = base.clone();
        let pct = if i % 2 == 0 { 10 } else { 30 };
        mutate(&mut edited, lang, &mut rng, pct);
        write_lines(&right.join(&new), &edited)?;
        renames.push((old, new));
    }

    for i in 0..DELETED * scale {
        let lang = lang_at(i);
        let path = namer.path(&mut rng, lang);
        let lines = if i < SAME_SIZE * scale {
            SAME_SIZE_LINES
        } else {
            size_at(i)
        };
        write_lines(&left.join(&path), &gen_file(lang, &mut rng, lines))?;
    }

    for i in 0..ADDED * scale {
        let lang = lang_at(i + 3);
        let path = namer.path(&mut rng, lang);
        let lines = if i < SAME_SIZE * scale {
            SAME_SIZE_LINES
        } else {
            size_at(i)
        };
        write_lines(&right.join(&path), &gen_file(lang, &mut rng, lines))?;
    }

    for _ in 0..BINARY * scale {
        let path = namer.path(&mut rng, Lang::Rust).with_extension("bin");
        let mut old = vec![0u8; 4096];
        for b in &mut old {
            *b = (rng.next() & 0xff) as u8;
        }
        let mut new = old.clone();
        new[100] ^= 0xff;
        new[2000] ^= 0xff;
        write_bytes(&left.join(&path), &old)?;
        write_bytes(&right.join(&path), &new)?;
    }

    for _ in 0..scale {
        let path = namer.path(&mut rng, Lang::Rust);
        let base = gen_file(Lang::Rust, &mut rng, HUGE_LINES);
        write_lines(&left.join(&path), &base)?;
        let mut edited = base.clone();
        mutate(&mut edited, Lang::Rust, &mut rng, 5);
        write_lines(&right.join(&path), &edited)?;
    }

    let manifest: Vec<String> = renames
        .iter()
        .map(|(o, n)| format!("{}\t{}", o.display(), n.display()))
        .collect();
    write_lines(&root.join("ground-truth.txt"), &manifest)?;

    Ok(Corpus {
        left,
        right,
        renames: Some(renames),
    })
}
