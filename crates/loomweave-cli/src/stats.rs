#[derive(Debug, Default)]
pub(crate) struct P95Accumulator {
    samples_ms: Vec<u64>,
}

impl P95Accumulator {
    pub(crate) fn record_many<I>(&mut self, samples_ms: I)
    where
        I: IntoIterator<Item = u64>,
    {
        self.samples_ms.extend(samples_ms);
    }

    pub(crate) fn p95_ms(&self) -> u64 {
        if self.samples_ms.is_empty() {
            return 0;
        }

        let mut sorted = self.samples_ms.clone();
        sorted.sort_unstable();
        let nearest_rank = (sorted.len() * 95).div_ceil(100);
        sorted[nearest_rank.saturating_sub(1)]
    }
}

#[cfg(test)]
mod tests {
    use super::P95Accumulator;

    #[test]
    fn p95_accumulator_uses_deterministic_nearest_rank() {
        let mut accumulator = P95Accumulator::default();
        accumulator.record_many((10..=1000).step_by(10));

        assert_eq!(accumulator.p95_ms(), 950);
    }
}
