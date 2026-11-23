//! The edit decision list.
//!
//! A [`Timeline`] is the source duration plus a list of [`Region`]s the user
//! can switch on and off. Cut regions (silences, manual cuts) are removed
//! from the output; censor regions (profanity) stay in but get muted or
//! bleeped. Everything downstream (rendering, captions, XML export) reads
//! the timeline, so toggling a region in the UI is all an edit needs.

use serde::{Deserialize, Serialize};

/// A time range in seconds on the *source* media.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

impl Span {
    pub fn new(start: f64, end: f64) -> Self {
        Self { start: start.min(end), end: start.max(end) }
    }

    pub fn len(&self) -> f64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.len() <= 0.0
    }

    pub fn overlaps(&self, other: &Span) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RegionKind {
    /// A detected pause. Removed from the output.
    Silence,
    /// A range the user marked by hand (or by deleting transcript words). Removed.
    Manual,
    /// A swear word. Kept, but muted or bleeped.
    Profanity,
}

impl RegionKind {
    pub fn is_cut(self) -> bool {
        matches!(self, RegionKind::Silence | RegionKind::Manual)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Region {
    pub id: u32,
    pub kind: RegionKind,
    pub span: Span,
    /// Whether this edit is applied. The UI toggles this.
    pub active: bool,
    /// Optional label shown in the UI (e.g. the censored word).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Timeline {
    pub duration: f64,
    pub regions: Vec<Region>,
}

impl Timeline {
    pub fn new(duration: f64) -> Self {
        Self { duration, regions: Vec::new() }
    }

    fn next_id(&self) -> u32 {
        self.regions.iter().map(|r| r.id + 1).max().unwrap_or(1)
    }

    pub fn add(&mut self, kind: RegionKind, span: Span, label: Option<String>) -> u32 {
        let id = self.next_id();
        let span = Span::new(span.start.max(0.0), span.end.min(self.duration));
        self.regions.push(Region { id, kind, span, active: true, label });
        id
    }

    /// Replace every region of `kind` with freshly detected spans.
    pub fn replace_kind(&mut self, kind: RegionKind, spans: impl IntoIterator<Item = (Span, Option<String>)>) {
        self.regions.retain(|r| r.kind != kind);
        for (span, label) in spans {
            self.add(kind, span, label);
        }
        self.regions.sort_by(|a, b| a.span.start.total_cmp(&b.span.start));
    }

    /// Active cut spans, sorted and merged.
    pub fn cuts(&self) -> Vec<Span> {
        merge(self.regions.iter().filter(|r| r.active && r.kind.is_cut()).map(|r| r.span))
    }

    /// Active censor spans, sorted and merged.
    pub fn censors(&self) -> Vec<Span> {
        merge(self.regions.iter().filter(|r| r.active && r.kind == RegionKind::Profanity).map(|r| r.span))
    }

    /// The source ranges that make it into the output, in order.
    pub fn kept(&self) -> Vec<Span> {
        let mut kept = Vec::new();
        let mut cursor = 0.0;
        for cut in self.cuts() {
            if cut.start > cursor {
                kept.push(Span::new(cursor, cut.start));
            }
            cursor = cursor.max(cut.end);
        }
        if cursor < self.duration {
            kept.push(Span::new(cursor, self.duration));
        }
        // Drop slivers too short to encode as even a single frame.
        kept.retain(|s| s.len() >= 0.02);
        kept
    }

    pub fn output_duration(&self) -> f64 {
        self.kept().iter().map(Span::len).sum()
    }

    /// Where source time `t` lands in the output, or `None` if it was cut.
    pub fn map_time(&self, t: f64) -> Option<f64> {
        map_time_in(&self.kept(), t)
    }
}

/// Map source time to output time given precomputed kept spans.
pub fn map_time_in(kept: &[Span], t: f64) -> Option<f64> {
    let mut offset = 0.0;
    for s in kept {
        if t < s.start {
            return None;
        }
        if t <= s.end {
            return Some(offset + (t - s.start));
        }
        offset += s.len();
    }
    None
}

/// Map a source span to output time, clipping it to the kept parts. Returns
/// `None` if none of it survives.
pub fn map_span_in(kept: &[Span], span: Span) -> Option<Span> {
    let mut offset = 0.0;
    let mut out: Option<Span> = None;
    for s in kept {
        let lo = span.start.max(s.start);
        let hi = span.end.min(s.end);
        if hi > lo {
            let a = offset + (lo - s.start);
            let b = offset + (hi - s.start);
            out = Some(match out {
                None => Span::new(a, b),
                Some(prev) => Span::new(prev.start, b),
            });
        }
        offset += s.len();
    }
    out
}

pub fn merge(spans: impl IntoIterator<Item = Span>) -> Vec<Span> {
    let mut v: Vec<Span> = spans.into_iter().filter(|s| !s.is_empty()).collect();
    v.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Span> = Vec::with_capacity(v.len());
    for s in v {
        match out.last_mut() {
            Some(last) if s.start <= last.end => last.end = last.end.max(s.end),
            _ => out.push(s),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline() -> Timeline {
        let mut t = Timeline::new(10.0);
        t.add(RegionKind::Silence, Span::new(2.0, 3.0), None);
        t.add(RegionKind::Silence, Span::new(5.0, 7.0), None);
        t.add(RegionKind::Profanity, Span::new(8.0, 8.5), Some("damn".into()));
        t
    }

    #[test]
    fn kept_is_complement_of_cuts() {
        let t = timeline();
        assert_eq!(t.kept(), vec![Span::new(0.0, 2.0), Span::new(3.0, 5.0), Span::new(7.0, 10.0)]);
        assert!((t.output_duration() - 7.0).abs() < 1e-9);
    }

    #[test]
    fn inactive_regions_are_ignored() {
        let mut t = timeline();
        t.regions[1].active = false;
        assert_eq!(t.cuts(), vec![Span::new(2.0, 3.0)]);
    }

    #[test]
    fn profanity_is_not_cut() {
        let t = timeline();
        assert_eq!(t.cuts().len(), 2);
        assert_eq!(t.censors(), vec![Span::new(8.0, 8.5)]);
    }

    #[test]
    fn maps_source_time_to_output() {
        let t = timeline();
        assert_eq!(t.map_time(1.0), Some(1.0));
        assert_eq!(t.map_time(2.5), None);
        assert_eq!(t.map_time(4.0), Some(3.0));
        assert_eq!(t.map_time(8.0), Some(5.0));
    }

    #[test]
    fn maps_span_across_a_cut() {
        let t = timeline();
        let kept = t.kept();
        assert_eq!(map_span_in(&kept, Span::new(1.5, 3.5)), Some(Span::new(1.5, 2.5)));
        assert_eq!(map_span_in(&kept, Span::new(5.5, 6.5)), None);
    }

    #[test]
    fn overlapping_cuts_merge() {
        let mut t = Timeline::new(10.0);
        t.add(RegionKind::Silence, Span::new(1.0, 3.0), None);
        t.add(RegionKind::Manual, Span::new(2.0, 4.0), None);
        assert_eq!(t.cuts(), vec![Span::new(1.0, 4.0)]);
    }
}
