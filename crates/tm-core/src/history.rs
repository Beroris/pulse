//! Fixed-capacity history buffers for graphs.
//!
//! A [`History`] never grows past its capacity: pushing into a full buffer overwrites the oldest
//! value. Unknown samples are stored as `NaN` so a graph can draw a gap instead of a false zero.

#[derive(Clone, Debug)]
pub struct History {
    buf: Vec<f32>,
    /// Index the next value is written to.
    head: usize,
    len: usize,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2);
        Self { buf: vec![f32::NAN; capacity], head: 0, len: 0 }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, value: f32) {
        self.buf[self.head] = value;
        self.head = (self.head + 1) % self.buf.len();
        self.len = (self.len + 1).min(self.buf.len());
    }

    pub fn push_opt(&mut self, value: Option<f32>) {
        self.push(value.unwrap_or(f32::NAN));
    }

    /// The newest value, if any and if it was known.
    pub fn last(&self) -> Option<f32> {
        if self.len == 0 {
            return None;
        }
        let i = (self.head + self.buf.len() - 1) % self.buf.len();
        let v = self.buf[i];
        (!v.is_nan()).then_some(v)
    }

    /// Values from oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        let cap = self.buf.len();
        let start = (self.head + cap - self.len) % cap;
        (0..self.len).map(move |i| self.buf[(start + i) % cap])
    }

    /// The newest `n` values, oldest first.
    pub fn tail(&self, n: usize) -> impl Iterator<Item = f32> + '_ {
        let skip = self.len.saturating_sub(n);
        self.iter().skip(skip)
    }

    /// Largest known value, or `None` if nothing known is stored.
    pub fn max(&self) -> Option<f32> {
        self.iter().filter(|v| !v.is_nan()).fold(None, |m, v| Some(m.map_or(v, |m: f32| m.max(v))))
    }

    /// Mean of the newest `n` known values, if at least `min_known` of them are known.
    pub fn mean_tail(&self, n: usize, min_known: usize) -> Option<f32> {
        let (sum, count) = self.tail(n).filter(|v| !v.is_nan()).fold((0.0f64, 0usize), |(s, c), v| (s + v as f64, c + 1));
        (count >= min_known.max(1)).then(|| (sum / count as f64) as f32)
    }

    /// Changes the capacity, keeping the newest values.
    pub fn resize(&mut self, capacity: usize) {
        let capacity = capacity.max(2);
        if capacity == self.buf.len() {
            return;
        }
        let keep: Vec<f32> = self.tail(capacity).collect();
        *self = Self::new(capacity);
        for v in keep {
            self.push(v);
        }
    }

    pub fn clear(&mut self) {
        self.buf.iter_mut().for_each(|v| *v = f32::NAN);
        self.head = 0;
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_grows_past_capacity() {
        let mut h = History::new(3);
        for v in 0..10 {
            h.push(v as f32);
        }
        assert_eq!(h.len(), 3);
        assert_eq!(h.iter().collect::<Vec<_>>(), vec![7.0, 8.0, 9.0]);
        assert_eq!(h.last(), Some(9.0));
    }

    #[test]
    fn unknown_values_are_gaps_not_zero() {
        let mut h = History::new(4);
        h.push(5.0);
        h.push_opt(None);
        assert_eq!(h.last(), None);
        assert_eq!(h.max(), Some(5.0));
        assert_eq!(h.mean_tail(4, 1), Some(5.0));
        assert_eq!(h.mean_tail(4, 2), None, "one known sample is not enough when two are required");
    }

    #[test]
    fn resize_keeps_newest() {
        let mut h = History::new(5);
        for v in 0..5 {
            h.push(v as f32);
        }
        h.resize(3);
        assert_eq!(h.iter().collect::<Vec<_>>(), vec![2.0, 3.0, 4.0]);
        h.resize(6);
        assert_eq!(h.iter().collect::<Vec<_>>(), vec![2.0, 3.0, 4.0]);
        assert_eq!(h.capacity(), 6);
    }

    #[test]
    fn tail_and_mean() {
        let mut h = History::new(10);
        for v in [10.0, 20.0, 30.0, 40.0] {
            h.push(v);
        }
        assert_eq!(h.tail(2).collect::<Vec<_>>(), vec![30.0, 40.0]);
        assert_eq!(h.mean_tail(2, 2), Some(35.0));
        assert_eq!(h.mean_tail(100, 1), Some(25.0));
    }
}
