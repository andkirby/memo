//! Deep memory measurements: states, scheduling, and display.
//!
//! A process row uses one `footprint` call for that PID alone. A group row
//! with several processes uses one call for exactly the group's PIDs, so
//! Apple counts memory shared inside the group once. Values are never added
//! across processes, and never depend on how PIDs happen to be batched.

use crate::footprint::{Memory, SetMeasurement};
use crate::scanner::format_size;
use std::sync::{atomic::{AtomicUsize, Ordering}, mpsc};
use std::thread;

/// Deep measurement of one process, taken alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcessDeep {
    /// Not measured yet.
    #[default]
    Pending,
    /// `footprint` could not read the process (often a root process) or the
    /// process exited. Not the same as zero.
    Unavailable,
    Measured(Memory),
}

impl ProcessDeep {
    pub fn memory(self) -> Option<Memory> {
        match self {
            Self::Measured(m) => Some(m),
            _ => None,
        }
    }
}

/// Deep measurement of a group row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupDeep {
    #[default]
    Pending,
    /// No member could be measured.
    Unavailable,
    /// Every member was in the measured set.
    Measured(Memory),
    /// Only `measured` of `members` processes were readable. The value is the
    /// footprint of that subset: a lower bound for the whole group.
    Partial { memory: Memory, measured: usize, members: usize },
}

impl GroupDeep {
    pub fn memory(self) -> Option<Memory> {
        match self {
            Self::Measured(m) | Self::Partial { memory: m, .. } => Some(m),
            _ => None,
        }
    }

    pub fn from_process(deep: ProcessDeep) -> Self {
        match deep {
            ProcessDeep::Pending => Self::Pending,
            ProcessDeep::Unavailable => Self::Unavailable,
            ProcessDeep::Measured(m) => Self::Measured(m),
        }
    }
}

/// Which deep value a cell shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric { Footprint, CompressedOrSwapped }

impl Metric {
    pub fn of(self, m: Memory) -> u64 {
        match self {
            Self::Footprint => m.footprint,
            Self::CompressedOrSwapped => m.compressed_or_swapped,
        }
    }
}

/// Cell text: a size, `≥size` for a partial group, `n/a` when unreadable,
/// `…` while pending.
pub fn group_cell(deep: GroupDeep, metric: Metric) -> String {
    match deep {
        GroupDeep::Pending => "…".into(),
        GroupDeep::Unavailable => "n/a".into(),
        GroupDeep::Measured(m) => format_size(metric.of(m)),
        GroupDeep::Partial { memory, .. } => format!("≥{}", format_size(metric.of(memory))),
    }
}

pub fn process_cell(deep: ProcessDeep, metric: Metric) -> String {
    group_cell(GroupDeep::from_process(deep), metric)
}

/// Canonical identity of a PID set: sorted, without duplicates.
pub type GroupKey = Vec<i32>;

pub fn group_key(pids: impl IntoIterator<Item = i32>) -> GroupKey {
    let mut key: Vec<i32> = pids.into_iter().collect();
    key.sort_unstable();
    key.dedup();
    key
}

/// Measures one PID set. Production code uses `footprint::measure_set`.
pub type Measurer<'a> = dyn Fn(&[i32]) -> anyhow::Result<SetMeasurement> + Sync + 'a;

/// Concurrent `footprint` calls for per-process measurements. Each call is
/// small, so this uses every available core.
pub fn process_jobs() -> usize {
    thread::available_parallelism().map_or(4, |n| n.get())
}

/// Concurrent group calls. Apple's tool already spreads one large set over
/// several cores, so a few calls at once are enough.
pub const GROUP_JOBS: usize = 2;

/// Measure each PID alone. `on_result` runs on the calling thread in
/// completion order.
pub fn measure_processes(
    pids: &[i32],
    jobs: usize,
    measure: &Measurer<'_>,
    mut on_result: impl FnMut(i32, ProcessDeep),
) {
    run_pool(pids, jobs, |&pid| measure_process(pid, measure), |&pid, deep| on_result(pid, deep));
}

fn measure_process(pid: i32, measure: &Measurer<'_>) -> ProcessDeep {
    match measure(&[pid]) {
        Ok(set) if set.pids == [pid] => ProcessDeep::Measured(set.memory),
        _ => ProcessDeep::Unavailable,
    }
}

/// Measure each group in `groups` (keys from `group_key`) as one PID set.
pub fn measure_groups(
    groups: &[GroupKey],
    jobs: usize,
    measure: &Measurer<'_>,
    mut on_result: impl FnMut(&GroupKey, GroupDeep),
) {
    run_pool(groups, jobs, |key| measure_group(key, measure), |key, deep| on_result(key, deep));
}

/// One call over every member, including members that could not be read
/// alone. `footprint` returns the readable subset, so coverage comes from the
/// PIDs in this call's output: all members is `Measured`, some is `Partial`,
/// none (or a failed call) is `Unavailable`.
pub fn measure_group(members: &[i32], measure: &Measurer<'_>) -> GroupDeep {
    let members = group_key(members.iter().copied());
    if members.is_empty() {
        return GroupDeep::Unavailable;
    }
    let Ok(set) = measure(&members) else { return GroupDeep::Unavailable };
    let measured = set.pids.iter().filter(|pid| members.binary_search(pid).is_ok()).count();
    match measured {
        0 => GroupDeep::Unavailable,
        n if n == members.len() => GroupDeep::Measured(set.memory),
        n => GroupDeep::Partial { memory: set.memory, measured: n, members: members.len() },
    }
}

/// Run `work` over `items` on `jobs` threads; deliver results on this thread.
fn run_pool<T: Sync, R: Send>(
    items: &[T],
    jobs: usize,
    work: impl Fn(&T) -> R + Sync,
    mut deliver: impl FnMut(&T, R),
) {
    if items.is_empty() {
        return;
    }
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel();
    thread::scope(|s| {
        for _ in 0..jobs.clamp(1, items.len()) {
            let tx = tx.clone();
            let (next, work) = (&next, &work);
            s.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    if tx.send((i, work(item))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        for (i, result) in rx {
            deliver(&items[i], result);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const MIB: u64 = 1024 * 1024;

    fn mem(footprint: u64, compressed_or_swapped: u64) -> Memory {
        Memory { footprint, compressed_or_swapped }
    }

    /// Fake `footprint`: each PID owns 10 MiB privately (4 MiB compressed);
    /// PIDs 1 and 2 also share one 32 MiB region (20 MiB compressed).
    /// PID 9 is unreadable. PID 7 fails alone but is readable in a larger
    /// set (a transient failure). Records every requested set.
    struct Fake { calls: Mutex<Vec<Vec<i32>>> }

    impl Fake {
        fn new() -> Self { Self { calls: Mutex::new(Vec::new()) } }

        fn measure(&self, pids: &[i32]) -> anyhow::Result<SetMeasurement> {
            self.calls.lock().unwrap().push(pids.to_vec());
            let readable: Vec<i32> = pids.iter().copied()
                .filter(|p| *p != 9 && !(*p == 7 && pids.len() == 1)).collect();
            if readable.is_empty() {
                anyhow::bail!("exit 66");
            }
            let mut m = mem(10 * MIB * readable.len() as u64, 4 * MIB * readable.len() as u64);
            if readable.contains(&1) || readable.contains(&2) {
                m.footprint += 32 * MIB;
                m.compressed_or_swapped += 20 * MIB;
            }
            Ok(SetMeasurement { pids: group_key(readable), memory: m })
        }

        fn group(&self, members: &[i32]) -> GroupDeep {
            measure_group(members, &|p| self.measure(p))
        }

        fn last_call(&self) -> Vec<i32> {
            self.calls.lock().unwrap().last().unwrap().clone()
        }
    }

    fn standalone(fake: &Fake, pids: &[i32]) -> HashMap<i32, ProcessDeep> {
        let mut out = HashMap::new();
        measure_processes(pids, 3, &|p| fake.measure(p), |pid, d| { out.insert(pid, d); });
        out
    }

    #[test]
    fn shared_memory_counts_once_per_group_and_fully_per_process() {
        let fake = Fake::new();
        let alone = standalone(&fake, &[1, 2, 3]);
        // Each sharer alone carries the whole shared region.
        assert_eq!(alone[&1], ProcessDeep::Measured(mem(42 * MIB, 24 * MIB)));
        assert_eq!(alone[&2], ProcessDeep::Measured(mem(42 * MIB, 24 * MIB)));
        assert_eq!(alone[&3], ProcessDeep::Measured(mem(10 * MIB, 4 * MIB)));

        // 10 + 10 private + 32 shared once, not 42 + 42.
        assert_eq!(fake.group(&[2, 1]), GroupDeep::Measured(mem(52 * MIB, 28 * MIB)));
        assert_eq!(fake.last_call(), vec![1, 2], "one call, exact members");
    }

    #[test]
    fn group_value_is_independent_of_other_processes_and_order() {
        let fake = Fake::new();
        standalone(&fake, &[1, 2, 3, 4, 5]);
        assert_eq!(fake.group(&[1, 2]), fake.group(&[2, 1, 2]));
        let calls = fake.calls.lock().unwrap();
        assert!(calls.iter().filter(|c| c.len() > 1).all(|c| c == &vec![1, 2]),
            "group calls contain only the group: {calls:?}");
        assert!(calls.iter().filter(|c| c.len() == 1).count() == 5, "one call per process");
    }

    #[test]
    fn the_group_call_requests_every_member_even_unreadable_ones() {
        let fake = Fake::new();
        let alone = standalone(&fake, &[1, 2, 7, 9]);
        assert_eq!(alone[&7], ProcessDeep::Unavailable);
        assert_eq!(alone[&9], ProcessDeep::Unavailable);

        // Unavailable alone, read in the group call: the row is complete.
        assert_eq!(fake.group(&[3, 7]), GroupDeep::Measured(mem(20 * MIB, 8 * MIB)));
        assert_eq!(fake.last_call(), vec![3, 7]);

        // A member missing from the group output makes a lower bound.
        let partial = fake.group(&[1, 2, 9]);
        assert_eq!(fake.last_call(), vec![1, 2, 9], "unreadable members are still requested");
        assert_eq!(partial, GroupDeep::Partial { memory: mem(52 * MIB, 28 * MIB), measured: 2, members: 3 });
        assert_eq!(group_cell(partial, Metric::Footprint), "≥52.0M");
        assert_eq!(fake.group(&[3, 9]), GroupDeep::Partial { memory: mem(10 * MIB, 4 * MIB), measured: 1, members: 2 });

        // Nothing readable: footprint exits 66.
        let none = fake.group(&[9]);
        assert_eq!(none, GroupDeep::Unavailable);
        assert_eq!(group_cell(none, Metric::Footprint), "n/a");
        assert_eq!(process_cell(ProcessDeep::Pending, Metric::Footprint), "…");
    }

    #[test]
    fn coverage_comes_from_the_group_output_not_earlier_calls() {
        // A process can exit between its own call and the group call.
        let deep = measure_group(&[1, 2], &|_| Ok(SetMeasurement { pids: vec![1], memory: mem(5, 1) }));
        assert_eq!(deep, GroupDeep::Partial { memory: mem(5, 1), measured: 1, members: 2 });
        let empty = measure_group(&[1, 2], &|_| Ok(SetMeasurement { pids: vec![], memory: mem(0, 0) }));
        assert_eq!(empty, GroupDeep::Unavailable);
        let failed = measure_group(&[1, 2], &|_| anyhow::bail!("footprint failed"));
        assert_eq!(failed, GroupDeep::Unavailable);
    }

    #[test]
    fn real_zero_is_a_measurement() {
        let deep = measure_group(&[4, 5], &|pids| Ok(SetMeasurement { pids: pids.to_vec(), memory: mem(0, 0) }));
        assert_eq!(deep, GroupDeep::Measured(mem(0, 0)));
        assert_eq!(group_cell(deep, Metric::CompressedOrSwapped), "0B");
    }
}
