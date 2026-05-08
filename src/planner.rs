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
        assert!(cycle.iter().all(|a| matches!(a, Action::Rename(_, _))));

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
        result.push(Action::Rename(first_from, "tmp".to_string()));
        result.extend(iter.rev());

        result.push(Action::Rename("tmp".to_string(), first_to));
    }

    // result.extend(topological_sort(actions, successors).unwrap());
    result.extend(topo_sort(&no_temp_needed, |a| successors(&no_temp_needed, a)).unwrap());
    result
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
    // let mut groups = topological_sort_into_groups(roots, successors).map_err(|_| ())?;
    // for g in groups.iter_mut() {
    //     g.sort()
    // }

    // Ok(groups.into_iter().flatten().collect())
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

/// this returns all actions that should happen AFTER this action
fn successors(actions: &[Action], action: &Action) -> Vec<Action> {
    actions
        .iter()
        .filter(|&a| action != a)
        .filter(|&other| {
            // If I depend on something, and `other` removes that, I go first
            (action
                .depends_on()
                .is_some_and(|f| other.clears() == Some(f)))
            // If I remove something and `other` creates it, I should remove it first
                || (action.clears().is_some_and(|f| other.creates() == Some(f)))
            // If I create something, and `a` depends on that, I go first
            // || (action.creates().is_some_and(|c| a.depends_on() == Some(c)))
        })
        .cloned()
        .collect()
}
