use crate::Action;
use pathfinding::prelude::*;
use std::{
    cmp::Ord,
    collections::HashMap,
    hash::Hash,
    path::{Path, PathBuf},
};

/// Order `actions` so they can be run one after another
/// `exists` should return true if a path is already taken on the filesystem,
/// it is used to pick a free temporary name when breaking rename cycles
pub fn plan_actions(actions: &[Action], exists: impl Fn(&Path) -> bool) -> Vec<Action> {
    let mut result = Vec::new();

    // detect all rename cycles (like rename A to B and B to A)
    // every action that is not in a cycle is its own group of 1
    let mut groups = scc(actions, |a| successors(actions, a));
    // when nothing else decides the order, cycles go first
    groups.sort_by_key(|group| group.len() == 1);
    let group_of: HashMap<&Action, usize> = groups
        .iter()
        .enumerate()
        .flat_map(|(i, group)| group.iter().map(move |a| (a, i)))
        .collect();

    // sort the groups topologically, so a cycle runs at the right time relative to everything else
    let indexes: Vec<usize> = (0..groups.len()).collect();
    let order = topo_sort(&indexes, |&i| {
        let mut next: Vec<usize> = groups[i]
            .iter()
            .flat_map(|a| successors(actions, a))
            .map(|a| group_of[&a])
            .filter(|&j| j != i)
            .collect();
        next.sort_unstable();
        next.dedup();
        next
    })
    .unwrap();

    for cycle in order.into_iter().map(|i| groups[i].clone()) {
        if cycle.len() == 1 {
            result.extend(cycle);
            continue;
        }
        // only rename actions can create a cycle
        assert!(
            cycle.iter().all(|a| matches!(a, Action::Rename(_, _))),
            "Invalid cycle: {cycle:?}"
        );

        let mut iter = cycle.into_iter();

        // A -> B -> C -> <loop back>
        // rename A -> tmp
        // Then walk the chain in reverse
        // C -> A
        // B -> B
        // Then rename tmp back to A, tmp -> A
        let (first_from, first_to) = match iter.next().unwrap() {
            Action::Rename(from, to) => (from, to),
            _ => unreachable!(),
        };
        let tmp = temp_path(&first_from, actions, &exists);
        result.push(Action::Rename(first_from, tmp.clone()));
        result.extend(iter.rev());

        result.push(Action::Rename(tmp, first_to));
    }

    result
}

/// A free path next to `from`, to temporarily move it out of the way
/// Tries `.name.koil0`, `.name.koil1`, ... until a path that does not exist,
/// and that no action creates or removes, is found
fn temp_path(from: &Path, actions: &[Action], exists: impl Fn(&Path) -> bool) -> PathBuf {
    let dir = from.parent().unwrap_or(Path::new(""));
    let name = from.file_name().unwrap().to_string_lossy();
    let taken = |path: &Path| {
        exists(path)
            || actions
                .iter()
                .any(|a| a.creates() == Some(path) || a.removes() == Some(path))
    };

    (0..)
        .map(|i| dir.join(format!(".{name}.koil{i}")))
        .find(|path| !taken(path))
        .unwrap()
}

/// this returns all actions that should happen AFTER this action
fn successors(actions: &[Action], action: &Action) -> Vec<Action> {
    actions
        .iter()
        .filter(|&a| action != a)
        // Return true if `action` should happen before `other`
        .filter(|&other| {
            // If I depend on something, and `other` removes that, I go first
            matches!((action.depends_on(), other.removes()),
                (Some(depend), Some(removed)) if depend.starts_with(removed))
            // If I remove something and `other` creates it, I should remove it first
            || matches!((action.removes(), other.creates()),
                (Some(removed), Some(created)) if created == removed)
            // If I create something new and `other` creates something inside it, I go first
            // (if `created` is also removed, paths inside it refer to the old one)
            || matches!((action.creates(), other.creates().and_then(Path::parent)),
                (Some(created), Some(parent)) if parent.starts_with(created)
                    && !actions.iter().any(|a| a.removes() == Some(created)))
        })
        .cloned()
        .collect()
}

/// A deterministic version of `pathfinding` `topological_sort`
fn topo_sort<N, FN, IN>(roots: &[N], successors: FN) -> Result<Vec<N>, ()>
where
    N: Eq + Hash + Clone + Ord,
    FN: FnMut(&N) -> IN,
    IN: IntoIterator<Item = N>,
{
    Ok(topological_sort_into_groups(roots, successors)
        .map_err(|_| ())?
        .into_iter()
        .flat_map(|mut g| {
            g.sort_unstable();
            g
        })
        .collect())
}

/// A deterministic version of `pathfinding` `strongly_connected_components`
fn scc<N, FN, IN>(nodes: &[N], successors: FN) -> Vec<Vec<N>>
where
    N: Clone + Hash + Eq + Ord,
    FN: FnMut(&N) -> IN,
    IN: IntoIterator<Item = N>,
{
    let mut cycles = strongly_connected_components(nodes, successors);
    for cycle in cycles.iter_mut() {
        // rotate the chain, so first element is always the smallest
        // So the output is always consistent
        // B -> C -> A -> <loop back>, rotate so the smallest element is first (A)
        // A -> B -> C -> <loop back>
        let (min_i, _) = cycle.iter().enumerate().min_by_key(|&(_, s)| s).unwrap();
        cycle.rotate_left(min_i);
    }
    // sort after rotating, since the order `pathfinding` returns is not deterministic
    cycles.sort_unstable();

    cycles
}
