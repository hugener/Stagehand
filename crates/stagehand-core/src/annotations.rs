use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// A filled shape a tool has drawn on the screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    /// The closed outline, as `(x, y)` in screen pixels.
    pub outline: Vec<(f32, f32)>,
    /// Red, green, blue and opacity, each from 0 to 1.
    pub color: [f32; 4],
}

#[derive(Default)]
struct Inner {
    watched: AtomicBool,
    shapes: Mutex<Vec<Shape>>,
}

/// What the tools are drawing on the screen right now.
///
/// A tool that draws on an overlay is invisible to anyone who is shown a single window
/// rather than the whole screen. This is how such a tool lets another one repeat its
/// drawing somewhere viewers can see it: the drawing tool publishes its shapes here on
/// every frame, and the other reads them. Clones share the same drawing.
#[derive(Clone, Default)]
pub struct Annotations(Arc<Inner>);

impl Annotations {
    pub fn new() -> Self {
        Self::default()
    }

    /// Says whether anything is reading the shapes. Nothing needs publishing while
    /// nothing is, which is nearly always.
    pub fn set_watched(&self, watched: bool) {
        self.0.watched.store(watched, Ordering::Relaxed);
    }

    pub fn is_watched(&self) -> bool {
        self.0.watched.load(Ordering::Relaxed)
    }

    /// Replaces what is drawn with `shapes`, and leaves the previous drawing in
    /// `shapes` so that its storage can be used again.
    pub fn publish(&self, shapes: &mut Vec<Shape>) {
        std::mem::swap(&mut *self.lock(), shapes);
    }

    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Hands the current drawing to `reader`. Publishing waits while it runs, so it
    /// should not take long.
    pub fn read<R>(&self, reader: impl FnOnce(&[Shape]) -> R) -> R {
        reader(&self.lock())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Shape>> {
        self.0.shapes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle() -> Shape {
        Shape {
            outline: vec![(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)],
            color: [1.0, 0.0, 0.0, 0.8],
        }
    }

    #[test]
    fn clones_share_one_drawing() {
        let drawn_by = Annotations::new();
        let read_by = drawn_by.clone();

        drawn_by.publish(&mut vec![triangle()]);

        assert_eq!(read_by.read(<[Shape]>::to_vec), vec![triangle()]);
    }

    #[test]
    fn publishing_hands_back_the_previous_drawing() {
        let annotations = Annotations::new();
        annotations.publish(&mut vec![triangle()]);

        let mut next = Vec::new();
        annotations.publish(&mut next);

        assert_eq!(next, vec![triangle()]);
        assert!(annotations.read(<[Shape]>::is_empty));
    }

    #[test]
    fn nothing_is_watching_until_told_so() {
        let annotations = Annotations::new();
        assert!(!annotations.is_watched());

        annotations.clone().set_watched(true);
        assert!(annotations.is_watched());

        annotations.publish(&mut vec![triangle()]);
        annotations.clear();
        assert!(annotations.read(<[Shape]>::is_empty));
    }
}
