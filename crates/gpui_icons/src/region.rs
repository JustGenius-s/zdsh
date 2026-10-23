// Derived from Morphicons v2.0.0 (74118e766f2e2f0026df334163fc1943eb01f6ab).
// Copyright (c) 2026 Guillermo. Licensed under MIT; see LICENSE-MORPHICONS.

use std::{collections::BTreeMap, f64::consts::PI};

use anyhow::{Context, Result, bail, ensure};

use crate::geometry::{
    BlockTransport, CubicPath, IconNode, MorphPlan, PlanItem, Point, SURJECTION_LIMIT, Sampled,
    apply_global, centroid, cost_matrix, fit_paths, make_item, normalize, pair_up, procrustes,
    resample_path, reverse_points, rotate_points,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FillRule {
    None,
    #[default]
    NonZero,
    EvenOdd,
}

#[derive(Clone, Copy, Debug)]
pub struct RegionOptions {
    pub cell: f64,
}

impl Default for RegionOptions {
    fn default() -> Self {
        Self { cell: 0.125 }
    }
}

#[derive(Clone, Debug)]
pub struct RegionPart {
    pub paths: Vec<CubicPath>,
    pub fill_rule: FillRule,
    pub stroke_width: f64,
}

#[derive(Clone, Debug)]
pub struct NormalizedRegion {
    pub paths: Vec<CubicPath>,
    pub samples: Vec<Sampled>,
}

struct FlattenedPart {
    lines: Vec<(Vec<Point>, bool)>,
    half_width: f64,
    fill_rule: FillRule,
}

struct Grid {
    origin: Point,
    cell: f64,
    columns: usize,
    rows: usize,
    length: usize,
}

pub fn signed_area(points: &[Point]) -> f64 {
    let Some(mut previous) = points.last().copied() else {
        return 0.0;
    };
    let mut area = 0.0;
    for &point in points {
        area += previous.x * point.y - point.x * previous.y;
        previous = point;
    }
    area / 2.0
}

pub fn inside(point: Point, polygon: &[Point]) -> bool {
    let Some(mut previous) = polygon.last().copied() else {
        return false;
    };
    let mut contained = false;
    for &current in polygon {
        if (current.y > point.y) != (previous.y > point.y)
            && point.x
                < (previous.x - current.x) * (point.y - current.y) / (previous.y - current.y)
                    + current.x
        {
            contained = !contained;
        }
        previous = current;
    }
    contained
}

pub fn distance_to_segment(point: Point, start: Point, end: Point) -> f64 {
    let direction = end - start;
    let length_squared = direction.dot(direction);
    let progress = if length_squared > 0.0 {
        ((point - start).dot(direction) / length_squared).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (point - start - direction * progress).length()
}

fn validate_path(path: &CubicPath) -> Result<()> {
    ensure!(!path.points.is_empty(), "region path has no points");
    ensure!(
        (path.points.len() - 1).is_multiple_of(3),
        "region path is not a cubic chain"
    );
    ensure!(
        path.points.iter().all(|point| point.is_finite()),
        "region path contains non-finite coordinates"
    );
    Ok(())
}

fn flatten(path: &CubicPath) -> Result<Vec<Point>> {
    validate_path(path)?;
    let mut output = vec![path.points[0]];
    for segment in path.points.windows(4).step_by(3) {
        let [start, first, second, end] = segment else {
            bail!("incomplete cubic in region path");
        };
        let flatness = (*start - *first * 2.0 + *second)
            .length()
            .max((*first - *second * 2.0 + *end).length());
        // The second-derivative bound limits each chord's deviation to 0.004 icon units.
        let steps = ((6.0 * flatness / (8.0 * 0.004)).sqrt().ceil() as usize).clamp(1, 128);
        for step in 1..=steps {
            let progress = step as f64 / steps as f64;
            let remaining = 1.0 - progress;
            output.push(
                *start * (remaining * remaining * remaining)
                    + *first * (3.0 * remaining * remaining * progress)
                    + *second * (3.0 * remaining * progress * progress)
                    + *end * (progress * progress * progress),
            );
        }
    }
    ensure!(
        output.iter().all(|point| point.is_finite()),
        "flattened region path contains non-finite coordinates"
    );
    Ok(output)
}

fn each_segment(points: &[Point], closed: bool, mut visit: impl FnMut(Point, Point)) {
    if let [point] = points {
        visit(*point, *point);
    }
    for edge in points.windows(2) {
        visit(edge[0], edge[1]);
    }
    if closed && points.len() > 2 {
        visit(points[points.len() - 1], points[0]);
    }
}

impl Grid {
    fn each_near(
        &self,
        start: Point,
        end: Point,
        radius: f64,
        mut visit: impl FnMut(usize, Point),
    ) {
        let first_column = ((start.x.min(end.x) - radius - self.origin.x) / self.cell)
            .floor()
            .max(0.0) as usize;
        let last_column = (((start.x.max(end.x) + radius - self.origin.x) / self.cell).ceil()
            as usize)
            .min(self.columns - 1);
        let first_row = ((start.y.min(end.y) - radius - self.origin.y) / self.cell)
            .floor()
            .max(0.0) as usize;
        let last_row = (((start.y.max(end.y) + radius - self.origin.y) / self.cell).ceil()
            as usize)
            .min(self.rows - 1);
        for row in first_row..=last_row {
            for column in first_column..=last_column {
                visit(
                    row * self.columns + column,
                    Point {
                        x: self.origin.x + column as f64 * self.cell,
                        y: self.origin.y + row as f64 * self.cell,
                    },
                );
            }
        }
    }
}

fn field_buffer(length: usize) -> Result<Vec<f32>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .context("allocating region distance field")?;
    values.resize(length, 1e9);
    Ok(values)
}

fn union_field(parts: &[FlattenedPart], grid: &Grid) -> Result<Vec<f32>> {
    let mut field = field_buffer(grid.length)?;
    let band = 2.0 * grid.cell;
    let mut distances = Vec::new();
    for part in parts {
        if part.half_width > 0.0 {
            for (points, closed) in &part.lines {
                each_segment(points, *closed, |start, end| {
                    grid.each_near(start, end, part.half_width + band, |index, point| {
                        let distance = distance_to_segment(point, start, end) - part.half_width;
                        if distance < field[index] as f64 {
                            field[index] = distance as f32;
                        }
                    });
                });
            }
        }
        if part.fill_rule == FillRule::None {
            continue;
        }
        if distances.is_empty() {
            distances = field_buffer(grid.length)?;
        } else {
            distances.fill(1e9);
        }
        for (points, _) in &part.lines {
            each_segment(points, true, |start, end| {
                grid.each_near(start, end, band, |index, point| {
                    let distance = distance_to_segment(point, start, end);
                    if distance < distances[index] as f64 {
                        distances[index] = distance as f32;
                    }
                });
            });
        }
        for row in 0..grid.rows {
            let vertical = grid.origin.y + row as f64 * grid.cell;
            let mut crossings = Vec::new();
            for (points, _) in &part.lines {
                each_segment(points, true, |start, end| {
                    if (start.y <= vertical) != (end.y <= vertical) {
                        crossings.push((
                            start.x + (vertical - start.y) * (end.x - start.x) / (end.y - start.y),
                            if end.y > start.y { 1_i64 } else { -1_i64 },
                        ));
                    }
                });
            }
            crossings.sort_by(|left, right| left.0.total_cmp(&right.0));
            let mut crossing_index = 0;
            let mut winding = 0_i64;
            for column in 0..grid.columns {
                let horizontal = grid.origin.x + column as f64 * grid.cell;
                while let Some(&(crossing, direction)) = crossings.get(crossing_index) {
                    if crossing >= horizontal {
                        break;
                    }
                    winding += direction;
                    crossing_index += 1;
                }
                let contained = match part.fill_rule {
                    FillRule::NonZero => winding != 0,
                    FillRule::EvenOdd => winding & 1 == 1,
                    FillRule::None => false,
                };
                let index = row * grid.columns + column;
                let distance = if contained {
                    -distances[index]
                } else {
                    distances[index]
                };
                field[index] = field[index].min(distance);
            }
        }
    }
    Ok(field)
}

#[derive(Default)]
struct ContourEdges {
    points: BTreeMap<usize, Point>,
    neighbors: BTreeMap<usize, Vec<usize>>,
    order: Vec<usize>,
}

impl ContourEdges {
    fn crossing(&mut self, key: usize, field: &[f32], grid: &Grid) -> Result<()> {
        if self.points.contains_key(&key) {
            return Ok(());
        }
        let node = key / 2;
        let column = node % grid.columns;
        let row = node / grid.columns;
        let other = node + if key & 1 == 1 { grid.columns } else { 1 };
        let start = *field.get(node).context("invalid region grid edge")? as f64;
        let end = *field.get(other).context("invalid region grid neighbor")? as f64;
        // Offsetting crossings from grid vertices keeps separate contours from touching.
        let progress = (start / (start - end)).clamp(1e-5, 1.0 - 1e-5);
        ensure!(progress.is_finite(), "invalid region edge crossing");
        let point = if key & 1 == 1 {
            Point {
                x: grid.origin.x + column as f64 * grid.cell,
                y: grid.origin.y + (row as f64 + progress) * grid.cell,
            }
        } else {
            Point {
                x: grid.origin.x + (column as f64 + progress) * grid.cell,
                y: grid.origin.y + row as f64 * grid.cell,
            }
        };
        self.points.insert(key, point);
        Ok(())
    }

    fn link(&mut self, first: usize, second: usize, field: &[f32], grid: &Grid) -> Result<()> {
        self.crossing(first, field, grid)?;
        self.crossing(second, field, grid)?;
        for (from, to) in [(first, second), (second, first)] {
            if !self.neighbors.contains_key(&from) {
                self.order.push(from);
            }
            self.neighbors.entry(from).or_default().push(to);
        }
        Ok(())
    }
}

fn trace(field: &[f32], grid: &Grid) -> Result<Vec<Vec<Point>>> {
    let mut edges = ContourEdges::default();
    for row in 0..grid.rows - 1 {
        for column in 0..grid.columns - 1 {
            let upper_left = row * grid.columns + column;
            let upper_right = upper_left + 1;
            let lower_right = upper_left + grid.columns + 1;
            let lower_left = upper_left + grid.columns;
            let upper_left_inside = field[upper_left] < 0.0;
            let upper_right_inside = field[upper_right] < 0.0;
            let lower_right_inside = field[lower_right] < 0.0;
            let lower_left_inside = field[lower_left] < 0.0;
            let top = 2 * upper_left;
            let right = 2 * upper_right + 1;
            let bottom = 2 * lower_left;
            let left = 2 * upper_left + 1;
            let mut active = Vec::with_capacity(4);
            if upper_left_inside != upper_right_inside {
                active.push(top);
            }
            if upper_right_inside != lower_right_inside {
                active.push(right);
            }
            if lower_left_inside != lower_right_inside {
                active.push(bottom);
            }
            if upper_left_inside != lower_left_inside {
                active.push(left);
            }
            match active.as_slice() {
                [first, second] => edges.link(*first, *second, field, grid)?,
                [_, _, _, _] => {
                    let center_inside = (field[upper_left] as f64
                        + field[upper_right] as f64
                        + field[lower_right] as f64
                        + field[lower_left] as f64)
                        / 4.0
                        < 0.0;
                    if center_inside == upper_left_inside {
                        edges.link(top, right, field, grid)?;
                        edges.link(bottom, left, field, grid)?;
                    } else {
                        edges.link(left, top, field, grid)?;
                        edges.link(right, bottom, field, grid)?;
                    }
                }
                [] => {}
                _ => bail!("region cell has an invalid crossing count"),
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut contours = Vec::new();
    for &start in &edges.order {
        if seen.contains(&start) {
            continue;
        }
        let mut contour = Vec::new();
        let mut previous = None;
        let mut current = start;
        while !seen.contains(&current) {
            seen.insert(current);
            contour.push(
                *edges
                    .points
                    .get(&current)
                    .context("region crossing is missing")?,
            );
            let neighbors = edges
                .neighbors
                .get(&current)
                .context("region crossing is disconnected")?;
            let [first, second] = neighbors.as_slice() else {
                bail!("region crossing does not have two neighbors");
            };
            let next = if Some(*first) != previous {
                *first
            } else {
                *second
            };
            previous = Some(current);
            current = next;
        }
        ensure!(current == start, "region contour did not close");
        if contour.len() >= 3 {
            contours.push(contour);
        }
    }
    Ok(contours)
}

fn simplify(points: &[Point], tolerance: f64) -> Vec<Point> {
    let count = points.len();
    if count < 4 {
        return points.to_vec();
    }
    let mut farthest = 1;
    let mut farthest_distance = -1.0;
    for index in 1..count {
        let distance = (points[index] - points[0]).length();
        if distance > farthest_distance {
            farthest = index;
            farthest_distance = distance;
        }
    }
    let mut keep = vec![false; count];
    keep[0] = true;
    keep[farthest] = true;
    let mut pending = vec![(0, farthest), (farthest, count)];
    while let Some((first, last)) = pending.pop() {
        let start = points[first % count];
        let end = points[last % count];
        let mut worst = None;
        let mut worst_distance = tolerance;
        for index in first + 1..last {
            let distance = distance_to_segment(points[index % count], start, end);
            if distance > worst_distance {
                worst = Some(index);
                worst_distance = distance;
            }
        }
        if let Some(index) = worst {
            keep[index % count] = true;
            pending.extend([(first, index), (index, last)]);
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(&point, keep)| keep.then_some(point))
        .collect()
}

fn recover_corners(points: &[Point], cell: f64) -> Vec<Point> {
    let count = points.len();
    if count < 4 {
        return points.to_vec();
    }
    let edge_length = |index: usize| (points[(index + 1) % count] - points[index % count]).length();
    let long_edges: Vec<_> = (0..count)
        .filter(|&index| edge_length(index) >= 2.5 * cell)
        .collect();
    if long_edges.len() < 2 {
        return points.to_vec();
    }
    let mut dropped = vec![false; count];
    let mut moved = BTreeMap::new();
    for index in 0..long_edges.len() {
        let first = long_edges[index];
        let second = long_edges[(index + 1) % long_edges.len()];
        let gap = (second + count - first - 1) % count;
        if !(1..=3).contains(&gap) {
            continue;
        }
        let gap_length: f64 = (1..=gap).map(|offset| edge_length(first + offset)).sum();
        if gap_length > 3.0 * cell {
            continue;
        }
        let first_end = (first + 1) % count;
        let first_direction = points[first_end] - points[first];
        let second_direction = points[(second + 1) % count] - points[second];
        let cross = first_direction.cross(second_direction);
        let angle = cross.atan2(first_direction.dot(second_direction)).abs();
        if angle < PI / 12.0 || cross == 0.0 {
            continue;
        }
        let progress = (points[second] - points[first]).cross(second_direction) / cross;
        let intersection = points[first] + first_direction * progress;
        let middle = (points[first_end] + points[second]) / 2.0;
        if !intersection.is_finite() || (intersection - middle).length() > 2.0 * cell {
            continue;
        }
        moved.insert(first_end, intersection);
        for offset in 2..=gap + 1 {
            dropped[(first + offset) % count] = true;
        }
    }
    points
        .iter()
        .enumerate()
        .filter_map(|(index, &point)| {
            (!dropped[index]).then(|| moved.get(&index).copied().unwrap_or(point))
        })
        .collect()
}

pub fn region_contours(parts: &[RegionPart], cell: f64) -> Result<Vec<Vec<Point>>> {
    ensure!(
        cell.is_finite() && cell > 0.0,
        "region cell size must be positive and finite"
    );
    let mut flattened = Vec::new();
    let mut minimum = Point {
        x: f64::INFINITY,
        y: f64::INFINITY,
    };
    let mut maximum = Point {
        x: f64::NEG_INFINITY,
        y: f64::NEG_INFINITY,
    };
    let mut padding: f64 = 0.0;
    for part in parts {
        ensure!(
            part.stroke_width.is_finite() && part.stroke_width >= 0.0,
            "region stroke width must be nonnegative and finite"
        );
        if part.stroke_width == 0.0 && part.fill_rule == FillRule::None {
            continue;
        }
        let mut lines = Vec::new();
        for path in &part.paths {
            let points = flatten(path)?;
            for point in &points {
                minimum.x = minimum.x.min(point.x);
                minimum.y = minimum.y.min(point.y);
                maximum.x = maximum.x.max(point.x);
                maximum.y = maximum.y.max(point.y);
            }
            lines.push((points, path.closed));
        }
        let half_width = part.stroke_width / 2.0;
        padding = padding.max(half_width);
        flattened.push(FlattenedPart {
            lines,
            half_width,
            fill_rule: part.fill_rule,
        });
    }
    if !minimum.is_finite() {
        return Ok(Vec::new());
    }
    padding += 3.0 * cell;
    let columns = ((maximum.x - minimum.x + 2.0 * padding) / cell).ceil() + 1.0;
    let rows = ((maximum.y - minimum.y + 2.0 * padding) / cell).ceil() + 1.0;
    ensure!(
        columns.is_finite()
            && rows.is_finite()
            && columns >= 2.0
            && rows >= 2.0
            && columns < usize::MAX as f64
            && rows < usize::MAX as f64,
        "region distance field dimensions are invalid"
    );
    let columns = columns as usize;
    let rows = rows as usize;
    let length = columns
        .checked_mul(rows)
        .context("region distance field dimensions overflow")?;
    ensure!(length <= usize::MAX / 2, "region edge identifiers overflow");
    let grid = Grid {
        origin: Point {
            x: minimum.x - padding,
            y: minimum.y - padding,
        },
        cell,
        columns,
        rows,
        length,
    };
    let field = union_field(&flattened, &grid)?;
    let contours: Vec<_> = trace(&field, &grid)?
        .into_iter()
        .filter(|points| signed_area(points).abs() > 0.01)
        .collect();
    let mut normalized = Vec::with_capacity(contours.len());
    for (index, contour) in contours.iter().enumerate() {
        let depth = contours
            .iter()
            .enumerate()
            .filter(|(other, polygon)| *other != index && inside(contour[0], polygon))
            .count();
        let hole = depth % 2 == 1;
        let oriented = if (signed_area(contour) > 0.0) != hole {
            contour.clone()
        } else {
            reverse_points(contour)
        };
        normalized.push(recover_corners(&simplify(&oriented, cell * 0.08), cell));
    }
    Ok(normalized)
}

fn contour_path(contour: &[Point]) -> Result<CubicPath> {
    let rounded: Vec<_> = contour
        .iter()
        .map(|point| Point {
            x: (point.x * 1000.0 + 0.5).floor() / 1000.0,
            y: (point.y * 1000.0 + 0.5).floor() / 1000.0,
        })
        .collect();
    let first = *rounded
        .first()
        .context("normalized region contour is empty")?;
    let mut points = vec![first];
    for index in 0..rounded.len() {
        let start = rounded[index];
        let end = rounded[(index + 1) % rounded.len()];
        points.extend([start.lerp(end, 1.0 / 3.0), start.lerp(end, 2.0 / 3.0), end]);
    }
    Ok(CubicPath {
        points,
        closed: true,
    })
}

/// The parts an icon's SVG nodes paint, following upstream `partsOf`, with the source viewBox
/// mapped onto the 24-unit grid.
pub fn icon_parts(nodes: &[IconNode], view_box: [f64; 4]) -> Result<Vec<RegionPart>> {
    let fill_icon = nodes.iter().any(|node| node.painted("fill"));
    let mut parts = Vec::with_capacity(nodes.len());
    // Unlike upstream, translucent layers stay in the silhouette: the flight is one opaque
    // shape, and the static SVG restores each layer's opacity at rest.
    for node in nodes {
        let stroke = if fill_icon {
            node.painted("stroke")
        } else {
            !node.attributes.contains_key("stroke") || node.painted("stroke")
        };
        let fill = fill_icon
            && (node.painted("fill") || (!node.attributes.contains_key("fill") && !stroke));
        if !stroke && !fill {
            continue;
        }
        let mut paths = normalize(&node.primitive()?)?;
        let scale = fit_paths(&mut paths, view_box, 24.0)?;
        let fill_rule = if !fill {
            FillRule::None
        } else if node
            .attributes
            .get("fill-rule")
            .is_some_and(|rule| rule.trim() == "evenodd")
        {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        };
        parts.push(RegionPart {
            paths,
            fill_rule,
            stroke_width: if stroke {
                node.number("stroke-width", 1.0) * scale
            } else {
                0.0
            },
        });
    }
    Ok(parts)
}

/// An icon without visible surface, such as Cursor's zero-area `minus-small`, yields no
/// contours and morphs from or to nothing.
pub fn normalize_region(parts: &[RegionPart], options: &RegionOptions) -> Result<NormalizedRegion> {
    let contours = region_contours(parts, options.cell)?;
    let paths = contours
        .iter()
        .map(|contour| contour_path(contour))
        .collect::<Result<Vec<_>>>()?;
    let samples = resample_region(&paths, None)?;
    Ok(NormalizedRegion { paths, samples })
}

fn samples_are_coarse(path: &CubicPath, samples: &[Point]) -> bool {
    if samples.is_empty() {
        return true;
    }
    let mut previous_match = 0;
    for &vertex in path.points.iter().step_by(3) {
        let mut matched = false;
        for offset in 0..samples.len() {
            let start = (previous_match + offset) % samples.len();
            let end = (start + 1) % samples.len();
            if distance_to_segment(vertex, samples[start], samples[end]) <= 0.3 {
                previous_match = start;
                matched = true;
                break;
            }
        }
        if !matched {
            return true;
        }
    }
    false
}

pub fn resample_region(paths: &[CubicPath], point_count: Option<usize>) -> Result<Vec<Sampled>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    for path in paths {
        validate_path(path)?;
    }
    let sample_at = |count| -> Result<Vec<Sampled>> {
        ensure!(count >= 2, "region sampling needs at least two points");
        paths
            .iter()
            .map(|path| {
                let closed_path = CubicPath {
                    points: path.points.clone(),
                    closed: true,
                };
                let mut last_error = None;
                for threshold in [PI / 4.0, PI / 3.0, PI / 2.0, PI] {
                    match resample_path(&closed_path, count, threshold) {
                        Ok(points) => {
                            return Ok(Sampled {
                                hole: signed_area(&points) < 0.0,
                                points,
                                closed: true,
                            });
                        }
                        // Dense corners can exhaust the intervals; relaxing the threshold preserves the sample budget.
                        Err(error) => last_error = Some(error),
                    }
                }
                Err(last_error.context("region sampling did not run")?)
                    .context("sampling region contour")
            })
            .collect()
    };
    let samples = sample_at(point_count.unwrap_or(64))?;
    if point_count.is_none()
        && paths
            .iter()
            .zip(&samples)
            .any(|(path, samples)| samples_are_coarse(path, &samples.points))
    {
        sample_at(128)
    } else {
        Ok(samples)
    }
}

fn folds(source: &[Point], target: &[Point]) -> bool {
    let middle: Vec<_> = source
        .iter()
        .zip(target)
        .map(|(&source, &target)| (source + target) / 2.0)
        .collect();
    let count = middle.len();
    for first in 0..count {
        let start = middle[first];
        let direction = middle[(first + 1) % count] - start;
        for second in first + 2..count - usize::from(first == 0) {
            let other_start = middle[second];
            let other_direction = middle[(second + 1) % count] - other_start;
            let first_side = direction.cross(other_start - start);
            let second_side = direction.cross(other_start + other_direction - start);
            if first_side * second_side >= 0.0 {
                continue;
            }
            let third_side = other_direction.cross(start - other_start);
            let fourth_side = other_direction.cross(start + direction - other_start);
            if third_side * fourth_side < 0.0 {
                return true;
            }
        }
    }
    false
}

fn outer_item(source: &[Point], target: &[Point]) -> Result<PlanItem> {
    ensure!(
        !source.is_empty() && source.len() == target.len(),
        "region pair has incompatible samples"
    );
    let stride = if source.len() > 64 { 2 } else { 1 };
    let subsample = |points: &[Point], offset| -> Vec<Point> {
        if stride > 1 {
            points.iter().skip(offset).step_by(2).copied().collect()
        } else {
            points.to_vec()
        }
    };
    let source_samples = subsample(source, 0);
    let source_centroid = centroid(&source_samples)?;
    let target_samples = [subsample(target, 0), subsample(target, 1)];
    let target_centroids = [centroid(&target_samples[0])?, centroid(&target_samples[1])?];
    let mut offsets = Vec::with_capacity(source.len());
    for offset in 0..source.len() {
        let parity = offset % stride;
        let transformed = rotate_points(&target_samples[parity], (offset - parity) / stride);
        let similarity = procrustes(
            &source_samples,
            &transformed,
            source_centroid,
            target_centroids[parity],
        )?;
        offsets.push((
            offset,
            similarity.residual + 0.3 * similarity.theta.abs() / PI,
        ));
    }
    offsets.sort_by(|left, right| left.1.total_cmp(&right.1));
    let best_score = offsets
        .first()
        .context("region pair has no candidate alignment")?
        .1;
    let mut best = None;
    for &(offset, score) in offsets.iter().take(16) {
        if score > best_score + 0.1 {
            break;
        }
        // Fixing the cyclic offset disables another alignment search while preserving winding.
        let mut item = make_item(
            source,
            &rotate_points(target, offset),
            false,
            false,
            false,
            0.3,
        )?;
        item.closed = true;
        if !folds(
            &subsample(&item.source_centered, 0),
            &subsample(&item.target_transformed, 0),
        ) {
            return Ok(item);
        }
        if best.is_none() {
            best = Some(item);
        }
    }
    best.context("region pair has no alignment")
}

fn parent_of(hole: &[Point], contours: &[Sampled]) -> Option<usize> {
    if hole.is_empty() {
        return None;
    }
    let mut parent = None;
    let mut parent_area = f64::INFINITY;
    for (index, contour) in contours.iter().enumerate() {
        if contour.hole {
            continue;
        }
        // A hole's centroid can lie inside a separate island, such as a camera lens.
        let votes = [0, hole.len() / 3, 2 * hole.len() / 3]
            .into_iter()
            .filter(|&vertex| inside(hole[vertex], &contour.points))
            .count();
        if votes < 2 {
            continue;
        }
        let area = signed_area(&contour.points).abs();
        if area < parent_area {
            parent = Some(index);
            parent_area = area;
        }
    }
    parent
}

struct AssignmentSearch<'a> {
    costs: &'a [Vec<f64>],
    skip: &'a [f64],
    one: &'a [bool],
    cover: bool,
    assignment: Vec<Option<usize>>,
    multiplicity: Vec<usize>,
    best: Vec<Option<usize>>,
    best_cost: f64,
}

impl AssignmentSearch<'_> {
    fn visit(&mut self, row: usize, accumulated: f64, covered: usize) {
        if accumulated >= self.best_cost
            || (self.cover && self.one.len() - covered > self.costs.len() - row)
        {
            return;
        }
        if row == self.costs.len() {
            self.best_cost = accumulated;
            self.best.clone_from(&self.assignment);
            return;
        }
        for column in 0..self.one.len() {
            if (self.one[column] && self.multiplicity[column] > 0)
                || !self.costs[row][column].is_finite()
            {
                continue;
            }
            self.assignment[row] = Some(column);
            self.multiplicity[column] += 1;
            self.visit(
                row + 1,
                accumulated + self.costs[row][column],
                covered + usize::from(self.multiplicity[column] == 1),
            );
            self.multiplicity[column] -= 1;
        }
        self.assignment[row] = None;
        self.visit(row + 1, accumulated + self.skip[row], covered);
    }
}

fn assign(
    costs: &[Vec<f64>],
    skip: &[f64],
    one: &[bool],
    cover: bool,
) -> Result<Vec<Option<usize>>> {
    ensure!(
        costs.len() == skip.len(),
        "region assignment has incompatible skip costs"
    );
    ensure!(
        costs.iter().all(|row| row.len() == one.len()),
        "region assignment matrix is not rectangular"
    );
    let mut combinations = 1_usize;
    for _ in costs {
        combinations = combinations.saturating_mul(one.len().saturating_add(1));
        if combinations > SURJECTION_LIMIT {
            break;
        }
    }
    if combinations > SURJECTION_LIMIT {
        let mut assignment = vec![None; costs.len()];
        let mut occupied = vec![false; one.len()];
        let mut pairs = Vec::new();
        for (row, row_costs) in costs.iter().enumerate() {
            for (column, &cost) in row_costs.iter().enumerate() {
                if cost.is_finite() {
                    pairs.push((cost, row, column));
                }
            }
        }
        pairs.sort_by(|left, right| left.0.total_cmp(&right.0));
        for (_, row, column) in pairs {
            if assignment[row].is_none() && !occupied[column] {
                assignment[row] = Some(column);
                occupied[column] = true;
            }
        }
        for (row, choice) in assignment.iter_mut().enumerate() {
            if choice.is_some() {
                continue;
            }
            for column in 0..one.len() {
                if !one[column]
                    && costs[row][column].is_finite()
                    && choice.is_none_or(|previous| costs[row][column] < costs[row][previous])
                {
                    *choice = Some(column);
                }
            }
            if let Some(column) = *choice {
                occupied[column] = true;
            }
        }
        ensure!(
            !cover || occupied.iter().all(|occupied| *occupied),
            "region assignment cannot cover every contour"
        );
        return Ok(assignment);
    }
    let mut search = AssignmentSearch {
        costs,
        skip,
        one,
        cover,
        assignment: vec![None; costs.len()],
        multiplicity: vec![0; one.len()],
        best: vec![None; costs.len()],
        best_cost: f64::INFINITY,
    };
    search.visit(0, 0.0, 0);
    ensure!(
        search.best_cost.is_finite(),
        "region assignment has no feasible solution"
    );
    Ok(search.best)
}

fn carry(point: Point, from: Point, to: Point, angle: f64, scale: f64) -> Point {
    let cosine = angle.cos() * scale;
    let sine = angle.sin() * scale;
    let relative = point - from;
    Point {
        x: to.x + relative.x * cosine - relative.y * sine,
        y: to.y + relative.x * sine + relative.y * cosine,
    }
}

struct Frame {
    source_centroid: Point,
    target_centroid: Point,
    theta: f64,
    log_sigma: f64,
    block: Option<BlockTransport>,
}

impl From<&PlanItem> for Frame {
    fn from(item: &PlanItem) -> Self {
        Self {
            source_centroid: item.source_centroid,
            target_centroid: item.target_centroid,
            theta: item.theta,
            log_sigma: item.log_sigma,
            block: item.block,
        }
    }
}

fn in_frame(
    source: &[Point],
    target: &[Point],
    frame: &Frame,
    hole: bool,
    half: Option<u8>,
) -> Result<PlanItem> {
    ensure!(
        !source.is_empty() && source.len() == target.len(),
        "region frame has incompatible samples"
    );
    let scale = frame.log_sigma.exp();
    ensure!(
        scale.is_finite() && scale > 0.0,
        "region frame has an invalid scale"
    );
    let origin = Point { x: 0.0, y: 0.0 };
    let source_centered: Vec<_> = source
        .iter()
        .map(|&point| point - frame.source_centroid)
        .collect();
    let transformed: Vec<_> = target
        .iter()
        .map(|&point| {
            carry(
                point,
                frame.target_centroid,
                origin,
                -frame.theta,
                1.0 / scale,
            )
        })
        .collect();
    let target_norm: f64 = target
        .iter()
        .map(|&point| {
            let relative = point - frame.target_centroid;
            relative.dot(relative)
        })
        .sum();
    let mut best_offset = 0;
    let mut best_error = f64::INFINITY;
    for offset in 0..source.len() {
        if best_error <= 0.0 {
            break;
        }
        let mut error = 0.0;
        for (index, &source_point) in source_centered.iter().enumerate() {
            let difference = source_point - transformed[(index + offset) % transformed.len()];
            error += difference.dot(difference);
            if error >= best_error {
                break;
            }
        }
        if error < best_error {
            best_error = error;
            best_offset = offset;
        }
    }
    Ok(PlanItem {
        source: source.to_vec(),
        source_centered,
        target_transformed: rotate_points(&transformed, best_offset),
        target_oriented: rotate_points(target, best_offset),
        source_centroid: frame.source_centroid,
        target_centroid: frame.target_centroid,
        theta: frame.theta,
        log_sigma: frame.log_sigma,
        residual: if target_norm > 1e-12 {
            (scale * scale * best_error / target_norm).sqrt()
        } else {
            0.0
        },
        closed: true,
        hole,
        half,
        block: frame.block,
    })
}

fn vanishing_hole(points: &[Point], outer: &PlanItem, grow: bool) -> Result<PlanItem> {
    let scale = outer.log_sigma.exp();
    let mut center = centroid(points)?;
    let nearby = if grow {
        &outer.target_oriented
    } else {
        &outer.source
    };
    ensure!(
        nearby.len() == outer.source_centered.len()
            && nearby.len() == outer.target_transformed.len(),
        "region outer frame has incompatible samples"
    );
    let mut total_weight = 0.0;
    let mut displacement = Point { x: 0.0, y: 0.0 };
    for ((&near, &source), &target) in nearby
        .iter()
        .zip(&outer.source_centered)
        .zip(&outer.target_transformed)
    {
        let relative = near - center;
        let weight = 1.0 / (relative.dot(relative) + 1e-9);
        total_weight += weight;
        displacement += (target - source) * weight;
    }
    ensure!(
        total_weight.is_finite() && total_weight > 0.0,
        "region hole has no deformation weights"
    );
    if grow {
        center = carry(
            center,
            outer.target_centroid,
            outer.source_centroid,
            -outer.theta,
            1.0 / scale,
        );
    }
    center += displacement * (if grow { -0.5 } else { 0.5 } / total_weight);
    let far = if grow {
        center
    } else {
        carry(
            center,
            outer.source_centroid,
            outer.target_centroid,
            outer.theta,
            scale,
        )
    };
    let dots = vec![far; points.len()];
    let frame = Frame::from(outer);
    if grow {
        in_frame(&dots, points, &frame, true, Some(1))
    } else {
        in_frame(points, &dots, &frame, true, Some(0))
    }
}

fn blend_weight(item: &PlanItem, progress: f64) -> f64 {
    (2.0 * progress - item.half.map_or(progress, f64::from)).clamp(0.0, 1.0)
}

fn residual_blend(item: &PlanItem, progress: f64) -> Vec<Point> {
    let weight = blend_weight(item, progress);
    item.source_centered
        .iter()
        .zip(&item.target_transformed)
        .map(|(&source, &target)| source.lerp(target, weight))
        .collect()
}

fn strays(index: usize, outer: &PlanItem, holes: &[PlanItem]) -> Result<usize> {
    let hole = holes.get(index).context("region hole index is invalid")?;
    let mut count = 0;
    for progress in [0.25, 0.5, 0.75] {
        if blend_weight(hole, progress) % 1.0 == 0.0 {
            continue;
        }
        let hole_points = residual_blend(hole, progress);
        let outer_points = residual_blend(outer, progress);
        let neighbors: Vec<_> = holes
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, item)| residual_blend(item, progress))
            .collect();
        for point in hole_points {
            if !inside(point, &outer_points)
                || neighbors.iter().any(|neighbor| inside(point, neighbor))
            {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn vanishing_piece(outer: &[Point], holes: &[Vec<Point>], grow: bool) -> Result<Vec<PlanItem>> {
    let center = centroid(outer)?;
    let frame = Frame {
        source_centroid: center,
        target_centroid: center,
        theta: 0.0,
        log_sigma: 0.0,
        block: None,
    };
    let dots = vec![center; outer.len()];
    std::iter::once(outer)
        .chain(holes.iter().map(Vec::as_slice))
        .enumerate()
        .map(|(index, points)| {
            if grow {
                in_frame(&dots, points, &frame, index > 0, None)
            } else {
                in_frame(points, &dots, &frame, index > 0, None)
            }
        })
        .collect()
}

fn vanish_cost(points: &[Point]) -> Result<f64> {
    let center = centroid(points)?;
    let costs = cost_matrix(&[points.to_vec()], &[vec![center; points.len()]])?;
    costs
        .first()
        .and_then(|row| row.first())
        .copied()
        .context("missing region collapse cost")
}

fn bounds(points: &[Point]) -> Result<[f64; 4]> {
    let first = points.first().context("region contour has no bounds")?;
    let mut bounds = [first.x, first.y, first.x, first.y];
    for point in &points[1..] {
        bounds[0] = bounds[0].min(point.x);
        bounds[1] = bounds[1].min(point.y);
        bounds[2] = bounds[2].max(point.x);
        bounds[3] = bounds[3].max(point.y);
    }
    Ok(bounds)
}

fn hole_costs(
    source: &[Vec<Point>],
    target: &[Vec<Point>],
    outer: &PlanItem,
) -> Result<Vec<Vec<f64>>> {
    let scale = outer.log_sigma.exp();
    let mut costs = cost_matrix(source, target)?;
    let target_bounds = target
        .iter()
        .map(|hole| {
            let transformed: Vec<_> = hole
                .iter()
                .map(|&point| {
                    carry(
                        point,
                        outer.target_centroid,
                        outer.source_centroid,
                        -outer.theta,
                        1.0 / scale,
                    )
                })
                .collect();
            bounds(&transformed)
        })
        .collect::<Result<Vec<_>>>()?;
    for (row, hole) in source.iter().enumerate() {
        let source_bounds = bounds(hole)?;
        for (column, target_bounds) in target_bounds.iter().enumerate() {
            let width =
                source_bounds[2].min(target_bounds[2]) - source_bounds[0].max(target_bounds[0]);
            let height =
                source_bounds[3].min(target_bounds[3]) - source_bounds[1].max(target_bounds[1]);
            let intersection = if width > 0.0 && height > 0.0 {
                width * height
            } else {
                0.0
            };
            let union = (source_bounds[2] - source_bounds[0])
                * (source_bounds[3] - source_bounds[1])
                + (target_bounds[2] - target_bounds[0]) * (target_bounds[3] - target_bounds[1])
                - intersection;
            if intersection < 0.25 * union {
                costs[row][column] = f64::INFINITY;
            }
        }
    }
    Ok(costs)
}

struct Pieces {
    outers: Vec<Vec<Point>>,
    holes: Vec<Vec<Vec<Point>>>,
}

fn pieces_of(contours: &[Sampled]) -> Result<Pieces> {
    let outer_indices: Vec<_> = contours
        .iter()
        .enumerate()
        .filter_map(|(index, contour)| (!contour.hole).then_some(index))
        .collect();
    ensure!(!outer_indices.is_empty(), "region has no outer contour");
    let mut holes = vec![Vec::new(); outer_indices.len()];
    for contour in contours {
        if !contour.hole {
            continue;
        }
        let parent = parent_of(&contour.points, contours);
        let mut outer =
            parent.and_then(|parent| outer_indices.iter().position(|&index| index == parent));
        if outer.is_none() {
            let center = centroid(&contour.points)?;
            let mut best_distance = f64::INFINITY;
            for (index, &outer_index) in outer_indices.iter().enumerate() {
                let distance = (centroid(&contours[outer_index].points)? - center).length();
                if distance < best_distance {
                    outer = Some(index);
                    best_distance = distance;
                }
            }
        }
        let outer = outer.context("region hole has no parent contour")?;
        holes[outer].push(contour.points.clone());
    }
    Ok(Pieces {
        outers: outer_indices
            .into_iter()
            .map(|index| contours[index].points.clone())
            .collect(),
        holes,
    })
}

fn fit_samples(contours: &[Sampled], point_count: usize) -> Result<Vec<Sampled>> {
    contours
        .iter()
        .map(|contour| {
            ensure!(contour.closed, "region contours must be closed");
            ensure!(
                contour.points.iter().all(|point| point.is_finite()),
                "region samples contain non-finite coordinates"
            );
            if contour.points.len() == point_count {
                return Ok(contour.clone());
            }
            ensure!(
                contour.points.len().checked_mul(2) == Some(point_count),
                "region contours must have equal or doubled sample counts"
            );
            let mut points = Vec::with_capacity(point_count);
            for index in 0..contour.points.len() {
                let start = contour.points[index];
                let end = contour.points[(index + 1) % contour.points.len()];
                points.extend([start, (start + end) / 2.0]);
            }
            Ok(Sampled {
                points,
                closed: true,
                hole: contour.hole,
            })
        })
        .collect()
}

pub fn build_region_plan(source: &[Sampled], target: &[Sampled]) -> Result<MorphPlan> {
    if source.is_empty() && target.is_empty() {
        return Ok(MorphPlan {
            items: Vec::new(),
            point_count: 64,
        });
    }
    let source_count = source.first().map_or(0, |contour| contour.points.len());
    let target_count = target.first().map_or(0, |contour| contour.points.len());
    let point_count = source_count.max(target_count);
    ensure!(
        point_count >= 2 && (point_count <= 64 || point_count.is_multiple_of(2)),
        "region plan has an unsupported sample count"
    );
    if source.is_empty() || target.is_empty() {
        // Zero-ink font glyphs keep their identity; their whole pieces grow or collapse together.
        let grow = source.is_empty();
        let pieces = pieces_of(&fit_samples(
            if grow { target } else { source },
            point_count,
        )?)?;
        let mut items = Vec::new();
        for (outer, holes) in pieces.outers.iter().zip(&pieces.holes) {
            items.extend(vanishing_piece(outer, holes, grow)?);
        }
        return Ok(MorphPlan { items, point_count });
    }
    let source = pieces_of(&fit_samples(source, point_count)?)?;
    let target = pieces_of(&fit_samples(target, point_count)?)?;
    let source_outer_count = source.outers.len();
    let target_outer_count = target.outers.len();
    let (large, small) = if source_outer_count < target_outer_count {
        (&target, &source)
    } else {
        (&source, &target)
    };
    let pairs: Vec<_> =
        if source_outer_count == target_outer_count || small.holes.iter().all(Vec::is_empty) {
            pair_up(&source.outers, &target.outers)?
                .into_iter()
                .map(|(source, target)| (Some(source), Some(target)))
                .collect()
        } else {
            // Duplicating an outer with holes would sum its winding to +2 and fill a -1 hole.
            let costs = cost_matrix(&large.outers, &small.outers)?;
            let skip = large
                .outers
                .iter()
                .map(|outer| Ok(1e6 + vanish_cost(outer)?))
                .collect::<Result<Vec<_>>>()?;
            let one: Vec<_> = small.holes.iter().map(|holes| !holes.is_empty()).collect();
            assign(&costs, &skip, &one, true)?
                .into_iter()
                .enumerate()
                .map(|(large, small)| {
                    if source_outer_count < target_outer_count {
                        (small, Some(large))
                    } else {
                        (Some(large), small)
                    }
                })
                .collect()
        };
    let mut items = Vec::new();
    let mut partial = false;
    for (source_index, target_index) in pairs {
        let (source_index, target_index) = match (source_index, target_index) {
            (Some(source_index), Some(target_index)) => (source_index, target_index),
            (None, Some(index)) => {
                partial = true;
                items.extend(vanishing_piece(
                    &target.outers[index],
                    &target.holes[index],
                    true,
                )?);
                continue;
            }
            (Some(index), None) => {
                partial = true;
                items.extend(vanishing_piece(
                    &source.outers[index],
                    &source.holes[index],
                    false,
                )?);
                continue;
            }
            (None, None) => bail!("region pair has no source or target"),
        };
        let mut outer = outer_item(&source.outers[source_index], &target.outers[target_index])?;
        outer.hole = false;
        let source_holes = &source.holes[source_index];
        let target_holes = &target.holes[target_index];
        let costs = hole_costs(source_holes, target_holes, &outer)?;
        let assignments = assign(
            &costs,
            &vec![1e6; source_holes.len()],
            &vec![true; target_holes.len()],
            false,
        )?;
        let frame = Frame::from(&outer);
        let mut holes = Vec::new();
        for (hole, assignment) in source_holes.iter().zip(&assignments) {
            holes.push(match assignment {
                Some(index) => in_frame(hole, &target_holes[*index], &frame, true, None)?,
                None => vanishing_hole(hole, &outer, false)?,
            });
        }
        for (index, hole) in target_holes.iter().enumerate() {
            if !assignments.contains(&Some(index)) {
                holes.push(vanishing_hole(hole, &outer, true)?);
            }
        }
        let mut index = 0;
        while index < holes.len() {
            if holes[index].half.is_some() {
                partial = true;
                index += 1;
                continue;
            }
            let stray_count = strays(index, &outer, &holes)?;
            if stray_count == 0 {
                index += 1;
                continue;
            }
            let original = holes.remove(index);
            let closing = vanishing_hole(&original.source, &outer, false)?;
            let opening = vanishing_hole(&original.target_oriented, &outer, true)?;
            holes.insert(index, closing);
            holes.insert(index + 1, opening);
            if strays(index, &outer, &holes)? + strays(index + 1, &outer, &holes)? >= stray_count {
                holes.remove(index + 1);
                holes[index] = original;
                index += 1;
            } else {
                partial = true;
                index += 2;
            }
        }
        items.push(outer);
        items.extend(holes);
    }
    if items.len() > 1 && !partial {
        apply_global(&mut items, point_count)?;
    }
    Ok(MorphPlan { items, point_count })
}
