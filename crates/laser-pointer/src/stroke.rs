//! The laser trail model: where the trail is, how wide it is at each sample, and when
//! it disappears.
//!
//! It mirrors `@excalidraw/laser-pointer` as excalidraw's laser trails use it:
//!
//! * incoming points are smoothed towards the previous point by `streamline`
//!   (`lerp(previous, raw, 1 - streamline)`),
//! * every sample carries its own timestamp,
//! * the half width at a sample is
//!   `size / 2 * min(ease_out(length_term), ease_out(time_term))`, where the length
//!   term tapers the trail over the trailing `decay_length` samples and the time term
//!   shrinks each sample away over `decay_time_ms`.
//!
//! The result is a comet-shaped stroke that is widest at the head and burns away from
//! the tail.

use std::collections::VecDeque;
use std::f32::consts::PI;

/// Number of segments used to approximate each end cap semicircle. Eight is enough to
/// look round at the widths a laser pointer uses without making the per-frame vertex
/// count grow noticeably.
const CAP_SEGMENTS: usize = 8;

/// A sample can never outlive `decay_time_ms`, so the trail is naturally bounded; this
/// only guards against a stalled renderer.
const MAX_SAMPLES: usize = 4096;

/// Half widths at or below this are too thin to draw.
const MIN_VISIBLE_RADIUS: f32 = 0.05;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokeOptions {
    /// Full width of the stroke at the head, in device independent pixels.
    pub size: f32,
    /// Smoothing factor from 0 to 0.95. Higher is smoother but lags further behind.
    pub streamline: f32,
    /// How long one sample takes to shrink to nothing.
    pub decay_time_ms: u32,
    /// How many trailing samples the taper is spread over.
    pub decay_length: u32,
    /// Samples closer than this to the previous one are dropped, so a pointer that is
    /// standing still does not pile up samples on one spot.
    pub min_distance: f32,
}

impl Default for StrokeOptions {
    fn default() -> Self {
        Self {
            size: 10.0,
            streamline: 0.4,
            decay_time_ms: 1000,
            decay_length: 50,
            min_distance: 0.75,
        }
    }
}

impl StrokeOptions {
    /// The options with every value brought into the range the model can work with.
    fn sanitised(self) -> Self {
        Self {
            size: self.size.max(1.0),
            streamline: self.streamline.clamp(0.0, 0.95),
            decay_time_ms: self.decay_time_ms.max(1),
            decay_length: self.decay_length.max(1),
            min_distance: self.min_distance.max(0.0),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    x: f32,
    y: f32,
    /// When the sample was added, which is what its decay is measured from.
    time_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct LaserStroke {
    options: StrokeOptions,
    points: VecDeque<Sample>,
    finished: bool,
    // Reused by `build_outline`, which runs several times per frame, so that drawing
    // does not allocate.
    radii: Vec<f32>,
    normals: Vec<Vec2>,
}

impl LaserStroke {
    pub fn new(options: StrokeOptions) -> Self {
        Self {
            options: options.sanitised(),
            ..Self::default()
        }
    }

    pub fn set_options(&mut self, options: StrokeOptions) {
        self.options = options.sanitised();
    }

    pub fn options(&self) -> &StrokeOptions {
        &self.options
    }

    /// Feeds a raw pointer position. `time_ms` is when the pointer was there.
    pub fn add_point(&mut self, x: f32, y: f32, time_ms: u64) {
        if self.finished {
            return;
        }
        let Some(last) = self.points.back() else {
            self.points.push_back(Sample { x, y, time_ms });
            return;
        };

        // Streamline: pull the incoming position back towards the previous sample.
        let pull = 1.0 - self.options.streamline;
        let dx = (x - last.x) * pull;
        let dy = (y - last.y) * pull;
        if dx * dx + dy * dy < self.options.min_distance * self.options.min_distance {
            return;
        }

        let sample = Sample {
            x: last.x + dx,
            y: last.y + dy,
            time_ms,
        };
        self.points.push_back(sample);
        while self.points.len() > MAX_SAMPLES {
            self.points.pop_front();
        }
    }

    /// Drops samples that have fully decayed. Returns true while the stroke still has
    /// something left to draw.
    pub fn prune(&mut self, now_ms: u64) -> bool {
        let lifetime = u64::from(self.options.decay_time_ms);
        while let Some(oldest) = self.points.front() {
            if now_ms.saturating_sub(oldest.time_ms) < lifetime {
                break;
            }
            self.points.pop_front();
        }
        !self.points.is_empty()
    }

    /// Empties the stroke and lets it accept points again.
    pub fn clear(&mut self) {
        self.points.clear();
        self.finished = false;
    }

    /// Stops the stroke accepting points. It keeps decaying until it is empty.
    pub fn finish(&mut self) {
        self.finished = true;
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    /// Half width of the sample at `index`, in device independent pixels. Samples are
    /// numbered from the tail; an index past the head gives zero.
    pub fn half_width_at(&self, index: usize, now_ms: u64) -> f32 {
        let Some(sample) = self.points.get(index) else {
            return 0.0;
        };

        let age = now_ms.saturating_sub(sample.time_ms);
        let time_term = (1.0 - age as f32 / self.options.decay_time_ms as f32).clamp(0.0, 1.0);

        let from_head = (self.points.len() - 1 - index) as f32;
        let decay_length = self.options.decay_length as f32;
        let length_term =
            ((decay_length - from_head.min(decay_length)) / decay_length).clamp(0.0, 1.0);

        self.options.size * 0.5 * ease_out(length_term).min(ease_out(time_term))
    }

    /// Builds the closed outline polygon of the trail at `now_ms` into `outline`,
    /// which is cleared first. `width_scale` scales every half width, which is how a
    /// renderer gets a wider glow pass and a narrower bright core pass out of the same
    /// centre line. Returns the number of vertices produced; zero means there is
    /// nothing visible to draw.
    pub fn build_outline(
        &mut self,
        now_ms: u64,
        outline: &mut Vec<Vec2>,
        width_scale: f32,
    ) -> usize {
        outline.clear();

        let count = self.points.len();
        if count == 0 {
            return 0;
        }
        if count == 1 {
            // A pointer that has not moved yet still shows the head dot.
            let radius = self.half_width_at(0, now_ms) * width_scale;
            if radius <= MIN_VISIBLE_RADIUS {
                return 0;
            }
            let centre = self.points[0];
            let segments = CAP_SEGMENTS * 2;
            outline.extend((0..segments).map(|step| {
                let angle = 2.0 * PI * step as f32 / segments as f32;
                Vec2 {
                    x: centre.x + angle.cos() * radius,
                    y: centre.y + angle.sin() * radius,
                }
            }));
            return outline.len();
        }

        // Half width and unit normal for every sample. The direction at an interior
        // sample is the central difference of its neighbours, which keeps the offset
        // curves smooth through corners; the ends use the adjacent segment.
        let mut radii = std::mem::take(&mut self.radii);
        let mut normals = std::mem::take(&mut self.normals);
        radii.clear();
        normals.clear();

        let mut last_normal = None;
        for index in 0..count {
            radii.push(self.half_width_at(index, now_ms) * width_scale);

            let previous = self.points[index.saturating_sub(1)];
            let next = self.points[(index + 1).min(count - 1)];
            let dx = next.x - previous.x;
            let dy = next.y - previous.y;
            let length = (dx * dx + dy * dy).sqrt();

            let normal = if length < 1e-4 {
                // The neighbours coincide: reuse the previous normal rather than a
                // degenerate one that would pinch the outline.
                last_normal.unwrap_or(Vec2 { x: 0.0, y: 1.0 })
            } else {
                let normal = Vec2 {
                    x: -dy / length,
                    y: dx / length,
                };
                last_normal = Some(normal);
                normal
            };
            normals.push(normal);
        }

        if radii.iter().copied().fold(0.0, f32::max) > MIN_VISIBLE_RADIUS {
            outline.reserve(count * 2 + CAP_SEGMENTS * 2 + 2);
            let offset = |index: usize, side: f32| Vec2 {
                x: self.points[index].x + normals[index].x * radii[index] * side,
                y: self.points[index].y + normals[index].y * radii[index] * side,
            };

            // Left side from tail to head, round the head, right side back to the
            // tail, round the tail. The tail cap is usually collapsed to a point by
            // the taper, but a stroke that has just started has a real width there.
            outline.extend((0..count).map(|index| offset(index, 1.0)));
            let head = count - 1;
            push_cap(outline, self.points[head], normals[head], radii[head]);
            outline.extend((0..count).rev().map(|index| offset(index, -1.0)));
            let tail_normal = Vec2 {
                x: -normals[0].x,
                y: -normals[0].y,
            };
            push_cap(outline, self.points[0], tail_normal, radii[0]);
        }

        self.radii = radii;
        self.normals = normals;
        outline.len()
    }
}

/// Adds the inner vertices of a semicircle around `centre`, starting from the side
/// `normal` points to and turning away from the stroke. The two ends of the semicircle
/// are the side vertices the caller adds itself.
fn push_cap(outline: &mut Vec<Vec2>, centre: Sample, normal: Vec2, radius: f32) {
    if radius <= MIN_VISIBLE_RADIUS {
        return;
    }
    let start = normal.y.atan2(normal.x);
    outline.extend((1..CAP_SEGMENTS).map(|step| {
        let angle = start - PI * step as f32 / CAP_SEGMENTS as f32;
        Vec2 {
            x: centre.x + angle.cos() * radius,
            y: centre.y + angle.sin() * radius,
        }
    }));
}

/// Cubic ease-out, the easing excalidraw applies to both decay terms.
fn ease_out(t: f32) -> f32 {
    let inverse = 1.0 - t;
    1.0 - inverse * inverse * inverse
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No smoothing and no minimum distance, so the geometry each test asks about
    /// follows exactly from the points it fed in.
    const EXACT: StrokeOptions = StrokeOptions {
        size: 10.0,
        streamline: 0.0,
        decay_time_ms: 1000,
        decay_length: 50,
        min_distance: 0.0,
    };

    /// Area enclosed by the polygon; positive only if it is closed and not degenerate.
    fn area(polygon: &[Vec2]) -> f32 {
        let doubled: f64 = (0..polygon.len())
            .map(|index| {
                let a = polygon[index];
                let b = polygon[(index + 1) % polygon.len()];
                f64::from(a.x) * f64::from(b.y) - f64::from(b.x) * f64::from(a.y)
            })
            .sum();
        (doubled.abs() * 0.5) as f32
    }

    /// A stroke going right from (100, 200), one sample every 8 ms.
    fn horizontal_stroke(start_ms: u64, samples: u64, step: f32) -> LaserStroke {
        let mut stroke = LaserStroke::new(EXACT);
        for index in 0..samples {
            stroke.add_point(100.0 + step * index as f32, 200.0, start_ms + index * 8);
        }
        stroke
    }

    fn outline_of(stroke: &mut LaserStroke, now_ms: u64, width_scale: f32) -> Vec<Vec2> {
        let mut outline = Vec::new();
        stroke.build_outline(now_ms, &mut outline, width_scale);
        outline
    }

    #[test]
    fn empty_stroke_produces_no_outline() {
        let mut stroke = LaserStroke::new(EXACT);
        let mut outline = vec![Vec2::default()];

        assert_eq!(stroke.build_outline(1000, &mut outline, 1.0), 0);
        assert!(outline.is_empty());
        assert!(stroke.is_empty());
    }

    #[test]
    fn single_point_renders_as_a_round_dot() {
        let mut stroke = LaserStroke::new(EXACT);
        stroke.add_point(50.0, 50.0, 1000);

        let outline = outline_of(&mut stroke, 1000, 1.0);

        assert!(outline.len() >= 8);
        // The head is `size` wide, so the dot has radius 5 and an area near 78.5.
        let area = area(&outline);
        assert!(area > 60.0 && area < 85.0, "area was {area}");
    }

    #[test]
    fn width_tapers_from_head_to_tail() {
        let mut stroke = horizontal_stroke(1000, 80, 3.0);
        let now = 1000 + 79 * 8;
        let count = stroke.point_count();

        let head = stroke.half_width_at(count - 1, now);
        let middle = stroke.half_width_at(count - 25, now);
        let tail = stroke.half_width_at(0, now);

        assert!((head - 5.0).abs() < 0.01, "head half width was {head}");
        assert!(
            middle > 0.0 && middle < head,
            "middle half width was {middle}"
        );
        assert_eq!(tail, 0.0);
        assert_eq!(stroke.half_width_at(count, now), 0.0);

        let outline = outline_of(&mut stroke, now, 1.0);
        assert!(outline.len() > 3);
        assert!(area(&outline) > 0.0);
    }

    #[test]
    fn samples_narrow_with_age_and_are_pruned_away() {
        let mut stroke = horizontal_stroke(1000, 40, 3.0);
        let fresh = 1000 + 39 * 8;
        let head = stroke.point_count() - 1;

        assert!(stroke.half_width_at(head, fresh + 700) < stroke.half_width_at(head, fresh));
        assert_eq!(stroke.half_width_at(head, fresh + 5000), 0.0);

        assert!(stroke.prune(fresh));
        assert_eq!(stroke.point_count(), 40);
        assert!(!stroke.prune(fresh + 5000));
        assert!(stroke.is_empty());
    }

    #[test]
    fn pruning_removes_only_the_samples_that_have_decayed() {
        let mut stroke = horizontal_stroke(1000, 40, 3.0);

        // Samples were added at 1000, 1008, ...; at 2016 the first three are a full
        // decay time old.
        assert!(stroke.prune(2016));

        assert_eq!(stroke.point_count(), 37);
    }

    #[test]
    fn width_scale_drives_glow_and_core_passes() {
        let mut stroke = horizontal_stroke(1000, 40, 3.0);
        let now = 1000 + 39 * 8;

        let body = area(&outline_of(&mut stroke, now, 1.0));
        let glow = area(&outline_of(&mut stroke, now, 2.4));
        let core = area(&outline_of(&mut stroke, now, 0.38));

        assert!(
            glow > body && body > core,
            "glow {glow}, body {body}, core {core}"
        );
    }

    #[test]
    fn streamline_smooths_towards_the_previous_point() {
        let mut stroke = LaserStroke::new(StrokeOptions {
            streamline: 0.5,
            ..EXACT
        });
        stroke.add_point(0.0, 0.0, 1000);
        stroke.add_point(100.0, 0.0, 1008);

        let outline = outline_of(&mut stroke, 1008, 1.0);

        // With streamline 0.5 the second sample lands halfway to the raw position, so
        // the outline reaches the head cap around x = 50 and no further than x = 55.
        assert_eq!(stroke.point_count(), 2);
        let furthest = outline.iter().map(|point| point.x).fold(f32::MIN, f32::max);
        assert!(
            (furthest - 55.0).abs() < 0.5,
            "outline reached x = {furthest}"
        );
    }

    #[test]
    fn stationary_pointer_does_not_pile_up_samples() {
        let mut stroke = LaserStroke::new(StrokeOptions {
            min_distance: 2.0,
            ..EXACT
        });
        stroke.add_point(10.0, 10.0, 1000);
        for index in 0..20 {
            stroke.add_point(10.0, 10.0, 1000 + index);
        }

        assert_eq!(stroke.point_count(), 1);
    }

    #[test]
    fn finish_stops_accepting_points_and_clear_resets() {
        let mut stroke = horizontal_stroke(1000, 10, 3.0);

        stroke.finish();
        stroke.add_point(999.0, 999.0, 1100);
        assert_eq!(stroke.point_count(), 10);
        assert!(stroke.is_finished());

        stroke.clear();
        assert!(!stroke.is_finished());
        assert!(stroke.is_empty());

        stroke.add_point(1.0, 1.0, 1200);
        assert_eq!(stroke.point_count(), 1);
    }

    #[test]
    fn doubling_back_still_produces_a_sane_outline() {
        let mut stroke = LaserStroke::new(EXACT);
        for index in 0..30u64 {
            stroke.add_point(100.0 + 4.0 * index as f32, 200.0, 1000 + index * 8);
        }
        for index in 0..30u64 {
            stroke.add_point(216.0 - 4.0 * index as f32, 200.0, 1240 + index * 8);
        }

        let outline = outline_of(&mut stroke, 1240 + 29 * 8, 1.0);

        assert!(outline.len() > 3);
        assert!(
            outline
                .iter()
                .all(|point| point.x.is_finite() && point.y.is_finite())
        );
    }

    #[test]
    fn options_are_brought_into_range() {
        let stroke = LaserStroke::new(StrokeOptions {
            size: 0.0,
            streamline: 3.0,
            decay_time_ms: 0,
            decay_length: 0,
            min_distance: -1.0,
        });

        assert_eq!(
            *stroke.options(),
            StrokeOptions {
                size: 1.0,
                streamline: 0.95,
                decay_time_ms: 1,
                decay_length: 1,
                min_distance: 0.0,
            }
        );
    }

    #[test]
    fn the_trail_never_holds_more_than_the_sample_limit() {
        let mut stroke = LaserStroke::new(EXACT);
        for index in 0..(MAX_SAMPLES as u64 + 100) {
            stroke.add_point(index as f32, 0.0, 1000);
        }

        assert_eq!(stroke.point_count(), MAX_SAMPLES);
    }
}
