use std::{
    cell::Cell,
    hash::{Hash, Hasher},
    ops::Range,
    rc::Rc,
    sync::Arc,
};

use collections::HashMap;
use gpui_kit::{
    App, Bounds, Element, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, Pixels,
    Point, SharedString, Size, Style, StyledText, TextStyle, TextStyleRefinement, WhiteSpace,
    Window, point, px, size,
};
use ratex_layout::{LayoutOptions, layout, to_display_list, validate_layout};
use ratex_svg::{SvgColorSyntax, SvgOptions, render_to_svg_with_color_syntax};
use ratex_types::math_style::MathStyle;
use smallvec::SmallVec;
use util::ResultExt;

use crate::{RenderedLine, SourceMapping, parser::MarkdownEvent};

pub(crate) type MathCache = HashMap<(SharedString, bool), Result<Arc<MathFormula>, SharedString>>;

#[derive(Debug)]
pub(crate) struct MathFormula {
    pub svg: Arc<[u8]>,
    path: SharedString,
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
}

pub(crate) fn prepare_math(
    events: &[(Range<usize>, MarkdownEvent)],
    previous: &MathCache,
) -> MathCache {
    let mut formulas = MathCache::default();
    for (_, event) in events {
        let (source, display) = match event {
            MarkdownEvent::InlineMath(source) => (source, false),
            MarkdownEvent::DisplayMath(source) => (source, true),
            _ => continue,
        };
        let key = (source.clone(), display);
        if formulas.contains_key(&key) {
            continue;
        }
        let formula = previous.get(&key).cloned().unwrap_or_else(|| {
            compile_math(source, display)
                .map(Arc::new)
                .map_err(|error| {
                    log::debug!("Could not render Markdown formula: {error}");
                    SharedString::from(error.to_string())
                })
        });
        formulas.insert(key, formula);
    }
    formulas
}

fn compile_math(source: &str, display: bool) -> anyhow::Result<MathFormula> {
    anyhow::ensure!(source.len() <= 64 * 1024, "Formula is too long");
    let nodes = ratex_parser::parse(source)?;
    let options = LayoutOptions {
        style: if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        },
        ..Default::default()
    };
    let layout = layout(&nodes, &options);
    validate_layout(&layout).map_err(anyhow::Error::msg)?;
    let display_list = to_display_list(&layout);
    anyhow::ensure!(
        display_list.width > 0. && display_list.height + display_list.depth > 0.,
        "Formula is empty"
    );
    anyhow::ensure!(
        [display_list.width, display_list.height, display_list.depth]
            .into_iter()
            .all(|dimension| dimension.is_finite() && dimension.abs() <= 4096.),
        "Formula dimensions are too large"
    );
    // Keep path coordinates precise; the view scales this font-independent SVG in ems.
    let svg = render_to_svg_with_color_syntax(
        &display_list,
        &SvgOptions {
            font_size: 40.,
            padding: 0.,
            embed_glyphs: true,
            ..Default::default()
        },
        SvgColorSyntax::Rgb,
    );
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    svg.hash(&mut hasher);
    Ok(MathFormula {
        svg: svg.into_bytes().into(),
        path: format!("markdown-math-{}", hasher.finish()).into(),
        width: display_list.width as f32,
        ascent: display_list.height as f32,
        descent: display_list.depth as f32,
    })
}

pub(crate) struct MathElement {
    formula: Arc<MathFormula>,
    source: StyledText,
    line: Rc<RenderedLine>,
    source_style: TextStyleRefinement,
    size: Size<Pixels>,
    formula_size: Size<Pixels>,
    formula_offset: Point<Pixels>,
    color: Hsla,
}

impl MathElement {
    pub(crate) fn new(
        formula: Arc<MathFormula>,
        source: &str,
        source_range: Range<usize>,
        text_style: TextStyle,
        display: bool,
        highlights: SmallVec<[(Range<usize>, Hsla); 1]>,
        window: &Window,
    ) -> (Self, Rc<RenderedLine>) {
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let math_font_size = font_size * 1.21;
        let formula_size = size(
            (math_font_size * formula.width).max(px(1.)),
            (math_font_size * (formula.ascent + formula.descent)).max(px(1.)),
        );
        let (height, top) = if display {
            (formula_size.height, px(0.))
        } else {
            let font_id = window.text_system().resolve_font(&text_style.font());
            let ascent = window.text_system().ascent(font_id, font_size);
            let descent = window.text_system().descent(font_id, font_size).abs();
            let line_height = window.pixel_snap(
                text_style
                    .line_height
                    .to_pixels(text_style.font_size, window.rem_size()),
            );
            let baseline_from_center = (ascent - descent) / 2.;
            // Flex rows center their children. Preserve the text baseline relative to that
            // center even when a fraction or integral needs more height than normal text.
            let height = line_height
                .max((math_font_size * formula.ascent - baseline_from_center) * 2.)
                .max((math_font_size * formula.descent + baseline_from_center) * 2.);
            let top = height / 2. + baseline_from_center - math_font_size * formula.ascent;
            (height, top)
        };
        let source: SharedString = source.replace(['\n', '\r'], " ").into();
        let source_run = text_style.to_run(source.len());
        let source_width = window
            .text_system()
            .shape_line(
                source.clone(),
                font_size,
                std::slice::from_ref(&source_run),
                None,
            )
            .width;
        // Retain a text layout for selection, copy, and accessibility while painting the SVG.
        let source_font_size = font_size * (formula_size.width / source_width.max(px(1.)));
        let source = StyledText::new(source).with_runs(vec![source_run]);
        let line = Rc::new(RenderedLine {
            layout: source.layout().clone(),
            visible_bounds: Cell::new(None),
            source_mappings: vec![SourceMapping {
                rendered_index: 0,
                source_index: source_range.start,
            }],
            source_end: source_range.end,
            language: None,
            text_align: text_style.text_align,
            highlights,
            code_chips: SmallVec::new(),
        });
        (
            Self {
                formula,
                source,
                line: line.clone(),
                source_style: TextStyleRefinement {
                    font_size: Some(source_font_size.into()),
                    line_height: Some(height.into()),
                    white_space: Some(WhiteSpace::Nowrap),
                    ..Default::default()
                },
                size: size(formula_size.width, height),
                formula_size,
                formula_offset: point(px(0.), top),
                color: text_style.color,
            },
            line,
        )
    }
}

impl Element for MathElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui_kit::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let (source_layout, ()) = window
            .with_text_style(Some(self.source_style.clone()), |window| {
                self.source.request_layout(None, inspector_id, window, cx)
            });
        let layout = window.request_layout(
            Style {
                size: self.size.map(Into::into),
                flex_shrink: 0.,
                ..Default::default()
            },
            [source_layout],
            cx,
        );
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.line
            .visible_bounds
            .set(Some(bounds.intersect(&window.content_mask().bounds)));
        self.source
            .prepaint(None, inspector_id, bounds, &mut (), window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.line.paint_highlights(window);
        window
            .paint_svg(
                Bounds::new(bounds.origin + self.formula_offset, self.formula_size),
                self.formula.path.clone(),
                Some(&self.formula.svg),
                Default::default(),
                self.color,
                cx,
            )
            .log_err();
    }
}

impl IntoElement for MathElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
