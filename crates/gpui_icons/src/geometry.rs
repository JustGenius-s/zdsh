// Derived from Morphicons v2.0.0 (74118e766f2e2f0026df334163fc1943eb01f6ab).
// Copyright (c) 2026 Guillermo. Licensed under MIT; see LICENSE-MORPHICONS.

use anyhow::{Context as _, Result, bail, ensure};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    f64::consts::{FRAC_PI_2, PI, TAU},
    ops::{Add, AddAssign, Div, Mul, Sub},
};

pub const SURJECTION_LIMIT: usize = 100_000;
const LENGTH_WEIGHT: f64 = 0.35;
const GLOBAL_RESIDUAL_THRESHOLD: f64 = 0.005;
const PERMUTATION_LIMIT: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn length(self) -> f64 {
        self.x.hypot(self.y)
    }

    pub fn distance(self, other: Self) -> f64 {
        (self - other).length()
    }

    pub fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y
    }

    pub fn cross(self, other: Self) -> f64 {
        self.x * other.y - self.y * other.x
    }

    pub fn lerp(self, other: Self, progress: f64) -> Self {
        self + (other - self) * progress
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }

    fn transformed(self, cosine: f64, sine: f64) -> Self {
        Self::new(
            self.x * cosine - self.y * sine,
            self.x * sine + self.y * cosine,
        )
    }
}

impl Add for Point {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }
}

impl AddAssign for Point {
    fn add_assign(&mut self, other: Self) {
        self.x += other.x;
        self.y += other.y;
    }
}

impl Sub for Point {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }
}

impl Mul<f64> for Point {
    type Output = Self;

    fn mul(self, factor: f64) -> Self {
        Self::new(self.x * factor, self.y * factor)
    }
}

impl Div<f64> for Point {
    type Output = Self;

    fn div(self, divisor: f64) -> Self {
        Self::new(self.x / divisor, self.y / divisor)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CubicPath {
    pub points: Vec<Point>,
    pub closed: bool,
}

impl CubicPath {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.points.len() >= 4 && (self.points.len() - 1).is_multiple_of(3),
            "a cubic path must contain 3m + 1 points and at least one segment"
        );
        ensure!(
            self.points.iter().all(|point| point.is_finite()),
            "a cubic path contains non-finite coordinates"
        );
        Ok(())
    }

    pub fn segment_count(&self) -> usize {
        self.points.len().saturating_sub(1) / 3
    }

    fn segments(&self) -> impl Iterator<Item = [Point; 4]> + '_ {
        self.points
            .windows(4)
            .step_by(3)
            .map(|segment| [segment[0], segment[1], segment[2], segment[3]])
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sampled {
    pub points: Vec<Point>,
    pub closed: bool,
    pub hole: bool,
}

#[derive(Clone, Debug)]
pub enum SvgPrimitive {
    Path(String),
    Line {
        start: Point,
        end: Point,
    },
    Circle {
        center: Point,
        radius: f64,
    },
    Ellipse {
        center: Point,
        radii: Point,
    },
    Rectangle {
        origin: Point,
        size: Point,
        radius_x: Option<f64>,
        radius_y: Option<f64>,
    },
    Polyline(Vec<Point>),
    Polygon(Vec<Point>),
}

#[derive(Clone, Debug, Deserialize)]
pub struct IconNode {
    pub tag: String,
    pub attributes: BTreeMap<String, String>,
}

impl IconNode {
    pub fn painted(&self, key: &str) -> bool {
        self.attributes
            .get(key)
            .is_some_and(|value| !value.trim().eq_ignore_ascii_case("none"))
    }

    fn optional_number(&self, key: &str) -> Option<f64> {
        self.attributes
            .get(key)
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|value| value.is_finite())
    }

    pub fn number(&self, key: &str, fallback: f64) -> f64 {
        self.optional_number(key).unwrap_or(fallback)
    }

    fn points(&self) -> Result<Vec<Point>> {
        let value = self.attributes.get("points").map_or("", String::as_str);
        let numbers = value
            .split(|character: char| character.is_whitespace() || character == ',')
            .filter(|number| !number.is_empty())
            .map(|number| {
                number
                    .parse::<f64>()
                    .ok()
                    .filter(|number| number.is_finite())
                    .with_context(|| format!("invalid SVG points {value:?}"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(numbers
            .chunks_exact(2)
            .map(|pair| Point::new(pair[0], pair[1]))
            .collect())
    }

    pub fn primitive(&self) -> Result<SvgPrimitive> {
        let number = |key| self.number(key, 0.0);
        Ok(match self.tag.as_str() {
            "path" => SvgPrimitive::Path(self.attributes.get("d").cloned().unwrap_or_default()),
            "line" => SvgPrimitive::Line {
                start: Point::new(number("x1"), number("y1")),
                end: Point::new(number("x2"), number("y2")),
            },
            "circle" => SvgPrimitive::Circle {
                center: Point::new(number("cx"), number("cy")),
                radius: number("r"),
            },
            "ellipse" => SvgPrimitive::Ellipse {
                center: Point::new(number("cx"), number("cy")),
                radii: Point::new(number("rx"), number("ry")),
            },
            "rect" => SvgPrimitive::Rectangle {
                origin: Point::new(number("x"), number("y")),
                size: Point::new(number("width"), number("height")),
                radius_x: self.optional_number("rx"),
                radius_y: self.optional_number("ry"),
            },
            "polyline" => SvgPrimitive::Polyline(self.points()?),
            "polygon" => SvgPrimitive::Polygon(self.points()?),
            tag => bail!("unsupported icon element <{tag}>"),
        })
    }
}

struct PathBuilder {
    points: Vec<Point>,
    start: Point,
    current: Point,
}

impl PathBuilder {
    fn new(start: Point) -> Self {
        Self {
            points: vec![start],
            start,
            current: start,
        }
    }

    fn cubic(&mut self, control_first: Point, control_second: Point, end: Point) {
        self.points.extend([control_first, control_second, end]);
        self.current = end;
    }

    fn line(&mut self, end: Point) {
        if (end.x - self.current.x).abs() < 1e-12 && (end.y - self.current.y).abs() < 1e-12 {
            return;
        }
        self.cubic(
            self.current.lerp(end, 1.0 / 3.0),
            self.current.lerp(end, 2.0 / 3.0),
            end,
        );
    }

    fn quadratic(&mut self, control: Point, end: Point) {
        self.cubic(
            self.current.lerp(control, 2.0 / 3.0),
            end.lerp(control, 2.0 / 3.0),
            end,
        );
    }

    fn arc(&mut self, arc: ArcSegment) {
        let start = self.current;
        if (arc.end.x - start.x).abs() < 1e-12 && (arc.end.y - start.y).abs() < 1e-12 {
            return;
        }
        let mut radii = Point::new(arc.radii.x.abs(), arc.radii.y.abs());
        if radii.x < 1e-12 || radii.y < 1e-12 {
            self.line(arc.end);
            return;
        }
        let rotation = arc.rotation_degrees * PI / 180.0;
        let cosine = rotation.cos();
        let sine = rotation.sin();
        let local_start = ((start - arc.end) / 2.0).transformed(cosine, -sine);
        let radius_correction = local_start.x * local_start.x / (radii.x * radii.x)
            + local_start.y * local_start.y / (radii.y * radii.y);
        if radius_correction > 1.0 {
            radii = radii * radius_correction.sqrt();
        }
        let radius_x_squared = radii.x * radii.x;
        let radius_y_squared = radii.y * radii.y;
        let start_x_squared = local_start.x * local_start.x;
        let start_y_squared = local_start.y * local_start.y;
        let radicand = ((radius_x_squared * radius_y_squared
            - radius_x_squared * start_y_squared
            - radius_y_squared * start_x_squared)
            / (radius_x_squared * start_y_squared + radius_y_squared * start_x_squared))
            .max(0.0);
        let coefficient = if arc.large_arc == arc.sweep {
            -1.0
        } else {
            1.0
        } * radicand.sqrt();
        let local_center = Point::new(
            coefficient * radii.x * local_start.y / radii.y,
            -coefficient * radii.y * local_start.x / radii.x,
        );
        let center = local_center.transformed(cosine, sine) + (start + arc.end) / 2.0;
        let angle_start = ((local_start.y - local_center.y) / radii.y)
            .atan2((local_start.x - local_center.x) / radii.x);
        let mut angle_delta = ((-local_start.y - local_center.y) / radii.y)
            .atan2((-local_start.x - local_center.x) / radii.x)
            - angle_start;
        if !arc.sweep && angle_delta > 0.0 {
            angle_delta -= TAU;
        } else if arc.sweep && angle_delta < 0.0 {
            angle_delta += TAU;
        }
        let slices = ((angle_delta.abs() / FRAC_PI_2 - 1e-9).ceil() as usize).max(1);
        let step = angle_delta / slices as f64;
        let alpha = 4.0 / 3.0 * (step / 4.0).tan();
        let evaluate = |angle: f64| {
            center
                + Point::new(radii.x * angle.cos(), radii.y * angle.sin()).transformed(cosine, sine)
        };
        let tangent = |angle: f64| {
            Point::new(-radii.x * angle.sin(), radii.y * angle.cos()).transformed(cosine, sine)
        };
        let mut previous_angle = angle_start;
        let mut previous_point = start;
        for slice in 1..=slices {
            let angle = angle_start + step * slice as f64;
            let end = if slice == slices {
                arc.end
            } else {
                evaluate(angle)
            };
            self.cubic(
                previous_point + tangent(previous_angle) * alpha,
                end - tangent(angle) * alpha,
                end,
            );
            previous_angle = angle;
            previous_point = end;
        }
    }

    fn finish(mut self, closed: bool) -> Option<CubicPath> {
        if closed {
            self.line(self.start);
        }
        (self.points.len() >= 4).then_some(CubicPath {
            points: self.points,
            closed,
        })
    }
}

struct ArcSegment {
    radii: Point,
    rotation_degrees: f64,
    large_arc: bool,
    sweep: bool,
    end: Point,
}

#[derive(Clone, Copy)]
enum LastControl {
    Cubic(Point),
    Quadratic(Point),
}

struct PathParser<'a> {
    input: &'a str,
    position: usize,
}

impl PathParser<'_> {
    fn skip_separators(&mut self) {
        while self
            .input
            .as_bytes()
            .get(self.position)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b',')
        {
            self.position += 1;
        }
    }

    fn number(&mut self) -> Result<f64> {
        self.skip_separators();
        let start = self.position;
        let bytes = self.input.as_bytes();
        if bytes
            .get(self.position)
            .is_some_and(|byte| matches!(byte, b'+' | b'-'))
        {
            self.position += 1;
        }
        let mut has_digit = false;
        while bytes.get(self.position).is_some_and(u8::is_ascii_digit) {
            self.position += 1;
            has_digit = true;
        }
        if bytes.get(self.position) == Some(&b'.') {
            self.position += 1;
            while bytes.get(self.position).is_some_and(u8::is_ascii_digit) {
                self.position += 1;
                has_digit = true;
            }
        }
        ensure!(
            has_digit,
            "expected an SVG number at byte {}",
            self.position
        );
        if bytes
            .get(self.position)
            .is_some_and(|byte| matches!(byte, b'e' | b'E'))
        {
            let exponent_start = self.position;
            self.position += 1;
            if bytes
                .get(self.position)
                .is_some_and(|byte| matches!(byte, b'+' | b'-'))
            {
                self.position += 1;
            }
            let digits_start = self.position;
            while bytes.get(self.position).is_some_and(u8::is_ascii_digit) {
                self.position += 1;
            }
            if self.position == digits_start {
                self.position = exponent_start;
            }
        }
        let number: f64 = self
            .input
            .get(start..self.position)
            .context("invalid SVG number boundaries")?
            .parse()
            .context("invalid SVG number")?;
        ensure!(
            number.is_finite(),
            "non-finite SVG coordinate at byte {start}"
        );
        Ok(number)
    }

    fn point(&mut self, origin: Point) -> Result<Point> {
        let point = Point::new(self.number()?, self.number()?) + origin;
        ensure!(point.is_finite(), "SVG relative coordinates overflowed");
        Ok(point)
    }

    fn flag(&mut self) -> Result<bool> {
        self.skip_separators();
        match self.input.as_bytes().get(self.position).copied() {
            Some(value @ (b'0' | b'1')) => {
                self.position += 1;
                Ok(value == b'1')
            }
            _ => bail!("expected SVG arc flag 0 or 1 at byte {}", self.position),
        }
    }
}

pub fn parse_path(input: &str) -> Result<Vec<CubicPath>> {
    let mut parser = PathParser { input, position: 0 };
    let mut paths = Vec::new();
    let mut builder: Option<PathBuilder> = None;
    let mut current = Point::default();
    let mut start = Point::default();
    let mut command = None;
    let mut last_control = None;
    let mut started = false;
    loop {
        parser.skip_separators();
        let Some(byte) = input.as_bytes().get(parser.position).copied() else {
            break;
        };
        if b"MmLlHhVvCcSsQqTtAaZz".contains(&byte) {
            command = Some(byte);
            parser.position += 1;
        } else {
            command = match command {
                Some(b'M') => Some(b'L'),
                Some(b'm') => Some(b'l'),
                Some(b'Z' | b'z') => {
                    bail!("stray SVG data after close at byte {}", parser.position)
                }
                Some(command) => Some(command),
                None => bail!("SVG path must start with M or m"),
            };
        }
        let command = command.context("SVG path has no command")?;
        let absolute_command = command.to_ascii_uppercase();
        ensure!(
            started || absolute_command == b'M',
            "SVG path must start with M or m"
        );
        let origin = if command.is_ascii_lowercase() {
            current
        } else {
            Point::default()
        };
        match absolute_command {
            b'M' => {
                if let Some(path) = builder.take().and_then(|builder| builder.finish(false)) {
                    paths.push(path);
                }
                current = parser.point(origin)?;
                start = current;
                builder = Some(PathBuilder::new(current));
                last_control = None;
                started = true;
            }
            b'L' | b'H' | b'V' => {
                let end = match absolute_command {
                    b'H' => Point::new(parser.number()? + origin.x, current.y),
                    b'V' => Point::new(current.x, parser.number()? + origin.y),
                    _ => parser.point(origin)?,
                };
                builder
                    .get_or_insert_with(|| PathBuilder::new(current))
                    .line(end);
                current = end;
                last_control = None;
            }
            b'C' | b'S' => {
                let control_first = if absolute_command == b'C' {
                    parser.point(origin)?
                } else if let Some(LastControl::Cubic(control)) = last_control {
                    current * 2.0 - control
                } else {
                    current
                };
                let control_second = parser.point(origin)?;
                let end = parser.point(origin)?;
                builder
                    .get_or_insert_with(|| PathBuilder::new(current))
                    .cubic(control_first, control_second, end);
                current = end;
                last_control = Some(LastControl::Cubic(control_second));
            }
            b'Q' | b'T' => {
                let control = if absolute_command == b'Q' {
                    parser.point(origin)?
                } else if let Some(LastControl::Quadratic(control)) = last_control {
                    current * 2.0 - control
                } else {
                    current
                };
                let end = parser.point(origin)?;
                builder
                    .get_or_insert_with(|| PathBuilder::new(current))
                    .quadratic(control, end);
                current = end;
                last_control = Some(LastControl::Quadratic(control));
            }
            b'A' => {
                let radii = parser.point(Point::default())?;
                let rotation_degrees = parser.number()?;
                let large_arc = parser.flag()?;
                let sweep = parser.flag()?;
                let end = parser.point(origin)?;
                builder
                    .get_or_insert_with(|| PathBuilder::new(current))
                    .arc(ArcSegment {
                        radii,
                        rotation_degrees,
                        large_arc,
                        sweep,
                        end,
                    });
                current = end;
                last_control = None;
            }
            b'Z' => {
                if let Some(path) = builder.take().and_then(|builder| builder.finish(true)) {
                    paths.push(path);
                }
                current = start;
                last_control = None;
            }
            _ => bail!("unsupported SVG path command"),
        }
    }
    if let Some(path) = builder.and_then(|builder| builder.finish(false)) {
        paths.push(path);
    }
    for path in &paths {
        path.validate()?;
    }
    Ok(paths)
}

fn polygon_path(points: &[Point], closed: bool) -> Option<CubicPath> {
    let (&start, remainder) = points.split_first()?;
    let mut builder = PathBuilder::new(start);
    for &point in remainder {
        builder.line(point);
    }
    builder.finish(closed)
}

fn ellipse_path(center: Point, radii: Point) -> Option<CubicPath> {
    if radii.x < 1e-12 || radii.y < 1e-12 {
        return None;
    }
    let offset = radii * (4.0 / 3.0 * (PI / 8.0).tan());
    let east = center.x + radii.x;
    let west = center.x - radii.x;
    let south = center.y + radii.y;
    let north = center.y - radii.y;
    let mut builder = PathBuilder::new(Point::new(east, center.y));
    builder.cubic(
        Point::new(east, center.y + offset.y),
        Point::new(center.x + offset.x, south),
        Point::new(center.x, south),
    );
    builder.cubic(
        Point::new(center.x - offset.x, south),
        Point::new(west, center.y + offset.y),
        Point::new(west, center.y),
    );
    builder.cubic(
        Point::new(west, center.y - offset.y),
        Point::new(center.x - offset.x, north),
        Point::new(center.x, north),
    );
    builder.cubic(
        Point::new(center.x + offset.x, north),
        Point::new(east, center.y - offset.y),
        Point::new(east, center.y),
    );
    builder.finish(true)
}

pub fn normalize(primitive: &SvgPrimitive) -> Result<Vec<CubicPath>> {
    let path = match primitive {
        SvgPrimitive::Path(path) => return parse_path(path),
        SvgPrimitive::Line { start, end } => polygon_path(&[*start, *end], false),
        SvgPrimitive::Circle { center, radius } => {
            ensure!(radius.is_finite(), "non-finite circle radius");
            ellipse_path(*center, Point::new(*radius, *radius))
        }
        SvgPrimitive::Ellipse { center, radii } => {
            ensure!(radii.is_finite(), "non-finite ellipse radii");
            ellipse_path(*center, *radii)
        }
        SvgPrimitive::Polyline(points) => polygon_path(points, false),
        SvgPrimitive::Polygon(points) => polygon_path(points, true),
        SvgPrimitive::Rectangle {
            origin,
            size,
            radius_x,
            radius_y,
        } => {
            ensure!(
                origin.is_finite() && size.is_finite(),
                "non-finite rectangle geometry"
            );
            ensure!(
                radius_x.is_none_or(f64::is_finite) && radius_y.is_none_or(f64::is_finite),
                "non-finite rectangle radius"
            );
            if size.x < 1e-12 || size.y < 1e-12 {
                return Ok(Vec::new());
            }
            let radius_x = radius_x.or(*radius_y).unwrap_or(0.0);
            let radius_y = radius_y.unwrap_or(radius_x).clamp(0.0, size.y / 2.0);
            let radius_x = radius_x.clamp(0.0, size.x / 2.0);
            let end = *origin + *size;
            if radius_x < 1e-12 || radius_y < 1e-12 {
                polygon_path(
                    &[
                        *origin,
                        Point::new(end.x, origin.y),
                        end,
                        Point::new(origin.x, end.y),
                    ],
                    true,
                )
            } else {
                let first_x = origin.x + radius_x;
                let second_x = end.x - radius_x;
                let first_y = origin.y + radius_y;
                let second_y = end.y - radius_y;
                let offset = Point::new(radius_x, radius_y) * (4.0 / 3.0 * (PI / 8.0).tan());
                let mut builder = PathBuilder::new(Point::new(first_x, origin.y));
                builder.line(Point::new(second_x, origin.y));
                builder.cubic(
                    Point::new(second_x + offset.x, origin.y),
                    Point::new(end.x, first_y - offset.y),
                    Point::new(end.x, first_y),
                );
                builder.line(Point::new(end.x, second_y));
                builder.cubic(
                    Point::new(end.x, second_y + offset.y),
                    Point::new(second_x + offset.x, end.y),
                    Point::new(second_x, end.y),
                );
                builder.line(Point::new(first_x, end.y));
                builder.cubic(
                    Point::new(first_x - offset.x, end.y),
                    Point::new(origin.x, second_y + offset.y),
                    Point::new(origin.x, second_y),
                );
                builder.line(Point::new(origin.x, first_y));
                builder.cubic(
                    Point::new(origin.x, first_y - offset.y),
                    Point::new(first_x - offset.x, origin.y),
                    Point::new(first_x, origin.y),
                );
                builder.finish(true)
            }
        }
    };
    if let Some(path) = &path {
        path.validate()?;
    }
    Ok(path.into_iter().collect())
}

/// Maps a source viewBox onto the icon grid uniformly, never fitting to the glyph's bounds,
/// and returns the scale so stroke widths can follow it.
pub fn fit_paths(paths: &mut [CubicPath], view_box: [f64; 4], grid: f64) -> Result<f64> {
    let [minimum_x, minimum_y, width, height] = view_box;
    ensure!(
        view_box.iter().all(|value| value.is_finite())
            && width > 0.0
            && height > 0.0
            && grid.is_finite()
            && grid > 0.0,
        "invalid SVG viewBox or target grid"
    );
    let scale = (grid / width).min(grid / height);
    let translation = Point::new(
        (grid - width * scale) / 2.0 - minimum_x * scale,
        (grid - height * scale) / 2.0 - minimum_y * scale,
    );
    for path in paths {
        path.validate()?;
        for point in &mut path.points {
            *point = *point * scale + translation;
        }
        path.validate()?;
    }
    Ok(scale)
}

const GAUSS_NODES: [f64; 4] = [
    0.18343464249564978,
    0.525532409916329,
    0.7966664774136267,
    0.9602898564975363,
];
const GAUSS_WEIGHTS: [f64; 4] = [
    0.362683783378362,
    0.31370664587788727,
    0.22238103445337448,
    0.10122853629037626,
];

fn cubic_speed(segment: [Point; 4], progress: f64) -> f64 {
    let [start, control_first, control_second, end] = segment;
    let remaining = 1.0 - progress;
    ((control_first - start) * (3.0 * remaining * remaining)
        + (control_second - control_first) * (6.0 * remaining * progress)
        + (end - control_second) * (3.0 * progress * progress))
        .length()
}

fn segment_length(segment: [Point; 4], progress: f64) -> f64 {
    let half = progress / 2.0;
    GAUSS_NODES
        .iter()
        .zip(GAUSS_WEIGHTS)
        .map(|(&node, weight)| {
            weight
                * (cubic_speed(segment, half + half * node)
                    + cubic_speed(segment, half - half * node))
        })
        .sum::<f64>()
        * half
}

pub fn evaluate_cubic(segment: [Point; 4], progress: f64) -> Point {
    let [start, control_first, control_second, end] = segment;
    let remaining = 1.0 - progress;
    start * remaining.powi(3)
        + control_first * (3.0 * remaining * remaining * progress)
        + control_second * (3.0 * remaining * progress * progress)
        + end * progress.powi(3)
}

fn endpoint_tangent(segment: [Point; 4], at_end: bool) -> Option<Point> {
    let [start, control_first, control_second, end] = segment;
    let candidates = if at_end {
        [end - control_second, end - control_first, end - start]
    } else {
        [control_first - start, control_second - start, end - start]
    };
    candidates
        .into_iter()
        .find(|tangent| tangent.dot(*tangent) > 1e-18)
}

pub fn detect_corners(path: &CubicPath, threshold: f64) -> Result<Vec<usize>> {
    path.validate()?;
    ensure!(
        threshold.is_finite() && threshold >= 0.0,
        "invalid corner threshold"
    );
    let active: Vec<_> = path
        .segments()
        .enumerate()
        .filter(|(_, segment)| segment_length(*segment, 1.0) > 1e-9)
        .collect();
    let mut corners = Vec::new();
    let mut inspect = |source: (usize, [Point; 4]), target: (usize, [Point; 4])| {
        if let (Some(incoming), Some(outgoing)) = (
            endpoint_tangent(source.1, true),
            endpoint_tangent(target.1, false),
        ) {
            let angle = incoming.cross(outgoing).atan2(incoming.dot(outgoing)).abs();
            if angle > threshold {
                corners.push(target.0);
            }
        }
    };
    for pair in active.windows(2) {
        inspect(pair[0], pair[1]);
    }
    if path.closed && active.len() > 1 {
        if let (Some(&first), Some(&last)) = (active.first(), active.last()) {
            inspect(last, first);
        }
    }
    corners.sort_unstable();
    corners.dedup();
    Ok(corners)
}

fn invert_length(segment: [Point; 4], distance: f64, length: f64) -> f64 {
    if distance <= 0.0 {
        return 0.0;
    }
    if distance >= length {
        return 1.0;
    }
    let mut lower = 0.0;
    let mut upper = 1.0;
    let mut progress = distance / length;
    for _ in 0..12 {
        let error = segment_length(segment, progress) - distance;
        if error.abs() < 1e-10 * length + 1e-14 {
            break;
        }
        if error > 0.0 {
            upper = progress;
        } else {
            lower = progress;
        }
        let speed = cubic_speed(segment, progress);
        let candidate = if speed > 1e-12 {
            progress - error / speed
        } else {
            (lower + upper) / 2.0
        };
        progress = if candidate > lower && candidate < upper {
            candidate
        } else {
            (lower + upper) / 2.0
        };
    }
    progress
}

pub fn resample_path(path: &CubicPath, count: usize, corner_threshold: f64) -> Result<Vec<Point>> {
    path.validate()?;
    ensure!(count >= 2, "a sampled path needs at least two points");
    let start = *path.points.first().context("empty cubic path")?;
    let segments: Vec<_> = path.segments().collect();
    let segment_count = segments.len();
    let lengths: Vec<_> = segments
        .iter()
        .map(|&segment| segment_length(segment, 1.0))
        .collect();
    let total_length: f64 = lengths.iter().sum();
    ensure!(total_length.is_finite(), "cubic arc length overflowed");
    if total_length < 1e-12 {
        return Ok(vec![start; count]);
    }
    let corners = detect_corners(path, corner_threshold)?;
    let anchors = if path.closed {
        if corners.is_empty() { vec![0] } else { corners }
    } else {
        let mut anchors = corners;
        anchors.extend([0, segment_count]);
        anchors.sort_unstable();
        anchors.dedup();
        anchors
    };
    let runs: Vec<_> = if path.closed {
        anchors
            .iter()
            .enumerate()
            .map(|(index, &start)| {
                let end = anchors
                    .get(index + 1)
                    .copied()
                    .unwrap_or(anchors[0] + segment_count);
                (start, end)
            })
            .collect()
    } else {
        anchors.windows(2).map(|pair| (pair[0], pair[1])).collect()
    };
    let intervals = if path.closed { count } else { count - 1 };
    ensure!(
        runs.len() <= intervals,
        "{count} samples cannot anchor {} runs",
        runs.len()
    );
    let run_lengths: Vec<f64> = runs
        .iter()
        .map(|&(start, end)| {
            (start..end)
                .map(|segment| lengths[segment % segment_count])
                .sum()
        })
        .collect();
    let length_sum: f64 = run_lengths.iter().sum();
    let denominator = if length_sum == 0.0 { 1.0 } else { length_sum };
    let ideal: Vec<_> = run_lengths
        .iter()
        .map(|length| intervals as f64 * length / denominator)
        .collect();
    let mut counts: Vec<_> = ideal
        .iter()
        .map(|quota| (quota.floor() as usize).max(1))
        .collect();
    let assigned: usize = counts.iter().sum();
    if assigned < intervals {
        // Quadrature noise must not choose a different corner allocation after a rotation.
        let mut order: Vec<_> = ideal
            .iter()
            .enumerate()
            .map(|(index, quota)| (index, ((quota - quota.floor()) * 1e9).round() as u64))
            .collect();
        order.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
        for index in 0..intervals - assigned {
            counts[order[index % order.len()].0] += 1;
        }
    } else {
        for _ in intervals..assigned {
            let mut largest = 0;
            for index in 1..counts.len() {
                if counts[index] > counts[largest] {
                    largest = index;
                }
            }
            ensure!(counts[largest] > 1, "cannot apportion sample intervals");
            counts[largest] -= 1;
        }
    }
    let mut output = Vec::with_capacity(count);
    for (run, &(start_segment, end_segment)) in runs.iter().enumerate() {
        output.push(segments[start_segment % segment_count][0]);
        let mut segment = start_segment;
        let mut accumulated = 0.0;
        for interval in 1..counts[run] {
            let distance = run_lengths[run] * interval as f64 / counts[run] as f64;
            while segment < end_segment - 1
                && accumulated + lengths[segment % segment_count] < distance
            {
                accumulated += lengths[segment % segment_count];
                segment += 1;
            }
            let index = segment % segment_count;
            let progress = if lengths[index] > 1e-12 {
                invert_length(segments[index], distance - accumulated, lengths[index])
            } else {
                0.0
            };
            output.push(evaluate_cubic(segments[index], progress));
        }
    }
    if !path.closed {
        output.push(*path.points.last().context("empty cubic path")?);
    }
    ensure!(
        output.len() == count,
        "arc-length sampling produced an incorrect point count"
    );
    Ok(output)
}

#[derive(Clone, Copy, Debug)]
pub struct Similarity {
    pub theta: f64,
    pub sigma: f64,
    pub residual: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct BlockTransport {
    pub offset: Point,
    pub drift: Point,
}

#[derive(Clone, Debug)]
pub struct PlanItem {
    pub source: Vec<Point>,
    pub source_centered: Vec<Point>,
    pub target_transformed: Vec<Point>,
    pub target_oriented: Vec<Point>,
    pub source_centroid: Point,
    pub target_centroid: Point,
    pub theta: f64,
    pub log_sigma: f64,
    pub residual: f64,
    pub closed: bool,
    pub hole: bool,
    pub half: Option<u8>,
    pub block: Option<BlockTransport>,
}

#[derive(Clone, Debug)]
pub struct MorphPlan {
    pub items: Vec<PlanItem>,
    pub point_count: usize,
}

pub fn centroid(points: &[Point]) -> Result<Point> {
    ensure!(
        !points.is_empty(),
        "cannot find the centroid of an empty contour"
    );
    let center = points
        .iter()
        .copied()
        .fold(Point::default(), |sum, point| sum + point)
        / points.len() as f64;
    ensure!(center.is_finite(), "contour centroid is non-finite");
    Ok(center)
}

pub fn polyline_length(points: &[Point]) -> f64 {
    points
        .windows(2)
        .map(|pair| pair[0].distance(pair[1]))
        .sum()
}

pub fn reverse_points(points: &[Point]) -> Vec<Point> {
    points.iter().rev().copied().collect()
}

pub fn rotate_points(points: &[Point], offset: usize) -> Vec<Point> {
    if points.is_empty() {
        return Vec::new();
    }
    let offset = offset % points.len();
    points[offset..]
        .iter()
        .chain(points[..offset].iter())
        .copied()
        .collect()
}

pub fn procrustes(
    source: &[Point],
    target: &[Point],
    source_centroid: Point,
    target_centroid: Point,
) -> Result<Similarity> {
    ensure!(
        !source.is_empty() && source.len() == target.len(),
        "Procrustes needs equally sized, non-empty contours"
    );
    let mut covariance_xx = 0.0;
    let mut covariance_xy = 0.0;
    let mut covariance_yx = 0.0;
    let mut covariance_yy = 0.0;
    let mut source_energy = 0.0;
    let mut target_energy = 0.0;
    for (&source, &target) in source.iter().zip(target) {
        let source = source - source_centroid;
        let target = target - target_centroid;
        covariance_xx += source.x * target.x;
        covariance_xy += source.x * target.y;
        covariance_yx += source.y * target.x;
        covariance_yy += source.y * target.y;
        source_energy += source.dot(source);
        target_energy += target.dot(target);
    }
    let theta = (covariance_xy - covariance_yx).atan2(covariance_xx + covariance_yy);
    let numerator = theta.cos() * (covariance_xx + covariance_yy)
        + theta.sin() * (covariance_xy - covariance_yx);
    let sigma = if source_energy > 1e-12 {
        numerator / source_energy
    } else {
        1.0
    }
    .max(1e-6);
    let residual_squared =
        (sigma * sigma * source_energy - 2.0 * sigma * numerator + target_energy).max(0.0);
    let residual = if target_energy > 1e-12 {
        (residual_squared / target_energy).sqrt()
    } else {
        0.0
    };
    ensure!(
        theta.is_finite()
            && sigma.is_finite()
            && residual.is_finite()
            && source_energy.is_finite()
            && target_energy.is_finite(),
        "Procrustes overflowed"
    );
    Ok(Similarity {
        theta,
        sigma,
        residual,
    })
}

struct Alignment {
    source: Vec<Point>,
    target: Vec<Point>,
    source_centroid: Point,
    target_centroid: Point,
    similarity: Similarity,
}

fn align_pair(
    source: &[Point],
    target: &[Point],
    source_closed: bool,
    target_closed: bool,
    allow_reverse: bool,
    rotation_weight: f64,
) -> Result<Alignment> {
    ensure!(
        rotation_weight.is_finite() && rotation_weight >= 0.0,
        "invalid rotation weight"
    );
    ensure!(
        source.len() == target.len() && !source.is_empty(),
        "alignment needs equally sized, non-empty contours"
    );
    let source_centroid = centroid(source)?;
    let target_centroid = centroid(target)?;
    let vary_source = source_closed && !target_closed;
    let base = if vary_source { source } else { target };
    let offsets = if source_closed || target_closed {
        base.len()
    } else {
        1
    };
    let mut best: Option<(Vec<Point>, Similarity)> = None;
    let mut best_score = f64::INFINITY;
    for direction in 0..if allow_reverse { 2 } else { 1 } {
        let walk = if direction == 1 {
            reverse_points(base)
        } else {
            base.to_vec()
        };
        for offset in 0..offsets {
            let candidate = rotate_points(&walk, offset);
            let similarity = if vary_source {
                procrustes(&candidate, target, source_centroid, target_centroid)?
            } else {
                procrustes(source, &candidate, source_centroid, target_centroid)?
            };
            let score = similarity.residual + rotation_weight * similarity.theta.abs() / PI;
            if score < best_score {
                best_score = score;
                best = Some((candidate, similarity));
            }
        }
    }
    let (best, similarity) = best.context("no contour alignment found")?;
    let (source, target) = if vary_source {
        (best, target.to_vec())
    } else {
        (source.to_vec(), best)
    };
    Ok(Alignment {
        source,
        target,
        source_centroid,
        target_centroid,
        similarity,
    })
}

pub fn cost_matrix(source: &[Vec<Point>], target: &[Vec<Point>]) -> Result<Vec<Vec<f64>>> {
    let target_centroids = target
        .iter()
        .map(|points| centroid(points))
        .collect::<Result<Vec<_>>>()?;
    let target_lengths: Vec<_> = target
        .iter()
        .map(|points| polyline_length(points))
        .collect();
    source
        .iter()
        .map(|points| {
            let source_centroid = centroid(points)?;
            let source_length = polyline_length(points);
            let row: Vec<_> = target_centroids
                .iter()
                .zip(&target_lengths)
                .map(|(&center, &length)| {
                    source_centroid.distance(center)
                        + LENGTH_WEIGHT * (source_length - length).abs()
                })
                .collect();
            ensure!(
                row.iter().all(|cost| cost.is_finite()),
                "subpath pairing cost overflowed"
            );
            Ok(row)
        })
        .collect()
}

struct PermutationSearch<'a> {
    costs: &'a [Vec<f64>],
    best_cost: f64,
    best: Vec<usize>,
}

impl PermutationSearch<'_> {
    fn search(&mut self, assignment: &mut [usize], index: usize, cost: f64) {
        if cost >= self.best_cost {
            return;
        }
        if index == assignment.len() {
            self.best_cost = cost;
            self.best.copy_from_slice(assignment);
            return;
        }
        for candidate in index..assignment.len() {
            assignment.swap(index, candidate);
            self.search(
                assignment,
                index + 1,
                cost + self.costs[index][assignment[index]],
            );
            assignment.swap(index, candidate);
        }
    }
}

fn best_permutation(costs: &[Vec<f64>]) -> Result<Vec<usize>> {
    let count = costs.len();
    ensure!(
        costs.iter().all(|row| row.len() == count),
        "permutation needs a square cost matrix"
    );
    if count > PERMUTATION_LIMIT {
        let mut pairs: Vec<_> = costs
            .iter()
            .enumerate()
            .flat_map(|(source, row)| {
                row.iter()
                    .enumerate()
                    .map(move |(target, &cost)| (cost, source, target))
            })
            .collect();
        pairs.sort_by(|left, right| left.0.total_cmp(&right.0));
        let mut assignment = vec![None; count];
        let mut used = vec![false; count];
        for (_, source, target) in pairs {
            if assignment[source].is_none() && !used[target] {
                assignment[source] = Some(target);
                used[target] = true;
            }
        }
        return assignment
            .into_iter()
            .map(|target| target.context("unmatched subpath"))
            .collect();
    }
    let mut assignment: Vec<_> = (0..count).collect();
    let mut search = PermutationSearch {
        costs,
        best_cost: f64::INFINITY,
        best: assignment.clone(),
    };
    search.search(&mut assignment, 0, 0.0);
    Ok(search.best)
}

pub fn assignment_count_exceeds(base: usize, exponent: usize, limit: usize) -> bool {
    if base == 0 {
        return exponent == 0 && limit == 0;
    }
    let mut count = 1usize;
    for _ in 0..exponent {
        if count > limit / base {
            return true;
        }
        count *= base;
    }
    count > limit
}

struct SurjectionSearch<'a> {
    costs: &'a [Vec<f64>],
    assignment: Vec<usize>,
    multiplicities: Vec<usize>,
    best_cost: f64,
    best: Option<Vec<usize>>,
}

impl SurjectionSearch<'_> {
    fn search(&mut self, index: usize, cost: f64, covered: usize) {
        if cost >= self.best_cost
            || self.multiplicities.len() - covered > self.assignment.len() - index
        {
            return;
        }
        if index == self.assignment.len() {
            self.best_cost = cost;
            self.best = Some(self.assignment.clone());
            return;
        }
        for candidate in 0..self.multiplicities.len() {
            self.assignment[index] = candidate;
            self.multiplicities[candidate] += 1;
            let newly_covered = usize::from(self.multiplicities[candidate] == 1);
            self.search(
                index + 1,
                cost + self.costs[index][candidate],
                covered + newly_covered,
            );
            self.multiplicities[candidate] -= 1;
        }
    }
}

fn best_surjection(costs: &[Vec<f64>]) -> Result<Vec<usize>> {
    let large_count = costs.len();
    let small_count = costs.first().context("empty surjection matrix")?.len();
    ensure!(
        small_count > 0
            && large_count >= small_count
            && costs.iter().all(|row| row.len() == small_count),
        "invalid surjection matrix"
    );
    if small_count == 1 {
        return Ok(vec![0; large_count]);
    }
    if assignment_count_exceeds(small_count, large_count, SURJECTION_LIMIT) {
        let mut assignment: Vec<_> = costs
            .iter()
            .map(|row| {
                let mut best = 0;
                for candidate in 1..small_count {
                    if row[candidate] < row[best] {
                        best = candidate;
                    }
                }
                best
            })
            .collect();
        let mut multiplicities = vec![0usize; small_count];
        for &target in &assignment {
            multiplicities[target] += 1;
        }
        for target in 0..small_count {
            if multiplicities[target] > 0 {
                continue;
            }
            let mut donor = None;
            let mut best_cost = f64::INFINITY;
            for source in 0..large_count {
                if multiplicities[assignment[source]] < 2 {
                    continue;
                }
                let extra = costs[source][target] - costs[source][assignment[source]];
                if extra < best_cost {
                    donor = Some(source);
                    best_cost = extra;
                }
            }
            let donor = donor.context("cannot cover every subpath in surjection")?;
            multiplicities[assignment[donor]] -= 1;
            assignment[donor] = target;
            multiplicities[target] += 1;
        }
        return Ok(assignment);
    }
    let mut search = SurjectionSearch {
        costs,
        assignment: vec![0; large_count],
        multiplicities: vec![0; small_count],
        best_cost: f64::INFINITY,
        best: None,
    };
    search.search(0, 0.0, 0);
    search
        .best
        .context("no valid surjective subpath assignment")
}

pub fn pair_up(source: &[Vec<Point>], target: &[Vec<Point>]) -> Result<Vec<(usize, usize)>> {
    ensure!(
        !source.is_empty() && !target.is_empty(),
        "an icon has no subpaths"
    );
    if source.len() == target.len() {
        Ok(best_permutation(&cost_matrix(source, target)?)?
            .into_iter()
            .enumerate()
            .collect())
    } else if source.len() < target.len() {
        Ok(best_surjection(&cost_matrix(target, source)?)?
            .into_iter()
            .enumerate()
            .map(|(target, source)| (source, target))
            .collect())
    } else {
        Ok(best_surjection(&cost_matrix(source, target)?)?
            .into_iter()
            .enumerate()
            .collect())
    }
}

pub fn make_item(
    source: &[Point],
    target: &[Point],
    source_closed: bool,
    target_closed: bool,
    allow_reverse: bool,
    rotation_weight: f64,
) -> Result<PlanItem> {
    let alignment = align_pair(
        source,
        target,
        source_closed,
        target_closed,
        allow_reverse,
        rotation_weight,
    )?;
    let cosine = (-alignment.similarity.theta).cos();
    let sine = (-alignment.similarity.theta).sin();
    let source_centered = alignment
        .source
        .iter()
        .map(|&point| point - alignment.source_centroid)
        .collect();
    let target_transformed = alignment
        .target
        .iter()
        .map(|&point| {
            (point - alignment.target_centroid).transformed(cosine, sine)
                / alignment.similarity.sigma
        })
        .collect();
    Ok(PlanItem {
        source: alignment.source,
        source_centered,
        target_transformed,
        target_oriented: alignment.target,
        source_centroid: alignment.source_centroid,
        target_centroid: alignment.target_centroid,
        theta: alignment.similarity.theta,
        log_sigma: alignment.similarity.sigma.ln(),
        residual: alignment.similarity.residual,
        closed: source_closed && target_closed,
        hole: false,
        half: None,
        block: None,
    })
}

pub fn apply_global(items: &mut [PlanItem], point_count: usize) -> Result<()> {
    ensure!(
        !items.is_empty() && point_count > 0,
        "global alignment needs non-empty contours"
    );
    ensure!(
        items.iter().all(|item| item.source.len() == point_count
            && item.target_oriented.len() == point_count
            && item.source_centered.len() == point_count
            && item.target_transformed.len() == point_count),
        "global alignment has inconsistent sample counts"
    );
    let source: Vec<_> = items
        .iter()
        .flat_map(|item| item.source.iter().copied())
        .collect();
    let target: Vec<_> = items
        .iter()
        .flat_map(|item| item.target_oriented.iter().copied())
        .collect();
    let source_centroid = centroid(&source)?;
    let similarity = procrustes(&source, &target, source_centroid, centroid(&target)?)?;
    if similarity.residual >= GLOBAL_RESIDUAL_THRESHOLD {
        return Ok(());
    }
    let cosine_inverse = (-similarity.theta).cos();
    let sine_inverse = (-similarity.theta).sin();
    let cosine = similarity.theta.cos();
    let sine = similarity.theta.sin();
    for item in items {
        let mut error_squared = 0.0;
        let mut target_energy = 0.0;
        for ((&target, &source), transformed) in item
            .target_oriented
            .iter()
            .zip(&item.source_centered)
            .zip(&mut item.target_transformed)
        {
            let target = target - item.target_centroid;
            *transformed = target.transformed(cosine_inverse, sine_inverse) / similarity.sigma;
            let error = source.transformed(cosine, sine) * similarity.sigma - target;
            error_squared += error.dot(error);
            target_energy += target.dot(target);
        }
        item.theta = similarity.theta;
        item.log_sigma = similarity.sigma.ln();
        item.residual = if target_energy > 1e-12 {
            (error_squared / target_energy).sqrt()
        } else {
            0.0
        };
        // Centroid lerp follows a chord and deforms a rotating multipart icon.
        let scale = item.log_sigma.exp();
        let offset = item.source_centroid - source_centroid;
        let rotated =
            offset.transformed(item.theta.cos() * scale, item.theta.sin() * scale) - offset;
        item.block = Some(BlockTransport {
            offset,
            drift: item.target_centroid - item.source_centroid - rotated,
        });
    }
    Ok(())
}

pub fn allocate_outputs(plan: &MorphPlan) -> Vec<Vec<Point>> {
    plan.items
        .iter()
        .map(|_| vec![Point::default(); plan.point_count])
        .collect()
}

pub fn interpolate_polar(
    plan: &MorphPlan,
    progress: f64,
    outputs: &mut [Vec<Point>],
) -> Result<()> {
    ensure!(progress.is_finite(), "non-finite morph progress");
    ensure!(
        outputs.len() == plan.items.len(),
        "morph output count does not match its plan"
    );
    for (item, output) in plan.items.iter().zip(outputs) {
        ensure!(
            output.len() == plan.point_count
                && item.source_centered.len() == plan.point_count
                && item.target_transformed.len() == plan.point_count,
            "morph output has an incorrect sample count"
        );
        let scale = (item.log_sigma * progress).exp();
        let angle = item.theta * progress;
        let cosine = angle.cos() * scale;
        let sine = angle.sin() * scale;
        let center = if let Some(block) = item.block {
            item.source_centroid + block.drift * progress + block.offset.transformed(cosine, sine)
                - block.offset
        } else {
            item.source_centroid.lerp(item.target_centroid, progress)
        };
        let blend = match item.half {
            Some(half @ 0..=1) => (2.0 * progress - f64::from(half)).clamp(0.0, 1.0),
            Some(_) => bail!("invalid half-flight marker"),
            None => progress,
        };
        for ((&source, &target), output) in item
            .source_centered
            .iter()
            .zip(&item.target_transformed)
            .zip(output)
        {
            *output = center + source.lerp(target, blend).transformed(cosine, sine);
            ensure!(output.is_finite(), "morph interpolation overflowed");
        }
    }
    Ok(())
}

pub fn snapshot(plan: &MorphPlan, outputs: &[Vec<Point>]) -> Result<Vec<Sampled>> {
    ensure!(
        outputs.len() == plan.items.len(),
        "morph snapshot count does not match its plan"
    );
    plan.items
        .iter()
        .zip(outputs)
        .map(|(item, points)| {
            ensure!(
                points.len() == plan.point_count && points.iter().all(|point| point.is_finite()),
                "invalid morph snapshot"
            );
            Ok(Sampled {
                points: points.clone(),
                closed: item.closed,
                hole: item.hole,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpringPreset {
    Smooth,
    #[default]
    Snappy,
    Bouncy,
}

impl SpringPreset {
    pub fn parameters(self) -> (f64, f64) {
        match self {
            Self::Smooth => (170.0, 26.0),
            Self::Snappy => (420.0, 30.0),
            Self::Bouncy => (300.0, 14.0),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub progress: f64,
    pub velocity: f64,
    pub stiffness: f64,
    pub damping: f64,
}

impl Default for Spring {
    fn default() -> Self {
        Self::new(SpringPreset::Snappy)
    }
}

impl Spring {
    pub fn new(preset: SpringPreset) -> Self {
        let (stiffness, damping) = preset.parameters();
        Self {
            progress: 1.0,
            velocity: 0.0,
            stiffness,
            damping,
        }
    }

    pub fn configure(&mut self, stiffness: f64, damping: f64) -> Result<()> {
        ensure!(
            stiffness.is_finite() && stiffness > 0.0 && damping.is_finite() && damping >= 0.0,
            "invalid spring parameters"
        );
        self.stiffness = stiffness;
        self.damping = damping;
        Ok(())
    }

    pub fn start(&mut self) {
        self.progress = 0.0;
        self.velocity = self.velocity.clamp(-14.0, 14.0);
    }

    pub fn step(&mut self, seconds: f64) -> Result<bool> {
        ensure!(
            seconds.is_finite() && seconds >= 0.0,
            "invalid spring frame duration"
        );
        ensure!(
            self.progress.is_finite()
                && self.velocity.is_finite()
                && self.stiffness.is_finite()
                && self.stiffness > 0.0
                && self.damping.is_finite()
                && self.damping >= 0.0,
            "invalid spring state"
        );
        let seconds = seconds.min(0.1);
        let steps = ((seconds / (1.0 / 240.0)).ceil() as usize).clamp(1, 16);
        let step = seconds / steps as f64;
        for _ in 0..steps {
            let acceleration =
                self.stiffness * (1.0 - self.progress) - self.damping * self.velocity;
            self.velocity += acceleration * step;
            self.progress += self.velocity * step;
        }
        ensure!(
            self.progress.is_finite() && self.velocity.is_finite(),
            "spring integration overflowed"
        );
        Ok((1.0 - self.progress).abs() < 0.001 && self.velocity.abs() < 0.02)
    }

    pub fn settle(&mut self) {
        self.progress = 1.0;
        self.velocity = 0.0;
    }
}
