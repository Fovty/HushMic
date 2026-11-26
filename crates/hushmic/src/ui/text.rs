//! Text in CSS line boxes.
//!
//! The boards set every line in a line box (`line-height`), with the glyphs
//! centred in it by half-leading. egui puts the baseline one ascent below
//! the row top, so a block is laid out with the style's line height and
//! painted where Chrome would put its baseline ([`Type::half_leading_at`]).
//!
//! Lines break where Chrome breaks them: greedy, at spaces, measured with the
//! exact CSS widths, the space a line ends on hanging past the edge. The kit
//! breaks the lines itself and hands egui one row per line: egui's own
//! wrapping starts a row at the fractional x it broke at, which sets that
//! row's glyphs up to a pixel off. (No `text-wrap: balance`; copy is written
//! for greedy breaking.)

use std::ops::Range;
use std::sync::Arc;

use eframe::egui::{
    self, text::LayoutJob, text::TextWrapping, Color32, Galley, Painter, Pos2, Rect, Response,
    Sense, TextFormat, Ui, Vec2, WidgetInfo, WidgetType,
};

use super::fonts::Type;

/// A laid-out block of text in one or more styles.
#[derive(Clone)]
pub struct TextBlock {
    pub galley: Arc<Galley>,
    /// The first section's style: its half-leading places the block.
    pub ty: Type,
    /// The widest line's CSS width (egui snaps a row's last glyph to the
    /// pixel when it reports the row's width).
    width: f32,
    /// The text as written (not uppercased, no line breaks): what screen
    /// readers get.
    source: String,
    /// Each line ends at the block's right edge ([`TextBlock::right`]).
    right: bool,
}

impl TextBlock {
    fn new(
        galley: Arc<Galley>,
        ty: Type,
        runs: &[(Range<usize>, Type)],
        indent: f32,
        source: String,
    ) -> TextBlock {
        let width = if galley.elided {
            galley.size().x
        } else {
            let text = galley.text();
            let mut w: f32 = 0.0;
            let mut start = 0;
            for (i, line) in text.split('\n').enumerate() {
                let end = start + line.len();
                let first = if i == 0 { indent } else { 0.0 };
                w = w.max(first + span_width(text, runs, start..end));
                start = end + 1;
            }
            w
        };
        TextBlock {
            galley,
            ty,
            width,
            source,
            right: false,
        }
    }

    /// The same lines, each ending at the block's right edge
    /// (`text-align: right`): a wrapped line under a right-anchored first
    /// one ends where it ends.
    pub fn right(self, ctx: &egui::Context) -> TextBlock {
        if self.right {
            return self;
        }
        let mut job = (*self.galley.job).clone();
        job.halign = egui::Align::RIGHT;
        TextBlock {
            galley: ctx.fonts_mut(|f| f.layout_job(job)),
            right: true,
            ..self
        }
    }

    /// The text as written, for accessible names (eyebrows are painted in
    /// capitals, wrapped lines broken).
    pub fn source(&self) -> &str {
        &self.source
    }

    /// One line cut with "…" ([`layout_elided`] ran out of room).
    pub fn elided(&self) -> bool {
        self.galley.elided
    }

    /// Width of the widest line and height of all line boxes.
    pub fn size(&self) -> Vec2 {
        let rows = self.galley.rows.len().max(1) as f32;
        egui::vec2(self.width, rows * self.ty.line)
    }

    pub fn rows(&self) -> usize {
        self.galley.rows.len()
    }

    pub fn text(&self) -> &str {
        self.galley.text()
    }

    /// Paint with the line boxes' top-left at `top_left`.
    pub fn paint(&self, painter: &Painter, top_left: Pos2) -> Rect {
        // right-aligned rows end at the galley's x = 0
        let x = if self.right { self.width } else { 0.0 };
        let pos = top_left + egui::vec2(x, self.ty.half_leading_at(painter.pixels_per_point()));
        painter.galley(pos, self.galley.clone(), Color32::PLACEHOLDER);
        Rect::from_min_size(top_left, self.size())
    }

    /// Paint vertically centred on `center_y` (one line box), starting at `x`.
    pub fn paint_centered_y(&self, painter: &Painter, x: f32, center_y: f32) -> Rect {
        let h = self.size().y;
        self.paint(painter, egui::pos2(x, center_y - h / 2.0))
    }

    /// The baseline of the first row, from the block's top, at `ppp`.
    pub fn baseline(&self, ppp: f32) -> f32 {
        self.ty.baseline_at(ppp)
    }
}

fn format(ty: Type, color: Color32) -> TextFormat {
    TextFormat {
        font_id: ty.font(),
        color,
        extra_letter_spacing: ty.tracking(),
        line_height: Some(ty.line),
        ..Default::default()
    }
}

fn cased(text: &str, ty: Type) -> String {
    if ty.upper {
        text.to_uppercase()
    } else {
        text.to_owned()
    }
}

/// The CSS width of `text[range]` set in `runs` (byte ranges and their
/// styles; no kerning across a style change).
fn span_width(text: &str, runs: &[(Range<usize>, Type)], range: Range<usize>) -> f32 {
    runs.iter()
        .map(|(r, ty)| {
            let (a, b) = (r.start.max(range.start), r.end.min(range.end));
            if a < b {
                super::fonts::line_width(&text[a..b], *ty)
            } else {
                0.0
            }
        })
        .sum()
}

/// Where Chrome ends the lines of `text` set in `runs` at `wrap` points:
/// the byte offsets of the spaces that end a line. Greedy; a line's last
/// space hangs; a word longer than the line overflows it. `indent` is room
/// taken before the first line (an inline icon).
fn breaks(text: &str, runs: &[(Range<usize>, Type)], wrap: f32, indent: f32) -> Vec<usize> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut room = wrap - indent;
    let mut last: Option<usize> = None;
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), '\n')))
    {
        if c != ' ' && c != '\n' {
            continue;
        }
        // the line so far, up to this word's end, must fit (1/64 px: Chrome's unit)
        if span_width(text, runs, start..i) > room + 1.0 / 64.0 {
            if let Some(b) = last {
                out.push(b);
                start = b + 1;
                room = wrap;
            }
        }
        if c == '\n' {
            start = i + 1;
            room = wrap;
            last = None;
        } else {
            last = Some(i);
        }
    }
    out
}

/// `job` with its lines broken at `wrap` the way Chrome breaks them (one
/// egui row per line).
fn break_lines(job: &mut LayoutJob, runs: &[(Range<usize>, Type)], wrap: f32, indent: f32) {
    job.wrap.max_width = f32::INFINITY;
    if !wrap.is_finite() {
        return;
    }
    let at = breaks(&job.text, runs, wrap, indent);
    if at.is_empty() {
        return;
    }
    // a space and a newline are one byte each: the sections' ranges hold
    let mut bytes = std::mem::take(&mut job.text).into_bytes();
    for i in at {
        bytes[i] = b'\n';
    }
    job.text = String::from_utf8(bytes).unwrap_or_default();
}

/// Lay out `text` wrapping at `wrap` points (`f32::INFINITY` for one line).
pub fn layout(ctx: &egui::Context, text: &str, ty: Type, color: Color32, wrap: f32) -> TextBlock {
    let mut job = LayoutJob::single_section(cased(text, ty), format(ty, color));
    let runs = [(0..job.text.len(), ty)];
    break_lines(&mut job, &runs, wrap, 0.0);
    TextBlock::new(
        ctx.fonts_mut(|f| f.layout_job(job)),
        ty,
        &runs,
        0.0,
        text.to_owned(),
    )
}

/// One line, shortened with "…" past `max_width` (key chips, the mic name).
pub fn layout_elided(
    ctx: &egui::Context,
    text: &str,
    ty: Type,
    color: Color32,
    max_width: f32,
) -> TextBlock {
    let mut job = LayoutJob::single_section(cased(text, ty), format(ty, color));
    job.wrap = TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let runs = [(0..job.text.len(), ty)];
    TextBlock::new(
        ctx.fonts_mut(|f| f.layout_job(job)),
        ty,
        &runs,
        0.0,
        text.to_owned(),
    )
}

/// `text` wrapped at `wrap` on at most `rows` lines: past them the last
/// line ends in "…" (a field that wraps before it shortens a name).
pub fn layout_rows(
    ctx: &egui::Context,
    text: &str,
    ty: Type,
    color: Color32,
    wrap: f32,
    rows: usize,
) -> TextBlock {
    let whole = layout(ctx, text, ty, color, wrap);
    if whole.rows() <= rows {
        return whole;
    }
    let mut job = LayoutJob::single_section(cased(text, ty), format(ty, color));
    job.wrap = TextWrapping {
        max_width: wrap,
        max_rows: rows,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    let runs = [(0..job.text.len(), ty)];
    TextBlock::new(
        ctx.fonts_mut(|f| f.layout_job(job)),
        ty,
        &runs,
        0.0,
        text.to_owned(),
    )
}

/// The exact width of `text` set on one line in `ty`.
pub fn width(_ctx: &egui::Context, text: &str, ty: Type) -> f32 {
    super::fonts::line_width(text, ty)
}

/// Text in several styles in one flow ("**Zoom is using your microphone
/// directly,** so it still hears the noise."). All spans share the first
/// span's line height.
///
/// One emphasis rule (UX panel D2): a medium lead in ink says what is
/// happening (a problem, a notice, a replay playing, the level's word),
/// at the start of its line, and the rest of the line is the regular
/// muted body. A line that only explains (nothing is happening) is one
/// weight.
#[derive(Default)]
pub struct Rich {
    job: LayoutJob,
    first: Option<Type>,
    runs: Vec<(Range<usize>, Type)>,
    indent: f32,
    source: String,
}

impl Rich {
    pub fn new() -> Rich {
        Rich::default()
    }

    pub fn span(mut self, text: &str, ty: Type, color: Color32) -> Rich {
        let line = self.first.get_or_insert(ty).line;
        let mut f = format(ty, color);
        f.line_height = Some(line);
        let start = self.job.text.len();
        self.source.push_str(text);
        self.job.append(&cased(text, ty), 0.0, f);
        self.runs.push((start..self.job.text.len(), ty));
        self
    }

    /// Empty room before the first glyph (for an inline icon).
    pub fn indent(mut self, points: f32) -> Rich {
        if let Some(s) = self.job.sections.first_mut() {
            s.leading_space = points;
            self.indent = points;
        }
        self
    }

    pub fn layout(mut self, ctx: &egui::Context, wrap: f32) -> TextBlock {
        break_lines(&mut self.job, &self.runs, wrap, self.indent);
        let ty = self.first.unwrap_or(Type::SUB);
        TextBlock::new(
            ctx.fonts_mut(|f| f.layout_job(self.job)),
            ty,
            &self.runs,
            self.indent,
            self.source,
        )
    }
}

/// A text widget: lays out at the available width, allocates its line boxes
/// and names itself for screen readers and UI tests.
pub fn label(ui: &mut Ui, text: &str, ty: Type, color: Color32) -> Response {
    let block = layout(ui.ctx(), text, ty, color, ui.available_width());
    show(ui, block)
}

/// Allocate and paint a block laid out elsewhere.
pub fn show(ui: &mut Ui, block: TextBlock) -> Response {
    let (rect, response) = ui.allocate_exact_size(block.size(), Sense::hover());
    if ui.is_rect_visible(rect) {
        block.paint(ui.painter(), rect.min);
    }
    let text = block.source().to_owned();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &text));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `right`: every line ends at the block's right edge where it is
    /// painted; the size, the lines and the words stay.
    #[test]
    fn right_aligned_lines_end_at_one_edge() {
        let ctx = egui::Context::default();
        super::super::fonts::install(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            let text = "Removes as much noise as it can. The pauses between words become silent.";
            let left = layout(ctx, text, Type::CAP, Color32::WHITE, 200.0);
            assert!(left.rows() >= 2);
            let w = left.size().x;
            let ends = |b: &TextBlock, x: f32| -> Vec<f32> {
                b.galley.rows.iter().map(|r| x + r.rect().right()).collect()
            };
            // left: the shorter lines end before the edge
            assert!(ends(&left, 0.0).iter().any(|e| e < &(w - 4.0)));
            let size = left.size();
            let right = left.clone().right(ctx);
            assert_eq!(right.size(), size);
            assert_eq!(right.text(), left.text());
            assert_eq!(right.source(), left.source());
            // painted at x = 0, each line ends at the block's right
            for e in ends(&right, right.width) {
                assert!((e - w).abs() <= 1.0, "{e} vs {w}");
            }
        });
    }

    #[test]
    fn line_boxes_set_the_height() {
        let ctx = egui::Context::default();
        super::super::fonts::install(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            let one = layout(
                ctx,
                "Microphone",
                Type::LABEL,
                Color32::WHITE,
                f32::INFINITY,
            );
            assert_eq!(one.size().y, 20.0);
            let two = layout(
                ctx,
                "Strength and model are remembered for each microphone.",
                Type::CAP,
                Color32::WHITE,
                120.0,
            );
            assert!(two.rows() >= 2);
            assert_eq!(two.size().y, two.rows() as f32 * 18.0);
            // Chrome's breaks: greedy at spaces, the last space hangs
            let w = width(ctx, "Strength and model", Type::CAP);
            let fit = layout(ctx, "Strength and model are", Type::CAP, Color32::WHITE, w);
            assert_eq!(fit.text(), "Strength and model\nare");
            assert!((fit.size().x - w).abs() < 0.01);
            let tight = layout(
                ctx,
                "Strength and model are",
                Type::CAP,
                Color32::WHITE,
                w - 0.1,
            );
            assert_eq!(tight.text(), "Strength and\nmodel are");
            let long = layout(ctx, "Unbreakable words", Type::CAP, Color32::WHITE, 10.0);
            assert_eq!(long.text(), "Unbreakable\nwords");
            let short = layout_elided(
                ctx,
                "Ctrl+Alt+Shift+Super+Space+Escape",
                Type::MONO,
                Color32::WHITE,
                60.0,
            );
            assert_eq!(short.rows(), 1);
            assert!(short.text().ends_with('…') || short.size().x <= 60.0);
        });
    }
}
