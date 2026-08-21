use crate::domain::diff::{DiffStat, InlineSpan, LineDiff, diff_inline, diff_lines, diff_stat};
use crate::domain::file_pair::FilePair;
use crate::domain::file_tree::{FlatEntry, TreeNode, build_tree, flatten_tree};
use crate::domain::fold::{DiffMode, FoldState};
use crate::domain::hunk::{AlignedRow, Hunk, build_aligned_rows, extract_hunks};
use crate::domain::review_state::ReviewState;
use crate::highlight::{
    DiffBg, DiffBgColors, Highlighter, SpanStyle, StyleInterner, StyledRows, StyledSpan,
    compose_line,
};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Default number of context lines around each hunk.
const HUNK_CONTEXT: usize = 3;

/// Font variant availability, populated by `configure_fonts` at startup.
#[derive(Clone, Copy)]
pub struct FontVariants {
    pub has_bold: bool,
    pub has_italic: bool,
    pub has_bold_italic: bool,
}

/// Compact line-indexed view over owned content. Stores the full text once
/// and a vector of byte offsets marking line starts, avoiding per-line heap
/// allocations entirely. Two allocations total: one for the content `String`,
/// one for the offsets `Vec<u32>`.
#[derive(Clone)]
pub struct LineIndex {
    /// The full file content.
    content: String,
    /// Byte offsets of each line start. `offsets[i]` is the start of line `i`.
    /// An implicit sentinel at `content.len()` marks the end of the last line.
    offsets: Vec<u32>,
    /// Whether the content ends with a newline (cached to avoid per-access check).
    trailing_newline: bool,
}

impl LineIndex {
    /// Build a `LineIndex` from owned content.
    pub fn new(content: String) -> Self {
        let mut offsets = Vec::new();
        let mut pos = 0usize;
        for line in content.split('\n') {
            offsets.push(pos as u32);
            pos += line.len() + 1; // +1 for the '\n'
        }
        let trailing_newline = content.ends_with('\n');
        // Remove trailing empty line caused by trailing newline.
        if trailing_newline && offsets.len() > 1 {
            offsets.pop();
        }
        // Empty content → no lines.
        if content.is_empty() {
            offsets.clear();
        }
        Self {
            content,
            offsets,
            trailing_newline,
        }
    }

    /// Build an empty `LineIndex` (no content, no lines).
    pub fn empty() -> Self {
        Self {
            content: String::new(),
            offsets: Vec::new(),
            trailing_newline: false,
        }
    }

    /// Number of lines.
    #[inline]
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// Whether there are no lines.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Get line text by 0-based index. Returns `None` if out of bounds.
    #[inline]
    pub fn get(&self, idx: usize) -> Option<&str> {
        if idx >= self.offsets.len() {
            return None;
        }
        let start = self.offsets[idx] as usize;
        let end = if idx + 1 < self.offsets.len() {
            // Next line start minus 1 (the '\n')
            self.offsets[idx + 1] as usize - 1
        } else {
            // Last line: content may or may not end with '\n'
            if self.trailing_newline {
                self.content.len() - 1
            } else {
                self.content.len()
            }
        };
        Some(&self.content[start..end])
    }

    /// Get line text by 0-based index, returning `""` if out of bounds.
    #[inline]
    pub fn line(&self, idx: usize) -> &str {
        self.get(idx).unwrap_or("")
    }
}

/// Per-file computed diff data, cached after first load.
pub struct FileDiffData {
    // Arc-shared with `SearchableFileData` so search snapshots are cheap.
    pub old_lines: Arc<LineIndex>,
    pub new_lines: Arc<LineIndex>,
    pub aligned_rows: Arc<Vec<AlignedRow>>,
    pub hunks: Vec<Hunk>,
    /// Pre-computed styled spans per aligned row, per side (left, right).
    /// Indexed by row index. Empty vec for padding rows.
    pub left_styled: StyledRows,
    pub right_styled: StyledRows,
    /// De-duplicated styles referenced by `left_styled`/`right_styled`.
    pub styles: Vec<SpanStyle>,
    /// If set, this file was too large for full diff computation.
    pub too_large_message: Option<String>,
    /// Whether this file is binary (non-UTF-8 content).
    pub binary: bool,
    /// Fold state for collapsing unchanged regions.
    pub fold_state: FoldState,
}

impl FileDiffData {
    /// Ensure unified offsets are computed and return total view rows for the given mode.
    pub fn total_view_rows_for_mode(&mut self, mode: DiffMode) -> usize {
        self.fold_state
            .total_view_rows_for_mode(mode, &self.aligned_rows)
    }

    /// Ensure unified offsets are computed if the mode requires it.
    pub fn ensure_unified_offsets_if_needed(&mut self, mode: DiffMode) {
        if mode == DiffMode::Unified {
            self.fold_state.ensure_unified_offsets(&self.aligned_rows);
        }
    }

    /// Find the data-row index for a given 1-based line number on the new (right) side.
    /// Falls back to old (left) side if the file has no new lines (e.g., deleted file).
    /// Returns None if the line number is out of range.
    pub fn line_to_data_row(&self, line: usize) -> Option<usize> {
        if line == 0 {
            return None;
        }
        let line_idx = line - 1; // Convert to 0-based

        // Prefer new-side (right_line).
        if !self.new_lines.is_empty() {
            return self.aligned_rows.iter().position(|row| match row {
                AlignedRow::Both { right_line, .. } | AlignedRow::RightOnly { right_line } => {
                    *right_line == line_idx
                }
                AlignedRow::LeftOnly { .. } => false,
            });
        }
        // Fallback to old-side (left_line) for deleted files.
        self.aligned_rows.iter().position(|row| match row {
            AlignedRow::Both { left_line, .. } | AlignedRow::LeftOnly { left_line } => {
                *left_line == line_idx
            }
            AlignedRow::RightOnly { .. } => false,
        })
    }

    /// Inverse of [`Self::line_to_data_row`]: the 1-based new-side line number at
    /// `data_row`, scanning forward when that row has no new-side line (a deletion).
    /// Returns None if no new-side line exists at or after `data_row`.
    pub fn data_row_to_line(&self, data_row: usize) -> Option<usize> {
        self.aligned_rows
            .get(data_row..)?
            .iter()
            .find_map(|row| match row {
                AlignedRow::Both { right_line, .. } | AlignedRow::RightOnly { right_line } => {
                    Some(*right_line + 1)
                }
                AlignedRow::LeftOnly { .. } => None,
            })
    }

    /// Create a placeholder for files that exceed the size limit.
    pub fn too_large_placeholder(
        msg: &str,
        fold_ctx: usize,
        fold_exp: usize,
        fold_rh: usize,
    ) -> Self {
        Self {
            old_lines: Arc::new(LineIndex::empty()),
            new_lines: Arc::new(LineIndex::empty()),
            aligned_rows: Arc::new(vec![]),
            hunks: vec![],
            left_styled: StyledRows::default(),
            right_styled: StyledRows::default(),
            styles: vec![],
            too_large_message: Some(msg.to_string()),
            binary: false,
            fold_state: FoldState::new(0, &[], fold_ctx, fold_exp, fold_rh),
        }
    }

    pub fn binary_placeholder(fold_ctx: usize, fold_exp: usize, fold_rh: usize) -> Self {
        Self {
            old_lines: Arc::new(LineIndex::empty()),
            new_lines: Arc::new(LineIndex::empty()),
            aligned_rows: Arc::new(vec![]),
            hunks: vec![],
            left_styled: StyledRows::default(),
            right_styled: StyledRows::default(),
            styles: vec![],
            too_large_message: None,
            binary: true,
            fold_state: FoldState::new(0, &[], fold_ctx, fold_exp, fold_rh),
        }
    }
}

/// Scroll position and momentum state for the diff view.
#[derive(Default)]
pub struct ScrollState {
    /// Vertical scroll offset in pixels (sub-pixel for smooth scrolling).
    pub y: f32,
    /// Vertical scroll velocity in pixels/second (for momentum scrolling).
    pub vy: f32,
    /// If set, the diff view should center this view-row in the viewport on the next frame.
    pub pending_center_row: Option<usize>,
    /// Horizontal scroll offset in pixels.
    pub x: f32,
    /// Maximum allowed horizontal scroll (based on visible line widths).
    pub max_x: f32,
    /// True while the user is dragging the horizontal scrollbar thumb.
    pub h_scrollbar_drag: bool,
    /// Accumulated mouse wheel delta waiting to be drained (for smooth scrolling).
    pub pending_wheel_y: f32,
    /// If set, navigate to this 1-based source line once the diff data becomes available.
    /// Used when goto-line is requested before the background diff has finished.
    pub pending_goto_line: Option<usize>,
    /// Deferred search navigation (file, data_row), set when the target
    /// file's diff is still being computed; applied when it lands.
    pub pending_search_nav: Option<(usize, usize)>,
}

/// State for the "review complete" popup notification.
#[derive(Default)]
pub struct ReviewCompletePopup {
    /// Whether the popup should be shown.
    pub show: bool,
    /// Set when user dismisses the popup; prevents re-trigger until count drops below total.
    pub dismissed: bool,
    /// Whether the popup was open on the previous frame (for input gating).
    pub was_open: bool,
}

/// Top-level application state shared between all UI panels.
#[allow(clippy::struct_excessive_bools)]
pub struct AppState {
    pub file_pairs: Vec<FilePair>,
    pub review_state: ReviewState,
    /// Index into `file_pairs` of the currently selected file.
    pub selected_file: usize,
    /// Scroll position and momentum.
    pub scroll: ScrollState,
    /// Cached diff data per file (by index into file_pairs).
    pub diff_cache: HashMap<usize, FileDiffData>,
    /// Shared highlighter (loaded once).
    pub highlighter: Arc<Highlighter>,
    /// Whether the sidebar is visible.
    pub sidebar_visible: bool,
    /// Whether the help overlay is visible.
    pub help_open: bool,
    /// Per-file diff statistics (eagerly computed at startup).
    pub diff_stats: Vec<DiffStat>,
    /// File tree for sidebar rendering.
    pub file_tree: Vec<TreeNode>,
    /// Cached flattened tree (invalidated on toggle_dir).
    flat_tree_cache: Option<Vec<FlatEntry>>,
    /// Set to true when selected_file changes; consumed by sidebar to scroll into view.
    pub sidebar_scroll_to_selected: bool,
    /// Rect of the diff view area (set after panel layout, used for scroll hit-testing).
    pub diff_rect: Option<eframe::egui::Rect>,
    /// Instrumentation: full-corpus searches handed to the pool.
    #[cfg(feature = "dev-tools")]
    pub search_dispatches: u32,
    /// Instrumentation: search results actually applied to the UI state. The gap
    /// against `search_dispatches` is whole-corpus work computed and then discarded.
    #[cfg(feature = "dev-tools")]
    pub search_applies: u32,
    /// Background diff computation results channel.
    bg_results: std::sync::mpsc::Receiver<(usize, FileDiffData)>,
    /// Number of files fully computed (for progress display).
    pub files_computed: usize,
    /// Application settings (loaded from config file).
    pub settings: crate::domain::settings::Settings,
    /// Cached diff view rendering context (derived from settings, computed once).
    pub diff_view_ctx: crate::ui::diff_view::DiffViewCtx,
    /// Cross-frame cache of shaped line galleys; invalidated per file on diff
    /// recompute and wholesale on display-scale changes.
    pub galley_cache: std::cell::RefCell<crate::ui::diff_view::GalleyCache>,
    /// Header display strings for the selected file; rebuilt on selection change.
    pub header_cache: crate::ui::diff_view::HeaderCache,
    /// Status-bar labels; rebuilt on scroll/selection/fold transitions.
    pub status_bar_cache: crate::ui::status_bar::StatusBarCache,
    /// Editor command resolved once at startup:
    /// `behavior.editor`, then `$VISUAL`, then `$EDITOR`.
    pub editor_cmd: Option<String>,
    /// Timestamp when the path was last copied to clipboard (for feedback indicator).
    pub copied_at: Option<std::time::Instant>,
    /// Deferred clipboard copy (set by keybind handler, consumed by UI).
    pub pending_copy_path: Option<String>,
    /// Deferred picker open request (set by header icon click, consumed by main loop).
    pub pending_open_picker: bool,
    /// Whether the quick picker overlay is currently open.
    pub picker_open: bool,
    /// Consecutive frames where `pixels_per_point` has been stable.
    /// Rendering is deferred until this reaches a threshold, avoiding the
    /// layout shift caused by winit's Wayland backend reporting an incorrect
    /// initial scale factor.
    pub ppi_stable_frames: u8,
    /// Last observed native pixels-per-point (used to detect DPI changes).
    pub last_ppi: f32,
    /// Directories excluded from the UI (relative path prefixes).
    pub excluded_dirs: Vec<std::path::PathBuf>,
    /// Per-file exclusion bitset (indexed by file_pairs index). Rebuilt on exclusion change.
    pub excluded_files: Vec<bool>,
    /// Cached visible file count (invalidated on exclusion change).
    pub cached_visible_count: usize,
    /// Cached number of reviewed files (excluding excluded dirs).
    pub cached_reviewed_count: usize,
    /// Cached total lines added across all files.
    pub cached_total_added: usize,
    /// Cached total lines deleted across all files.
    pub cached_total_deleted: usize,
    /// Current diff display mode (side-by-side or unified).
    pub diff_mode: crate::domain::fold::DiffMode,
    /// One-shot: `default_diff_mode = "auto"` awaits resolution on the first
    /// rendered frame (once window geometry is trustworthy).
    pub auto_diff_mode_pending: bool,
    /// Review-complete popup state.
    pub review_complete: ReviewCompletePopup,
    /// Search state for Ctrl+F find-in-diff.
    pub search: crate::domain::search::SearchState,
    /// Background search results channel (query, per-file results).
    bg_search_results: Arc<Mutex<crate::domain::search::BgSearchResults>>,
    /// egui context for requesting repaints from background threads.
    ctx: Option<eframe::egui::Context>,
    /// File indices currently being force-computed (prevents duplicate spawns).
    pub force_computing: HashSet<usize>,
    /// Receivers for force-computed diff results (file index + one-shot channel).
    force_receivers: Vec<(usize, std::sync::mpsc::Receiver<FileDiffData>)>,
}

impl AppState {
    /// Shared constructor logic for `new` and `new_for_test`.
    #[allow(clippy::too_many_arguments)]
    fn build(
        file_pairs: Vec<FilePair>,
        review_state: ReviewState,
        highlighter: Arc<Highlighter>,
        settings: crate::domain::settings::Settings,
        diff_view_ctx: crate::ui::diff_view::DiffViewCtx,
        diff_cache: HashMap<usize, FileDiffData>,
        diff_stats: Vec<DiffStat>,
        bg_results: std::sync::mpsc::Receiver<(usize, FileDiffData)>,
        files_computed: usize,
        ctx: Option<eframe::egui::Context>,
    ) -> Self {
        let total_added: usize = diff_stats.iter().map(|s| s.added).sum();
        let total_deleted: usize = diff_stats.iter().map(|s| s.deleted).sum();
        let file_tree = build_tree(
            &file_pairs
                .iter()
                .enumerate()
                .map(|(i, fp)| (i, fp.relative_path.as_path()))
                .collect::<Vec<_>>(),
        );
        let file_pairs_len = file_pairs.len();
        Self {
            file_pairs,
            review_state,
            selected_file: 0,
            scroll: ScrollState {
                max_x: f32::MAX,
                ..Default::default()
            },
            diff_cache,
            sidebar_visible: settings.behavior.sidebar_width > 0.0,
            help_open: false,
            diff_stats,
            flat_tree_cache: Some(flatten_tree(&file_tree, 0)),
            file_tree,
            sidebar_scroll_to_selected: true,
            diff_rect: None,
            #[cfg(feature = "dev-tools")]
            search_dispatches: 0,
            #[cfg(feature = "dev-tools")]
            search_applies: 0,
            bg_results,
            files_computed,
            diff_view_ctx,
            galley_cache: std::cell::RefCell::new(crate::ui::diff_view::GalleyCache::default()),
            header_cache: crate::ui::diff_view::HeaderCache::default(),
            status_bar_cache: crate::ui::status_bar::StatusBarCache::default(),
            editor_cmd: settings
                .behavior
                .editor
                .clone()
                .or_else(|| std::env::var("VISUAL").ok())
                .or_else(|| std::env::var("EDITOR").ok()),
            copied_at: None,
            pending_copy_path: None,
            pending_open_picker: false,
            picker_open: false,
            ppi_stable_frames: 0,
            last_ppi: 0.0,
            excluded_dirs: Vec::new(),
            excluded_files: vec![false; file_pairs_len],
            cached_visible_count: file_pairs_len,
            cached_reviewed_count: 0,
            cached_total_added: total_added,
            cached_total_deleted: total_deleted,
            diff_mode: match settings.behavior.default_diff_mode {
                crate::domain::settings::DiffModePreference::Unified => DiffMode::Unified,
                // Auto starts side-by-side provisionally; resolved before first render.
                _ => DiffMode::SideBySide,
            },
            auto_diff_mode_pending: settings.behavior.default_diff_mode
                == crate::domain::settings::DiffModePreference::Auto,
            review_complete: ReviewCompletePopup::default(),
            highlighter,
            settings,
            search: crate::domain::search::SearchState::default(),
            bg_search_results: Arc::new(Mutex::new(None)),
            ctx,
            force_computing: HashSet::new(),
            force_receivers: Vec::new(),
        }
    }

    pub fn new(
        file_pairs: Vec<FilePair>,
        review_state: ReviewState,
        theme_path: Option<&Path>,
        ctx: eframe::egui::Context,
        settings: crate::domain::settings::Settings,
        font_variants: FontVariants,
    ) -> Self {
        let highlighter = Arc::new(Highlighter::new(theme_path));

        // Phase 1: Read all files and compute diffs in parallel (one pass).
        // Results are cached for reuse in phases 2 & 3.
        let phase1: Vec<PairDiff> = file_pairs.par_iter().map(read_and_diff).collect();

        let diff_stats: Vec<DiffStat> = phase1.iter().map(|p| p.stat).collect();

        // Phase 2: Compute full diff data for file 0 using cached contents + diff.
        let mut diff_cache = HashMap::new();
        let fold_ctx = settings.behavior.fold_context;
        let fold_exp = settings.behavior.fold_expand_step;
        let fold_rh = settings.behavior.fold_row_height;
        let mut cached_contents: Vec<Option<PairDiff>> = phase1.into_iter().map(Some).collect();

        if !file_pairs.is_empty() {
            let first = cached_contents[0]
                .take()
                .expect("phase 1 populated entry 0");
            let data = if first.binary {
                FileDiffData::binary_placeholder(fold_ctx, fold_exp, fold_rh)
            } else {
                let filename = file_pairs[0].relative_path.to_string_lossy();
                let old_filename = file_pairs[0]
                    .old_relative_path
                    .as_ref()
                    .map_or_else(|| filename.clone(), |p| p.to_string_lossy());
                compute_diff_from_contents_with_diff(
                    first.old_content,
                    first.new_content,
                    Some(first.diff),
                    &filename,
                    &old_filename,
                    &highlighter,
                    &settings,
                    false,
                )
            };
            diff_cache.insert(0, data);
        }

        // Phase 3: Spawn background thread to compute remaining files using cached data.
        // Pre-filter: create placeholders for large and binary files so the bg thread skips them.
        let max_lines = settings.behavior.max_diff_lines;
        for (i, entry) in cached_contents.iter_mut().enumerate().skip(1) {
            if let Some(p) = entry.as_ref() {
                let (old_lines, new_lines) = (p.old_line_count, p.new_line_count);
                if p.binary {
                    diff_cache.insert(
                        i,
                        FileDiffData::binary_placeholder(fold_ctx, fold_exp, fold_rh),
                    );
                    *entry = None;
                } else if max_lines > 0 && (old_lines > max_lines || new_lines > max_lines) {
                    let msg = format!(
                        "File too large for diff ({old_lines} / {new_lines} lines, limit {max_lines})",
                    );
                    diff_cache.insert(
                        i,
                        FileDiffData::too_large_placeholder(&msg, fold_ctx, fold_exp, fold_rh),
                    );
                    *entry = None;
                }
            }
        }

        let (bg_tx, bg_results) = std::sync::mpsc::channel();
        // files_computed includes file 0 (eager) + too-large files (placeholders created above).
        // Force-computed files don't re-increment this; they replace existing placeholders.
        let files_computed =
            usize::from(!file_pairs.is_empty()) + diff_cache.len().saturating_sub(1);

        if file_pairs.len() > 1 {
            let hl = Arc::clone(&highlighter);
            let pairs: Vec<FilePair> = file_pairs.clone();
            let bg_settings = settings.clone();
            let bg_ctx = ctx.clone();
            std::thread::spawn(move || {
                cached_contents
                    .into_par_iter()
                    .enumerate()
                    .skip(1)
                    .for_each(|(i, cached)| {
                        let Some(cached) = cached else {
                            return; // skipped (binary/too large, already has placeholder)
                        };
                        let filename = pairs[i].relative_path.to_string_lossy();
                        let old_filename = pairs[i]
                            .old_relative_path
                            .as_ref()
                            .map_or_else(|| filename.clone(), |p| p.to_string_lossy());
                        let data = compute_diff_from_contents_with_diff(
                            cached.old_content,
                            cached.new_content,
                            Some(cached.diff),
                            &filename,
                            &old_filename,
                            &hl,
                            &bg_settings,
                            false,
                        );
                        let _ = bg_tx.send((i, data));
                        // Deadline instead of immediate: completions within the
                        // window share one repaint, so the load doesn't drive
                        // the frame loop at full rate just for the progress text.
                        bg_ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    });
            });
        }

        let diff_view_ctx =
            crate::ui::diff_view::DiffViewCtx::from_settings(&settings, font_variants);

        Self::build(
            file_pairs,
            review_state,
            highlighter,
            settings,
            diff_view_ctx,
            diff_cache,
            diff_stats,
            bg_results,
            files_computed,
            Some(ctx),
        )
    }

    /// Dispatch a force-compute for a file that exceeded the size limit.
    /// Re-reads from disk and computes the full diff on a background thread.
    pub fn dispatch_force_compute(&mut self, idx: usize) {
        if self.force_computing.contains(&idx) || idx >= self.file_pairs.len() {
            return;
        }
        self.force_computing.insert(idx);

        let (tx, rx) = std::sync::mpsc::channel();
        self.force_receivers.push((idx, rx));

        let pair = self.file_pairs[idx].clone();
        let hl = Arc::clone(&self.highlighter);
        let settings = self.settings.clone();
        let ctx = self.ctx.clone();

        rayon::spawn(move || {
            let read = read_and_diff(&pair);
            let data = if read.binary {
                FileDiffData::binary_placeholder(
                    settings.behavior.fold_context,
                    settings.behavior.fold_expand_step,
                    settings.behavior.fold_row_height,
                )
            } else {
                let filename = pair.relative_path.to_string_lossy();
                let old_filename = pair
                    .old_relative_path
                    .as_ref()
                    .map_or_else(|| filename.clone(), |p| p.to_string_lossy());
                compute_diff_from_contents_with_diff(
                    read.old_content,
                    read.new_content,
                    Some(read.diff),
                    &filename,
                    &old_filename,
                    &hl,
                    &settings,
                    true, // skip size guard
                )
            };
            let _ = tx.send(data);
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    /// Drain any completed background diff results into the cache.
    pub fn poll_background(&mut self) {
        let mut had_new = false;
        while let Ok((idx, data)) = self.bg_results.try_recv() {
            had_new = true;
            self.files_computed += 1;
            // Don't cache results for excluded files.
            if !self.is_file_excluded(idx) {
                self.diff_cache.insert(idx, data);
                self.galley_cache.borrow_mut().invalidate_file(idx);
            }
        }

        // Poll force-compute receivers (one-shot channels for "Calculate anyway").
        self.force_receivers.retain(|(idx, rx)| {
            match rx.try_recv() {
                Ok(data) => {
                    had_new = true;
                    self.force_computing.remove(idx);
                    self.diff_cache.insert(*idx, data);
                    self.galley_cache.borrow_mut().invalidate_file(*idx);
                    false // receiver consumed, remove
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => true, // still pending
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.force_computing.remove(idx); // clean up on sender panic
                    false
                }
            }
        });

        // Collect background search results. This runs before the dispatch check so
        // that a landing result can hand straight off to a pending dispatch within
        // the same frame — nothing else guarantees another repaint to come back for.
        let bg_search = self
            .bg_search_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((query, file_results)) = bg_search {
            #[cfg(feature = "dev-tools")]
            {
                self.search_applies += 1;
            }
            self.search
                .apply_background_results(query, file_results, self.selected_file);
            self.search
                .rebuild_display_cache(&self.diff_cache, &self.file_pairs);
        }

        // Newly arrived diffs are not covered by the current results.
        if had_new && self.search.open && !self.search.query.is_empty() {
            self.search.mark_corpus_grown();
        }

        if let Some(deadline) = self.search.pending_dispatch_at {
            let now = std::time::Instant::now();
            if now < deadline {
                if let Some(ctx) = &self.ctx {
                    ctx.request_repaint_after(deadline - now);
                }
            } else {
                self.search.pending_dispatch_at = None;
                if self.search.query.is_empty() {
                    self.search.clear_results();
                } else {
                    // Re-arms itself if a search is still in flight.
                    self.dispatch_background_search();
                }
            }
        }
    }

    /// Dispatch a search to the background thread pool.
    ///
    /// Only one search runs at a time: overlapping passes race on the single result
    /// slot, so the loser's whole-corpus work is discarded unread, and whichever
    /// finishes last wins even if it snapshotted fewer files. If a search is already
    /// in flight this arms the pending deadline instead, and the collect step in
    /// `poll_background` picks it up as soon as the in-flight one lands.
    pub fn dispatch_background_search(&mut self) {
        use crate::domain::search::{SearchableFileData, compute_file_matches};

        if self.search.searching {
            self.search.pending_dispatch_at = Some(std::time::Instant::now());
            return;
        }

        #[cfg(feature = "dev-tools")]
        {
            self.search_dispatches += 1;
        }

        let query = self.search.query.clone();
        self.search.searching = true;

        // Snapshot only the data needed for searching (lightweight, no styled spans).
        let file_data: Vec<(usize, SearchableFileData)> = self
            .diff_cache
            .iter()
            .filter(|&(&idx, _)| !self.is_file_excluded(idx))
            .map(|(&idx, data)| (idx, SearchableFileData::from_diff_data(data)))
            .collect();

        let bg_results = Arc::clone(&self.bg_search_results);
        let ctx = self.ctx.clone();

        rayon::spawn(move || {
            let results: HashMap<usize, Vec<crate::domain::search::SearchMatch>> = file_data
                .par_iter()
                .map(|(idx, data)| (*idx, compute_file_matches(*idx, data, &query)))
                .collect();

            *bg_results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((query, results));

            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    /// Create state for testing (no background thread, no egui context needed).
    #[cfg(test)]
    pub fn new_for_test(file_pairs: Vec<FilePair>, review_state: ReviewState) -> Self {
        let highlighter = Arc::new(Highlighter::new(None));
        let defaults = crate::domain::settings::Settings::default();
        let phase1: Vec<PairDiff> = file_pairs.iter().map(read_and_diff).collect();
        let diff_stats: Vec<DiffStat> = phase1.iter().map(|p| p.stat).collect();
        let mut diff_cache = HashMap::new();
        for (i, read) in phase1.into_iter().enumerate() {
            let data = if read.binary {
                FileDiffData::binary_placeholder(
                    defaults.behavior.fold_context,
                    defaults.behavior.fold_expand_step,
                    defaults.behavior.fold_row_height,
                )
            } else {
                let filename = file_pairs[i].relative_path.to_string_lossy();
                let old_filename = file_pairs[i]
                    .old_relative_path
                    .as_ref()
                    .map_or_else(|| filename.clone(), |p| p.to_string_lossy());
                compute_diff_from_contents_with_diff(
                    read.old_content,
                    read.new_content,
                    Some(read.diff),
                    &filename,
                    &old_filename,
                    &highlighter,
                    &defaults,
                    false,
                )
            };
            diff_cache.insert(i, data);
        }
        let files_computed = file_pairs.len();
        let diff_view_ctx = crate::ui::diff_view::DiffViewCtx::from_settings(
            &defaults,
            FontVariants {
                has_bold: false,
                has_italic: false,
                has_bold_italic: false,
            },
        );
        Self::build(
            file_pairs,
            review_state,
            highlighter,
            defaults,
            diff_view_ctx,
            diff_cache,
            diff_stats,
            std::sync::mpsc::channel().1,
            files_computed,
            None,
        )
    }

    /// Get the currently selected file pair. Panics in debug builds if out of bounds.
    pub fn selected_pair(&self) -> &FilePair {
        debug_assert!(
            self.selected_file < self.file_pairs.len(),
            "selected_file {} out of bounds (len {})",
            self.selected_file,
            self.file_pairs.len(),
        );
        &self.file_pairs[self.selected_file]
    }

    /// Recompute cached review/visible counts. Call after review state or exclusion changes.
    pub fn refresh_review_counts(&mut self) {
        self.cached_visible_count = self.review_state.total_count_excluding(&self.excluded_dirs);
        self.cached_reviewed_count = self
            .review_state
            .reviewed_count_excluding(&self.excluded_dirs);
        // Recompute aggregate diff stats for visible (non-excluded) files.
        let mut added = 0usize;
        let mut deleted = 0usize;
        for (i, stat) in self.diff_stats.iter().enumerate() {
            if !self.is_file_excluded(i) {
                added += stat.added;
                deleted += stat.deleted;
            }
        }
        self.cached_total_added = added;
        self.cached_total_deleted = deleted;

        let all_reviewed = self.cached_visible_count > 0
            && self.cached_reviewed_count == self.cached_visible_count;
        if all_reviewed && !self.review_complete.dismissed {
            self.review_complete.show = true;
        }
        if !all_reviewed {
            self.review_complete.dismissed = false;
        }
    }

    /// Rebuild the per-file exclusion bitset from `excluded_dirs`.
    #[cfg(test)]
    fn rebuild_excluded_files(&mut self) {
        for (i, fp) in self.file_pairs.iter().enumerate() {
            self.excluded_files[i] = self
                .excluded_dirs
                .iter()
                .any(|dir| fp.relative_path.starts_with(dir));
        }
    }

    /// Open the currently selected file in `editor_cmd` (resolved at startup).
    /// Only opens the new (right) file — opening deleted files is not useful.
    pub fn open_in_editor(&self) {
        let pair = self.selected_pair();
        let Some(file_path) = pair.right_path.as_ref() else {
            // Deleted file — nothing useful to open.
            return;
        };

        // Resolve symlinks so the editor opens the real file.
        let resolved = std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.clone());

        let Some(cmd) = self.editor_cmd.as_deref() else {
            eprintln!(
                "No editor configured. Set behavior.editor in config.toml, or $VISUAL / $EDITOR."
            );
            return;
        };

        let line = self.editor_target_line().unwrap_or(1);
        let argv = crate::domain::editor::build_argv(cmd, &resolved, line);
        if argv.is_empty() {
            return;
        }

        match std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .spawn()
        {
            Ok(_) => {}
            Err(e) => eprintln!("Failed to open editor '{cmd}': {e}"),
        }
    }

    /// New-side line number to open the editor at: the vertical middle of the
    /// on-screen rows, since editors center the line they are given.
    /// None when the diff isn't available yet or the file has no new-side lines.
    fn editor_target_line(&self) -> Option<usize> {
        let diff_data = self.diff_cache.get(&self.selected_file)?;
        // Unified row mapping panics without computed offsets; fall back to
        // side-by-side for files that have not been rendered in unified mode yet.
        let effective_mode = match self.diff_mode {
            DiffMode::Unified if diff_data.fold_state.unified_offsets_ref().is_some() => {
                DiffMode::Unified
            }
            _ => DiffMode::SideBySide,
        };
        let total_view_rows = match effective_mode {
            DiffMode::SideBySide => diff_data.fold_state.total_view_rows(),
            DiffMode::Unified => diff_data
                .fold_state
                .total_view_rows_unified_cached()
                .unwrap_or_else(|| diff_data.fold_state.total_view_rows()),
        };
        let line_at = |view_row| {
            diff_data.data_row_to_line(
                diff_data
                    .fold_state
                    .view_row_to_data_row_for_mode(view_row, effective_mode),
            )
        };

        // Middle of the visible *rows*, not of the viewport: a view shorter than
        // the viewport (short file, or most of it folded away) leaves slack below
        // the last row, and aiming at the viewport's center overshoots into it.
        let top = self.scroll_row().min(total_view_rows.saturating_sub(1));
        let bottom = (top + self.viewport_rows().unwrap_or(0)).min(total_view_rows);
        let middle = usize::midpoint(top, bottom);
        // Past the last new-side line (a file ending in deletions) — the top row
        // is the best remaining anchor.
        line_at(middle).or_else(|| line_at(top))
    }

    /// Get the cached flattened file tree, recomputing if needed.
    pub fn flat_tree(&self) -> &[FlatEntry] {
        // Cache is always populated after construction; treat absence as empty.
        static EMPTY: &[FlatEntry] = &[];
        self.flat_tree_cache.as_deref().unwrap_or(EMPTY)
    }

    /// Invalidate and rebuild the flattened tree cache (call after toggle_dir).
    pub fn rebuild_flat_tree(&mut self) {
        self.flat_tree_cache = Some(flatten_tree(&self.file_tree, 0));
    }

    /// Select a file by index. Resets scroll position.
    pub fn select_file(&mut self, idx: usize) {
        if idx < self.file_pairs.len() {
            self.selected_file = idx;
            self.scroll.y = 0.0;
            self.scroll.vy = 0.0;
            self.scroll.x = 0.0;
            self.sidebar_scroll_to_selected = true;
            self.search.rebuild_render_map(idx);
        }
    }

    /// Current top visible row index (derived from scroll_y).
    pub fn scroll_row(&self) -> usize {
        (self.scroll.y / self.settings.behavior.line_height) as usize
    }

    /// Number of rows that fit in the diff viewport.
    /// None before the first panel layout, when `diff_rect` is still unset.
    pub fn viewport_rows(&self) -> Option<usize> {
        self.diff_rect.map(|r| {
            (r.height() / self.settings.behavior.line_height)
                .floor()
                .max(1.0) as usize
        })
    }

    /// Set scroll to a specific row (snapping to row boundary, cancels momentum).
    pub fn scroll_to_row(&mut self, row: usize) {
        self.scroll.y = row as f32 * self.settings.behavior.line_height;
        self.scroll.vy = 0.0;
    }

    /// Toggle between SideBySide and Unified diff modes, preserving scroll position.
    pub fn toggle_diff_mode(&mut self) {
        let old_mode = self.diff_mode;
        let new_mode = match old_mode {
            crate::domain::fold::DiffMode::SideBySide => crate::domain::fold::DiffMode::Unified,
            crate::domain::fold::DiffMode::Unified => crate::domain::fold::DiffMode::SideBySide,
        };
        let scroll_row = self.scroll_row();
        self.diff_mode = new_mode;
        // Scroll preservation needs the diff data; skip it while the file is
        // still computing (mode toggle is global, so it still applies).
        if let Some(diff_data) = self.diff_cache.get_mut(&self.selected_file) {
            let data_row = diff_data
                .fold_state
                .view_row_to_data_row_for_mode(scroll_row, old_mode);
            diff_data.ensure_unified_offsets_if_needed(new_mode);
            let new_view_row = diff_data
                .fold_state
                .data_to_view_row_for_mode(data_row, new_mode)
                .unwrap_or(0);
            self.scroll.y = new_view_row as f32 * self.settings.behavior.line_height;
        }
        self.scroll.vy = 0.0;
        self.scroll.pending_wheel_y = 0.0;
    }

    /// Clamp scroll_y to valid range for current file.
    /// `viewport_rows` is the number of fully visible rows in the viewport.
    pub fn clamp_scroll_y(&mut self, total_rows: usize, viewport_rows: usize) {
        let last_start_row = total_rows.saturating_sub(viewport_rows);
        let max_y = last_start_row as f32 * self.settings.behavior.line_height;
        self.scroll.y = self.scroll.y.clamp(0.0, max_y);

        if self.scroll.y <= 0.0 || self.scroll.y >= max_y {
            self.scroll.vy = 0.0;
        }
    }

    /// Mark current file as reviewed and select next unreviewed file.
    /// With nothing left to advance to, re-surface the review-complete popup.
    pub fn mark_reviewed_and_next(&mut self) {
        let path = self.selected_pair().relative_path.clone();
        self.review_state.mark_reviewed(&path);
        let next = self
            .review_state
            .next_unreviewed_after_excluding(&path, &self.excluded_dirs)
            .cloned();
        if next.is_none() {
            self.review_complete.dismissed = false;
        }
        self.refresh_review_counts();
        if let Some(next) = next
            && let Some(idx) = self
                .file_pairs
                .iter()
                .position(|fp| fp.relative_path == next)
        {
            self.select_file(idx);
        }
    }

    /// Select next file (wrapping around), skipping excluded files.
    pub fn select_next_file(&mut self) {
        if self.file_pairs.is_empty() {
            return;
        }
        let n = self.file_pairs.len();
        for offset in 1..=n {
            let idx = (self.selected_file + offset) % n;
            if !self.is_file_excluded(idx) {
                self.select_file(idx);
                return;
            }
        }
    }

    /// Select previous file (wrapping around), skipping excluded files.
    pub fn select_prev_file(&mut self) {
        if self.file_pairs.is_empty() {
            return;
        }
        let n = self.file_pairs.len();
        for offset in 1..=n {
            let idx = (self.selected_file + n - offset) % n;
            if !self.is_file_excluded(idx) {
                self.select_file(idx);
                return;
            }
        }
    }

    /// Check if a file is excluded via directory exclusion. O(1) lookup.
    pub fn is_file_excluded(&self, idx: usize) -> bool {
        self.excluded_files.get(idx).copied().unwrap_or(false)
    }

    /// Exclude a directory (hides all files under it, frees their diff cache).
    /// Returns `false` if excluding would hide all files (operation rejected).
    pub fn exclude_dir(&mut self, dir: &std::path::Path) -> bool {
        // Check that at least one file would remain visible.
        let would_remain = self.file_pairs.iter().enumerate().any(|(i, _)| {
            !self.is_file_excluded(i) && !self.file_pairs[i].relative_path.starts_with(dir)
        });
        if !would_remain {
            return false;
        }
        // Dedup check.
        if !self.excluded_dirs.iter().any(|d| d == dir) {
            self.excluded_dirs.push(dir.to_path_buf());
        }
        // Rebuild bitset and evict diff_cache only for newly-excluded files.
        for (i, fp) in self.file_pairs.iter().enumerate() {
            if !self.excluded_files[i] && fp.relative_path.starts_with(dir) {
                self.excluded_files[i] = true;
                self.diff_cache.remove(&i);
            }
        }
        // If currently selected file is now excluded, select next visible.
        if self.is_file_excluded(self.selected_file) {
            self.select_next_file();
        }
        self.refresh_review_counts();
        true
    }
}

/// Read a file as a string, returning `None` if the file is binary (contains null bytes).
fn read_text_file(path: &Path) -> Option<String> {
    match fs::read(path) {
        Ok(bytes) => {
            // Check the first 8 KiB for null bytes (same heuristic as git).
            let check_len = bytes.len().min(8192);
            if bytes[..check_len].contains(&0) {
                return None; // binary
            }
            String::from_utf8(bytes).ok()
        }
        Err(e) => {
            eprintln!("warning: failed to read {}: {e}", path.display());
            Some(String::new())
        }
    }
}

/// Both sides of a file pair, read and line-diffed. The raw input to
/// [`compute_diff_from_contents_with_diff`], which turns it into the rendered
/// [`FileDiffData`].
pub struct PairDiff {
    pub stat: DiffStat,
    pub old_content: String,
    pub new_content: String,
    pub diff: LineDiff,
    /// Either side was non-UTF-8 or contained null bytes.
    pub binary: bool,
    /// Line counts for the `max_diff_lines` guard. Counted here, on the worker
    /// that already holds the content, so the guard doesn't rescan every file
    /// serially on the UI thread.
    pub old_line_count: usize,
    pub new_line_count: usize,
}

/// Read both sides of a pair and compute their line diff and stats.
pub fn read_and_diff(pair: &FilePair) -> PairDiff {
    let old_result = pair.left_path.as_ref().map(|p| read_text_file(p));
    let new_result = pair.right_path.as_ref().map(|p| read_text_file(p));

    // If either side is binary, flag the whole pair as binary.
    let binary = matches!(old_result, Some(None)) || matches!(new_result, Some(None));

    let old_content = old_result.flatten().unwrap_or_default();
    let new_content = new_result.flatten().unwrap_or_default();

    let old_line_count = old_content.lines().count();
    let new_line_count = new_content.lines().count();

    let diff = diff_lines(&old_content, &new_content);
    let stat = diff_stat(&diff.ops);
    PairDiff {
        stat,
        old_content,
        new_content,
        diff,
        binary,
        old_line_count,
        new_line_count,
    }
}

/// Core diff computation. If `pre_diff` is `Some`, reuses the pre-computed diff ops
/// instead of running `diff_lines` again.
#[allow(clippy::too_many_arguments)]
pub fn compute_diff_from_contents_with_diff(
    old_content: String,
    new_content: String,
    pre_diff: Option<LineDiff>,
    filename: &str,
    old_filename: &str,
    highlighter: &Highlighter,
    settings: &crate::domain::settings::Settings,
    skip_size_guard: bool,
) -> FileDiffData {
    let colors = &settings.colors;
    let diff_bg_colors = DiffBgColors {
        added: colors.bg_added.to_rgba(),
        removed: colors.bg_removed.to_rgba(),
    };
    let inline_old_bg = colors.bg_inline_removed.to_rgba();
    let inline_new_bg = colors.bg_inline_added.to_rgba();
    let fold_ctx = settings.behavior.fold_context;
    let fold_exp = settings.behavior.fold_expand_step;
    let fold_rh = settings.behavior.fold_row_height;
    let max_lines = settings.behavior.max_diff_lines;

    // Collect lines once for both the size guard and later use.
    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    // Size guard: skip expensive computation for very large files.
    if !skip_size_guard
        && max_lines > 0
        && (old_lines.len() > max_lines || new_lines.len() > max_lines)
    {
        let msg = format!(
            "File too large for diff ({} / {} lines, limit {max_lines})",
            old_lines.len(),
            new_lines.len(),
        );
        return FileDiffData::too_large_placeholder(&msg, fold_ctx, fold_exp, fold_rh);
    }

    let diff = pre_diff.unwrap_or_else(|| diff_lines(&old_content, &new_content));
    let aligned_rows = build_aligned_rows(&diff.ops);
    let hunks = extract_hunks(&aligned_rows, &diff.ops, HUNK_CONTEXT);

    let old_highlight = if old_content.is_empty() {
        Highlighter::empty_file()
    } else {
        highlighter.highlight_file(&old_content, old_filename)
    };
    let new_highlight = if new_content.is_empty() {
        Highlighter::empty_file()
    } else {
        highlighter.highlight_file(&new_content, filename)
    };

    let default_fg = highlighter.default_fg();

    // Pre-compute styled spans for all aligned rows.
    let mut interner = StyleInterner::default();
    let mut left_styled = StyledRows::with_row_capacity(aligned_rows.len());
    let mut right_styled = StyledRows::with_row_capacity(aligned_rows.len());

    for row in &aligned_rows {
        match row {
            AlignedRow::Both {
                left_line,
                right_line,
                modified,
            } => {
                let old_text = old_lines.get(*left_line).copied().unwrap_or("");
                let new_text = new_lines.get(*right_line).copied().unwrap_or("");
                // Word-level spans for a changed pair; both sides below use them.
                let inline = modified.then(|| diff_inline(old_text, new_text));

                // Left side.
                let old_syntax = old_highlight
                    .lines
                    .get(*left_line)
                    .map_or(&[] as &[_], std::vec::Vec::as_slice);
                let diff_bg_l = if *modified {
                    DiffBg::ModifiedOld
                } else {
                    DiffBg::None
                };
                let mut styled_l = compose_line(
                    old_syntax,
                    diff_bg_l,
                    old_text.len(),
                    default_fg,
                    diff_bg_colors,
                );
                if let Some((old_spans, _)) = inline.as_ref() {
                    apply_inline_highlights_inplace(
                        &mut styled_l,
                        old_spans,
                        true,
                        inline_old_bg,
                        inline_new_bg,
                    );
                }
                left_styled.push_row(&styled_l, &mut interner);

                // Right side.
                let new_syntax = new_highlight
                    .lines
                    .get(*right_line)
                    .map_or(&[] as &[_], std::vec::Vec::as_slice);
                let diff_bg_r = if *modified {
                    DiffBg::ModifiedNew
                } else {
                    DiffBg::None
                };
                let mut styled_r = compose_line(
                    new_syntax,
                    diff_bg_r,
                    new_text.len(),
                    default_fg,
                    diff_bg_colors,
                );
                if let Some((_, new_spans)) = inline.as_ref() {
                    apply_inline_highlights_inplace(
                        &mut styled_r,
                        new_spans,
                        false,
                        inline_old_bg,
                        inline_new_bg,
                    );
                }
                right_styled.push_row(&styled_r, &mut interner);
            }
            AlignedRow::LeftOnly { left_line } => {
                let old_text = old_lines.get(*left_line).copied().unwrap_or("");
                let old_syntax = old_highlight
                    .lines
                    .get(*left_line)
                    .map_or(&[] as &[_], std::vec::Vec::as_slice);
                left_styled.push_row(
                    &compose_line(
                        old_syntax,
                        DiffBg::Removed,
                        old_text.len(),
                        default_fg,
                        diff_bg_colors,
                    ),
                    &mut interner,
                );
                right_styled.push_row(&[], &mut interner);
            }
            AlignedRow::RightOnly { right_line } => {
                let new_text = new_lines.get(*right_line).copied().unwrap_or("");
                let new_syntax = new_highlight
                    .lines
                    .get(*right_line)
                    .map_or(&[] as &[_], std::vec::Vec::as_slice);
                left_styled.push_row(&[], &mut interner);
                right_styled.push_row(
                    &compose_line(
                        new_syntax,
                        DiffBg::Added,
                        new_text.len(),
                        default_fg,
                        diff_bg_colors,
                    ),
                    &mut interner,
                );
            }
        }
    }

    let fold_state = FoldState::new(aligned_rows.len(), &hunks, fold_ctx, fold_exp, fold_rh);

    FileDiffData {
        old_lines: Arc::new(LineIndex::new(old_content)),
        new_lines: Arc::new(LineIndex::new(new_content)),
        aligned_rows: Arc::new(aligned_rows),
        hunks,
        left_styled,
        right_styled,
        styles: interner.finish(),
        too_large_message: None,
        binary: false,
        fold_state,
    }
}

/// Apply inline diff highlights in-place, splitting spans at highlight boundaries.
fn apply_inline_highlights_inplace(
    styled: &mut Vec<StyledSpan>,
    inline: &[InlineSpan],
    is_old_side: bool,
    inline_old_bg: [u8; 4],
    inline_new_bg: [u8; 4],
) {
    use crate::domain::diff::InlineTag;

    let highlight_bg = if is_old_side {
        inline_old_bg
    } else {
        inline_new_bg
    };
    let target_tag = if is_old_side {
        InlineTag::Delete
    } else {
        InlineTag::Insert
    };

    let highlight_ranges: Vec<std::ops::Range<usize>> = inline
        .iter()
        .filter(|s| s.tag == target_tag)
        .map(|s| s.range.clone())
        .collect();

    if highlight_ranges.is_empty() {
        return;
    }

    let mut result = Vec::with_capacity(styled.len() + highlight_ranges.len());
    for span in styled.drain(..) {
        let mut remaining_start = span.range.start;
        let remaining_end = span.range.end;

        for hr in &highlight_ranges {
            if hr.end <= remaining_start || hr.start >= remaining_end {
                continue;
            }
            if remaining_start < hr.start {
                result.push(StyledSpan {
                    range: remaining_start..hr.start,
                    ..span.clone()
                });
                remaining_start = hr.start;
            }
            let hl_end = hr.end.min(remaining_end);
            result.push(StyledSpan {
                range: remaining_start..hl_end,
                fg: span.fg,
                bg: highlight_bg,
                bold: span.bold,
                italic: span.italic,
            });
            remaining_start = hl_end;
        }

        if remaining_start < remaining_end {
            result.push(StyledSpan {
                range: remaining_start..remaining_end,
                ..span
            });
        }
    }

    *styled = result;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::file_pair::FileChangeKind;
    use std::path::PathBuf;

    /// Diff data with every row treated as changed, so nothing is folded and
    /// view rows map 1:1 to data rows.
    fn make_diff_data(rows: Vec<AlignedRow>) -> FileDiffData {
        let n = rows.len();
        make_diff_data_with_hunks(rows, vec![Hunk { row_range: 0..n }])
    }

    fn make_diff_data_with_hunks(rows: Vec<AlignedRow>, hunks: Vec<Hunk>) -> FileDiffData {
        let n = rows.len();
        FileDiffData {
            old_lines: Arc::new(LineIndex::empty()),
            new_lines: Arc::new(LineIndex::empty()),
            aligned_rows: Arc::new(rows),
            fold_state: FoldState::new(n, &hunks, 3, 20, 2),
            hunks,
            left_styled: crate::highlight::StyledRows::default(),
            right_styled: crate::highlight::StyledRows::default(),
            styles: Vec::new(),
            too_large_message: None,
            binary: false,
        }
    }

    // ── data_row_to_line ─────────────────────────────────────────────

    #[test]
    fn data_row_to_line_both_and_right_only() {
        let data = make_diff_data(vec![
            AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: false,
            },
            AlignedRow::RightOnly { right_line: 1 },
        ]);
        assert_eq!(data.data_row_to_line(0), Some(1));
        assert_eq!(data.data_row_to_line(1), Some(2));
    }

    #[test]
    fn data_row_to_line_scans_past_deletions() {
        let data = make_diff_data(vec![
            AlignedRow::LeftOnly { left_line: 0 },
            AlignedRow::LeftOnly { left_line: 1 },
            AlignedRow::Both {
                left_line: 2,
                right_line: 0,
                modified: false,
            },
        ]);
        assert_eq!(data.data_row_to_line(0), Some(1));
    }

    #[test]
    fn data_row_to_line_none_when_only_deletions_remain() {
        let data = make_diff_data(vec![
            AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: false,
            },
            AlignedRow::LeftOnly { left_line: 1 },
        ]);
        assert_eq!(data.data_row_to_line(1), None);
    }

    #[test]
    fn data_row_to_line_none_out_of_range() {
        assert_eq!(make_diff_data(vec![]).data_row_to_line(0), None);
        let data = make_diff_data(vec![AlignedRow::RightOnly { right_line: 0 }]);
        assert_eq!(data.data_row_to_line(5), None);
    }

    #[test]
    fn data_row_to_line_round_trips_with_line_to_data_row() {
        let rows = vec![
            AlignedRow::Both {
                left_line: 0,
                right_line: 0,
                modified: false,
            },
            AlignedRow::LeftOnly { left_line: 1 },
            AlignedRow::RightOnly { right_line: 1 },
            AlignedRow::Both {
                left_line: 2,
                right_line: 2,
                modified: true,
            },
        ];
        let mut data = make_diff_data(rows);
        data.new_lines = Arc::new(LineIndex::new("a\nb\nc".to_string()));
        for line in 1..=3 {
            let row = data
                .line_to_data_row(line)
                .expect("line is within the new-side range");
            assert_eq!(data.data_row_to_line(row), Some(line));
        }
    }

    // ── editor_target_line ───────────────────────────────────────────

    /// A viewport `rows` tall, in the row units `scroll_row` uses.
    fn set_viewport_rows(state: &mut AppState, rows: usize) {
        let height = rows as f32 * state.settings.behavior.line_height;
        state.diff_rect = Some(eframe::egui::Rect::from_min_size(
            eframe::egui::pos2(0.0, 0.0),
            eframe::egui::vec2(800.0, height),
        ));
    }

    #[test]
    fn editor_target_line_without_viewport_uses_top_row() {
        let mut state = make_state(&["a.rs"]);
        state.diff_cache.insert(
            0,
            make_diff_data(vec![
                AlignedRow::Both {
                    left_line: 0,
                    right_line: 0,
                    modified: false,
                },
                AlignedRow::LeftOnly { left_line: 1 },
                AlignedRow::RightOnly { right_line: 1 },
            ]),
        );
        assert_eq!(state.editor_target_line(), Some(1));
        state.scroll_to_row(2);
        assert_eq!(state.editor_target_line(), Some(2));
        // Viewport top is a deleted line — scans forward to the next new-side line.
        state.scroll_to_row(1);
        assert_eq!(state.editor_target_line(), Some(2));
    }

    #[test]
    fn editor_target_line_aims_at_viewport_middle() {
        let mut state = make_state(&["a.rs"]);
        let rows: Vec<AlignedRow> = (0..100)
            .map(|i| AlignedRow::Both {
                left_line: i,
                right_line: i,
                modified: true,
            })
            .collect();
        state.diff_cache.insert(0, make_diff_data(rows));
        set_viewport_rows(&mut state, 20);

        // Rows 0..20 visible → row 10 → line 11.
        assert_eq!(state.editor_target_line(), Some(11));
        state.scroll_to_row(30);
        assert_eq!(state.editor_target_line(), Some(41));
    }

    #[test]
    fn editor_target_line_ignores_viewport_slack_below_short_file() {
        let mut state = make_state(&["a.rs"]);
        let rows: Vec<AlignedRow> = (0..5)
            .map(|i| AlignedRow::RightOnly { right_line: i })
            .collect();
        state.diff_cache.insert(0, make_diff_data(rows));
        // Viewport much taller than the file: aim at the middle of the 5 rows,
        // not at the viewport's center (which would clamp to the last line).
        set_viewport_rows(&mut state, 40);
        assert_eq!(state.editor_target_line(), Some(3));
    }

    #[test]
    fn editor_target_line_ignores_viewport_slack_at_end_of_file() {
        let mut state = make_state(&["a.rs"]);
        let rows: Vec<AlignedRow> = (0..30)
            .map(|i| AlignedRow::RightOnly { right_line: i })
            .collect();
        state.diff_cache.insert(0, make_diff_data(rows));
        set_viewport_rows(&mut state, 20);
        // Scrolled past the last full screenful: rows 20..30 are on screen, so
        // the middle is row 25, not row 29.
        state.scroll_to_row(20);
        assert_eq!(state.editor_target_line(), Some(26));
    }

    #[test]
    fn editor_target_line_targets_hunk_when_folds_shrink_the_view() {
        let mut state = make_state(&["a.rs"]);
        // A long file whose only change sits in the middle: everything else
        // folds away, leaving a view far shorter than the viewport.
        let rows: Vec<AlignedRow> = (0..90)
            .map(|i| AlignedRow::Both {
                left_line: i,
                right_line: i,
                modified: (44..46).contains(&i),
            })
            .collect();
        state.diff_cache.insert(
            0,
            make_diff_data_with_hunks(rows, vec![Hunk { row_range: 44..46 }]),
        );
        set_viewport_rows(&mut state, 40);
        // Visible: leading fold, context lines 42..44, the change on 45..46,
        // context 47..49, trailing fold. The middle lands on the change.
        assert_eq!(state.editor_target_line(), Some(46));
    }

    #[test]
    fn editor_target_line_falls_back_when_middle_has_no_new_side() {
        let mut state = make_state(&["a.rs"]);
        let mut rows = vec![AlignedRow::Both {
            left_line: 0,
            right_line: 0,
            modified: false,
        }];
        // File ends in a long run of deletions: the middle row has no new-side
        // line at or after it, so the top row's line is used.
        rows.extend((1..40).map(|i| AlignedRow::LeftOnly { left_line: i }));
        state.diff_cache.insert(0, make_diff_data(rows));
        set_viewport_rows(&mut state, 20);
        assert_eq!(state.editor_target_line(), Some(1));
    }

    #[test]
    fn editor_target_line_none_without_diff_data() {
        let state = make_state(&["a.rs"]);
        assert_eq!(state.editor_target_line(), None);
    }

    fn make_pairs(names: &[&str]) -> Vec<FilePair> {
        names
            .iter()
            .map(|n| FilePair {
                relative_path: PathBuf::from(n),
                old_relative_path: None,
                kind: FileChangeKind::Modified,
                left_path: None,
                right_path: None,
                left_mode: None,
                right_mode: None,
            })
            .collect()
    }

    fn make_state(names: &[&str]) -> AppState {
        let pairs = make_pairs(names);
        let paths: Vec<PathBuf> = pairs.iter().map(|p| p.relative_path.clone()).collect();
        let review = ReviewState::new(paths);
        AppState::new_for_test(pairs, review)
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn select_file_resets_scroll() {
        let mut s = make_state(&["a.rs", "b.rs", "c.rs"]);
        s.scroll.y = 42.0 * 20.0;
        s.scroll.x = 100.0;
        s.select_file(2);
        assert_eq!(s.selected_file, 2);
        assert_eq!(s.scroll.y, 0.0);
        assert_eq!(s.scroll.x, 0.0);
    }

    #[test]
    fn select_file_out_of_bounds_is_noop() {
        let mut s = make_state(&["a.rs"]);
        s.select_file(99);
        assert_eq!(s.selected_file, 0);
    }

    #[test]
    fn select_next_wraps() {
        let mut s = make_state(&["a.rs", "b.rs", "c.rs"]);
        s.select_file(2);
        s.select_next_file();
        assert_eq!(s.selected_file, 0);
    }

    #[test]
    fn select_prev_wraps() {
        let mut s = make_state(&["a.rs", "b.rs", "c.rs"]);
        // selected_file starts at 0
        s.select_prev_file();
        assert_eq!(s.selected_file, 2);
    }

    #[test]
    fn select_next_empty_is_noop() {
        let mut s = make_state(&[]);
        s.select_next_file(); // should not panic
        assert_eq!(s.selected_file, 0);
    }

    #[test]
    fn select_prev_empty_is_noop() {
        let mut s = make_state(&[]);
        s.select_prev_file(); // should not panic
        assert_eq!(s.selected_file, 0);
    }

    #[test]
    fn mark_reviewed_and_next_advances() {
        let mut s = make_state(&["a.rs", "b.rs", "c.rs"]);
        // Mark a.rs reviewed, should advance to b.rs
        s.mark_reviewed_and_next();
        assert!(s.review_state.is_reviewed(Path::new("a.rs")));
        assert_eq!(s.selected_file, 1);
    }

    #[test]
    fn mark_reviewed_and_next_all_reviewed() {
        let mut s = make_state(&["a.rs", "b.rs"]);
        s.review_state.mark_reviewed(Path::new("a.rs"));
        s.review_state.mark_reviewed(Path::new("b.rs"));
        // All reviewed — should stay on current file
        let before = s.selected_file;
        s.mark_reviewed_and_next();
        assert_eq!(s.selected_file, before);
    }

    #[test]
    fn mark_reviewed_and_next_retriggers_dismissed_popup() {
        let mut s = make_state(&["a.rs", "b.rs"]);
        s.review_state.mark_reviewed(Path::new("a.rs"));
        s.review_state.mark_reviewed(Path::new("b.rs"));
        s.refresh_review_counts();
        // Popup dismissed via "Go back".
        s.review_complete.show = false;
        s.review_complete.dismissed = true;

        s.mark_reviewed_and_next();
        assert!(s.review_complete.show);
        assert!(!s.review_complete.dismissed);
    }

    #[test]
    fn mark_reviewed_and_next_keeps_popup_closed_while_files_remain() {
        let mut s = make_state(&["a.rs", "b.rs"]);
        s.mark_reviewed_and_next();
        assert_eq!(s.selected_file, 1);
        assert!(!s.review_complete.show);
    }

    #[test]
    fn is_file_excluded_matches_prefix() {
        let mut s = make_state(&["src/a.rs", "src/b.rs", "tests/c.rs"]);
        s.excluded_dirs.push(PathBuf::from("src"));
        s.rebuild_excluded_files();
        assert!(s.is_file_excluded(0));
        assert!(s.is_file_excluded(1));
        assert!(!s.is_file_excluded(2));
    }

    #[test]
    fn select_next_skips_excluded() {
        let mut s = make_state(&["a.rs", "src/b.rs", "c.rs"]);
        s.excluded_dirs.push(PathBuf::from("src"));
        s.rebuild_excluded_files();
        // Start at a.rs (idx 0), next should skip src/b.rs (idx 1) -> c.rs (idx 2)
        s.select_next_file();
        assert_eq!(s.selected_file, 2);
    }

    #[test]
    fn select_prev_skips_excluded() {
        let mut s = make_state(&["a.rs", "src/b.rs", "c.rs"]);
        s.excluded_dirs.push(PathBuf::from("src"));
        s.rebuild_excluded_files();
        s.select_file(2);
        // prev from c.rs should skip src/b.rs -> a.rs
        s.select_prev_file();
        assert_eq!(s.selected_file, 0);
    }

    #[test]
    fn select_next_wraps_skipping_excluded() {
        let mut s = make_state(&["a.rs", "src/b.rs"]);
        s.excluded_dirs.push(PathBuf::from("src"));
        s.rebuild_excluded_files();
        // Next from a.rs should wrap back to a.rs (only non-excluded file)
        s.select_next_file();
        assert_eq!(s.selected_file, 0);
    }

    #[test]
    fn exclude_dir_selects_next_visible() {
        let mut s = make_state(&["src/a.rs", "tests/b.rs"]);
        // Selected is idx 0 (src/a.rs)
        assert_eq!(s.selected_file, 0);
        s.exclude_dir(Path::new("src"));
        // Should auto-select tests/b.rs (idx 1)
        assert_eq!(s.selected_file, 1);
    }

    #[test]
    fn exclude_dir_evicts_diff_cache() {
        let mut s = make_state(&["src/a.rs", "b.rs"]);
        // Fake a cache entry
        s.diff_cache.insert(
            0,
            crate::app::FileDiffData {
                old_lines: Arc::new(LineIndex::empty()),
                new_lines: Arc::new(LineIndex::empty()),
                aligned_rows: Arc::new(vec![]),
                hunks: vec![],
                left_styled: StyledRows::default(),
                right_styled: StyledRows::default(),
                styles: vec![],
                too_large_message: None,
                binary: false,
                fold_state: crate::domain::fold::FoldState::new(0, &[], 3, 5, 20),
            },
        );
        assert!(s.diff_cache.contains_key(&0));
        s.exclude_dir(Path::new("src"));
        assert!(!s.diff_cache.contains_key(&0));
    }

    #[test]
    fn exclude_dir_rejects_if_all_would_be_excluded() {
        let mut s = make_state(&["src/a.rs", "src/b.rs"]);
        // Excluding "src" would hide everything — should be rejected.
        assert!(!s.exclude_dir(Path::new("src")));
        assert!(s.excluded_dirs.is_empty());
    }

    #[test]
    fn exclude_dir_rejects_when_already_partially_excluded() {
        let mut s = make_state(&["src/a.rs", "tests/b.rs", "lib/c.rs"]);
        assert!(s.exclude_dir(Path::new("src")));
        assert!(s.exclude_dir(Path::new("tests")));
        // Excluding "lib" would hide the last visible file — should be rejected.
        assert!(!s.exclude_dir(Path::new("lib")));
        assert!(!s.excluded_dirs.contains(&PathBuf::from("lib")));
    }

    // ── LineIndex tests ──────────────────────────────────────────────

    #[test]
    fn line_index_basic() {
        let idx = LineIndex::new("hello\nworld\nfoo".to_string());
        assert_eq!(idx.len(), 3);
        assert_eq!(idx.line(0), "hello");
        assert_eq!(idx.line(1), "world");
        assert_eq!(idx.line(2), "foo");
        assert_eq!(idx.get(3), None);
        assert_eq!(idx.line(3), "");
    }

    #[test]
    fn line_index_trailing_newline() {
        let idx = LineIndex::new("a\nb\n".to_string());
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.line(0), "a");
        assert_eq!(idx.line(1), "b");
    }

    #[test]
    fn line_index_single_line() {
        let idx = LineIndex::new("only line".to_string());
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.line(0), "only line");
    }

    #[test]
    fn line_index_single_line_trailing_newline() {
        let idx = LineIndex::new("only line\n".to_string());
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.line(0), "only line");
    }

    #[test]
    fn line_index_empty() {
        let idx = LineIndex::new(String::new());
        assert_eq!(idx.len(), 0);
        assert!(idx.is_empty());
        assert_eq!(idx.line(0), "");
    }

    #[test]
    fn line_index_empty_lines() {
        let idx = LineIndex::new("\n\n".to_string());
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.line(0), "");
        assert_eq!(idx.line(1), "");
    }

    #[test]
    fn line_index_matches_str_lines() {
        let content = "first\nsecond\nthird\nfourth";
        let idx = LineIndex::new(content.to_string());
        let expected: Vec<&str> = content.lines().collect();
        for (i, exp) in expected.iter().enumerate() {
            assert_eq!(idx.line(i), *exp, "mismatch at line {i}");
        }
        assert_eq!(idx.len(), expected.len());
    }

    /// The `AppState::new` pre-filter, which substitutes placeholders for
    /// oversized and binary files so the background pass skips them. It runs
    /// only for indices >= 1 (file 0 is computed eagerly), and it reads the
    /// line counts carried on `PairDiff` rather than rescanning the content.
    #[test]
    fn prefilter_substitutes_placeholders_past_file_zero() {
        use crate::domain::file_pair::walk_and_pair;
        use crate::domain::review_state::ReviewState;

        let dir = tempfile::tempdir().expect("tempdir");
        let (left, right) = (dir.path().join("left"), dir.path().join("right"));
        std::fs::create_dir_all(&left).expect("create left");
        std::fs::create_dir_all(&right).expect("create right");
        let write = |root: &Path, name: &str, bytes: &[u8]| {
            std::fs::write(root.join(name), bytes).expect("write fixture");
        };

        // Names chosen so the small file sorts first: the other two then land
        // past index 0, which is the only place the pre-filter looks.
        write(&left, "a_small.txt", b"one\ntwo\nthree\n");
        write(&right, "a_small.txt", b"one\nTWO\nthree\n");
        let big_old = "old line\n".repeat(20);
        let big_new = "new line\n".repeat(20);
        write(&left, "b_large.txt", big_old.as_bytes());
        write(&right, "b_large.txt", big_new.as_bytes());
        write(&left, "c_binary.bin", b"\x00\x01\x02payload\x00");
        write(&right, "c_binary.bin", b"\x00\x01\x03payload\x00");

        // Rename detection is irrelevant here (every file exists on both
        // sides), so pin the limit rather than shelling out to git config.
        let pairs = walk_and_pair(
            &left,
            &right,
            false,
            crate::domain::settings::RenameLimit::Fixed(0),
        )
        .expect("walk");
        let index_of = |name: &str| {
            pairs
                .iter()
                .position(|p| p.relative_path == Path::new(name))
                .unwrap_or_else(|| panic!("{name} missing from pairs"))
        };
        let (small, large, binary) = (
            index_of("a_small.txt"),
            index_of("b_large.txt"),
            index_of("c_binary.bin"),
        );
        assert_eq!(small, 0, "small file must sort first for this test to bite");

        let defaults = crate::domain::settings::Settings::default();
        let settings = crate::domain::settings::Settings {
            behavior: crate::domain::settings::BehaviorSettings {
                max_diff_lines: 10,
                ..defaults.behavior
            },
            ..defaults
        };
        let review = ReviewState::new(pairs.iter().map(|p| p.relative_path.clone()).collect());
        let state = AppState::new(
            pairs,
            review,
            None,
            eframe::egui::Context::default(),
            settings,
            FontVariants {
                has_bold: false,
                has_italic: false,
                has_bold_italic: false,
            },
        );

        // Under the limit and computed eagerly — a real diff, not a placeholder.
        let small_data = &state.diff_cache[&small];
        assert!(small_data.too_large_message.is_none());
        assert!(!small_data.binary);
        assert!(!small_data.aligned_rows.is_empty());

        // 20 lines a side against a limit of 10. The counts in the message come
        // from `PairDiff`, so a wrong count here means the wrong field was read.
        let msg = state.diff_cache[&large]
            .too_large_message
            .as_deref()
            .expect("oversized file should get a too-large placeholder");
        assert!(msg.contains("20 / 20"), "unexpected message: {msg}");
        assert!(msg.contains("limit 10"), "unexpected message: {msg}");
        assert!(state.diff_cache[&large].aligned_rows.is_empty());

        // Binary is checked before size, and yields a different placeholder.
        assert!(state.diff_cache[&binary].binary);
        assert!(state.diff_cache[&binary].too_large_message.is_none());
    }
}
