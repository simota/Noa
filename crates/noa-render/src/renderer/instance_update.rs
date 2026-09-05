//! CPU patches carried through to GPU uploads, including rebuilds that happen
//! without a draw (occluded tabs and atlas stabilization retries).

use super::CellInstance;
use std::ops::Range;

#[derive(Default)]
pub(super) struct InstanceChanges {
    ranges: Vec<Range<usize>>,
}

impl InstanceChanges {
    pub(super) fn record(&mut self, mut range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let start = self.ranges.partition_point(|old| old.end < range.start);
        let mut end = start;
        while let Some(old) = self.ranges.get(end) {
            if old.start > range.end {
                break;
            }
            range.start = range.start.min(old.start);
            range.end = range.end.max(old.end);
            end += 1;
        }
        self.ranges.splice(start..end, [range]);
    }

    /// Compare only a changed pane (or its small overlay), preserving the
    /// retained instance stream outside the patch. Clean panes skip this
    /// method entirely when their offsets still match.
    pub(super) fn patch(
        &mut self,
        instances: &mut Vec<CellInstance>,
        start: usize,
        source: &[CellInstance],
    ) {
        let overlap = (instances.len() - start).min(source.len());
        let first = source[..overlap]
            .iter()
            .zip(&instances[start..])
            .position(|(new, old)| new != old)
            .unwrap_or(overlap);
        if first == source.len() {
            return;
        }
        let end = if overlap == source.len() {
            source
                .iter()
                .zip(&instances[start..])
                .rposition(|(new, old)| new != old)
                .unwrap()
                + 1
        } else {
            source.len()
        };
        let copied_end = end.min(overlap);
        instances[start + first..start + copied_end].copy_from_slice(&source[first..copied_end]);
        if end > overlap {
            instances.extend_from_slice(&source[overlap..end]);
        }
        self.record(start + first..start + end);
    }

    pub(super) fn take(&mut self, len: usize) -> impl Iterator<Item = Range<usize>> + '_ {
        // A later rebuild can shorten or remove a pane before the next draw.
        self.ranges.drain(..).filter_map(move |range| {
            let range = range.start..range.end.min(len);
            (!range.is_empty()).then_some(range)
        })
    }

    pub(super) fn clear(&mut self) {
        self.ranges.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(values: &[u16]) -> Vec<CellInstance> {
        values
            .iter()
            .map(|&value| CellInstance {
                grid_pos: [value, 0],
                ..bytemuck::Zeroable::zeroed()
            })
            .collect()
    }

    #[test]
    fn pending_uploads_reconstruct_the_latest_stream_after_multiple_rebuilds() {
        let mut cpu = cells(&[1, 2, 3, 4, 5, 6]);
        let mut gpu = cpu.clone();
        let mut changes = InstanceChanges::default();
        changes.patch(&mut cpu, 0, &cells(&[1, 9, 3]));
        changes.patch(&mut cpu, 3, &cells(&[4, 8, 6]));
        changes.patch(&mut cpu, 0, &cells(&[1, 2, 3])); // return to the old value before upload
        changes.patch(&mut cpu, 6, &cells(&[7, 8]));
        gpu.resize(cpu.len(), cells(&[0])[0]);
        for range in changes.take(cpu.len()) {
            gpu[range.clone()].copy_from_slice(&cpu[range]);
        }
        assert_eq!(gpu, cpu);
        assert!(changes.take(cpu.len()).next().is_none());
    }

    #[test]
    fn patches_skip_identical_data_and_bound_distant_changes() {
        let mut cpu = cells(&[1, 2, 3, 4, 5, 6, 7]);
        let mut changes = InstanceChanges::default();
        changes.patch(&mut cpu, 0, &cells(&[1, 2, 3]));
        assert!(changes.take(cpu.len()).next().is_none());
        changes.patch(&mut cpu, 0, &cells(&[1, 9, 3]));
        changes.patch(&mut cpu, 4, &cells(&[5, 8, 7]));
        assert_eq!(
            changes.take(cpu.len()).collect::<Vec<_>>(),
            vec![1..2, 5..6]
        );
        assert_eq!(cpu, cells(&[1, 9, 3, 4, 5, 8, 7]));
    }

    #[test]
    fn overlap_merges_and_removed_tail_never_uploads() {
        let mut changes = InstanceChanges::default();
        for range in [10..20, 2..5, 4..11, 30..40] {
            changes.record(range);
        }
        assert_eq!(changes.take(15).collect::<Vec<_>>(), vec![2..15]);
    }
}
