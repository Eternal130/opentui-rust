//! `TextTableRenderable` — port of
//! `packages/core/src/renderables/TextTable.ts` (@opentui 0.5.14).
//!
//! Contract-level port of the settled-tree slice: options merge
//! (:135-188), cell construction (`createCell` :608-623 — one
//! `TextBuffer`/`TextBufferView` pair per cell over the chunk content),
//! the layout solver (`computeLayout` :674-705: intrinsic column widths
//! → expand-to-full or fit-to-constraint, row heights from wrapped line
//! counts, offsets with borders/gaps), the measure function
//! (`setupMeasureFunc` :469-499), painting (`renderSelf` :442-462 →
//! `drawBorders` :994-1014 over `buffer.drawGrid` + `drawCells`
//! :1016-1035 over `drawTextBuffer` per cell).
//!
//! Deferred (upstream liveness/selection machinery, registered in
//! PORT-MATRIX notes): the content-diff cell reuse (`updateCellsDiff`
//! :534-606 — the port rebuilds cells per content change), mouse/keyboard
//! cell selection (:356-435, :1084-1317 — no selection consumer in the
//! headless profile; `selectionBg`/`selectionFg` are carried but inert),
//! and the property-setter dirty tracking (the settled tree is rebuilt
//! instead).

use crate::buffer::OptimizedBuffer;
use crate::color::{parse_color, Rgba};
use crate::renderables::text::WrapMode;
use crate::styled_text::TextChunk;
use crate::text_buffer::TextDefaults;
use crate::text_buffer::{
    compute_virtual_lines, draw_text_view, TextDocument, TextViewState, Viewport,
};

// `MEASURE_HEIGHT` (TextTable.ts:17) is 10_000 upstream — the text
// measure path ignores height, and the port's `TextDocument::measure`
// takes no height at all, so the sentinel has no Rust spelling.

/// `TextTableColumnWidthMode` (TextTable.ts:21).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ColumnWidthMode {
    #[default]
    /// `"full"` — the table default
    Full,
    /// `"content"`
    Content,
}

/// `TextTableColumnFitter` (TextTable.ts:22).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ColumnFitter {
    #[default]
    Proportional,
    Balanced,
}

/// `ResolvedTableBorderLayout` (TextTable.ts:24-31).
#[derive(Debug, Clone, Copy, Default)]
struct BorderLayout {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
    inner_vertical: bool,
    inner_horizontal: bool,
}

/// `TextTableLayout` (TextTable.ts:39-48) — the i32-array twins collapse
/// into the plain offset vectors.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextTableLayout {
    pub column_widths: Vec<u32>,
    pub row_heights: Vec<u32>,
    pub column_offsets: Vec<i32>,
    pub row_offsets: Vec<i32>,
    pub table_width: u32,
    pub table_height: u32,
}

/// `TextTableOptions` (TextTable.ts:75-97) merged with the
/// `RenderableOptions` subset the table consumes. Colors pre-parsed
/// (upstream `parseColor`s at the constructor :174-184).
#[derive(Debug, Clone, Default)]
pub struct TextTableOptions {
    /// `content` — rows of cells, each cell a chunk list (or empty).
    pub content: Vec<Vec<Vec<TextChunk>>>,
    pub wrap_mode: Option<WrapMode>,
    pub column_width_mode: Option<ColumnWidthMode>,
    pub column_fitter: Option<ColumnFitter>,
    pub cell_padding: Option<u32>,
    pub cell_padding_x: Option<u32>,
    pub cell_padding_y: Option<u32>,
    pub column_gap: Option<u32>,
    pub show_borders: Option<bool>,
    pub border: Option<bool>,
    pub outer_border: Option<bool>,
    pub border_style: Option<String>,
    pub border_color: Option<Rgba>,
    pub border_background_color: Option<Rgba>,
    pub background_color: Option<Rgba>,
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub attributes: Option<u32>,
}

/// The `BorderCharacters` grid set (lib/border.ts:55-98 exact glyphs) —
/// the 11-glyph form `borderCharsToArray` packs (lib/border.ts:148-163).
#[derive(Debug, Clone, Copy)]
pub struct GridBorderChars {
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub horizontal: char,
    pub vertical: char,
    pub top_t: char,
    pub bottom_t: char,
    pub left_t: char,
    pub right_t: char,
    pub cross: char,
}

const GRID_SINGLE: GridBorderChars = GridBorderChars {
    top_left: '┌',
    top_right: '┐',
    bottom_left: '└',
    bottom_right: '┘',
    horizontal: '─',
    vertical: '│',
    top_t: '┬',
    bottom_t: '┴',
    left_t: '├',
    right_t: '┤',
    cross: '┼',
};
const GRID_DOUBLE: GridBorderChars = GridBorderChars {
    top_left: '╔',
    top_right: '╗',
    bottom_left: '╚',
    bottom_right: '╝',
    horizontal: '═',
    vertical: '║',
    top_t: '╦',
    bottom_t: '╩',
    left_t: '╠',
    right_t: '╣',
    cross: '╬',
};
const GRID_ROUNDED: GridBorderChars = GridBorderChars {
    top_left: '╭',
    top_right: '╮',
    bottom_left: '╰',
    bottom_right: '╯',
    horizontal: '─',
    vertical: '│',
    top_t: '┬',
    bottom_t: '┴',
    left_t: '├',
    right_t: '┤',
    cross: '┼',
};
const GRID_HEAVY: GridBorderChars = GridBorderChars {
    top_left: '┏',
    top_right: '┓',
    bottom_left: '┗',
    bottom_right: '┛',
    horizontal: '━',
    vertical: '┃',
    top_t: '┳',
    bottom_t: '┻',
    left_t: '┣',
    right_t: '┫',
    cross: '╋',
};

/// `parseBorderStyle` over the `BorderCharArrays` table
/// (lib/border.ts:27-39, 165-170) — unknown → "single".
#[must_use]
pub fn grid_border_chars(style: &str) -> &'static GridBorderChars {
    match style {
        "double" => &GRID_DOUBLE,
        "rounded" => &GRID_ROUNDED,
        "heavy" => &GRID_HEAVY,
        _ => &GRID_SINGLE,
    }
}

/// The resolved constructor state (TextTable.ts:100-184).
#[derive(Debug, Clone)]
pub struct TableConfig {
    /// `_wrapMode` (default "word", :137/:163)
    pub wrap_mode: WrapMode,
    /// `_columnWidthMode` (default "full", :138/:164)
    pub column_width_mode: ColumnWidthMode,
    /// `_columnFitter` (`resolveColumnFitter` :1352-1358)
    pub column_fitter: ColumnFitter,
    /// `_cellPaddingX`/`_cellPaddingY` (`resolveCellPadding` :1360-1366)
    pub cell_padding_x: u32,
    pub cell_padding_y: u32,
    /// `_columnGap` (`resolveColumnGap` :1368-1374)
    pub column_gap: u32,
    /// `_showBorders` (default true, :144/:169)
    pub show_borders: bool,
    /// `_border` (default true, :145/:170)
    pub border: bool,
    /// `_outerBorder` (default = border, :146/:171-172)
    pub outer_border: bool,
    /// `_borderStyle` (`parseBorderStyle`, default "single")
    pub border_style: String,
    /// `_borderColor` (default #FFFFFF, :151/:177)
    pub border_color: Rgba,
    /// `_borderBackgroundColor` (default transparent, :152/:178-180)
    pub border_background_color: Rgba,
    /// `_backgroundColor` (default transparent, :153/:181)
    pub background_color: Rgba,
    /// `_defaultFg` (default #FFFFFF, :154/:182)
    pub default_fg: Rgba,
    /// `_defaultBg` (default transparent, :155/:183)
    pub default_bg: Rgba,
    /// `_defaultAttributes` (default 0, :156/:184)
    pub default_attributes: u32,
}

impl Default for TableConfig {
    fn default() -> Self {
        // `_defaultOptions` (TextTable.ts:135-157)
        TableConfig {
            wrap_mode: WrapMode::Word,
            column_width_mode: ColumnWidthMode::Full,
            column_fitter: ColumnFitter::Proportional,
            cell_padding_x: 0,
            cell_padding_y: 0,
            column_gap: 0,
            show_borders: true,
            border: true,
            outer_border: true,
            border_style: "single".into(),
            border_color: parse_color("#FFFFFF"),
            border_background_color: parse_color("transparent"),
            background_color: parse_color("transparent"),
            default_fg: parse_color("#FFFFFF"),
            default_bg: parse_color("transparent"),
            default_attributes: 0,
        }
    }
}

/// The layout-solver core — cells + resolved config. Shared by the
/// paint state and the taffy measure context (the `MeasureCtx` clone of
/// `setupMeasureFunc`'s closure over `this`).
#[derive(Debug, Clone, Default)]
pub struct TableCore {
    /// `rebuildCells` (:501-532) — one `TextDocument` per cell
    /// (`createCell` :608-623: TextBuffer with the table's default
    /// fg/bg/attributes + `setStyledText(toStyledText(content))`).
    pub cells: Vec<Vec<TextDocument>>,
    pub config: TableConfig,
}

/// `MeasuredTable` — the `MeasureCtx` payload (the native measure
/// target over `computeLayout`, TextTable.ts:469-499).
#[derive(Debug, Clone, Default)]
pub struct MeasuredTable {
    pub core: TableCore,
}

impl TableCore {
    /// `rebuildCells` (:501-506) — row/column counts from the content.
    pub fn row_count(&self) -> usize {
        self.cells.len()
    }

    pub fn column_count(&self) -> usize {
        self.cells.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// `resolveBorderLayout` (TextTable.ts:967-976).
    fn border_layout(&self) -> BorderLayout {
        let columns = self.column_count();
        let rows = self.row_count();
        BorderLayout {
            left: self.config.outer_border,
            right: self.config.outer_border,
            top: self.config.outer_border,
            bottom: self.config.outer_border,
            inner_vertical: self.config.border && columns > 1,
            inner_horizontal: self.config.border && rows > 1,
        }
    }

    /// `getHorizontalCellPadding` / `getVerticalCellPadding`
    /// (TextTable.ts:1344-1350) — padding counts both sides.
    fn horizontal_padding(&self) -> u32 {
        self.config.cell_padding_x * 2
    }

    fn vertical_padding(&self) -> u32 {
        self.config.cell_padding_y * 2
    }

    /// `getInterColumnGap` / `getTotalInterColumnGap`
    /// (TextTable.ts:930-940) — gaps replace the inner vertical borders
    /// when those are off.
    fn inter_column_gap(&self, b: &BorderLayout) -> u32 {
        if b.inner_vertical {
            0
        } else {
            self.config.column_gap
        }
    }

    fn total_inter_column_gap(&self, b: &BorderLayout) -> u32 {
        self.column_count().saturating_sub(1) as u32 * self.inter_column_gap(b)
    }

    /// `getVerticalBorderCount` (TextTable.ts:978-984).
    fn vertical_border_count(&self, b: &BorderLayout) -> u32 {
        u32::from(b.left)
            + u32::from(b.right)
            + if b.inner_vertical {
                self.column_count().saturating_sub(1) as u32
            } else {
                0
            }
    }

    /// `getHorizontalBorderCount` (TextTable.ts:986-992).
    #[allow(dead_code)]
    fn horizontal_border_count(&self, b: &BorderLayout) -> u32 {
        u32::from(b.top)
            + u32::from(b.bottom)
            + if b.inner_horizontal {
                self.row_count().saturating_sub(1) as u32
            } else {
                0
            }
    }

    /// `isFullWidthMode` (TextTable.ts:707-709).
    fn is_full_width_mode(&self) -> bool {
        self.config.column_width_mode == ColumnWidthMode::Full
    }

    /// `resolveLayoutWidthConstraint` (TextTable.ts:1332-1342): a width
    /// only constrains when wrapping is enabled or the table stretches
    /// full-width.
    fn resolve_width_constraint(&self, width: Option<f32>) -> Option<u32> {
        let w = width?;
        if !w.is_finite() || w <= 0.0 {
            return None;
        }
        if self.config.wrap_mode != WrapMode::None || self.is_full_width_mode() {
            Some(w.max(1.0).floor() as u32)
        } else {
            None
        }
    }

    /// `computeColumnWidths` (TextTable.ts:711-753): intrinsic max line
    /// width per column (+padding), then expand ("full") or fit
    /// (wrap-enabled) to the content budget.
    fn compute_column_widths(&self, max_table_width: Option<u32>, b: &BorderLayout) -> Vec<u32> {
        let columns = self.column_count();
        let horizontal_padding = self.horizontal_padding();
        let mut intrinsic = vec![1 + horizontal_padding; columns];

        for row in &self.cells {
            for (col, cell) in row.iter().enumerate() {
                // measureForDimensions(0, MEASURE_HEIGHT) → intrinsic
                let (w, _) = cell.measure(self.config.wrap_mode, 0);
                let measured = w.max(1) + horizontal_padding;
                intrinsic[col] = intrinsic[col].max(measured);
            }
        }

        let Some(max_table_width) = max_table_width else {
            return intrinsic;
        };
        if max_table_width == 0 {
            return intrinsic;
        }

        let max_content_width = (max_table_width as i64
            - self.vertical_border_count(b) as i64
            - self.total_inter_column_gap(b) as i64)
            .max(1);
        let current: i64 = intrinsic.iter().map(|&w| w as i64).sum();

        if current == max_content_width {
            return intrinsic;
        }
        if current < max_content_width {
            if self.is_full_width_mode() {
                return self.expand_column_widths(&intrinsic, max_content_width);
            }
            return intrinsic;
        }
        if self.config.wrap_mode == WrapMode::None {
            return intrinsic;
        }
        self.fit_column_widths(&intrinsic, max_content_width)
    }

    /// `expandColumnWidths` (TextTable.ts:755-777): even split of the
    /// surplus, remainder to the first columns.
    fn expand_column_widths(&self, widths: &[u32], target: i64) -> Vec<u32> {
        let base: Vec<i64> = widths.iter().map(|&w| (w as i64).max(1)).collect();
        let total: i64 = base.iter().sum();
        if total >= target {
            return base.iter().map(|&w| w as u32).collect();
        }
        let columns = base.len() as i64;
        let extra = target - total;
        let shared = extra / columns;
        let remainder = extra % columns;
        base.iter()
            .enumerate()
            .map(|(idx, &w)| (w + shared + i64::from((idx as i64) < remainder)) as u32)
            .collect()
    }

    /// `fitColumnWidths` (TextTable.ts:779-785).
    fn fit_column_widths(&self, widths: &[u32], target: i64) -> Vec<u32> {
        if self.config.column_fitter == ColumnFitter::Balanced {
            self.fit_column_widths_balanced(widths, target)
        } else {
            self.fit_column_widths_proportional(widths, target)
        }
    }

    /// `fitColumnWidthsProportional` (TextTable.ts:787-790) over
    /// `allocateProportionalColumnWidths` (text-table-width.ts).
    fn fit_column_widths_proportional(&self, widths: &[u32], target: i64) -> Vec<u32> {
        let min_width = 1 + self.horizontal_padding();
        allocate_proportional_column_widths(widths, target, min_width as i64)
    }

    /// `fitColumnWidthsBalanced` (TextTable.ts:792-824).
    fn fit_column_widths_balanced(&self, widths: &[u32], target: i64) -> Vec<u32> {
        let min_width = (1 + self.horizontal_padding()) as i64;
        let columns = widths.len();
        let base: Vec<i64> = widths.iter().map(|&w| (w as i64).max(1)).collect();
        let total: i64 = base.iter().sum();
        if columns == 0 || total <= target {
            return base.iter().map(|&w| w as u32).collect();
        }

        let even_share = (target / columns as i64).max(min_width);
        let preferred: Vec<i64> = base.iter().map(|&w| w.min(even_share)).collect();
        let preferred_total: i64 = preferred.iter().sum();
        let floor_widths: Vec<i64> = if preferred_total <= target {
            preferred
        } else {
            vec![min_width; columns]
        };
        let floor_total: i64 = floor_widths.iter().sum();
        let clamped_target = target.max(floor_total);

        if total <= clamped_target {
            return base.iter().map(|&w| w as u32).collect();
        }

        let shrinkable: Vec<i64> = base
            .iter()
            .zip(&floor_widths)
            .map(|(&w, &f)| w - f)
            .collect();
        let total_shrinkable: i64 = shrinkable.iter().sum();
        if total_shrinkable <= 0 {
            return floor_widths.iter().map(|&w| w as u32).collect();
        }

        let target_shrink = total - clamped_target;
        let shrink = self.allocate_shrink_by_weight(&shrinkable, target_shrink);
        base.iter()
            .zip(&shrink)
            .zip(&floor_widths)
            .map(|((&w, &s), &f)| (w - s).max(f) as u32)
            .collect()
    }

    /// `allocateShrinkByWeight` (TextTable.ts:826-888) — sqrt-weighted
    /// shrink with largest-remainder distribution (ties → larger
    /// shrinkable wins).
    fn allocate_shrink_by_weight(&self, shrinkable: &[i64], target_shrink: i64) -> Vec<i64> {
        let mut shrink = vec![0i64; shrinkable.len()];
        if target_shrink <= 0 {
            return shrink;
        }
        let weights: Vec<f64> = shrinkable
            .iter()
            .map(|&v| if v <= 0 { 0.0 } else { (v as f64).sqrt() })
            .collect();
        let total_weight: f64 = weights.iter().sum();
        if total_weight <= 0.0 {
            return shrink;
        }

        let mut fractions = vec![0.0f64; shrinkable.len()];
        let mut used = 0i64;
        for idx in 0..shrinkable.len() {
            if shrinkable[idx] <= 0 || weights[idx] <= 0.0 {
                continue;
            }
            let exact = weights[idx] / total_weight * target_shrink as f64;
            let whole = shrinkable[idx].min(exact.floor() as i64);
            shrink[idx] = whole;
            fractions[idx] = exact - whole as f64;
            used += whole;
        }

        let mut remaining = target_shrink - used;
        while remaining > 0 {
            let mut best_idx: i32 = -1;
            let mut best_fraction = -1.0f64;
            for idx in 0..shrinkable.len() {
                if shrinkable[idx] - shrink[idx] <= 0 {
                    continue;
                }
                if best_idx == -1
                    || fractions[idx] > best_fraction
                    || (fractions[idx] == best_fraction
                        && shrinkable[idx] > shrinkable[best_idx as usize])
                {
                    best_idx = idx as i32;
                    best_fraction = fractions[idx];
                }
            }
            if best_idx == -1 {
                break;
            }
            shrink[best_idx as usize] += 1;
            fractions[best_idx as usize] = 0.0;
            remaining -= 1;
        }
        shrink
    }

    /// `computeRowHeights` (TextTable.ts:890-908): max wrapped line
    /// count per row (+padding).
    fn compute_row_heights(&self, column_widths: &[u32]) -> Vec<u32> {
        let horizontal_padding = self.horizontal_padding();
        let vertical_padding = self.vertical_padding();
        let mut row_heights = vec![1 + vertical_padding; self.row_count()];

        for (row_idx, row) in self.cells.iter().enumerate() {
            for (col_idx, cell) in row.iter().enumerate() {
                let width = (column_widths.get(col_idx).copied().unwrap_or(1))
                    .saturating_sub(horizontal_padding)
                    .max(1);
                let (_, lines) = cell.measure(self.config.wrap_mode, width);
                let line_count = lines.max(1);
                row_heights[row_idx] = row_heights[row_idx].max(line_count + vertical_padding);
            }
        }
        row_heights
    }

    /// `computeOffsets` (TextTable.ts:910-928).
    fn compute_offsets(
        &self,
        parts: &[u32],
        start_boundary: bool,
        end_boundary: bool,
        include_inner_boundaries: bool,
        inner_gap: u32,
    ) -> Vec<i32> {
        let mut offsets = vec![if start_boundary { 0 } else { -1 }];
        let mut cursor = offsets[0];
        for (idx, &size) in parts.iter().enumerate() {
            let size = size.max(1) as i32;
            let separator_after = if idx < parts.len() - 1 {
                if include_inner_boundaries {
                    1
                } else {
                    inner_gap as i32
                }
            } else if end_boundary {
                1
            } else {
                0
            };
            cursor += size + separator_after;
            offsets.push(cursor);
        }
        offsets
    }

    /// `computeLayout` (TextTable.ts:674-705).
    pub fn compute_layout(&self, max_table_width: Option<u32>) -> TextTableLayout {
        if self.row_count() == 0 || self.column_count() == 0 {
            // `createEmptyLayout` (TextTable.ts:1319-1330)
            return TextTableLayout {
                column_widths: Vec::new(),
                row_heights: Vec::new(),
                column_offsets: vec![0],
                row_offsets: vec![0],
                table_width: 0,
                table_height: 0,
            };
        }
        let border_layout = self.border_layout();
        let column_widths = self.compute_column_widths(max_table_width, &border_layout);
        let row_heights = self.compute_row_heights(&column_widths);
        let column_offsets = self.compute_offsets(
            &column_widths,
            border_layout.left,
            border_layout.right,
            border_layout.inner_vertical,
            self.inter_column_gap(&border_layout),
        );
        let row_offsets = self.compute_offsets(
            &row_heights,
            border_layout.top,
            border_layout.bottom,
            border_layout.inner_horizontal,
            0,
        );
        let table_width = (*column_offsets.last().unwrap_or(&0) + 1).max(0) as u32;
        let table_height = (*row_offsets.last().unwrap_or(&0) + 1).max(0) as u32;
        TextTableLayout {
            column_widths,
            row_heights,
            column_offsets,
            row_offsets,
            table_width,
            table_height,
        }
    }

    /// `setupMeasureFunc`'s measure closure (TextTable.ts:470-496):
    /// resolve the constraint from the incoming width, compute the
    /// layout, clamp ≥ 1; under AtMost (a definite available width with
    /// no fixed width of our own) the reported width clamps to it.
    /// Intrinsic height is always kept.
    pub fn measure(&self, known_width: Option<f32>, available_width: Option<f32>) -> (f32, f32) {
        // hasWidthConstraint = widthMode !== Undefined && finite
        let raw: Option<u32> = known_width
            .filter(|w| w.is_finite())
            .or_else(|| available_width.filter(|w| w.is_finite()))
            .map(|w| w.max(1.0).floor() as u32);
        let constraint = self.resolve_width_constraint(raw.map(|r| r as f32));
        let layout = self.compute_layout(constraint);
        let mut measured_width = layout.table_width.max(1) as f32;
        let measured_height = layout.table_height.max(1) as f32;
        // widthMode === AtMost → min(rawConstraint, measured)
        if known_width.is_none() {
            if let Some(raw) = raw {
                measured_width = measured_width.min(raw as f32);
            }
        }
        (measured_width, measured_height)
    }

    /// `applyLayoutToViews` (TextTable.ts:942-965) — per-cell wrap width
    /// + viewport, in row-major cell order.
    pub fn cell_views(&self, layout: &TextTableLayout) -> Vec<(Option<u32>, Viewport)> {
        let horizontal_padding = self.horizontal_padding();
        let vertical_padding = self.vertical_padding();
        let mut views = Vec::new();
        for (row_idx, row) in self.cells.iter().enumerate() {
            for (col_idx, _) in row.iter().enumerate() {
                let col_width = layout.column_widths.get(col_idx).copied().unwrap_or(1);
                let row_height = layout.row_heights.get(row_idx).copied().unwrap_or(1);
                let content_width = col_width.saturating_sub(horizontal_padding).max(1);
                let content_height = row_height.saturating_sub(vertical_padding).max(1);
                let wrap_width = if self.config.wrap_mode == WrapMode::None {
                    None
                } else {
                    Some(content_width)
                };
                views.push((
                    wrap_width,
                    Viewport {
                        x: 0,
                        y: 0,
                        width: content_width,
                        height: content_height,
                    },
                ));
            }
        }
        views
    }
}

/// `allocateProportionalColumnWidths` — text-table-width.ts:12-88
/// (exact): clamp to min, water-fill sqrt weights in weight order, then
/// largest-remainder correction by squared-cross-product priority.
/// Integer widths fit i64 throughout, so the BigInt fallback is exact
/// arithmetic by construction.
#[must_use]
pub fn allocate_proportional_column_widths(
    widths: &[u32],
    target_width: i64,
    min_width: i64,
) -> Vec<u32> {
    // comparePriority (text-table-width.ts:1-9)
    fn compare_priority(
        left_growth: i64,
        left_capacity: i64,
        right_growth: i64,
        right_capacity: i64,
    ) -> std::cmp::Ordering {
        // the squared cross-products exceed i64 for pathological widths
        // (upstream's BigInt fallback, text-table-width.ts:14-17)
        let left = (left_growth as i128) * (left_growth as i128) * right_capacity as i128;
        let right = (right_growth as i128) * (right_growth as i128) * left_capacity as i128;
        left.cmp(&right)
    }

    let base: Vec<i64> = widths.iter().map(|&w| (w as i64).max(min_width)).collect();
    let total_base: i64 = base.iter().sum();
    let capacity: Vec<i64> = base.iter().map(|&w| w - min_width).collect();
    let capacity_total: i64 = capacity.iter().sum();
    let floor_total = min_width * base.len() as i64;
    let available = (target_width - floor_total)
        .max(0)
        .min(total_base - floor_total);

    if available == 0 {
        return vec![min_width as u32; base.len()];
    }
    if available == capacity_total {
        return base.iter().map(|&w| w as u32).collect();
    }

    let mut growth = vec![0i64; base.len()];
    let mut active: Vec<(usize, f64)> = capacity
        .iter()
        .enumerate()
        .filter(|&(_, &cap)| cap > 0)
        .map(|(idx, &cap)| (idx, (cap as f64).sqrt()))
        .collect();
    // JS `.sort((a,b) => a.weight - b.weight)` — stable ascending
    active.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    if active.len() == capacity.len() && capacity.iter().all(|&c| c == capacity[0]) {
        let shared = available / capacity.len() as i64;
        let remainder = available % capacity.len() as i64;
        return growth
            .iter()
            .enumerate()
            .map(|(idx, _)| (min_width + shared + i64::from((idx as i64) < remainder)) as u32)
            .collect();
    }

    let mut remaining = available;
    let mut total_weight: f64 = active.iter().map(|(_, w)| w).sum();

    for &(idx, weight) in &active {
        if remaining as f64 / total_weight <= weight {
            break;
        }
        growth[idx] = capacity[idx];
        remaining -= capacity[idx];
        total_weight -= weight;
    }
    let level = remaining as f64 / total_weight;
    for &(idx, weight) in &active {
        if growth[idx] == capacity[idx] {
            continue;
        }
        growth[idx] = growth[idx]
            .min((level * weight).floor() as i64)
            .min(capacity[idx]);
    }

    let mut allocated: i64 = growth.iter().sum();

    while allocated > available {
        let mut worst_idx: i32 = -1;
        for idx in 0..base.len() {
            if growth[idx] == 0 {
                continue;
            }
            let cmp = if worst_idx == -1 {
                std::cmp::Ordering::Greater
            } else {
                compare_priority(
                    growth[idx],
                    capacity[idx],
                    growth[worst_idx as usize],
                    capacity[worst_idx as usize],
                )
            };
            if cmp == std::cmp::Ordering::Greater
                || (cmp == std::cmp::Ordering::Equal && idx > worst_idx as usize)
            {
                worst_idx = idx as i32;
            }
        }
        if worst_idx == -1 {
            break;
        }
        growth[worst_idx as usize] -= 1;
        allocated -= 1;
    }

    while allocated < available {
        let mut best_idx: i32 = -1;
        for idx in 0..base.len() {
            if growth[idx] >= capacity[idx] {
                continue;
            }
            let cmp = if best_idx == -1 {
                std::cmp::Ordering::Less
            } else {
                compare_priority(
                    growth[idx] + 1,
                    capacity[idx],
                    growth[best_idx as usize] + 1,
                    capacity[best_idx as usize],
                )
            };
            if cmp == std::cmp::Ordering::Less {
                best_idx = idx as i32;
            }
        }
        if best_idx == -1 {
            break;
        }
        growth[best_idx as usize] += 1;
        allocated += 1;
    }

    growth.iter().map(|&g| (g + min_width) as u32).collect()
}

/// The drawn state — the cells + config plus the layout applied at the
/// current width (`rebuildLayoutForCurrentWidth`, TextTable.ts:653-672)
/// and the per-cell virtual-line caches (the port's paint-time analog
/// of the live `TextBufferView`s).
pub struct TextTableState {
    pub core: TableCore,
    layout: TextTableLayout,
    layout_width: Option<u32>,
    /// per-cell (wrap width, viewport) — `applyLayoutToViews`
    cell_views: Vec<(Option<u32>, Viewport)>,
    /// per-cell cached vlines (filled by `prepare_paint`)
    cell_vlines: Vec<Vec<crate::text_buffer::VirtualLine>>,
    cell_vlines_valid: bool,
}

impl TextTableState {
    /// The constructor (TextTable.ts:159-188) + `rebuildCells`.
    #[must_use]
    pub fn from_options(opts: &TextTableOptions) -> Self {
        let defaults = TableConfig::default();
        let cell_padding_x = opts
            .cell_padding_x
            .unwrap_or(opts.cell_padding.unwrap_or(0));
        let cell_padding_y = opts
            .cell_padding_y
            .unwrap_or(opts.cell_padding.unwrap_or(0));
        let border = opts.border.unwrap_or(true);
        let config = TableConfig {
            wrap_mode: opts.wrap_mode.unwrap_or(defaults.wrap_mode),
            column_width_mode: opts.column_width_mode.unwrap_or(defaults.column_width_mode),
            column_fitter: opts.column_fitter.unwrap_or(defaults.column_fitter),
            cell_padding_x,
            cell_padding_y,
            column_gap: opts.column_gap.unwrap_or(0),
            show_borders: opts.show_borders.unwrap_or(true),
            border,
            outer_border: opts.outer_border.unwrap_or(border),
            border_style: opts.border_style.clone().unwrap_or_else(|| "single".into()),
            border_color: opts.border_color.unwrap_or(defaults.border_color),
            border_background_color: opts
                .border_background_color
                .unwrap_or(defaults.border_background_color),
            background_color: opts.background_color.unwrap_or(defaults.background_color),
            default_fg: opts.fg.unwrap_or(defaults.default_fg),
            default_bg: opts.bg.unwrap_or(defaults.default_bg),
            default_attributes: opts.attributes.unwrap_or(0),
        };
        // `createCell` (:608-623): a TextBuffer with the table defaults
        // over `toStyledText(content)` (:625-635 — chunk list or "").
        let defaults = TextDefaults {
            fg: config.default_fg,
            bg: config.default_bg,
            attributes: config.default_attributes,
        };
        let cells = opts
            .content
            .iter()
            .map(|row| {
                row.iter()
                    .map(|chunks| {
                        // `toStyledText` (:625-635) — a chunk list, or
                        // `stringToStyledText("")` for null/undefined
                        if chunks.is_empty() {
                            TextDocument::from_styled(&[], defaults)
                        } else {
                            TextDocument::from_styled(chunks, defaults)
                        }
                    })
                    .collect()
            })
            .collect();
        TextTableState {
            core: TableCore { cells, config },
            layout: TextTableLayout::default(),
            layout_width: None,
            cell_views: Vec::new(),
            cell_vlines: Vec::new(),
            cell_vlines_valid: false,
        }
    }

    /// The measure-context payload (the clone of `this` the upstream
    /// measure closure captures).
    #[must_use]
    pub fn measured(&self) -> MeasuredTable {
        MeasuredTable {
            core: self.core.clone(),
        }
    }

    /// `onResize` → `rebuildLayoutForCurrentWidth`
    /// (TextTable.ts:437-440, 653-672): re-solve the layout whenever the
    /// constraint width moved.
    pub fn update_layout(&mut self, width: u32, _height: u32) {
        let constraint = self.core.resolve_width_constraint(Some(width as f32));
        if self.layout_width == constraint && !self.cell_views.is_empty() {
            return;
        }
        self.layout = self.core.compute_layout(constraint);
        self.cell_views = self.core.cell_views(&self.layout);
        self.cell_vlines_valid = false;
        self.layout_width = constraint;
    }

    /// Fill the per-cell vline caches (the paint walk is `&self`; the
    /// live views hold their own wrapped lines).
    pub fn prepare_paint(&mut self) {
        if self.cell_vlines_valid {
            return;
        }
        self.cell_vlines = self
            .core
            .cells
            .iter()
            .flatten()
            .zip(&self.cell_views)
            .map(|(doc, (wrap_width, _))| {
                let wrap = if self.core.config.wrap_mode == WrapMode::None {
                    None
                } else {
                    *wrap_width
                }
                .filter(|w| *w > 0);
                compute_virtual_lines(doc, self.core.config.wrap_mode, wrap)
            })
            .collect();
        self.cell_vlines_valid = true;
    }

    /// `renderSelf` (TextTable.ts:442-462): background clear, borders,
    /// cells — at screen coords.
    pub fn paint(&self, buffer: &mut OptimizedBuffer, screen_x: i32, screen_y: i32) {
        if self.core.row_count() == 0 || self.core.column_count() == 0 {
            return;
        }
        // `buffer.clear(this._backgroundColor)` (:451) — the port paints
        // into the shared buffer, so a transparent clear composes as a
        // no-op (only an opaque background fills).
        if self.core.config.background_color.alpha() > 0 {
            buffer.fill_rect(
                screen_x,
                screen_y,
                self.layout.table_width,
                self.layout.table_height,
                self.core.config.background_color,
            );
        }
        self.draw_borders(buffer, screen_x, screen_y);
        self.draw_cells(buffer, screen_x, screen_y);
    }

    /// `drawBorders` (TextTable.ts:994-1014).
    fn draw_borders(&self, buffer: &mut OptimizedBuffer, screen_x: i32, screen_y: i32) {
        if !self.core.config.show_borders {
            return;
        }
        buffer.draw_grid(crate::buffer::DrawGridOptions {
            x: screen_x,
            y: screen_y,
            border_chars: grid_border_chars(&self.core.config.border_style),
            border_fg: self.core.config.border_color,
            border_bg: self.core.config.border_background_color,
            column_offsets: &self.layout.column_offsets,
            row_offsets: &self.layout.row_offsets,
            draw_inner: self.core.config.border,
            draw_outer: self.core.config.outer_border,
        });
    }

    /// `drawCells`/`drawCellRange` (TextTable.ts:1016-1035): each cell
    /// at (columnOffset + 1 + cellPaddingX, rowOffset + 1 +
    /// cellPaddingY) through `drawTextBuffer`.
    fn draw_cells(&self, buffer: &mut OptimizedBuffer, screen_x: i32, screen_y: i32) {
        let pad_x = self.core.config.cell_padding_x as i32;
        let pad_y = self.core.config.cell_padding_y as i32;
        let mut cell_idx = 0usize;
        for (row_idx, row) in self.core.cells.iter().enumerate() {
            let cell_y = self.layout.row_offsets.get(row_idx).copied().unwrap_or(0) + 1 + pad_y;
            for col_idx in 0..row.len() {
                let cell_x = self
                    .layout
                    .column_offsets
                    .get(col_idx)
                    .copied()
                    .unwrap_or(0)
                    + 1
                    + pad_x;
                let Some((_, viewport)) = self.cell_views.get(cell_idx) else {
                    cell_idx += 1;
                    continue;
                };
                let view = TextViewState {
                    text_align: crate::text_buffer::TextAlign::Left,
                    tab_indicator: None,
                    tab_indicator_color: None,
                    viewport: Some(*viewport),
                };
                let empty;
                let vlines: &[crate::text_buffer::VirtualLine] = if self.cell_vlines_valid {
                    self.cell_vlines
                        .get(cell_idx)
                        .map(|v| v.as_slice())
                        .unwrap_or(&[])
                } else {
                    empty = Vec::new();
                    &empty
                };
                if !vlines.is_empty() {
                    draw_text_view(
                        buffer,
                        &self.core.cells[row_idx][col_idx],
                        &view,
                        vlines,
                        screen_x + cell_x,
                        screen_y + cell_y,
                    );
                }
                cell_idx += 1;
            }
        }
    }
}

// header alignment note: upstream `TextTableRenderable` has no
// per-column text alignment — markdown `|:---:|` row data is unused by
// both sides (Markdown.ts passes no align), so parity holds by
// construction.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::styled_text::string_to_styled_text;

    fn cell(s: &str) -> Vec<TextChunk> {
        string_to_styled_text(s).chunks
    }

    fn core(content: Vec<Vec<Vec<TextChunk>>>, f: impl FnOnce(&mut TextTableOptions)) -> TableCore {
        let mut opts = TextTableOptions {
            content,
            ..Default::default()
        };
        f(&mut opts);
        TextTableState::from_options(&opts).core
    }

    /// TextTable.test.ts:189-196 "proportional allocation preserves
    /// square-root priority regressions".
    #[test]
    fn proportional_allocation_regressions() {
        assert_eq!(
            allocate_proportional_column_widths(&[91, 9], 37, 1),
            vec![28, 9]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[91, 9], 38, 1),
            vec![29, 9]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[1000, 100], 401, 1),
            vec![305, 96]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[1000, 100], 402, 1),
            vec![305, 97]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[4, 49, 4, 54, 38], 104, 1),
            vec![4, 33, 4, 34, 29]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[2_516_760, 2_528_584], 58_886, 1),
            vec![29_408, 29_478]
        );
    }

    /// TextTable.test.ts:198-207 "…resolves exact priority ties by lower
    /// index".
    #[test]
    fn proportional_allocation_ties_by_lower_index() {
        assert_eq!(
            allocate_proportional_column_widths(&[7, 7, 7], 10, 3),
            vec![4, 3, 3]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[7, 7, 7], 11, 3),
            vec![4, 4, 3]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[7, 7, 7], 12, 3),
            vec![4, 4, 4]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[7, 7, 7], 13, 3),
            vec![5, 4, 4]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[7, 7, 7], 17, 3),
            vec![6, 6, 5]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[7, 7, 7], 20, 3),
            vec![7, 7, 6]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[19, 3], 5, 1),
            vec![4, 1]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[4_000_000_001, 1_000_000_001], 300_001, 1),
            vec![200_001, 100_000]
        );
    }

    /// TextTable.test.ts:249-254 "…clamps targets and preserves hard
    /// minimums and intrinsic caps".
    #[test]
    fn proportional_allocation_clamps() {
        assert_eq!(
            allocate_proportional_column_widths(&[4, 49, 4, 54, 38], 3, 1),
            vec![1, 1, 1, 1, 1]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[4, 49, 4, 54, 38], 149, 1),
            vec![4, 49, 4, 54, 38]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[4, 49, 4, 54, 38], 200, 1),
            vec![4, 49, 4, 54, 38]
        );
        assert_eq!(
            allocate_proportional_column_widths(&[2, 8, 20], 14, 3),
            vec![3, 5, 6]
        );
    }

    /// TextTable.test.ts:209-227 "renders unequal-capacity priority
    /// ties in lower column index order" — width 8, cells
    /// ABCDEFGHIJKLMNOPQRS + XYZ give vertical borders at columns
    /// 0/5/7 (computeLayout column offsets, TextTable.ts:682-694).
    #[test]
    fn layout_priority_tie_offsets() {
        let content = vec![vec![cell("ABCDEFGHIJKLMNOPQRS"), cell("XYZ")]];
        let c = core(content, |o| {
            o.wrap_mode = Some(WrapMode::Word);
        });
        let layout = c.compute_layout(Some(8));
        // fit path: maxContentWidth = 8 - 3 borders = 5 → proportional
        assert_eq!(layout.column_widths, vec![4, 1]);
        assert_eq!(layout.column_offsets, vec![0, 5, 7]);
        assert_eq!(layout.table_width, 8);
    }

    /// TextTable.test.ts:229-247 "renders padded unequal-capacity
    /// priority ties in lower column index order" — cellPaddingX 1, width 12.
    #[test]
    fn layout_padded_priority_tie_offsets() {
        let content = vec![vec![cell("ABCDEFGHIJKLMNOPQRS"), cell("XYZ")]];
        let c = core(content, |o| {
            o.wrap_mode = Some(WrapMode::Word);
            o.cell_padding_x = Some(1);
        });
        let layout = c.compute_layout(Some(12));
        // maxContentWidth = 12 - 3 = 9; minWidth = 1 + 2 = 3
        assert_eq!(layout.column_widths, vec![6, 3]);
        assert_eq!(layout.column_offsets, vec![0, 7, 11]);
        assert_eq!(layout.table_width, 12);
    }

    /// TextTable.test.ts:358-383 "keeps intrinsic width in content mode
    /// when extra space is available" (:740-745 — content mode returns
    /// intrinsic widths).
    #[test]
    fn content_mode_keeps_intrinsic_width() {
        let content = vec![vec![cell("A"), cell("B")], vec![cell("1"), cell("2")]];
        let c = core(content, |o| {
            o.wrap_mode = Some(WrapMode::Word);
            o.column_width_mode = Some(ColumnWidthMode::Content);
        });
        let layout = c.compute_layout(Some(34));
        assert_eq!(layout.column_widths, vec![1, 1]);
        assert_eq!(layout.table_width, 5); // 1+1+1 border, 1+1
        assert!(layout.table_width < 33);
    }

    /// TextTable.test.ts:386-409 "fills available width by default in
    /// full mode" (`expandColumnWidths` :755-777).
    #[test]
    fn full_mode_fills_available_width() {
        let content = vec![vec![cell("A"), cell("B")], vec![cell("1"), cell("2")]];
        let c = core(content, |o| {
            o.wrap_mode = Some(WrapMode::Word);
        });
        let layout = c.compute_layout(Some(34));
        assert_eq!(layout.column_widths.len(), 2);
        let sum: u32 = layout.column_widths.iter().sum();
        // 34 - 3 borders split between the columns
        assert_eq!(sum, 31);
        assert_eq!(layout.table_width, 34);
    }

    /// TextTable.test.ts:460-485 "applies cell padding when provided" —
    /// padding widens the columns (:713/:721) and the row heights
    /// (:893/:903).
    #[test]
    fn padding_widens_cells() {
        let content = vec![vec![cell("A"), cell("B")]];
        let c = core(content, |o| {
            o.cell_padding_x = Some(1);
            o.cell_padding_y = Some(1);
        });
        let layout = c.compute_layout(None);
        // intrinsic: 1 + 2*padX per column
        assert_eq!(layout.column_widths, vec![3, 3]);
        assert_eq!(layout.row_heights, vec![3]);
        // offsets include the +1 padding shift inside the borders
        let views = c.cell_views(&layout);
        assert_eq!(
            views,
            vec![
                (
                    Some(1),
                    Viewport {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1
                    }
                ),
                (
                    Some(1),
                    Viewport {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1
                    }
                )
            ]
        );
    }

    /// TextTable.test.ts:335-356 "wraps content and fits columns when
    /// width is constrained" — row heights grow with the wrapped line
    /// count (`computeRowHeights` :890-908).
    #[test]
    fn constrained_width_wraps_rows() {
        let content = vec![
            vec![cell("ID"), cell("Description")],
            vec![
                cell("1"),
                cell("This is a long sentence that should wrap across multiple visual lines"),
            ],
            vec![cell("2"), cell("Short")],
        ];
        let c = core(content, |o| {
            o.wrap_mode = Some(WrapMode::Word);
        });
        let layout = c.compute_layout(Some(34));
        assert_eq!(layout.table_width, 34);
        // the wrapping row must be taller than the others
        assert!(layout.row_heights[1] > layout.row_heights[0]);
        assert!(layout.row_heights[1] > layout.row_heights[2]);
    }

    /// TextTable.ts:707-709 + 1332-1342 — wrapMode "none" keeps
    /// intrinsic widths under a constraint (:748-750).
    #[test]
    fn no_wrap_keeps_intrinsic() {
        let content = vec![vec![cell("AAAA"), cell("B")]];
        let c = core(content, |o| {
            o.wrap_mode = Some(WrapMode::None);
            o.column_width_mode = Some(ColumnWidthMode::Content);
        });
        let layout = c.compute_layout(Some(6));
        assert_eq!(layout.column_widths, vec![4, 1]);
        assert_eq!(layout.table_width, 8);
    }

    /// measure (setupMeasureFunc, TextTable.ts:469-499): AtMost clamps
    /// the reported width to the constraint; the height stays intrinsic.
    #[test]
    fn measure_at_most_clamps_width() {
        let content = vec![vec![cell("A"), cell("B")]];
        let c = core(content, |o| {
            o.wrap_mode = Some(WrapMode::Word);
        });
        // unconstrained: intrinsic [1,1] + 3 vertical borders = 5 wide,
        // 3 rows (2 borders + 1 content)
        let (w, h) = c.measure(None, None);
        assert_eq!((w as u32, h as u32), (5, 3));
        // AtMost 40 in full mode: table fills, clamped to 40
        let (w, h) = c.measure(None, Some(40.0));
        assert_eq!((w as u32, h as u32), (40, 3));
        // Exactly (a known width): the measured width is the layout's
        let (w, _) = c.measure(Some(20.0), None);
        assert_eq!(w as u32, 20);
    }

    /// grid_border_chars — lib/border.ts:55-98 glyphs.
    #[test]
    fn border_char_tables() {
        assert_eq!(grid_border_chars("single").cross, '┼');
        assert_eq!(grid_border_chars("single").top_t, '┬');
        assert_eq!(grid_border_chars("double").left_t, '╠');
        assert_eq!(grid_border_chars("rounded").top_left, '╭');
        assert_eq!(grid_border_chars("heavy").horizontal, '━');
        // unknown falls back to single (parseBorderStyle, border.ts:27-39)
        assert_eq!(grid_border_chars("nope").cross, '┼');
    }
}
