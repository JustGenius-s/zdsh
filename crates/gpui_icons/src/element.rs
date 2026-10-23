use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    panic::Location,
    sync::{Arc, OnceLock},
    time::Instant,
};

use anyhow::{Result, anyhow};
use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, Hitbox, Hsla, InspectorElementId,
    InteractiveElement, Interactivity, IntoElement, LayoutId, PathBuilder, PathStyle, Pixels,
    Radians, RenderOnce, SharedString, StyleRefinement, Styled, Transformation,
    TransformationMatrix, Window,
};
use gpui_kit::{
    assets::IconNamed,
    component::{Sizable, Size},
};

use crate::{
    catalog::{self, Glyph},
    geometry::{self, MorphPlan, Point, Sampled, Spring, SpringPreset},
    region,
};

struct PreparedGlyph {
    glyph: Arc<Glyph>,
    samples: OnceLock<Result<Vec<Sampled>, String>>,
}

impl PreparedGlyph {
    fn samples(&self) -> Result<&[Sampled]> {
        self.samples
            .get_or_init(|| {
                region::icon_parts(&self.glyph.nodes, self.glyph.view_box)
                    .and_then(|parts| {
                        region::normalize_region(&parts, &region::RegionOptions::default())
                    })
                    .map(|normalized| normalized.samples)
                    .map_err(|error| format!("{error:#}"))
            })
            .as_ref()
            .map(Vec::as_slice)
            .map_err(|error| anyhow!("{error}"))
    }
}

type PlanKey = (u32, u64, u32, u64);

#[derive(Default)]
struct GeometryCache {
    glyphs: HashMap<u32, Arc<PreparedGlyph>>,
    plans: VecDeque<(PlanKey, Arc<MorphPlan>)>,
}

#[derive(Default)]
struct PaintedIds {
    painting: bool,
    ids: HashSet<GlobalElementId>,
}

thread_local! {
    static GEOMETRY: RefCell<GeometryCache> = RefCell::new(GeometryCache::default());
    // Icons built at one call site share their default id, and GPUI hands a sibling with the same
    // id the state the previous one just stored, so different glyphs would retarget each other
    // every frame. GPUI prepaints the whole frame before painting it, so a prepaint after a paint
    // starts a new frame.
    static PAINTED: RefCell<PaintedIds> = RefCell::new(PaintedIds::default());
}

fn prepare(glyph: Arc<Glyph>) -> Result<Arc<PreparedGlyph>> {
    GEOMETRY.with_borrow_mut(|cache| {
        if let Some(prepared) = cache.glyphs.get(&glyph.id) {
            return Ok(prepared.clone());
        }
        let prepared = Arc::new(PreparedGlyph {
            glyph,
            samples: OnceLock::new(),
        });
        cache.glyphs.insert(prepared.glyph.id, prepared.clone());
        Ok(prepared)
    })
}

fn rotate_point(point: Point, angle: f64) -> Point {
    let (sine, cosine) = angle.sin_cos();
    let horizontal = point.x - 12.0;
    let vertical = point.y - 12.0;
    Point {
        x: horizontal * cosine - vertical * sine + 12.0,
        y: horizontal * sine + vertical * cosine + 12.0,
    }
}

fn rotated_samples(samples: &[Sampled], angle: f64) -> Vec<Sampled> {
    samples
        .iter()
        .map(|sample| Sampled {
            points: sample
                .points
                .iter()
                .map(|&point| rotate_point(point, angle))
                .collect(),
            closed: sample.closed,
            hole: sample.hole,
        })
        .collect()
}

fn cached_plan(
    source: &PreparedGlyph,
    source_angle: f64,
    target: &PreparedGlyph,
    target_angle: f64,
) -> Result<Arc<MorphPlan>> {
    let key = (
        source.glyph.id,
        source_angle.to_bits(),
        target.glyph.id,
        target_angle.to_bits(),
    );
    GEOMETRY.with_borrow_mut(|cache| {
        if let Some(index) = cache
            .plans
            .iter()
            .position(|(existing, _)| *existing == key)
        {
            if let Some(entry) = cache.plans.remove(index) {
                let plan = entry.1.clone();
                cache.plans.push_back(entry);
                return Ok(plan);
            }
        }
        let plan = Arc::new(region::build_region_plan(
            &rotated_samples(source.samples()?, source_angle),
            &rotated_samples(target.samples()?, target_angle),
        )?);
        if cache.plans.len() >= 128 {
            cache.plans.pop_front();
        }
        cache.plans.push_back((key, plan.clone()));
        Ok(plan)
    })
}

struct MotionState {
    target: Arc<PreparedGlyph>,
    angle: f64,
    plan: Option<Arc<MorphPlan>>,
    output: Vec<Vec<Point>>,
    spring: Spring,
    timestamp: Instant,
    spinning: bool,
    spin_period: f64,
    spin_angle: f64,
}

impl MotionState {
    fn new(target: Arc<PreparedGlyph>, angle: f64, spinning: bool) -> Self {
        let mut spring = Spring::new(SpringPreset::Snappy);
        spring.settle();
        Self {
            target,
            angle,
            plan: None,
            output: Vec::new(),
            spring,
            timestamp: Instant::now(),
            spinning,
            spin_period: 1.0,
            spin_angle: 0.0,
        }
    }

    fn settle(&mut self) {
        self.plan = None;
        self.output.clear();
        self.spring.settle();
        self.spin_angle = 0.0;
    }

    fn retarget(&mut self, target: Arc<PreparedGlyph>, angle: f64, spinning: bool) -> Result<()> {
        if self.target.glyph.id == target.glyph.id
            && self.angle == angle
            && self.spinning == spinning
        {
            return Ok(());
        }
        let plan = if self.plan.is_some() || self.spin_angle != 0.0 {
            let source = match &self.plan {
                Some(plan) => geometry::snapshot(plan, &self.output)?,
                None => rotated_samples(self.target.samples()?, self.angle),
            };
            Arc::new(region::build_region_plan(
                &rotated_samples(&source, self.spin_angle),
                &rotated_samples(target.samples()?, angle),
            )?)
        } else {
            cached_plan(&self.target, self.angle, &target, angle)?
        };
        let mut output = geometry::allocate_outputs(&plan);
        geometry::interpolate_polar(&plan, 0.0, &mut output)?;
        self.target = target;
        self.angle = angle;
        self.plan = Some(plan);
        self.output = output;
        self.spring.start();
        self.spring.velocity = self.spring.velocity.clamp(-14.0, 14.0);
        self.spinning = spinning;
        self.spin_angle = 0.0;
        Ok(())
    }

    fn advance(&mut self, now: Instant) -> Result<()> {
        let elapsed = now
            .saturating_duration_since(self.timestamp)
            .as_secs_f64()
            .min(0.1);
        self.timestamp = now;
        if self.spinning {
            self.spin_angle = (self.spin_angle
                + elapsed * std::f64::consts::TAU / self.spin_period)
                % std::f64::consts::TAU;
        }
        if let Some(plan) = &self.plan {
            let settled = self.spring.step(elapsed)?;
            if settled {
                self.plan = None;
                self.output.clear();
            } else {
                geometry::interpolate_polar(plan, self.spring.progress, &mut self.output)?;
            }
        }
        Ok(())
    }
}

pub struct MorphIcon {
    interactivity: Interactivity,
    path: SharedString,
    data: Option<Arc<[u8]>>,
    transformation: Transformation,
    angle: f64,
    spinning: bool,
    spin_period: f64,
}

impl MorphIcon {
    #[track_caller]
    pub fn new(id: impl Into<ElementId>, path: impl Into<SharedString>) -> Self {
        let mut interactivity = Interactivity::new();
        interactivity.element_id = Some(id.into());
        Self {
            interactivity,
            path: path.into(),
            data: None,
            transformation: Transformation::default(),
            angle: 0.0,
            spinning: false,
            spin_period: 1.0,
        }
    }

    pub fn transform(mut self, transformation: Transformation) -> Self {
        self.transformation = transformation;
        self
    }

    pub fn rotate(mut self, angle: impl Into<Radians>) -> Self {
        self.angle = f64::from(angle.into().0);
        self
    }

    pub fn spinning(mut self, spinning: bool) -> Self {
        self.spinning = spinning;
        self
    }

    pub fn spin_period(mut self, seconds: f64) -> Self {
        self.spin_period = if seconds.is_finite() {
            seconds.max(0.01)
        } else {
            1.0
        };
        self
    }

    fn with_data(mut self, data: Option<Arc<[u8]>>) -> Self {
        self.data = data;
        self
    }
}

impl Styled for MorphIcon {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.interactivity.base_style
    }
}

impl InteractiveElement for MorphIcon {
    fn interactivity(&mut self) -> &mut Interactivity {
        &mut self.interactivity
    }
}

impl IntoElement for MorphIcon {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

fn paint_samples(
    output: &[Vec<Point>],
    angle: f64,
    bounds: Bounds<Pixels>,
    transform: TransformationMatrix,
    color: Hsla,
    window: &mut Window,
) -> Result<()> {
    // Flights share the static SVG's inset view box so a glyph keeps its size when it starts moving.
    let [view_left, view_top, view_width, view_height] = catalog::metadata()?.display_view_box;
    let mut builder = PathBuilder::fill().with_style(PathStyle::Fill(
        gpui::FillOptions::default().with_fill_rule(gpui::FillRule::NonZero),
    ));
    for contour in output {
        for (index, &point) in contour.iter().enumerate() {
            let point = rotate_point(point, angle);
            let position = transform.apply(gpui::point(
                bounds.left() + bounds.size.width * ((point.x - view_left) / view_width) as f32,
                bounds.top() + bounds.size.height * ((point.y - view_top) / view_height) as f32,
            ));
            if index == 0 {
                builder.move_to(position);
            } else {
                builder.line_to(position);
            }
        }
        if !contour.is_empty() {
            builder.close();
        }
    }
    window.paint_path(builder.build()?, color);
    Ok(())
}

impl Element for MorphIcon {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        self.interactivity.element_id.clone()
    }
    fn source_location(&self) -> Option<&'static Location<'static>> {
        self.interactivity.source_location()
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let layout = self.interactivity.request_layout(
            global_id,
            inspector_id,
            window,
            cx,
            |style, window, cx| window.request_layout(style, None, cx),
        );
        (layout, ())
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Hitbox> {
        PAINTED.with_borrow_mut(|painted| {
            if painted.painting {
                painted.painting = false;
                painted.ids.clear();
            }
        });
        self.interactivity.prepaint(
            global_id,
            inspector_id,
            bounds,
            bounds.size,
            window,
            cx,
            |_, _, hitbox, _, _| hitbox,
        )
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.interactivity.paint(
            global_id,
            inspector_id,
            bounds,
            hitbox.as_ref(),
            window,
            cx,
            |_, window, cx| {
                // Interactivity::paint has pushed this element's text style, so this is the
                // element's own color when set and the inherited one otherwise, like Kit icons.
                let color = window.text_style().color;
                let glyph = match catalog::glyph_for_path(&self.path)
                    .and_then(|glyph| glyph.map(prepare).transpose())
                {
                    Ok(glyph) => glyph,
                    Err(error) => {
                        log::error!("Preparing icon {}: {error:#}", self.path);
                        None
                    }
                };
                let transform = self.transformation.into_matrix(bounds.center(), 1.0);
                let unique_id = global_id.filter(|global_id| {
                    PAINTED.with_borrow_mut(|painted| {
                        painted.painting = true;
                        painted.ids.insert((*global_id).clone())
                    })
                });
                if let Some((glyph, global_id)) = glyph.zip(unique_id) {
                    window.with_element_state(
                        global_id,
                        |previous: Option<MotionState>, window| {
                            let mut state = previous.unwrap_or_else(|| {
                                MotionState::new(glyph.clone(), self.angle, self.spinning)
                            });
                            state.spin_period = self.spin_period;
                            if cx.reduce_motion() || !window.is_visible() {
                                state.target = glyph;
                                state.angle = self.angle;
                                state.spinning = self.spinning;
                                state.settle();
                                state.timestamp = Instant::now();
                            } else if let Err(error) =
                                state.advance(Instant::now()).and_then(|()| {
                                    state.retarget(glyph.clone(), self.angle, self.spinning)
                                })
                            {
                                log::error!("Animating icon {}: {error:#}", self.path);
                                state = MotionState::new(glyph, self.angle, self.spinning);
                            }
                            let result = if state.plan.is_some() {
                                paint_samples(
                                    &state.output,
                                    state.spin_angle,
                                    bounds,
                                    transform,
                                    color,
                                    window,
                                )
                            } else {
                                window.paint_svg(
                                    bounds,
                                    self.path.clone(),
                                    Some(state.target.glyph.svg.as_bytes()),
                                    self.transformation
                                        .into_matrix(bounds.center(), window.scale_factor())
                                        .compose(
                                            Transformation::rotate(gpui::radians(
                                                (state.angle + state.spin_angle) as f32,
                                            ))
                                            .into_matrix(bounds.center(), window.scale_factor()),
                                        ),
                                    color,
                                    cx,
                                )
                            };
                            if let Err(error) = result {
                                log::error!("Painting icon {}: {error:#}", self.path);
                            }
                            if !cx.reduce_motion()
                                && window.is_visible()
                                && (state.plan.is_some() || state.spinning)
                                && !window.content_mask().bounds.intersect(&bounds).is_empty()
                            {
                                window.request_animation_frame();
                            }
                            ((), state)
                        },
                    );
                } else {
                    let transformation = self
                        .transformation
                        .into_matrix(bounds.center(), window.scale_factor())
                        .compose(
                            Transformation::rotate(gpui::radians(self.angle as f32))
                                .into_matrix(bounds.center(), window.scale_factor()),
                        );
                    if let Err(error) = window.paint_svg(
                        bounds,
                        self.path.clone(),
                        self.data.as_deref(),
                        transformation,
                        color,
                        cx,
                    ) {
                        log::error!("Painting icon {}: {error:#}", self.path);
                    }
                }
            },
        );
    }
}

#[derive(Clone, IntoElement)]
pub struct Icon {
    id: ElementId,
    path: SharedString,
    data: Option<Arc<[u8]>>,
    style: StyleRefinement,
    size: Option<Size>,
    transformation: Transformation,
    angle: f64,
    spinning: bool,
    spin_period: f64,
}

impl Default for Icon {
    #[track_caller]
    fn default() -> Self {
        let location = Location::caller();
        Self {
            id: ElementId::CodeLocation(*location),
            path: "".into(),
            data: None,
            style: StyleRefinement::default(),
            size: None,
            transformation: Transformation::default(),
            angle: 0.0,
            spinning: false,
            spin_period: 1.0,
        }
    }
}

impl Icon {
    #[track_caller]
    pub fn new(name: impl IconNamed) -> Self {
        Self::default().path(name.path())
    }
    #[track_caller]
    pub fn empty() -> Self {
        Self::default()
    }
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = id.into();
        self
    }
    pub fn path(mut self, path: impl Into<SharedString>) -> Self {
        self.path = path.into();
        self.data = None;
        self
    }
    pub fn data(mut self, data: &[u8]) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        data.hash(&mut hash);
        self.path = format!("gpui-icons/data/{}", hash.finish()).into();
        self.data = Some(Arc::from(data));
        self
    }
    pub fn transform(mut self, transformation: Transformation) -> Self {
        self.transformation = transformation;
        self
    }
    pub fn rotate(mut self, angle: impl Into<Radians>) -> Self {
        self.angle = f64::from(angle.into().0);
        self
    }
    pub fn spinning(mut self, spinning: bool) -> Self {
        self.spinning = spinning;
        self
    }

    pub fn spin_period(mut self, seconds: f64) -> Self {
        self.spin_period = if seconds.is_finite() {
            seconds.max(0.01)
        } else {
            1.0
        };
        self
    }

    pub fn into_kit(self) -> gpui_kit::component::Icon {
        let mut icon = gpui_kit::component::Icon::empty();
        *icon.style() = self.style;
        icon = match self.data {
            Some(data) => icon.data(&data),
            None => icon.path(self.path),
        };
        if let Some(size) = self.size {
            icon = icon.with_size(size);
        }
        if self.angle == 0.0 {
            icon.transform(self.transformation)
        } else {
            icon.rotate(gpui::radians(self.angle as f32))
        }
    }
}

impl From<Icon> for gpui_kit::component::Icon {
    fn from(icon: Icon) -> Self {
        icon.into_kit()
    }
}

impl Styled for Icon {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Sizable for Icon {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = Some(size.into());
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let has_size = self.style.size.width.is_some() || self.style.size.height.is_some();
        let mut icon = MorphIcon::new(self.id, self.path)
            .with_data(self.data)
            .transform(self.transformation)
            .rotate(gpui::radians(self.angle as f32))
            .spinning(self.spinning)
            .spin_period(self.spin_period);
        *icon.style() = self.style;
        icon = icon.flex_shrink_0();
        if !has_size {
            icon = icon.size(window.text_style().font_size.to_pixels(window.rem_size()));
        }
        if let Some(size) = self.size {
            icon = match size {
                Size::Size(size) => icon.size(size),
                Size::XSmall => icon.size_3(),
                Size::Small => icon.size_3p5(),
                Size::Medium => icon.size_4(),
                Size::Large => icon.size_6(),
            };
        }
        icon
    }
}
