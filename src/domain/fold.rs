use crate::domain::hunk::{AlignedRow, Hunk};
use std::ops::Range;

/// Diff display mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiffMode {
    #[default]
    SideBySide,
    Unified,
}

/// Sub-row within a unified view row (for `Both { modified: true }` which expands to 2 rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnifiedSubRow {
    /// Single line (context, LeftOnly, RightOnly).
    Single,
    /// Old/deleted sub-row of a modified pair.
    Old,
    /// New/added sub-row of a modified pair.
    New,
}

/// Compute the number of unified view rows for a single aligned row.
fn unified_row_height(row: &AlignedRow) -> usize {
    match row {
        AlignedRow::Both { modified: true, .. } => 2,
        _ => 1,
    }
}

/// A foldable region of unchanged lines between two hunks.
#[derive(Debug, Clone)]
struct FoldRegion {
    /// Full range of data-row indices in the gap.
    full_range: Range<usize>,
    /// Context lines visible at the top of this region.
    visible_top: usize,
    /// Context lines visible at the bottom of this region.
    visible_bottom: usize,
    /// Whether expand-up is allowed (false for file-start).
    can_expand_up: bool,
    /// Whether expand-down is allowed (false for file-end).
    can_expand_down: bool,
}

impl FoldRegion {
    /// Number of hidden data rows in this region.
    fn hidden_count(&self) -> usize {
        let len = self.full_range.len();
        len.saturating_sub(self.visible_top + self.visible_bottom)
    }

    /// Whether this region is currently collapsed (has hidden rows).
    fn is_collapsed(&self) -> bool {
        self.hidden_count() > 0
    }
}

/// A segment in the view: either visible data rows or a fold separator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// A contiguous range of visible data rows (indices into aligned_rows).
    Visible { data_range: Range<usize> },
    /// A fold separator replacing hidden rows.
    Fold {
        fold_id: usize,
        hidden_count: usize,
        show_expand_up: bool,
        show_expand_down: bool,
        /// Pre-formatted label, e.g. "35 lines hidden".
        label: String,
    },
}

impl Segment {
    /// Number of view rows this segment occupies.
    pub fn height(&self, fold_row_height: usize) -> usize {
        match self {
            Segment::Visible { data_range } => data_range.len(),
            Segment::Fold {
                show_expand_up,
                show_expand_down,
                ..
            } => (*show_expand_up as usize + *show_expand_down as usize) * fold_row_height,
        }
    }
}

/// Result of resolving a view-row index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedRow {
    /// A data row (index into aligned_rows).
    Data(usize),
    /// An expand-up fold separator row.
    ExpandUp { fold_id: usize, hidden_count: usize },
    /// An expand-down fold separator row.
    ExpandDown { fold_id: usize, hidden_count: usize },
}

/// Fold state for a single file's diff view.
#[derive(Debug, Clone)]
pub struct FoldState {
    /// Total number of data rows (aligned_rows.len()).
    total_data_rows: usize,
    /// Foldable regions computed from hunk gaps.
    regions: Vec<FoldRegion>,
    /// Ordered segments (rebuilt on fold changes).
    segments: Vec<Segment>,
    /// Cached total view rows (side-by-side).
    total_view_rows: usize,
    /// Lines of context around hunks.
    fold_context: usize,
    /// Lines revealed per expand click.
    fold_expand_step: usize,
    /// View-rows per fold separator.
    fold_row_height: usize,
    /// Pre-computed prefix-sum for unified view-row offsets.
    /// Length = total_data_rows + 1. offset[i] = cumulative unified view rows for data rows 0..i.
    /// Lazily computed on first unified access; cleared on fold mutation.
    unified_offsets: Option<Vec<usize>>,
    /// Cached total unified view rows (cleared with unified_offsets).
    total_view_rows_unified: Option<usize>,
}

impl FoldState {
    /// Create fold state from total data rows and hunks.
    /// Hunks must be sorted by `row_range.start`.
    pub fn new(
        total_data_rows: usize,
        hunks: &[Hunk],
        fold_context: usize,
        fold_expand_step: usize,
        fold_row_height: usize,
    ) -> Self {
        let regions = compute_regions(total_data_rows, hunks, fold_context);
        let mut state = FoldState {
            total_data_rows,
            regions,
            segments: Vec::new(),
            total_view_rows: 0,
            fold_context,
            fold_expand_step,
            fold_row_height,
            unified_offsets: None,
            total_view_rows_unified: None,
        };
        state.rebuild();
        state
    }

    /// Total number of view rows (for scroll clamping).
    pub fn total_view_rows(&self) -> usize {
        self.total_view_rows
    }

    /// The segment list.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Resolve a view-row index to the nearest data-row index.
    /// For fold separators, returns the data row at the fold boundary
    /// (end of preceding visible range, or start of following visible range).
    pub fn view_row_to_data_row(&self, view_row: usize) -> usize {
        let mut remaining = view_row;
        let mut last_data_end = 0usize;
        for seg in &self.segments {
            let h = seg.height(self.fold_row_height);
            if remaining < h {
                return match seg {
                    Segment::Visible { data_range } => data_range.start + remaining,
                    Segment::Fold { .. } => {
                        // On a fold: return the data row just before the hidden range.
                        last_data_end.saturating_sub(1)
                    }
                };
            }
            if let Segment::Visible { data_range } = seg {
                last_data_end = data_range.end;
            }
            remaining -= h;
        }
        self.total_data_rows.saturating_sub(1)
    }

    pub fn resolve_view_row(&self, view_row: usize) -> ResolvedRow {
        let mut remaining = view_row;
        for seg in &self.segments {
            let h = seg.height(self.fold_row_height);
            if remaining < h {
                return match seg {
                    Segment::Visible { data_range } => {
                        ResolvedRow::Data(data_range.start + remaining)
                    }
                    Segment::Fold {
                        fold_id,
                        hidden_count,
                        show_expand_up,
                        ..
                    } => {
                        if *show_expand_up && remaining < self.fold_row_height {
                            ResolvedRow::ExpandUp {
                                fold_id: *fold_id,
                                hidden_count: *hidden_count,
                            }
                        } else {
                            ResolvedRow::ExpandDown {
                                fold_id: *fold_id,
                                hidden_count: *hidden_count,
                            }
                        }
                    }
                };
            }
            remaining -= h;
        }
        ResolvedRow::Data(self.total_data_rows.saturating_sub(1))
    }

    /// Map a data-row index to its view-row index. Returns None if hidden.
    pub fn data_to_view_row(&self, data_idx: usize) -> Option<usize> {
        let mut view_row = 0;
        for seg in &self.segments {
            match seg {
                Segment::Visible { data_range } => {
                    if data_range.contains(&data_idx) {
                        return Some(view_row + data_idx - data_range.start);
                    }
                    view_row += data_range.len();
                }
                Segment::Fold { .. } => {
                    view_row += seg.height(self.fold_row_height);
                }
            }
        }
        None
    }

    // ── Unified mode support ──

    /// Borrow the pre-computed unified offsets (if computed).
    pub fn unified_offsets_ref(&self) -> Option<&[usize]> {
        self.unified_offsets.as_deref()
    }

    /// Return the cached unified total view rows (if already computed).
    pub fn total_view_rows_unified_cached(&self) -> Option<usize> {
        self.total_view_rows_unified
    }

    /// Build the unified view-row offset prefix-sum from aligned rows.
    /// Must be called with the full aligned_rows slice.
    pub fn ensure_unified_offsets(&mut self, aligned_rows: &[AlignedRow]) {
        if self.unified_offsets.is_some() {
            return;
        }
        let n = aligned_rows.len();
        let mut offsets = Vec::with_capacity(n + 1);
        offsets.push(0);
        for row in aligned_rows {
            let prev = *offsets
                .last()
                .expect("offsets is never empty; initialized with push(0)");
            offsets.push(prev + unified_row_height(row));
        }
        self.unified_offsets = Some(offsets);
    }

    /// Total view rows in unified mode. Requires `ensure_unified_offsets` to have been called.
    pub fn total_view_rows_unified(&mut self, aligned_rows: &[AlignedRow]) -> usize {
        if let Some(cached) = self.total_view_rows_unified {
            return cached;
        }
        self.ensure_unified_offsets(aligned_rows);
        let offsets = self
            .unified_offsets
            .as_ref()
            .expect("ensure_unified_offsets called on line above");
        let mut total = 0;
        for seg in &self.segments {
            total += match seg {
                Segment::Visible { data_range } => {
                    offsets[data_range.end] - offsets[data_range.start]
                }
                Segment::Fold {
                    show_expand_up,
                    show_expand_down,
                    ..
                } => (*show_expand_up as usize + *show_expand_down as usize) * self.fold_row_height,
            };
        }
        self.total_view_rows_unified = Some(total);
        total
    }

    /// Total view rows for the given mode.
    pub fn total_view_rows_for_mode(
        &mut self,
        mode: DiffMode,
        aligned_rows: &[AlignedRow],
    ) -> usize {
        match mode {
            DiffMode::SideBySide => self.total_view_rows,
            DiffMode::Unified => self.total_view_rows_unified(aligned_rows),
        }
    }

    /// Resolve a unified-mode view row to data row + sub-row.
    /// Requires `ensure_unified_offsets` to have been called.
    pub fn resolve_unified_view_row(&self, view_row: usize) -> (ResolvedRow, UnifiedSubRow) {
        let offsets = self
            .unified_offsets
            .as_ref()
            .expect("ensure_unified_offsets must be called before resolve_unified_view_row");
        let mut remaining = view_row;
        for seg in &self.segments {
            match seg {
                Segment::Visible { data_range } => {
                    let seg_height = offsets[data_range.end] - offsets[data_range.start];
                    if remaining < seg_height {
                        // Binary search within this segment for the data row.
                        // remaining is offset from the start of this segment's unified view rows.
                        let base_offset = offsets[data_range.start];
                        let target = base_offset + remaining;
                        // Find the largest i in data_range where offsets[i] <= target.
                        let idx = match offsets[data_range.start..data_range.end]
                            .binary_search(&target)
                        {
                            Ok(i) => data_range.start + i,
                            Err(i) => data_range.start + i.saturating_sub(1),
                        };
                        let row_offset = target - offsets[idx];
                        let row_height = offsets[idx + 1] - offsets[idx];
                        let sub = if row_height == 2 {
                            if row_offset == 0 {
                                UnifiedSubRow::Old
                            } else {
                                UnifiedSubRow::New
                            }
                        } else {
                            UnifiedSubRow::Single
                        };
                        return (ResolvedRow::Data(idx), sub);
                    }
                    remaining -= seg_height;
                }
                Segment::Fold {
                    fold_id,
                    hidden_count,
                    show_expand_up,
                    ..
                } => {
                    let h = seg.height(self.fold_row_height);
                    if remaining < h {
                        let resolved = if *show_expand_up && remaining < self.fold_row_height {
                            ResolvedRow::ExpandUp {
                                fold_id: *fold_id,
                                hidden_count: *hidden_count,
                            }
                        } else {
                            ResolvedRow::ExpandDown {
                                fold_id: *fold_id,
                                hidden_count: *hidden_count,
                            }
                        };
                        return (resolved, UnifiedSubRow::Single);
                    }
                    remaining -= h;
                }
            }
        }
        (
            ResolvedRow::Data(self.total_data_rows.saturating_sub(1)),
            UnifiedSubRow::Single,
        )
    }

    /// Count data rows at the start of `data_range` that lie entirely before
    /// `view_rows` unified view rows from the segment start. Used by the
    /// renderer to jump to the scroll window in O(log n) instead of walking
    /// the segment row by row. Requires `ensure_unified_offsets`.
    pub fn unified_rows_before(&self, data_range: &Range<usize>, view_rows: usize) -> usize {
        let offsets = self
            .unified_offsets
            .as_ref()
            .expect("ensure_unified_offsets must be called before unified_rows_before");
        // A row is entirely before the window when its end offset is within
        // the skipped view rows.
        let target = offsets[data_range.start] + view_rows;
        offsets[data_range.start + 1..=data_range.end].partition_point(|&end| end <= target)
    }

    /// Map a data-row index to its unified view-row index. Returns None if hidden.
    pub fn data_to_unified_view_row(&self, data_idx: usize) -> Option<usize> {
        let offsets = self.unified_offsets.as_ref()?;
        let mut view_row = 0;
        for seg in &self.segments {
            match seg {
                Segment::Visible { data_range } => {
                    if data_range.contains(&data_idx) {
                        return Some(view_row + offsets[data_idx] - offsets[data_range.start]);
                    }
                    view_row += offsets[data_range.end] - offsets[data_range.start];
                }
                Segment::Fold { .. } => {
                    view_row += seg.height(self.fold_row_height);
                }
            }
        }
        None
    }

    /// Map a data-row index to its view-row for the given mode. Returns None if hidden.
    pub fn data_to_view_row_for_mode(&self, data_idx: usize, mode: DiffMode) -> Option<usize> {
        match mode {
            DiffMode::SideBySide => self.data_to_view_row(data_idx),
            DiffMode::Unified => self.data_to_unified_view_row(data_idx),
        }
    }

    /// Convert a view row to a data-row index for the given mode.
    pub fn view_row_to_data_row_for_mode(&self, view_row: usize, mode: DiffMode) -> usize {
        match mode {
            DiffMode::SideBySide => self.view_row_to_data_row(view_row),
            DiffMode::Unified => {
                let (resolved, _sub) = self.resolve_unified_view_row(view_row);
                match resolved {
                    ResolvedRow::Data(idx) => idx,
                    ResolvedRow::ExpandUp { .. } | ResolvedRow::ExpandDown { .. } => {
                        // On a fold bar: find the nearest data row by scanning segments.
                        let mut last_data_end = 0usize;
                        let offsets = self
                            .unified_offsets
                            .as_ref()
                            .expect("unified offsets must be computed");
                        let mut vr = 0usize;
                        for seg in &self.segments {
                            let h = match seg {
                                Segment::Visible { data_range } => {
                                    offsets[data_range.end] - offsets[data_range.start]
                                }
                                Segment::Fold { .. } => seg.height(self.fold_row_height),
                            };
                            if vr + h > view_row {
                                // This is the fold containing view_row.
                                return last_data_end.saturating_sub(1);
                            }
                            if let Segment::Visible { data_range } = seg {
                                last_data_end = data_range.end;
                            }
                            vr += h;
                        }
                        self.total_data_rows.saturating_sub(1)
                    }
                }
            }
        }
    }

    /// Expand upward at the given fold region.
    pub fn expand_up(&mut self, fold_id: usize) {
        if let Some(r) = self.regions.get_mut(fold_id) {
            if !r.can_expand_up {
                return;
            }
            let max = r.full_range.len().saturating_sub(r.visible_bottom);
            r.visible_top = (r.visible_top + self.fold_expand_step).min(max);
            self.rebuild();
        }
    }

    /// Expand downward at the given fold region.
    pub fn expand_down(&mut self, fold_id: usize) {
        if let Some(r) = self.regions.get_mut(fold_id) {
            if !r.can_expand_down {
                return;
            }
            let max = r.full_range.len().saturating_sub(r.visible_top);
            r.visible_bottom = (r.visible_bottom + self.fold_expand_step).min(max);
            self.rebuild();
        }
    }

    /// Fold all regions back to default context.
    pub fn fold_all(&mut self) {
        for r in &mut self.regions {
            r.visible_top = if r.can_expand_up {
                self.fold_context
            } else {
                0
            };
            r.visible_bottom = if r.can_expand_down {
                self.fold_context
            } else {
                0
            };
        }
        self.rebuild();
    }

    /// Expand all regions fully (no folds).
    pub fn unfold_all(&mut self) {
        for r in &mut self.regions {
            r.visible_top = r.full_range.len();
            r.visible_bottom = 0;
        }
        self.rebuild();
    }

    /// Ensure a specific data row is visible by expanding its containing fold region.
    /// Returns true if a fold was expanded, false if the row was already visible.
    pub fn expose_data_row(&mut self, data_idx: usize) -> bool {
        // Find which fold region (if any) hides this data row.
        let region_idx = self.regions.iter().position(|r| {
            if !r.is_collapsed() {
                return false;
            }
            let hidden_start = r.full_range.start + r.visible_top;
            let hidden_end = r.full_range.end.saturating_sub(r.visible_bottom);
            (hidden_start..hidden_end).contains(&data_idx)
        });
        let Some(rid) = region_idx else {
            return false; // Already visible
        };

        // We want to reveal a window of (fold_expand_step + 2*fold_context) rows centered
        // on the target, while keeping rows outside that window folded.
        let window = self.fold_expand_step + 2 * self.fold_context;
        let half = window / 2;

        let r = &self.regions[rid];
        let rs = r.full_range.start;
        let re = r.full_range.end;
        let orig_top = r.visible_top;
        let orig_bottom = r.visible_bottom;
        let can_up = r.can_expand_up;
        let can_down = r.can_expand_down;

        // Window bounds (clamped to region).
        let win_start = data_idx.saturating_sub(half).max(rs);
        let win_end = (data_idx + half + 1).min(re);

        // Rows before the window that are currently hidden.
        let hidden_before_start = rs + orig_top;
        let hidden_after_end = re.saturating_sub(orig_bottom);

        // Before-region: covers rs..win_start (includes original visible_top).
        // After-region: covers win_end..re (includes original visible_bottom).
        let before_len = win_start.saturating_sub(rs);
        let after_len = re.saturating_sub(win_end);

        let before_hidden = win_start.saturating_sub(hidden_before_start);
        let after_hidden = hidden_after_end.saturating_sub(win_end);

        let mut new_regions: Vec<FoldRegion> = Vec::new();

        // Before fold (if there are hidden rows before the window).
        if before_hidden > 0 && before_len > 0 {
            new_regions.push(FoldRegion {
                full_range: rs..win_start,
                visible_top: orig_top,
                visible_bottom: 0, // window edge takes over
                can_expand_up: can_up,
                can_expand_down: true,
            });
        }

        // After fold (if there are hidden rows after the window).
        if after_hidden > 0 && after_len > 0 {
            new_regions.push(FoldRegion {
                full_range: win_end..re,
                visible_top: 0, // window edge takes over
                visible_bottom: orig_bottom,
                can_expand_up: true,
                can_expand_down: can_down,
            });
        }

        // Replace the original region with the new one(s).
        // If no new fold regions needed (entire region now visible), just remove it.
        self.regions.splice(rid..=rid, new_regions);

        self.rebuild();
        true
    }

    /// Rebuild segments from current region state. O(R).
    fn rebuild(&mut self) {
        self.segments.clear();
        self.unified_offsets = None;
        self.total_view_rows_unified = None;
        let mut cursor = 0usize;

        for (id, region) in self.regions.iter().enumerate() {
            let s = region.full_range.start;
            let e = region.full_range.end;
            let t = region.visible_top;
            let b = region.visible_bottom;
            let visible_top_end = s + t;
            let visible_bot_start = e.saturating_sub(b);

            // Visible rows before this fold region (from cursor to visible_top_end).
            if cursor < visible_top_end {
                self.segments.push(Segment::Visible {
                    data_range: cursor..visible_top_end,
                });
            }

            // Fold separator if region is collapsed.
            if region.is_collapsed() {
                let hidden = region.hidden_count();
                // When the hidden region is small enough to expand in one click
                // and both directions are available, emit a single "expand" row.
                let (up, down) = if hidden <= self.fold_expand_step
                    && region.can_expand_up
                    && region.can_expand_down
                {
                    (true, false) // Single row; click expands fully.
                } else {
                    (region.can_expand_up, region.can_expand_down)
                };
                self.segments.push(Segment::Fold {
                    fold_id: id,
                    hidden_count: hidden,
                    show_expand_up: up,
                    show_expand_down: down,
                    label: format!("{hidden} lines hidden"),
                });
            }

            // Move cursor past the fold to the visible bottom rows.
            // The visible bottom rows will be emitted as part of the next
            // iteration's "visible rows before fold" or the trailing segment.
            cursor = visible_bot_start;
        }

        // Remaining rows after the last region.
        if cursor < self.total_data_rows {
            self.segments.push(Segment::Visible {
                data_range: cursor..self.total_data_rows,
            });
        }

        self.total_view_rows = self
            .segments
            .iter()
            .map(|s| s.height(self.fold_row_height))
            .sum();
    }
}

/// Compute fold regions from hunk gaps.
fn compute_regions(total_data_rows: usize, hunks: &[Hunk], fold_context: usize) -> Vec<FoldRegion> {
    if hunks.is_empty() {
        return Vec::new();
    }

    let mut regions = Vec::new();
    let num_gaps = hunks.len() + 1;

    for i in 0..num_gaps {
        let gap_start = if i == 0 {
            0
        } else {
            hunks[i - 1].row_range.end
        };
        let gap_end = if i == hunks.len() {
            total_data_rows
        } else {
            hunks[i].row_range.start
        };

        if gap_start >= gap_end {
            continue;
        }

        let is_first = i == 0;
        let is_last = i == hunks.len();
        let top_ctx = if is_first { 0 } else { fold_context };
        let bot_ctx = if is_last { 0 } else { fold_context };

        let gap_len = gap_end - gap_start;
        if gap_len > top_ctx + bot_ctx {
            regions.push(FoldRegion {
                full_range: gap_start..gap_end,
                visible_top: top_ctx,
                visible_bottom: bot_ctx,
                can_expand_up: !is_first,
                can_expand_down: !is_last,
            });
        }
    }

    regions
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn make_hunks(ranges: &[(usize, usize)]) -> Vec<Hunk> {
        ranges
            .iter()
            .map(|(s, e)| Hunk { row_range: *s..*e })
            .collect()
    }

    /// Test helper: create FoldState with default fold params (context=5, expand_step=20, row_height=2).
    fn test_fold(total: usize, hunks: &[Hunk]) -> FoldState {
        FoldState::new(total, hunks, 5, 20, 2)
    }

    #[test]
    fn test_no_hunks_no_folding() {
        let fs = test_fold(100, &[]);
        assert_eq!(fs.total_view_rows(), 100);
        assert_eq!(fs.segments().len(), 1);
        assert_eq!(fs.segments()[0], Segment::Visible { data_range: 0..100 });
    }

    #[test]
    fn test_single_hunk_middle() {
        // 100 rows, hunk at 40..50, so gaps 0..40 and 50..100.
        // Gap 0..40: file-start, top_ctx=0, bot_ctx=5, len=40 > 5 → fold
        //   visible_top=0, visible_bottom=5 → hidden = 40 - 0 - 5 = 35
        // Gap 50..100: file-end, top_ctx=5, bot_ctx=0, len=50 > 5 → fold
        //   visible_top=5, visible_bottom=0 → hidden = 50 - 5 - 0 = 45
        let hunks = make_hunks(&[(40, 50)]);
        let fs = test_fold(100, &hunks);

        // Expected segments:
        // 1. Fold(0, hidden=35, up=false, down=true)
        // 2. Visible(35..50)  — bottom 5 of gap0 + hunk
        // 3. Fold(1, hidden=45, up=true, down=false)
        // 4. Visible(95..100) — wait, no. Let me trace rebuild:
        //
        // Region 0: full=0..40, vis_top=0, vis_bot=5, hidden=35
        //   cursor=0, visible_top_end=0+0=0, visible_bot_start=40-5=35
        //   cursor < 0? no → no visible before fold
        //   fold: Fold(0, 35, up=false, down=true)
        //   cursor = 35
        //
        // Region 1: full=50..100, vis_top=5, vis_bot=0, hidden=45
        //   s=50, visible_top_end=50+5=55, visible_bot_start=100-0=100
        //   cursor=35 < 55 → Visible(35..55)
        //   fold: Fold(1, 45, up=true, down=false)
        //   cursor = 100
        //
        // After loop: cursor=100, total=100, no trailing.
        // Total view rows: 1 + 20 + 1 + 0 = 22

        assert_eq!(fs.segments().len(), 3);
        assert_eq!(
            fs.segments()[0],
            Segment::Fold {
                fold_id: 0,
                hidden_count: 35,
                show_expand_up: false,
                show_expand_down: true,
                label: "35 lines hidden".to_string(),
            }
        );
        assert_eq!(fs.segments()[1], Segment::Visible { data_range: 35..55 });
        assert_eq!(
            fs.segments()[2],
            Segment::Fold {
                fold_id: 1,
                hidden_count: 45,
                show_expand_up: true,
                show_expand_down: false,
                label: "45 lines hidden".to_string(),
            }
        );
        // Heights: 2 + 20 + 2 = 24
        assert_eq!(fs.total_view_rows(), 24);
    }

    #[test]
    fn test_view_row_to_data_row_on_fold_at_start() {
        // Same setup as test_single_hunk_middle: fold at start (rows 0..35 hidden).
        // Segments: Fold(0,35,down) | Visible(35..55) | Fold(1,45,up)
        let hunks = make_hunks(&[(40, 50)]);
        let fs = test_fold(100, &hunks);

        // View-row 0 lands on the first fold (Fold 0, expand_down).
        // No preceding visible segment, so last_data_end=0, returns 0.saturating_sub(1) = 0.
        assert_eq!(fs.view_row_to_data_row(0), 0);
        // View-row 1 also in the fold (frh=2).
        assert_eq!(fs.view_row_to_data_row(1), 0);
        // View-row 2 is the first visible data row (35).
        assert_eq!(fs.view_row_to_data_row(2), 35);
        // View-row 3 → data row 36.
        assert_eq!(fs.view_row_to_data_row(3), 36);
    }

    #[test]
    fn test_view_row_to_data_row_on_fold_at_end() {
        let hunks = make_hunks(&[(40, 50)]);
        let fs = test_fold(100, &hunks);

        // Fold 1 starts at view-row 22 (2 fold + 20 visible).
        // Preceding visible ends at data row 55, so last_data_end=55.
        // Returns 55 - 1 = 54.
        assert_eq!(fs.view_row_to_data_row(22), 54);
    }

    #[test]
    fn test_next_hunk_from_initial_fold() {
        use crate::domain::hunk::next_hunk_row;
        let hunks = make_hunks(&[(40, 50)]);
        let fs = test_fold(100, &hunks);

        // From view-row 0 (fold at start), data_row = 0.
        let data_row = fs.view_row_to_data_row(0);
        assert_eq!(data_row, 0);
        // next_hunk from row 0 → first hunk starts at row 40 (start > 0).
        let next = next_hunk_row(&hunks, data_row);
        assert_eq!(next, Some(40));
        // Convert back to view-row.
        let view = fs.data_to_view_row(40).unwrap();
        assert_eq!(view, 7); // fold(2) + (40-35) = 7
    }

    #[test]
    fn test_small_gap_not_folded() {
        // Gap of 8 lines between two hunks. FOLD_CONTEXT=5, so 5+5=10 > 8 → no fold.
        let hunks = make_hunks(&[(0, 10), (18, 30)]);
        let fs = test_fold(30, &hunks);

        // Gaps: 10..18 (len 8, not folded), trailing 30..30 (empty).
        // No regions, so single visible segment.
        assert_eq!(fs.segments().len(), 1);
        assert_eq!(fs.segments()[0], Segment::Visible { data_range: 0..30 });
        assert_eq!(fs.total_view_rows(), 30);
    }

    #[test]
    fn test_resolve_view_row() {
        let hunks = make_hunks(&[(40, 50)]);
        let fs = test_fold(100, &hunks);

        // Segment 0: Fold(0), height 1 (down only)
        // Segment 1: Visible(35..55), height 20
        // Segment 2: Fold(1), height 1 (up only)

        // View row 0 → Fold expand-down (no expand-up for file-start)
        assert_eq!(
            fs.resolve_view_row(0),
            ResolvedRow::ExpandDown {
                fold_id: 0,
                hidden_count: 35
            }
        );
        // View row 1 → still expand-down (FOLD_ROW_HEIGHT=2)
        assert_eq!(
            fs.resolve_view_row(1),
            ResolvedRow::ExpandDown {
                fold_id: 0,
                hidden_count: 35
            }
        );

        // View row 2 → Data(35)
        assert_eq!(fs.resolve_view_row(2), ResolvedRow::Data(35));

        // View row 21 → Data(54)
        assert_eq!(fs.resolve_view_row(21), ResolvedRow::Data(54));

        // View row 22 → Fold expand-up (no expand-down for file-end)
        assert_eq!(
            fs.resolve_view_row(22),
            ResolvedRow::ExpandUp {
                fold_id: 1,
                hidden_count: 45
            }
        );
    }

    #[test]
    fn test_data_to_view_row() {
        let hunks = make_hunks(&[(40, 50)]);
        let fs = test_fold(100, &hunks);

        // Data row 0 is hidden (inside fold region 0).
        assert_eq!(fs.data_to_view_row(0), None);

        // Data row 35 is first visible data row → view row 2 (after FOLD_ROW_HEIGHT=2).
        assert_eq!(fs.data_to_view_row(35), Some(2));

        // Data row 45 (inside hunk) → view row 2 + (45 - 35) = 12.
        assert_eq!(fs.data_to_view_row(45), Some(12));

        // Data row 55 is hidden (inside fold region 1's hidden area).
        assert_eq!(fs.data_to_view_row(55), None);

        // Data row 99 is hidden.
        assert_eq!(fs.data_to_view_row(99), None);
    }

    #[test]
    fn test_expand_up() {
        let hunks = make_hunks(&[(40, 50)]);
        let mut fs = test_fold(100, &hunks);

        // Region 1 (50..100): can_expand_up=true, visible_top=5
        let old_total = fs.total_view_rows();
        fs.expand_up(1);
        // visible_top should increase by FOLD_EXPAND_STEP (20), from 5 to 25.
        // Hidden was 45, now 45 - 20 = 25.
        assert_eq!(fs.total_view_rows(), old_total + 20);

        // Data row 55 should now be visible.
        assert!(fs.data_to_view_row(55).is_some());
    }

    #[test]
    fn test_expand_down() {
        let hunks = make_hunks(&[(40, 50)]);
        let mut fs = test_fold(100, &hunks);

        // Region 0 (0..40): can_expand_down=true, visible_bottom=5
        let old_total = fs.total_view_rows();
        fs.expand_down(0);
        // visible_bottom should increase by 20, from 5 to 25.
        assert_eq!(fs.total_view_rows(), old_total + 20);
    }

    #[test]
    fn test_expand_up_on_file_start_noop() {
        let hunks = make_hunks(&[(40, 50)]);
        let mut fs = test_fold(100, &hunks);
        let old_total = fs.total_view_rows();
        fs.expand_up(0); // Region 0 is file-start, can_expand_up=false
        assert_eq!(fs.total_view_rows(), old_total);
    }

    #[test]
    fn test_fold_all_unfold_all() {
        let hunks = make_hunks(&[(40, 50)]);
        let mut fs = test_fold(100, &hunks);

        fs.unfold_all();
        assert_eq!(fs.total_view_rows(), 100);
        // All data rows visible.
        for i in 0..100 {
            assert!(
                fs.data_to_view_row(i).is_some(),
                "row {i} should be visible"
            );
        }

        fs.fold_all();
        assert_eq!(fs.total_view_rows(), 24); // Same as initial
    }

    #[test]
    fn test_multiple_hunks() {
        // Two hunks with a large gap between them.
        let hunks = make_hunks(&[(10, 20), (80, 90)]);
        let fs = test_fold(100, &hunks);

        // Gaps: 0..10 (file-start), 20..80 (mid, len=60), 90..100 (file-end)
        // Region 0: 0..10, top=0, bot=5, hidden=5
        // Region 1: 20..80, top=5, bot=5, hidden=50
        // Region 2: 90..100, top=5, bot=0, hidden=5
        //
        // Rebuild trace:
        // R0: full=0..10, vis_top=0, vis_bot=5
        //   vis_top_end=0, vis_bot_start=5, cursor=0
        //   Fold(0, 5, up=false, down=true) h=2
        //   cursor=5
        // R1: full=20..80, vis_top=5, vis_bot=5
        //   vis_top_end=25, vis_bot_start=75, cursor=5
        //   Visible(5..25) h=20
        //   Fold(1, 50, up=true, down=true) h=4
        //   cursor=75
        // R2: full=90..100, vis_top=5, vis_bot=0
        //   vis_top_end=95, vis_bot_start=100, cursor=75
        //   Visible(75..95) h=20
        //   Fold(2, 5, up=true, down=false) h=2
        //   cursor=100
        // Total: 2 + 20 + 4 + 20 + 2 = 48

        assert_eq!(fs.segments().len(), 5);
        assert_eq!(fs.total_view_rows(), 48);
    }

    #[test]
    fn test_expand_fully_removes_fold() {
        // Small fold region that can be fully expanded.
        let hunks = make_hunks(&[(5, 10), (25, 30)]);
        let mut fs = test_fold(30, &hunks);

        // Gap 10..25, len=15. top=5, bot=5, hidden=5.
        // One expand_up of 20 lines → visible_top = min(25, 15-5) = 10.
        // Now hidden = 15 - 10 - 5 = 0. Region fully expanded.
        fs.expand_up(0); // The mid-gap region.

        // Find the mid-gap region's fold_id. It's the only region with can_expand_up=true
        // and can_expand_down=true. Let's check by looking for absence of Fold segments
        // for that region.
        let _has_mid_fold = fs
            .segments()
            .iter()
            .any(|s| matches!(s, Segment::Fold { fold_id: 0, .. }));
        // Region 0 is actually... let me check which region index the mid gap is.
        // Gaps: 0..5 (file-start, len=5, 0+5=5 not > 5, no region),
        //       10..25 (mid, len=15, 5+5=10 < 15, region 0),
        //       25..30... wait, gap after hunk 1 is 30..30 = empty.
        // Actually: gap_starts = [0, 10, 30], gap_ends = [5, 25, 30]
        // Gap 0: 0..5, len=5, top=0, bot=5 → 5 > 0+5=5? No. Not folded.
        // Gap 1: 10..25, len=15, top=5, bot=5 → 15 > 10? Yes. Region 0.
        // Gap 2: 30..30, empty.
        // So region 0 is the mid gap.
        fs.expand_up(0);

        // After full expansion, no fold separator for region 0.
        let fold_count = fs
            .segments()
            .iter()
            .filter(|s| matches!(s, Segment::Fold { .. }))
            .count();
        assert_eq!(fold_count, 0);
        assert_eq!(fs.total_view_rows(), 30);
    }

    #[test]
    fn test_adjacent_hunks_no_gap() {
        // Two adjacent hunks with no gap.
        let hunks = make_hunks(&[(10, 20), (20, 30)]);
        let fs = test_fold(50, &hunks);

        // Gaps: 0..10 (file-start), 20..20 (empty), 30..50 (file-end)
        // R0: 0..10, top=0, bot=5, len=10>5 → fold, hidden=5
        // R1: 30..50, top=5, bot=0, len=20>5 → fold, hidden=15
        //
        // Rebuild: Fold(0,h=2), Visible(5..35,h=30), Fold(1,h=2) → 3 segments
        assert_eq!(fs.segments().len(), 3);
        assert_eq!(fs.total_view_rows(), 34);
    }

    #[test]
    fn test_hunk_at_file_start() {
        let hunks = make_hunks(&[(0, 10)]);
        let fs = test_fold(50, &hunks);

        // Gap 0: 0..0 (empty)
        // Gap 1: 10..50, file-end, top=5, bot=0, len=40 > 5 → fold, hidden=35
        // Segments: Visible(0..15), Fold(0, 35, up=true, down=false)
        assert_eq!(fs.segments().len(), 2);
        assert_eq!(fs.segments()[0], Segment::Visible { data_range: 0..15 });
    }

    #[test]
    fn test_hunk_at_file_end() {
        let hunks = make_hunks(&[(40, 50)]);
        let fs = test_fold(50, &hunks);

        // Gap 0: 0..40, file-start, top=0, bot=5, len=40 > 5 → fold, hidden=35
        // Gap 1: 50..50, empty
        // Segments: Fold(0, 35, up=false, down=true), Visible(35..50)
        assert_eq!(fs.segments().len(), 2);
        assert_eq!(fs.segments()[1], Segment::Visible { data_range: 35..50 });
    }

    // ── Unified mode tests ──

    fn make_aligned_rows(spec: &str) -> Vec<AlignedRow> {
        let mut left = 0usize;
        let mut right = 0usize;
        spec.chars()
            .map(|c| match c {
                'C' => {
                    let r = AlignedRow::Both {
                        left_line: left,
                        right_line: right,
                        modified: false,
                    };
                    left += 1;
                    right += 1;
                    r
                }
                'M' => {
                    let r = AlignedRow::Both {
                        left_line: left,
                        right_line: right,
                        modified: true,
                    };
                    left += 1;
                    right += 1;
                    r
                }
                'L' => {
                    let r = AlignedRow::LeftOnly { left_line: left };
                    left += 1;
                    r
                }
                'R' => {
                    let r = AlignedRow::RightOnly { right_line: right };
                    right += 1;
                    r
                }
                _ => panic!("unknown spec char: {c}"),
            })
            .collect()
    }

    #[test]
    fn test_unified_offsets_all_context() {
        let rows = make_aligned_rows("CCCCC");
        let mut fs = test_fold(5, &[]);
        fs.ensure_unified_offsets(&rows);
        let offsets = fs.unified_offsets_ref().unwrap();
        assert_eq!(offsets, &[0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_unified_offsets_with_modified() {
        let rows = make_aligned_rows("CMC");
        let hunks = make_hunks(&[(1, 2)]);
        let mut fs = test_fold(3, &hunks);
        fs.ensure_unified_offsets(&rows);
        let offsets = fs.unified_offsets_ref().unwrap();
        assert_eq!(offsets, &[0, 1, 3, 4]);
    }

    #[test]
    fn test_unified_rows_before() {
        // Heights: C=1 M=2 C=1 M=2 C=1 → offsets [0, 1, 3, 4, 6, 7].
        let rows = make_aligned_rows("CMCMC");
        let mut fs = test_fold(5, &make_hunks(&[(0, 5)]));
        fs.ensure_unified_offsets(&rows);

        let range = 0..5;
        assert_eq!(fs.unified_rows_before(&range, 0), 0);
        // Row 0 (height 1) ends exactly at 1 view row → entirely before.
        assert_eq!(fs.unified_rows_before(&range, 1), 1);
        // Row 1 (modified, height 2) straddles → still kept.
        assert_eq!(fs.unified_rows_before(&range, 2), 1);
        assert_eq!(fs.unified_rows_before(&range, 3), 2);
        assert_eq!(fs.unified_rows_before(&range, 6), 4);
        // Whole segment before the window.
        assert_eq!(fs.unified_rows_before(&range, 7), 5);

        // Sub-range starting mid-file: offsets are relative to range start.
        let sub = 2..5; // heights 1, 2, 1
        assert_eq!(fs.unified_rows_before(&sub, 0), 0);
        assert_eq!(fs.unified_rows_before(&sub, 1), 1);
        assert_eq!(fs.unified_rows_before(&sub, 2), 1);
        assert_eq!(fs.unified_rows_before(&sub, 3), 2);
    }

    #[test]
    fn test_unified_offsets_left_right_only() {
        let rows = make_aligned_rows("CLRC");
        let hunks = make_hunks(&[(1, 3)]);
        let mut fs = test_fold(4, &hunks);
        fs.ensure_unified_offsets(&rows);
        let offsets = fs.unified_offsets_ref().unwrap();
        assert_eq!(offsets, &[0, 1, 2, 3, 4]);
    }

    #[test]
    fn test_unified_total_view_rows() {
        let rows = make_aligned_rows("CCCCCM");
        let hunks = make_hunks(&[(5, 6)]);
        let mut fs = test_fold(6, &hunks);
        let total = fs.total_view_rows_unified(&rows);
        assert_eq!(total, 7);
    }

    #[test]
    fn test_unified_resolve_context_row() {
        let rows = make_aligned_rows("CCC");
        let mut fs = test_fold(3, &[]);
        fs.ensure_unified_offsets(&rows);
        let (resolved, sub) = fs.resolve_unified_view_row(1);
        assert_eq!(resolved, ResolvedRow::Data(1));
        assert_eq!(sub, UnifiedSubRow::Single);
    }

    #[test]
    fn test_unified_resolve_modified_sub_rows() {
        let rows = make_aligned_rows("CMC");
        let hunks = make_hunks(&[(1, 2)]);
        let mut fs = test_fold(3, &hunks);
        fs.ensure_unified_offsets(&rows);

        let (r, s) = fs.resolve_unified_view_row(0);
        assert_eq!(r, ResolvedRow::Data(0));
        assert_eq!(s, UnifiedSubRow::Single);

        let (r, s) = fs.resolve_unified_view_row(1);
        assert_eq!(r, ResolvedRow::Data(1));
        assert_eq!(s, UnifiedSubRow::Old);

        let (r, s) = fs.resolve_unified_view_row(2);
        assert_eq!(r, ResolvedRow::Data(1));
        assert_eq!(s, UnifiedSubRow::New);

        let (r, s) = fs.resolve_unified_view_row(3);
        assert_eq!(r, ResolvedRow::Data(2));
        assert_eq!(s, UnifiedSubRow::Single);
    }

    #[test]
    fn test_unified_data_to_view_row() {
        let rows = make_aligned_rows("CMC");
        let hunks = make_hunks(&[(1, 2)]);
        let mut fs = test_fold(3, &hunks);
        fs.ensure_unified_offsets(&rows);

        assert_eq!(fs.data_to_unified_view_row(0), Some(0));
        assert_eq!(fs.data_to_unified_view_row(1), Some(1));
        assert_eq!(fs.data_to_unified_view_row(2), Some(3));
    }

    #[test]
    fn test_unified_view_row_to_data_row_for_mode() {
        let rows = make_aligned_rows("CMC");
        let hunks = make_hunks(&[(1, 2)]);
        let mut fs = test_fold(3, &hunks);
        fs.ensure_unified_offsets(&rows);

        assert_eq!(fs.view_row_to_data_row_for_mode(0, DiffMode::Unified), 0);
        assert_eq!(fs.view_row_to_data_row_for_mode(1, DiffMode::Unified), 1);
        assert_eq!(fs.view_row_to_data_row_for_mode(2, DiffMode::Unified), 1);
        assert_eq!(fs.view_row_to_data_row_for_mode(3, DiffMode::Unified), 2);
    }

    #[test]
    fn test_unified_all_modified() {
        let rows = make_aligned_rows("MMM");
        let hunks = make_hunks(&[(0, 3)]);
        let mut fs = test_fold(3, &hunks);
        let total = fs.total_view_rows_unified(&rows);
        assert_eq!(total, 6);

        fs.ensure_unified_offsets(&rows);
        let (r, s) = fs.resolve_unified_view_row(4);
        assert_eq!(r, ResolvedRow::Data(2));
        assert_eq!(s, UnifiedSubRow::Old);
        let (r, s) = fs.resolve_unified_view_row(5);
        assert_eq!(r, ResolvedRow::Data(2));
        assert_eq!(s, UnifiedSubRow::New);
    }

    #[test]
    fn test_unified_empty_file() {
        let rows: Vec<AlignedRow> = vec![];
        let mut fs = test_fold(0, &[]);
        let total = fs.total_view_rows_unified(&rows);
        assert_eq!(total, 0);
    }

    #[test]
    fn test_unified_offsets_cleared_on_fold_mutation() {
        let rows = make_aligned_rows("CMC");
        let hunks = make_hunks(&[(1, 2)]);
        let mut fs = test_fold(3, &hunks);
        fs.ensure_unified_offsets(&rows);
        assert!(fs.unified_offsets_ref().is_some());

        fs.fold_all();
        assert!(fs.unified_offsets_ref().is_none());
        assert!(fs.total_view_rows_unified_cached().is_none());
    }

    #[test]
    fn test_unified_last_row_modified_fence_post() {
        let rows = make_aligned_rows("CCM");
        let hunks = make_hunks(&[(2, 3)]);
        let mut fs = test_fold(3, &hunks);
        fs.ensure_unified_offsets(&rows);

        let total = fs.total_view_rows_unified(&rows);
        assert_eq!(total, 4);

        let (r, s) = fs.resolve_unified_view_row(3);
        assert_eq!(r, ResolvedRow::Data(2));
        assert_eq!(s, UnifiedSubRow::New);
    }

    #[test]
    fn test_unified_with_active_folds() {
        // 100 rows with a hunk at 40..50. test_fold uses context=5, expand_step=20, row_height=2.
        // Segments: Fold(0, 35 hidden, up=false, down=true), Visible(35..55),
        //           Fold(1, 40 hidden, up=true, down=false)
        // Make rows: 0..40 context, 40..50 modified, 50..100 context.
        let mut rows = Vec::new();
        for i in 0..100usize {
            if (40..50).contains(&i) {
                rows.push(AlignedRow::Both {
                    left_line: i,
                    right_line: i,
                    modified: true,
                });
            } else {
                rows.push(AlignedRow::Both {
                    left_line: i,
                    right_line: i,
                    modified: false,
                });
            }
        }
        let hunks = make_hunks(&[(40, 50)]);
        let mut fs = test_fold(100, &hunks);

        // Total in SBS: fold(2) + visible(20) + fold(2) = 24 view rows.
        assert_eq!(fs.total_view_rows(), 24);

        // In unified, the 10 modified rows expand to 20, so visible segment = 30 view rows.
        // Total: fold(2) + 30 + fold(2) = 34.
        let total = fs.total_view_rows_unified(&rows);
        assert_eq!(total, 34);

        // Hidden data row (e.g., row 0) should return None.
        assert_eq!(fs.data_to_unified_view_row(0), None);

        // First visible data row (35) is context: view row = 2 (after fold_row_height=2).
        assert_eq!(fs.data_to_unified_view_row(35), Some(2));

        // Modified data row 40: context rows 35..40 = 5 view rows, so view row = 2 + 5 = 7.
        assert_eq!(fs.data_to_unified_view_row(40), Some(7));

        // Resolve the fold bar (view rows 0,1) — should return fold.
        let (r, _) = fs.resolve_unified_view_row(0);
        assert!(matches!(r, ResolvedRow::ExpandDown { .. }));

        // View row 2 = first visible data row (35).
        let (r, s) = fs.resolve_unified_view_row(2);
        assert_eq!(r, ResolvedRow::Data(35));
        assert_eq!(s, UnifiedSubRow::Single);

        // view_row_to_data_row_for_mode on a fold bar should return nearby data row.
        let dr = fs.view_row_to_data_row_for_mode(0, DiffMode::Unified);
        // Falls back to last_data_end.saturating_sub(1) = 0 (no previous visible segment).
        // Actually last_data_end starts at 0 and no visible segment precedes this fold.
        // saturating_sub(1) on 0 = 0, which is a hidden row but that's the best we can do.
        assert!(dr < 100); // just ensure no panic
    }

    #[test]
    fn test_unified_data_to_view_row_hidden_returns_none() {
        // Same setup as above — data row 10 is hidden inside a fold.
        let mut rows = Vec::new();
        for i in 0..100usize {
            if (40..50).contains(&i) {
                rows.push(AlignedRow::Both {
                    left_line: i,
                    right_line: i,
                    modified: true,
                });
            } else {
                rows.push(AlignedRow::Both {
                    left_line: i,
                    right_line: i,
                    modified: false,
                });
            }
        }
        let hunks = make_hunks(&[(40, 50)]);
        let mut fs = test_fold(100, &hunks);
        fs.ensure_unified_offsets(&rows);

        assert_eq!(fs.data_to_unified_view_row(10), None);
        assert_eq!(fs.data_to_unified_view_row(99), None);
    }

    // --- expose_data_row tests ---

    #[test]
    fn test_expose_data_row_already_visible() {
        let hunks = make_hunks(&[(40, 50)]);
        let mut fs = test_fold(100, &hunks);
        assert!(!fs.expose_data_row(45));
    }

    #[test]
    fn test_expose_data_row_in_folded_region() {
        let hunks = make_hunks(&[(40, 50)]);
        let mut fs = test_fold(100, &hunks);
        let before_rows = fs.total_view_rows();

        assert!(fs.expose_data_row(15));
        assert!(fs.total_view_rows() > before_rows);
        assert!(
            fs.segments()
                .iter()
                .any(|s| matches!(s, Segment::Visible { data_range } if data_range.contains(&15)))
        );
    }

    #[test]
    fn test_expose_data_row_splits_fold() {
        let hunks = make_hunks(&[(100, 110)]);
        let mut fs = FoldState::new(200, &hunks, 5, 20, 2);
        let regions_before = fs.regions.len();

        assert!(fs.expose_data_row(50));
        assert!(fs.regions.len() >= regions_before);
    }

    #[test]
    fn test_expose_data_row_near_fold_edge() {
        let hunks = make_hunks(&[(40, 50)]);
        let mut fs = test_fold(100, &hunks);
        assert!(fs.expose_data_row(33));
    }

    #[test]
    fn test_expose_data_row_out_of_range() {
        let hunks = make_hunks(&[(40, 50)]);
        let mut fs = test_fold(100, &hunks);
        assert!(!fs.expose_data_row(200));
    }
}
