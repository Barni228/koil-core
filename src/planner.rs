use crate::Action;
use pathfinding::prelude::*;
use std::{cmp::Ord, hash::Hash};

pub fn plan_actions(actions: &[Action]) -> Vec<Action> {
    let mut result = Vec::new();

    // non cycles go here
    let mut no_temp_needed = Vec::new();
    // detect all rename cycles (like rename A to B and B to A)
    let cycles = scc(actions, |a| successors(actions, a));

    for cycle in cycles {
        // if this is not really a cycle, sort it topologically
        if cycle.len() == 1 {
            no_temp_needed.push(cycle.into_iter().next().unwrap());
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
        result.push(Action::Rename(first_from, "tmp".into()));
        result.extend(iter.rev());

        result.push(Action::Rename("tmp".into(), first_to));
    }

    // result.extend(topological_sort(actions, successors).unwrap());
    result.extend(topo_sort(&no_temp_needed, |a| successors(&no_temp_needed, a)).unwrap());
    result
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
            //             // // If I create a directory and `other` depends on something in that dir, I go first
            //             || dbg!(matches!((action.creates(), other.creates().and_then(|d| d.parent())),
            //                 (Some(created), Some(parent)) if parent.starts_with(created)))
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
    cycles.sort_unstable();
    for cycle in cycles.iter_mut() {
        // rotate the chain, so first element is always the smallest
        // So the output is always consistent
        // B -> C -> A -> <loop back>, rotate so the smallest element is first (A)
        // A -> B -> C -> <loop back>
        let (min_i, _) = cycle.iter().enumerate().min_by_key(|&(_, s)| s).unwrap();
        cycle.rotate_left(min_i);
    }

    cycles
}
