use std::collections::BTreeMap;

/// Entropy threshold and minimum candidate length for one alphabet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntropyTuning {
    pub min_len: usize,
    pub min_entropy: f64,
}

impl EntropyTuning {
    pub const BASE64: Self = Self {
        min_len: 20,
        min_entropy: 4.5,
    };
    pub const HEX: Self = Self {
        min_len: 40,
        min_entropy: 3.0,
    };

    pub(crate) fn accepts(self, candidate: &[u8]) -> bool {
        candidate.len() >= self.min_len && shannon_entropy(candidate) >= self.min_entropy
    }
}

pub(crate) fn shannon_entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = BTreeMap::<u8, usize>::new();
    for byte in bytes {
        *counts.entry(*byte).or_default() += 1;
    }
    let len = usize_to_f64(bytes.len());
    counts
        .values()
        .map(|count| {
            let p = usize_to_f64(*count) / len;
            -p * p.log2()
        })
        .sum()
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::shannon_entropy;

    #[test]
    fn entropy_is_low_for_repetition() {
        assert!(shannon_entropy(b"aaaaaaaaaaaaaaaaaaaaaaaa") < 0.1);
    }

    #[test]
    fn entropy_is_higher_for_varied_alphabet() {
        assert!(shannon_entropy(b"0123456789abcdefABCDEF+/") > 4.0);
    }
}
