//! Only reorder paths whose conservative pixel rectangles are disjoint.
use std::{collections::HashMap, ops::Range};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rect(pub [u32; 4]);
impl Rect {
    pub fn intersects(self, other: Self) -> bool {
        let [a, b, c, d] = self.0;
        let [e, f, g, h] = other.0;
        a <= g && e <= c && b <= h && f <= d
    }
    pub fn union(self, other: Self) -> Self {
        let [a, b, c, d] = self.0;
        let [e, f, g, h] = other.0;
        Self([a.min(e), b.min(f), c.max(g), d.max(h)])
    }
}

#[derive(Default)]
pub(super) struct Plan {
    pub groups: Vec<Range<usize>>,
    pub comparisons: u64,
    pub barriers: u64,
}

pub(super) fn plan(rects: &[Option<Rect>]) -> Plan {
    const MAX_PATHS: usize = 128;
    const MAX_COMPARISONS: usize = 4096;
    const MAX_CELLS: u64 = 4096;
    const CELL: u32 = 64;
    let mut out = Plan::default();
    let mut grid: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    let mut start = 0;
    let mut comparisons = 0;
    let mut cells = 0;
    for (i, rect) in rects.iter().copied().enumerate() {
        let Some(rect) = rect else { continue };
        let [x0, y0, x1, y1] = rect.0.map(|x| x / CELL);
        let count = (x1 - x0 + 1) as u64 * (y1 - y0 + 1) as u64;
        let mut conflict = i - start >= MAX_PATHS || cells + count > MAX_CELLS;
        let mut visited = [false; MAX_PATHS];
        if !conflict {
            'search: for y in y0..=y1 {
                for x in x0..=x1 {
                    for &j in grid.get(&(x, y)).into_iter().flatten() {
                        if visited[j - start] {
                            continue;
                        }
                        visited[j - start] = true;
                        comparisons += 1;
                        out.comparisons += 1;
                        if comparisons > MAX_COMPARISONS || rect.intersects(rects[j].unwrap()) {
                            conflict = true;
                            break 'search;
                        }
                    }
                }
            }
        }
        if conflict {
            if start < i {
                out.groups.push(start..i);
            }
            start = i;
            comparisons = 0;
            cells = 0;
            grid.clear();
            out.barriers += 1;
        }
        if count > MAX_CELLS {
            out.groups.push(i..i + 1);
            start = i + 1;
            continue;
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                grid.entry((x, y)).or_default().push(i);
            }
        }
        cells += count;
    }
    if start < rects.len() {
        out.groups.push(start..rects.len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn touching_and_overlap_are_barriers() {
        let p = plan(&[
            Some(Rect([0, 0, 9, 9])),
            Some(Rect([9, 0, 20, 9])),
            Some(Rect([23, 0, 30, 9])),
        ]);
        assert_eq!(p.groups, [0..1, 1..3]);
    }
    #[test]
    fn grid_keeps_a_hundred_disjoint_paths_together() {
        let rects: Vec<_> = (0..100)
            .map(|i| {
                Some(Rect([
                    i % 10 * 50,
                    i / 10 * 50,
                    i % 10 * 50 + 44,
                    i / 10 * 50 + 44,
                ]))
            })
            .collect();
        let p = plan(&rects);
        assert_eq!(p.groups.len(), 1);
        assert_eq!(p.groups[0], 0..100);
    }
    #[test]
    fn huge_rectangles_and_sparse_inputs_remain_bounded_and_ordered() {
        let rects = [
            None,
            Some(Rect([0, 0, u32::MAX, u32::MAX])),
            None,
            Some(Rect([3, 4, 5, 6])),
        ];
        let p = plan(&rects);
        assert_eq!(p.groups, [0..1, 1..2, 2..4]);
    }
    #[test]
    fn generated_groups_never_contain_intersecting_rectangles() {
        let mut seed = 831_u32;
        let rects: Vec<_> = (0..2000)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let x = seed % 1000;
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let y = seed % 1000;
                Some(Rect([x, y, x + seed % 80, y + seed % 60]))
            })
            .collect();
        let p = plan(&rects);
        let mut cursor = 0;
        for g in p.groups {
            assert_eq!(g.start, cursor);
            assert!(g.len() <= 128);
            for i in g.clone() {
                for j in i + 1..g.end {
                    assert!(!rects[i].unwrap().intersects(rects[j].unwrap()));
                }
            }
            cursor = g.end;
        }
        assert_eq!(cursor, rects.len());
    }
}
