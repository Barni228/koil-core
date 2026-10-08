use crate::Action;
use pathfinding::prelude::*;
use std::{
    cmp::Ord,
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
    ops::Bound,
    path::{Path, PathBuf},
};

/// Order `actions` so they can be run one after another
/// `exists` should return true if a path is already taken on the filesystem,
/// it is used to pick a free temporary name when breaking cycles
pub fn plan_actions(actions: &[Action], exists: impl Fn(&Path) -> bool) -> Vec<Action> {
    plan(actions, &exists, actions.len())
}

/// See [`plan_actions`], breaking at most `breaks` more cycles: each break takes an action out
/// of one, so the changes never need more, but actions that no diff makes (two renames of
/// one path) could make cycles without end
fn plan(actions: &[Action], exists: &dyn Fn(&Path) -> bool, breaks: usize) -> Vec<Action> {
    let mut result = Vec::new();
    // the paths an action creates or removes, which a temp path can not be
    let used: HashSet<&Path> = (actions.iter())
        .flat_map(|a| a.creates().into_iter().chain(a.removes()))
        .collect();
    let taken = |path: &Path| exists(path) || used.contains(path);

    for cycle in order(actions) {
        if cycle.len() == 1 {
            result.extend(cycle);
            continue;
        }
        let Some((broken, next_to)) = break_at(&cycle).filter(|_| breaks > 0) else {
            // the changes never make one like this, and running it fails rather than doing
            // something else
            result.extend(cycle);
            continue;
        };

        // A -> B -> C -> <loop back>
        // rename A -> tmp
        // Then the rest of the chain, which is no cycle anymore
        // C -> A
        // B -> C
        // Then rename tmp to where A goes, tmp -> B
        let tmp = temp_path(next_to, taken);
        let mut rest = cycle;
        let (first, last) = match rest.remove(broken) {
            Action::Rename(from, to) => (Action::Rename(from, tmp.clone()), to),
            Action::Copy(from, to) => (Action::Copy(from, tmp.clone()), to),
            _ => unreachable!("break_at only breaks a rename or a copy"),
        };
        rest.push(Action::Rename(tmp, last));
        result.push(first);
        result.extend(plan(&rest, &taken, breaks - 1));
    }

    result
}

/// Where to break `cycle` (a group of [`order`] with more than one action): the index of the
/// rename or copy to run through a temp path first, and the path to put the temp path next to
/// One whose source is inside a path another one removes (like moving `a/x` to `a` while `a`
/// is deleted, or renamed) goes out of that path first, next to its outermost one, so it can
/// be removed: the deepest such source, so nothing else in the cycle is inside it
/// Else it is a cycle of renames (like swapping two names), broken at its first
/// `None` for a cycle that can not be broken this way
fn break_at(cycle: &[Action]) -> Option<(usize, &Path)> {
    fn source(action: &Action) -> Option<&Path> {
        match action {
            Action::Rename(from, _) | Action::Copy(from, _) => Some(from),
            _ => None,
        }
    }
    // the outermost path that another action of the cycle removes, with `i`'s source inside
    let around = |i: usize| {
        let from = source(&cycle[i])?;
        (cycle.iter().enumerate())
            .filter(|&(j, _)| j != i)
            .filter_map(|(_, other)| other.removes())
            .filter(|&removed| removed != from && from.starts_with(removed))
            .min_by_key(|removed| removed.components().count())
    };
    let depth = |i: usize| source(&cycle[i]).map_or(0, |from| from.components().count());
    let nested = (0..cycle.len()).filter_map(|i| Some((i, around(i)?)));
    // the deepest, and of those, the first action (the cycle's order is not)
    let deepest = nested
        .max_by(|&(i, _), &(j, _)| (depth(i).cmp(&depth(j))).then_with(|| cycle[j].cmp(&cycle[i])));
    if deepest.is_some() {
        return deepest;
    }
    let renames = cycle.iter().all(|a| matches!(a, Action::Rename(_, _)));
    let first = (0..cycle.len()).min_by(|&i, &j| cycle[i].cmp(&cycle[j]))?;
    match &cycle[first] {
        Action::Rename(from, _) if renames => Some((first, from)),
        _ => None,
    }
}

/// `actions` in the order they can run, in groups: an action alone, or a cycle (like rename A
/// to B and B to A, or delete `a` and move `a/x` to `a`), which can only run through a temp
/// path (see [`plan_actions`])
pub fn order(actions: &[Action]) -> Vec<Vec<Action>> {
    let successors = Successors::new(actions);
    // detect all cycles (like rename A to B and B to A)
    // every action that is not in a cycle is its own group of 1
    let mut groups = scc(actions, |a| successors.of(a));
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
            .flat_map(|a| successors.of(a))
            .map(|a| group_of[&a])
            .filter(|&j| j != i)
            .collect();
        next.sort_unstable();
        next.dedup();
        next
    })
    .unwrap();

    order
        .into_iter()
        .map(|i| std::mem::take(&mut groups[i]))
        .collect()
}

/// For each of `actions`, the indexes of the others it can not run without: the one that
/// removes the path it creates (which is taken until then), and the ones that create a new dir
/// it creates something in, as [`plan_actions`] runs them first
/// Only what each needs directly, so in a chain of renames, the first needs only the second
pub fn needs(actions: &[Action]) -> Vec<Vec<usize>> {
    // at most one action removes or creates a path, as an ID has one path, and update rejects
    // two entries with one name
    let removed: HashMap<&Path, usize> = (actions.iter().enumerate())
        .filter_map(|(i, a)| Some((a.removes()?, i)))
        .collect();
    // if a created path is also removed, paths inside it refer to the old one
    let created: HashMap<&Path, usize> = (actions.iter().enumerate())
        .filter_map(|(i, a)| Some((a.creates()?, i)))
        .filter(|(path, _)| !removed.contains_key(path))
        .collect();
    (actions.iter().enumerate())
        .map(|(i, action)| {
            let Some(path) = action.creates() else {
                return Vec::new();
            };
            let in_dirs = path.ancestors().skip(1).filter_map(|dir| created.get(dir));
            let mut needs: Vec<usize> = (removed.get(path).into_iter().chain(in_dirs))
                .copied()
                .filter(|&j| j != i)
                .collect();
            needs.sort_unstable();
            needs.dedup();
            needs
        })
        .collect()
}

/// A free path next to `from`, to temporarily move it out of the way
/// Tries `.name.koil0`, `.name.koil1`, ... until a path that is not `taken` (on the filesystem,
/// or by an action that creates or removes it) is found
fn temp_path(from: &Path, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let dir = from.parent().unwrap_or(Path::new(""));
    let name = from.file_name().unwrap().to_string_lossy();

    (0..)
        .map(|i| dir.join(format!(".{name}.koil{i}")))
        .find(|path| !taken(path))
        .unwrap()
}

/// What actions should happen after each one, found by their paths rather than by comparing
/// every two actions (which took 14 s to order 10,000 deletes)
struct Successors<'a> {
    actions: &'a [Action],
    /// The indexes of the actions that remove each path
    removes: HashMap<&'a Path, Vec<usize>>,
    /// The indexes of the actions that create each path, in order, so the paths inside a dir
    /// come right after it
    creates: BTreeMap<&'a Path, Vec<usize>>,
}

impl<'a> Successors<'a> {
    fn new(actions: &'a [Action]) -> Self {
        let mut removes: HashMap<&Path, Vec<usize>> = HashMap::new();
        let mut creates: BTreeMap<&Path, Vec<usize>> = BTreeMap::new();
        for (i, action) in actions.iter().enumerate() {
            if let Some(path) = action.removes() {
                removes.entry(path).or_default().push(i);
            }
            if let Some(path) = action.creates() {
                creates.entry(path).or_default().push(i);
            }
        }
        Successors {
            actions,
            removes,
            creates,
        }
    }

    /// The actions that should happen AFTER `action`, in the order of the actions
    fn of(&self, action: &Action) -> Vec<Action> {
        let mut after: Vec<usize> = Vec::new();
        // If I depend on something, and `other` removes that (or a dir it is in), I go first
        if let Some(depend) = action.depends_on() {
            for dir in depend.ancestors() {
                after.extend(self.removes.get(dir).into_iter().flatten());
            }
        }
        // If I remove something and `other` creates it, I should remove it first
        if let Some(removed) = action.removes() {
            after.extend(self.creates.get(removed).into_iter().flatten());
        }
        // If I create something new and `other` creates something inside it, I go first
        // (if `created` is also removed, paths inside it refer to the old one)
        if let Some(created) = action.creates()
            && !self.removes.contains_key(created)
        {
            let after_it = (Bound::Excluded(created), Bound::Unbounded);
            let inside = (self.creates.range::<Path, _>(after_it))
                .take_while(|(path, _)| path.starts_with(created));
            after.extend(inside.flat_map(|(_, others)| others));
        }
        after.sort_unstable();
        after.dedup();
        (after.into_iter().map(|i| &self.actions[i]))
            .filter(|&other| other != action)
            .cloned()
            .collect()
    }
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
