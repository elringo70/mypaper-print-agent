//! Page-selection helpers shared by the UI hint and the reprint path.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SubsetError {
    #[error("no pages selected")]
    Empty,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Group sorted page numbers into contiguous ranges: [1,2,3,5,7,8] →
/// [(1,3),(5,5),(7,8)]. Pure and unit-tested (also reused by the UI hint).
pub fn group_contiguous(mut pages: Vec<u32>) -> Vec<(u32, u32)> {
    pages.sort_unstable();
    pages.dedup();
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for p in pages {
        match ranges.last_mut() {
            Some(last) if p == last.1 + 1 => last.1 = p,
            _ => ranges.push((p, p)),
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_contiguous_ranges() {
        assert_eq!(
            group_contiguous(vec![1, 2, 3, 5, 7, 8]),
            vec![(1, 3), (5, 5), (7, 8)]
        );
        assert_eq!(group_contiguous(vec![3, 1, 2, 2]), vec![(1, 3)]);
        assert!(group_contiguous(vec![]).is_empty());
    }
}
