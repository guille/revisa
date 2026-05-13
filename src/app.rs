use crate::domain::diff::{DiffStat, InlineSpan, LineDiff, diff_inline, diff_lines, diff_stat};
use crate::domain::file_pair::FilePair;
use crate::domain::file_tree::{FlatEntry, TreeNode, build_tree, flatten_tree};
use crate::domain::fold::{DiffMode, FoldState};
use crate::domain::hunk::{AlignedRow, Hunk, build_aligned_rows, extract_hunks};
use crate::domain::review_state::ReviewState;
use crate::highlight::{DiffBg, DiffBgColors, Highlighter, StyledSpan, compose_line};
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

/// Inline diff data for a single modified line pair.
struct InlineDiffPair {
    old_spans: Vec<InlineSpan>,
    new_spans: Vec<InlineSpan>,
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
    pub old_lines: LineIndex,
    pub new_lines: LineIndex,
    pub aligned_rows: Vec<AlignedRow>,
    pub hunks: Vec<Hunk>,
    /// Pre-computed styled spans per aligned row, per side (left, right).
    /// Indexed by row index. Empty vec for padding rows.
    pub left_styled: Vec<Vec<StyledSpan>>,
    pub right_styled: Vec<Vec<StyledSpan>>,
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

    /// Create a placeholder for files that exceed the size limit.
    pub fn too_large_placeholder(
        msg: &str,
        fold_ctx: usize,
        fold_exp: usize,
        fold_rh: usize,
    ) -> Self {
        Self {
            old_lines: LineIndex::empty(),
            new_lines: LineIndex::empty(),
            aligned_rows: vec![],
            hunks: vec![],
            left_styled: vec![],
            right_styled: vec![],
            too_large_message: Some(msg.to_string()),
            binary: false,
            fold_state: FoldState::new(0, &[], fold_ctx, fold_exp, fold_rh),
        }
    }

    pub fn binary_placeholder(fold_ctx: usize, fold_exp: usize, fold_rh: usize) -> Self {
        Self {
            old_lines: LineIndex::empty(),
            new_lines: LineIndex::empty(),
            aligned_rows: vec![],
            hunks: vec![],
            left_styled: vec![],
            right_styled: vec![],
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
    /// Background diff computation results channel.
    bg_results: std::sync::mpsc::Receiver<(usize, FileDiffData)>,
    /// Number of files fully computed (for progress display).
    pub files_computed: usize,
    /// Application settings (loaded from config file).
    pub settings: crate::domain::settings::Settings,
    /// Cached diff view rendering context (derived from settings, computed once).
    pub diff_view_ctx: crate::ui::diff_view::DiffViewCtx,
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
            bg_results,
            files_computed,
            diff_view_ctx,
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
            diff_mode: settings.behavior.default_diff_mode,
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
        let phase1: Vec<(DiffStat, String, String, LineDiff, bool)> =
            file_pairs.par_iter().map(read_and_diff).collect();

        let diff_stats: Vec<DiffStat> = phase1.iter().map(|(s, _, _, _, _)| *s).collect();

        // Phase 2: Compute full diff data for file 0 using cached contents + diff.
        let mut diff_cache = HashMap::new();
        let fold_ctx = settings.behavior.fold_context;
        let fold_exp = settings.behavior.fold_expand_step;
        let fold_rh = settings.behavior.fold_row_height;
        let mut cached_contents: Vec<Option<(String, String, LineDiff, bool)>> = phase1
            .into_iter()
            .map(|(_, old, new, diff, bin)| Some((old, new, diff, bin)))
            .collect();

        if !file_pairs.is_empty() {
            let (old, new, diff, is_binary) = cached_contents[0]
                .take()
                .expect("phase 1 populated entry 0");
            let data = if is_binary {
                FileDiffData::binary_placeholder(fold_ctx, fold_exp, fold_rh)
            } else {
                let filename = file_pairs[0].relative_path.to_string_lossy();
                let old_filename = file_pairs[0]
                    .old_relative_path
                    .as_ref()
                    .map_or_else(|| filename.clone(), |p| p.to_string_lossy());
                compute_diff_from_contents_with_diff(
                    old,
                    new,
                    Some(diff),
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
            if let Some((old, new, _, is_binary)) = entry.as_ref() {
                if *is_binary {
                    diff_cache.insert(
                        i,
                        FileDiffData::binary_placeholder(fold_ctx, fold_exp, fold_rh),
                    );
                    *entry = None;
                } else if max_lines > 0 {
                    let old_lines = old.lines().count();
                    let new_lines = new.lines().count();
                    if old_lines > max_lines || new_lines > max_lines {
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
                        let Some((old, new, diff, _is_binary)) = cached else {
                            return; // skipped (binary/too large, already has placeholder)
                        };
                        let filename = pairs[i].relative_path.to_string_lossy();
                        let old_filename = pairs[i]
                            .old_relative_path
                            .as_ref()
                            .map_or_else(|| filename.clone(), |p| p.to_string_lossy());
                        let data = compute_diff_from_contents_with_diff(
                            old,
                            new,
                            Some(diff),
                            &filename,
                            &old_filename,
                            &hl,
                            &bg_settings,
                            false,
                        );
                        let _ = bg_tx.send((i, data));
                        bg_ctx.request_repaint();
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
            let (_, old, new, diff, is_binary) = read_and_diff(&pair);
            let data = if is_binary {
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
                    old,
                    new,
                    Some(diff),
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
            }
        }

        // Poll force-compute receivers (one-shot channels for "Calculate anyway").
        self.force_receivers.retain(|(idx, rx)| {
            match rx.try_recv() {
                Ok(data) => {
                    had_new = true;
                    self.force_computing.remove(idx);
                    self.diff_cache.insert(*idx, data);
                    false // receiver consumed, remove
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => true, // still pending
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.force_computing.remove(idx); // clean up on sender panic
                    false
                }
            }
        });

        // If new diffs arrived and search is active, re-dispatch search to cover new files.
        if had_new && self.search.open && !self.search.query.is_empty() {
            self.dispatch_background_search();
        }

        // Check debounce timer — dispatch search if enough time has elapsed.
        if let Some(changed_at) = self.search.query_changed_at {
            let debounce = std::time::Duration::from_millis(200);
            let elapsed = changed_at.elapsed();
            if elapsed >= debounce {
                self.search.query_changed_at = None;
                if self.search.query.is_empty() {
                    self.search.clear_results();
                } else {
                    self.dispatch_background_search();
                }
            } else if let Some(ctx) = &self.ctx {
                // Schedule a repaint for when the debounce timer expires.
                ctx.request_repaint_after(debounce.saturating_sub(elapsed));
            }
        }

        // Collect background search results.
        let mut bg_search = self
            .bg_search_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((query, file_results)) = bg_search.take() {
            self.search
                .apply_background_results(query, file_results, self.selected_file);
            self.search
                .rebuild_display_cache(&self.diff_cache, &self.file_pairs);
        }
    }

    /// Dispatch a search to the background thread pool.
    pub fn dispatch_background_search(&mut self) {
        use crate::domain::search::{SearchableFileData, compute_file_matches};

        let query = self.search.query.clone();
        self.search.dispatched_query.clone_from(&query);
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
        let phase1: Vec<(DiffStat, String, String, LineDiff, bool)> =
            file_pairs.iter().map(read_and_diff).collect();
        let diff_stats: Vec<DiffStat> = phase1.iter().map(|(s, _, _, _, _)| *s).collect();
        let mut diff_cache = HashMap::new();
        for (i, (_, old, new, diff, is_binary)) in phase1.into_iter().enumerate() {
            let data = if is_binary {
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
                    old,
                    new,
                    Some(diff),
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

    /// Open the currently selected file in an external editor.
    /// Uses `settings.behavior.editor`, falling back to `$VISUAL` then `$EDITOR`.
    /// Only opens the new (right) file — opening deleted files is not useful.
    pub fn open_in_editor(&self) {
        let pair = self.selected_pair();
        let Some(file_path) = pair.right_path.as_ref() else {
            // Deleted file — nothing useful to open.
            return;
        };

        // Resolve symlinks so the editor opens the real file.
        let resolved = std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.clone());

        let editor_cmd = self
            .settings
            .behavior
            .editor
            .clone()
            .or_else(|| std::env::var("VISUAL").ok())
            .or_else(|| std::env::var("EDITOR").ok());

        let Some(cmd) = editor_cmd else {
            eprintln!(
                "No editor configured. Set behavior.editor in config.toml, or $VISUAL / $EDITOR."
            );
            return;
        };

        // Split command, respecting single/double quotes (e.g. `'/usr/bin/my editor' --wait`).
        let parts = split_shell_words(&cmd);
        if parts.is_empty() {
            return;
        }

        match std::process::Command::new(&parts[0])
            .args(&parts[1..])
            .arg(&resolved)
            .spawn()
        {
            Ok(_) => {}
            Err(e) => eprintln!("Failed to open editor '{cmd}': {e}"),
        }
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
        let diff_data = self
            .diff_cache
            .get_mut(&self.selected_file)
            .expect("selected file always present in diff_cache");
        let data_row = diff_data
            .fold_state
            .view_row_to_data_row_for_mode(scroll_row, old_mode);
        diff_data.ensure_unified_offsets_if_needed(new_mode);
        let new_view_row = diff_data
            .fold_state
            .data_to_view_row_for_mode(data_row, new_mode)
            .unwrap_or(0);
        self.diff_mode = new_mode;
        let line_height = self.settings.behavior.line_height;
        self.scroll.y = new_view_row as f32 * line_height;
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
    pub fn mark_reviewed_and_next(&mut self) {
        let path = self.selected_pair().relative_path.clone();
        self.review_state.mark_reviewed(&path);
        self.refresh_review_counts();
        if let Some(next) = self
            .review_state
            .next_unreviewed_after_excluding(&path, &self.excluded_dirs)
        {
            let next = next.clone();
            if let Some(idx) = self
                .file_pairs
                .iter()
                .position(|fp| fp.relative_path == next)
            {
                self.select_file(idx);
            }
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

/// Read file contents and compute diff + stats in one pass.
/// Returns (stat, old_content, new_content, diff, is_binary) so callers can reuse the data.
pub fn read_and_diff(pair: &FilePair) -> (DiffStat, String, String, LineDiff, bool) {
    let old_result = pair.left_path.as_ref().map(|p| read_text_file(p));
    let new_result = pair.right_path.as_ref().map(|p| read_text_file(p));

    // If either side is binary, flag the whole pair as binary.
    let is_binary = matches!(old_result, Some(None)) || matches!(new_result, Some(None));

    let old_content = old_result.flatten().unwrap_or_default();
    let new_content = new_result.flatten().unwrap_or_default();

    let diff = diff_lines(&old_content, &new_content);
    let stat = diff_stat(&diff.ops);
    (stat, old_content, new_content, diff, is_binary)
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

    // Compute inline (word-level) diffs for modified line pairs.
    // Indexed by aligned_row index for O(1) lookup and cache locality.
    let mut inline_diffs: Vec<Option<InlineDiffPair>> =
        (0..aligned_rows.len()).map(|_| None).collect();
    for (row_idx, row) in aligned_rows.iter().enumerate() {
        if let AlignedRow::Both {
            left_line,
            right_line,
            modified: true,
        } = row
        {
            let old_text = old_lines.get(*left_line).copied().unwrap_or("");
            let new_text = new_lines.get(*right_line).copied().unwrap_or("");
            let (old_spans, new_spans) = diff_inline(old_text, new_text);
            inline_diffs[row_idx] = Some(InlineDiffPair {
                old_spans,
                new_spans,
            });
        }
    }

    let default_fg = highlighter.default_fg();

    // Pre-compute styled spans for all aligned rows.
    let mut left_styled = Vec::with_capacity(aligned_rows.len());
    let mut right_styled = Vec::with_capacity(aligned_rows.len());

    for (row_idx, row) in aligned_rows.iter().enumerate() {
        match row {
            AlignedRow::Both {
                left_line,
                right_line,
                modified,
            } => {
                // Left side.
                let old_text = old_lines.get(*left_line).copied().unwrap_or("");
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
                if let Some(pair) = modified.then(|| inline_diffs[row_idx].as_ref()).flatten() {
                    apply_inline_highlights_inplace(
                        &mut styled_l,
                        &pair.old_spans,
                        true,
                        inline_old_bg,
                        inline_new_bg,
                    );
                }
                left_styled.push(styled_l);

                // Right side.
                let new_text = new_lines.get(*right_line).copied().unwrap_or("");
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
                if let Some(pair) = modified.then(|| inline_diffs[row_idx].as_ref()).flatten() {
                    apply_inline_highlights_inplace(
                        &mut styled_r,
                        &pair.new_spans,
                        false,
                        inline_old_bg,
                        inline_new_bg,
                    );
                }
                right_styled.push(styled_r);
            }
            AlignedRow::LeftOnly { left_line } => {
                let old_text = old_lines.get(*left_line).copied().unwrap_or("");
                let old_syntax = old_highlight
                    .lines
                    .get(*left_line)
                    .map_or(&[] as &[_], std::vec::Vec::as_slice);
                left_styled.push(compose_line(
                    old_syntax,
                    DiffBg::Removed,
                    old_text.len(),
                    default_fg,
                    diff_bg_colors,
                ));
                right_styled.push(vec![]);
            }
            AlignedRow::RightOnly { right_line } => {
                let new_text = new_lines.get(*right_line).copied().unwrap_or("");
                let new_syntax = new_highlight
                    .lines
                    .get(*right_line)
                    .map_or(&[] as &[_], std::vec::Vec::as_slice);
                left_styled.push(vec![]);
                right_styled.push(compose_line(
                    new_syntax,
                    DiffBg::Added,
                    new_text.len(),
                    default_fg,
                    diff_bg_colors,
                ));
            }
        }
    }

    let fold_state = FoldState::new(aligned_rows.len(), &hunks, fold_ctx, fold_exp, fold_rh);

    FileDiffData {
        old_lines: LineIndex::new(old_content),
        new_lines: LineIndex::new(new_content),
        aligned_rows,
        hunks,
        left_styled,
        right_styled,
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

/// Split a shell command string into words, respecting single and double quotes.
/// Does not handle escape sequences — just basic quoting.
fn split_shell_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;

    for ch in s.chars() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::file_pair::FileChangeKind;
    use std::path::PathBuf;

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
                old_lines: LineIndex::empty(),
                new_lines: LineIndex::empty(),
                aligned_rows: vec![],
                hunks: vec![],
                left_styled: vec![],
                right_styled: vec![],
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

    // ── split_shell_words tests ──────────────────────────────────────

    #[test]
    fn shell_words_simple() {
        assert_eq!(split_shell_words("code --wait"), vec!["code", "--wait"]);
    }

    #[test]
    fn shell_words_single_quotes() {
        assert_eq!(
            split_shell_words("'/usr/bin/my editor' --wait"),
            vec!["/usr/bin/my editor", "--wait"]
        );
    }

    #[test]
    fn shell_words_double_quotes() {
        assert_eq!(
            split_shell_words(r#""my editor" arg1 arg2"#),
            vec!["my editor", "arg1", "arg2"]
        );
    }

    #[test]
    fn shell_words_empty() {
        assert!(split_shell_words("").is_empty());
        assert!(split_shell_words("   ").is_empty());
    }

    #[test]
    fn shell_words_extra_whitespace() {
        assert_eq!(split_shell_words("  vim   file  "), vec!["vim", "file"]);
    }
}
