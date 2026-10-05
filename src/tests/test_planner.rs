use super::*;

/// [`planner::plan_actions`], as if nothing exists on the filesystem
fn plan_actions(actions: &[Action]) -> Vec<Action> {
    planner::plan_actions(actions, |_| false)
}

#[test]
fn test_delete_rename() {
    assert_eq!(
        vec![delete("A"), rename("B", "A")],
        plan_actions(&[rename("B", "A"), delete("A")])
    )
}

#[test]
fn test_rename_chain() {
    assert_eq!(
        vec![rename("B", "C"), rename("A", "B")],
        plan_actions(&[rename("A", "B"), rename("B", "C")])
    )
}

#[test]
fn test_copy_delete() {
    assert_eq!(
        vec![copy("A", "B"), delete("A")],
        plan_actions(&[delete("A"), copy("A", "B")])
    )
}

// --- trivial --------------------------------------------------------------

#[test]
fn test_empty_input() {
    assert_eq!(plan_actions(&[]), vec![]);
}

#[test]
fn test_single_rename() {
    assert_eq!(vec![rename("A", "B")], plan_actions(&[rename("A", "B")]));
}

// --- swaps / B-cycles -----------------------------------------------------

#[test]
fn test_swap_ab() {
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("B", "A"),
            rename(".A.koil0", "B")
        ],
        plan_actions(&[rename("A", "B"), rename("B", "A")])
    );
}

#[test]
fn test_swap_ab_reversed_input_order() {
    // Same rename, input listed the other way; result must still be valid
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("B", "A"),
            rename(".A.koil0", "B")
        ],
        plan_actions(&[rename("B", "A"), rename("A", "B")])
    )
}

// --- chains (no cycle) ----------------------------------------------------

#[test]
fn test_chain_two() {
    // A -> B, B -> C  — B must be renamed before A
    assert_eq!(
        vec![rename("B", "C"), rename("A", "B")],
        plan_actions(&[rename("A", "B"), rename("B", "C")])
    );
}

#[test]
fn test_chain_three() {
    assert_eq!(
        vec![rename("C", "D"), rename("B", "C"), rename("A", "B")],
        plan_actions(&[rename("A", "B"), rename("B", "C"), rename("C", "D")])
    );
}

#[test]
fn test_chain_five() {
    assert_eq!(
        vec![
            rename("E", "F"),
            rename("D", "E"),
            rename("C", "D"),
            rename("B", "C"),
            rename("A", "B")
        ],
        plan_actions(&[
            rename("A", "B"),
            rename("B", "C"),
            rename("C", "D"),
            rename("D", "E"),
            rename("E", "F")
        ])
    );
}

#[test]
fn test_chain_input_order_scrambled() {
    // Reversed input order — the validator catches any use-before-free
    assert_eq!(
        vec![rename("C", "D"), rename("B", "C"), rename("A", "B")],
        plan_actions(&[rename("C", "D"), rename("A", "B"), rename("B", "C")])
    );
}

// --- larger cycles --------------------------------------------------------

#[test]
fn test_cycle_three() {
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("C", "A"),
            rename("B", "C"),
            rename(".A.koil0", "B")
        ],
        plan_actions(&[rename("A", "B"), rename("B", "C"), rename("C", "A")])
    );
}

#[test]
fn test_cycle_four() {
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("D", "A"),
            rename("C", "D"),
            rename("B", "C"),
            rename(".A.koil0", "B")
        ],
        plan_actions(&[
            rename("A", "B"),
            rename("B", "C"),
            rename("C", "D"),
            rename("D", "A")
        ])
    );
}

#[test]
fn test_cycle_five() {
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("E", "A"),
            rename("D", "E"),
            rename("C", "D"),
            rename("B", "C"),
            rename(".A.koil0", "B")
        ],
        plan_actions(&[
            rename("A", "B"),
            rename("B", "C"),
            rename("C", "D"),
            rename("D", "E"),
            rename("E", "A")
        ])
    );
}

// --- multiple independent groups ------------------------------------------

#[test]
fn test_two_independent_renames() {
    assert_eq!(
        vec![rename("A", "B"), rename("P", "Q")],
        plan_actions(&[rename("A", "B"), rename("P", "Q")])
    );
}

#[test]
fn test_three_independent_renames() {
    assert_eq!(
        vec![rename("A", "B"), rename("C", "D"), rename("E", "F")],
        plan_actions(&[rename("A", "B"), rename("C", "D"), rename("E", "F")])
    );
}

#[test]
fn test_two_independent_chains() {
    // I have this order, but its valid so sadly i have to accept it
    // I would like it to do: B -> C, A -> B, Q -> R, P -> Q
    // (so it finishes one chain, then goes to the other chain)
    assert_eq!(
        vec![
            rename("B", "C"),
            rename("Q", "R"),
            rename("A", "B"),
            rename("P", "Q")
        ],
        plan_actions(&[
            rename("A", "B"),
            rename("B", "C"),
            rename("P", "Q"),
            rename("Q", "R")
        ])
    );
}

#[test]
fn test_two_independent_swaps() {
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("B", "A"),
            rename(".A.koil0", "B"),
            rename("P", ".P.koil0"),
            rename("Q", "P"),
            rename(".P.koil0", "Q")
        ],
        plan_actions(&[
            rename("A", "B"),
            rename("B", "A"),
            rename("P", "Q"),
            rename("Q", "P")
        ])
    );
}

// --- mixed: cycle + free rename / chain -----------------------------------

#[test]
fn test_swap_plus_lone_rename() {
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("B", "A"),
            rename(".A.koil0", "B"),
            rename("P", "Q")
        ],
        plan_actions(&[rename("A", "B"), rename("B", "A"), rename("P", "Q")])
    );
}

#[test]
fn test_cycle_plus_chain() {
    // A <-> B (swap), C -> D -> E (chain)
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("B", "A"),
            rename(".A.koil0", "B"),
            rename("D", "E"),
            rename("C", "D"),
        ],
        plan_actions(&[
            rename("A", "B"),
            rename("B", "A"),
            rename("C", "D"),
            rename("D", "E")
        ])
    );
}

#[test]
fn test_two_swaps_plus_chain() {
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("B", "A"),
            rename(".A.koil0", "B"),
            rename("P", ".P.koil0"),
            rename("Q", "P"),
            rename(".P.koil0", "Q"),
            rename("Y", "Z"),
            rename("X", "Y"),
        ],
        plan_actions(&[
            rename("A", "B"),
            rename("B", "A"),
            rename("P", "Q"),
            rename("Q", "P"),
            rename("X", "Y"),
            rename("Y", "Z"),
        ])
    );
}

#[test]
fn test_cycle3_plus_lone_rename() {
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("C", "A"),
            rename("B", "C"),
            rename(".A.koil0", "B"),
            rename("P", "Q")
        ],
        plan_actions(&[
            rename("A", "B"),
            rename("B", "C"),
            rename("C", "A"),
            rename("P", "Q")
        ])
    );
}

// --- ADD ----------------------------------------------------------------

#[test]
fn test_add() {
    assert_eq!(vec![add("A")], plan_actions(&[add("A")]));
}

#[test]
fn test_add_chain() {
    assert_eq!(
        vec![rename("A", "B"), add("A")],
        plan_actions(&[add("A"), rename("A", "B")])
    );
}

#[test]
fn test_remove() {
    assert_eq!(vec![delete("A")], plan_actions(&[delete("A")]));
}

#[test]
fn test_remove_chain() {
    assert_eq!(
        vec![delete("A"), rename("B", "A")],
        plan_actions(&[rename("B", "A"), delete("A")])
    );
}

#[test]
fn test_add_remove() {
    assert_eq!(
        vec![delete("A"), add("A")],
        plan_actions(&[add("A"), delete("A")])
    );
}

////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

// --- remove: multiple / ordering -----------------------------------------

#[test]
fn test_two_removes() {
    assert_eq!(
        vec![delete("A"), delete("B")],
        plan_actions(&[delete("A"), delete("B")])
    );
}

#[test]
fn test_remove_then_rename_into_freed_slot() {
    // slot A is deleted, then slot B is renamed into slot A
    // rename depends on delete happening first
    assert_eq!(
        vec![delete("A"), rename("B", "A")],
        plan_actions(&[rename("B", "A"), delete("A")])
    );
}

#[test]
fn test_remove_frees_slot_for_chain() {
    // C -> A is blocked by A, which is deleted; so delete first, then chain
    assert_eq!(
        vec![delete("A"), rename("C", "A")],
        plan_actions(&[rename("C", "A"), delete("A")])
    );
}

#[test]
fn test_two_removes_then_renames_into_freed_slots() {
    assert_eq!(
        vec![delete("A"), delete("B"), rename("C", "A"), rename("D", "B")],
        plan_actions(&[rename("C", "A"), rename("D", "B"), delete("A"), delete("B")])
    );
}

#[test]
fn test_remove_with_unrelated_rename() {
    assert_eq!(
        vec![delete("A"), rename("P", "Q")],
        plan_actions(&[delete("A"), rename("P", "Q")])
    );
}

// --- add: multiple / ordering --------------------------------------------

#[test]
fn test_two_adds() {
    assert_eq!(
        vec![add("A"), add("B")],
        plan_actions(&[add("A"), add("B")])
    );
}

#[test]
fn test_add_into_slot_freed_by_rename() {
    // slot A is vacated by rename A->B; then something is created at slot A
    // rename must happen before the add
    assert_eq!(
        vec![rename("A", "B"), add("A")],
        plan_actions(&[add("A"), rename("A", "B")])
    );
}

#[test]
fn test_add_into_slot_freed_by_chain() {
    // chain: A->B->C, then add into slot A
    // chain emits tail-first: (B,C),(A,B), then add at A
    assert_eq!(
        vec![rename("B", "C"), rename("A", "B"), add("A")],
        plan_actions(&[add("A"), rename("A", "B"), rename("B", "C")])
    );
}

#[test]
fn test_add_with_unrelated_rename() {
    assert_eq!(
        vec![rename("P", "Q"), add("A")],
        plan_actions(&[add("A"), rename("P", "Q")])
    );
}

#[test]
fn test_two_adds_with_rename_dependency() {
    // both adds depend on renames clearing their target slots
    assert_eq!(
        vec![rename("A", "B"), rename("C", "D"), add("A"), add("C")],
        plan_actions(&[add("A"), rename("A", "B"), add("C"), rename("C", "D")])
    );
}

// --- add + remove combinations -------------------------------------------

#[test]
fn test_add_and_remove_independent() {
    // no shared slots — both are free to go in input order
    assert_eq!(
        vec![delete("A"), add("B")],
        plan_actions(&[delete("A"), add("B")])
    );
}

#[test]
fn test_remove_slot_then_add_same_slot() {
    // slot A is removed, then slot A is re-created — delete must go first
    assert_eq!(
        vec![delete("A"), add("A")],
        plan_actions(&[add("A"), delete("A")])
    );
}

#[test]
fn test_remove_slot_add_same_slot_plus_unrelated_rename() {
    assert_eq!(
        vec![delete("A"), rename("P", "Q"), add("A")],
        plan_actions(&[add("A"), delete("A"), rename("P", "Q")])
    );
}

#[test]
fn test_rename_into_removed_slot_then_add_original() {
    // slot A removed, slot B renamed to A, slot C added
    assert_eq!(
        vec![delete("A"), add("C"), rename("B", "A")],
        plan_actions(&[rename("B", "A"), delete("A"), add("C")])
    );
}

#[test]
fn test_chain_ending_in_remove_starting_with_add() {
    // add at A, rename A->B, delete B
    // delete goes immediately; then rename; then add
    assert_eq!(
        vec![delete("B"), rename("A", "B"), add("A")],
        plan_actions(&[add("A"), rename("A", "B"), delete("B")])
    );
}

// --- remove inside longer chains -----------------------------------------

#[test]
fn test_chain_with_remove_at_tail() {
    // rename A->B, rename B->C, delete C
    // C deleted first (unblocked), then B->C, then A->B
    assert_eq!(
        vec![delete("C"), rename("B", "C"), rename("A", "B")],
        plan_actions(&[rename("A", "B"), rename("B", "C"), delete("C")])
    );
}

#[test]
fn test_delete_middle_of_chain() {
    // delete B, rename C->B, rename A to something unrelated
    // delete first, then C->B (slot now free), then A->P
    assert_eq!(
        vec![delete("B"), rename("A", "P"), rename("C", "B")],
        plan_actions(&[rename("C", "B"), delete("B"), rename("A", "P")])
    );
}

// --- add / remove with cycles --------------------------------------------

#[test]
fn test_cycle_plus_remove() {
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("B", "A"),
            rename(".A.koil0", "B"),
            delete("P"),
        ],
        plan_actions(&[rename("A", "B"), rename("B", "A"), delete("P")])
    );
}

#[test]
fn test_cycle_plus_add() {
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("B", "A"),
            rename(".A.koil0", "B"),
            add("P"),
        ],
        plan_actions(&[rename("A", "B"), rename("B", "A"), add("P")])
    );
}

#[test]
fn test_cycle_plus_add_into_freed_slot() {
    // cycle swaps A<->B; separately, slot C freed by rename C->D, then add at C
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("B", "A"),
            rename(".A.koil0", "B"),
            rename("C", "D"),
            add("C"),
        ],
        plan_actions(&[
            rename("A", "B"),
            rename("B", "A"),
            add("C"),
            rename("C", "D"),
        ])
    );
}

#[test]
fn test_cycle_plus_remove_frees_for_rename() {
    // cycle A<->B; slot P deleted, then Q renamed into P
    assert_eq!(
        vec![
            rename("A", ".A.koil0"),
            rename("B", "A"),
            rename(".A.koil0", "B"),
            delete("P"),
            rename("Q", "P"),
        ],
        plan_actions(&[
            rename("A", "B"),
            rename("B", "A"),
            rename("Q", "P"),
            delete("P"),
        ])
    );
}

// --- multiple adds and removes together ----------------------------------

#[test]
fn test_many_adds_and_removes_independent() {
    assert_eq!(
        vec![delete("A"), delete("B"), add("P"), add("Q"),],
        plan_actions(&[delete("A"), delete("B"), add("P"), add("Q")])
    );
}

#[test]
fn test_add_remove_add_remove_interleaved() {
    // all independent slots
    assert_eq!(
        vec![delete("A"), delete("C"), add("B"), add("D")],
        plan_actions(&[delete("A"), add("B"), delete("C"), add("D")])
    );
}

// --- panic cases for add/remove ------------------------------------------

// #[test]
// #[should_panic]
// fn test_duplicate_add_destination_panics() {
//     // two adds targeting the same slot
//     plan_actions(&[add("A"), add("A")]);
// }

// #[test]
// #[should_panic]
// fn test_add_and_rename_same_destination_panics() {
//     // add and rename both want to create slot A
//     plan_actions(&[add("A"), rename("B", "A")]);
// }

// #[test]
// #[should_panic]
// fn test_duplicate_remove_source_panics() {
//     // two actions both clearing slot A
//     plan_actions(&[delete("A"), rename("A", "B")]);
// }

// --- copy: basic ----------------------------------------------------------

#[test]
fn test_copy_single() {
    assert_eq!(vec![copy("A", "B")], plan_actions(&[copy("A", "B")]));
}

#[test]
fn test_copy_into_occupied_slot() {
    // B must be renamed away before copy lands in B
    assert_eq!(
        vec![rename("B", "C"), copy("A", "B")],
        plan_actions(&[copy("A", "B"), rename("B", "C")])
    );
}

#[test]
fn test_copy_into_deleted_slot() {
    assert_eq!(
        vec![delete("B"), copy("A", "B")],
        plan_actions(&[copy("A", "B"), delete("B")])
    );
}

#[test]
fn test_copy_source_also_renamed() {
    // A is copied to C, and A is also renamed to B
    assert_eq!(
        vec![copy("A", "C"), rename("A", "B")],
        plan_actions(&[copy("A", "C"), rename("A", "B")])
    );
}

#[test]
fn test_copy_then_rename_into_copy_dst() {
    // copy needs A to exist, so rename first
    assert_eq!(
        vec![rename("C", "A"), copy("A", "B")],
        plan_actions(&[copy("A", "B"), rename("C", "A")])
    );
}

#[test]
fn test_two_copies_independent() {
    assert_eq!(
        vec![copy("A", "B"), copy("C", "D")],
        plan_actions(&[copy("A", "B"), copy("C", "D")])
    );
}

#[test]
fn test_copy_and_rename_independent() {
    assert_eq!(
        vec![rename("P", "Q"), copy("A", "B")],
        plan_actions(&[rename("P", "Q"), copy("A", "B")])
    );
}

#[test]
fn test_copy_dst_blocked_by_chain() {
    // chain B->C->D; copy A->B; B must be vacated before copy
    assert_eq!(
        vec![rename("C", "D"), rename("B", "C"), copy("A", "B")],
        plan_actions(&[copy("A", "B"), rename("B", "C"), rename("C", "D")])
    );
}

// TODO: this is very weird (B gets overwritten, should not happen)
// whoever is validating stuff should check for this, so it panics
#[test]
fn test_copy_dst_blocked_by_cycle() {
    // swap B<->C; copy A->B; swap must resolve before copy lands in B
    assert_eq!(
        vec![
            rename("B", ".B.koil0"),
            rename("C", "B"),
            rename(".B.koil0", "C"),
            copy("A", "B"),
        ],
        plan_actions(&[copy("A", "B"), rename("B", "C"), rename("C", "B")])
    );
}

#[test]
fn test_copy_with_add_and_delete() {
    assert_eq!(
        vec![delete("B"), add("C"), copy("A", "B")],
        plan_actions(&[copy("A", "B"), delete("B"), add("C")])
    );
}

////////////////////////////////////////////////////////////////////////////////

// --- path / directory rename ordering ------------------------------------

#[test]
fn test_remove_child_then_rename_parent_dir() {
    // remove a/b before renaming a/ -> c/ (so we don't move a/b along for the ride)
    assert_eq!(
        vec![delete("A/B"), rename("A", "C")],
        plan_actions(&[rename("A", "C"), delete("A/B")])
    );
}

#[test]
fn test_rename_child_before_renaming_parent_dir() {
    // a/b -> a/d must happen before a -> c, otherwise a/b is gone
    assert_eq!(
        vec![rename("A/B", "A/D"), rename("A", "C")],
        plan_actions(&[rename("A", "C"), rename("A/B", "A/D")])
    );
}

#[test]
fn test_add_child_before_renaming_parent_dir() {
    // adding a/b should happen before a is renamed to c
    assert_eq!(
        vec![add("A/B"), rename("A", "C")],
        plan_actions(&[add("A/B"), rename("A", "C")])
    );
}

#[test]
fn test_copy_out_of_dir_before_dir_rename() {
    // copy a/b -> x/b before renaming a -> c
    assert_eq!(
        vec![copy("A/B", "X/B"), rename("A", "C")],
        plan_actions(&[copy("A/B", "X/B"), rename("A", "C")])
    );
}

#[test]
fn test_multiple_child_removes_then_parent_rename() {
    // remove a/b and a/c before renaming a -> d
    assert_eq!(
        vec![delete("A/B"), delete("A/C"), rename("A", "D")],
        plan_actions(&[rename("A", "D"), delete("A/B"), delete("A/C")])
    );
}

#[test]
fn test_child_rename_and_child_remove_then_parent_rename() {
    // rename a/b -> a/d and remove a/c before renaming a -> e
    assert_eq!(
        vec![delete("A/C"), rename("A/B", "A/D"), rename("A", "E")],
        plan_actions(&[rename("A", "E"), rename("A/B", "A/D"), delete("A/C")])
    );
}

#[test]
fn test_nested_dir_rename_order() {
    // a/b -> a/c must happen before a -> d
    // d/c -> e must happen after a is renamed (it's now d/c)
    assert_eq!(
        vec![rename("A/B", "A/C"), rename("A", "D")],
        plan_actions(&[rename("A", "D"), rename("A/B", "A/C")])
    );
}

#[test]
fn test_two_sibling_dir_renames_with_child_removals() {
    // a/x removed before a -> b; p/y removed before p -> q
    assert_eq!(
        vec![
            delete("A/X"),
            delete("P/Y"),
            rename("A", "B"),
            rename("P", "Q")
        ],
        plan_actions(&[
            rename("A", "B"),
            rename("P", "Q"),
            delete("A/X"),
            delete("P/Y")
        ])
    );
}

// This probably passes only because of the sorting
#[test]
fn test_add_child() {
    assert_eq!(
        vec![add("A"), add("A/B")],
        plan_actions(&[add("A/B"), add("A")])
    );
}

// --- creating inside a new directory ---------------------------------------

#[test]
fn test_add_dir_before_file_inside() {
    // `CreateFile` sorts before `CreateDir`, so this needs a real dependency
    assert_eq!(
        vec![add_dir("A"), add("A/B")],
        plan_actions(&[add("A/B"), add_dir("A")])
    );
}

#[test]
fn test_add_dir_before_nested_dir() {
    assert_eq!(
        vec![add_dir("A"), add_dir("A/B"), add("A/B/C")],
        plan_actions(&[add("A/B/C"), add_dir("A/B"), add_dir("A")])
    );
}

#[test]
fn test_add_dir_before_rename_into_it() {
    assert_eq!(
        vec![add_dir("A"), rename("X", "A/X")],
        plan_actions(&[rename("X", "A/X"), add_dir("A")])
    );
}

#[test]
fn test_add_dir_before_copy_into_it() {
    assert_eq!(
        vec![add_dir("A"), copy("X", "A/X")],
        plan_actions(&[copy("X", "A/X"), add_dir("A")])
    );
}

#[test]
fn test_rename_dir_before_add_inside_new_name() {
    // A -> C must happen before C/B can be created
    assert_eq!(
        vec![rename("A", "C"), add("C/B")],
        plan_actions(&[add("C/B"), rename("A", "C")])
    );
}

#[test]
fn test_swap_children_before_swapping_parent_dirs() {
    // A/x and A/y are inside the old A, so they must be swapped before B takes A's place
    assert_eq!(
        vec![
            rename("A/x", "A/.x.koil0"),
            rename("A/y", "A/x"),
            rename("A/.x.koil0", "A/y"),
            rename("A", ".A.koil0"),
            rename("B", "A"),
            rename(".A.koil0", "B")
        ],
        plan_actions(&[
            rename("A", "B"),
            rename("B", "A"),
            rename("A/x", "A/y"),
            rename("A/y", "A/x")
        ])
    );
}

#[test]
fn test_add_dir_does_not_affect_siblings() {
    // AB is not inside A, so no dependency (plain sort order)
    assert_eq!(
        vec![add("AB"), add_dir("A")],
        plan_actions(&[add_dir("A"), add("AB")])
    );
}

// --- determinism ------------------------------------------------------------

/// Every ordering of `items`
fn permutations(items: &[Action]) -> Vec<Vec<Action>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut result = Vec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let first = rest.remove(i);
        for mut perm in permutations(&rest) {
            perm.insert(0, first.clone());
            result.push(perm);
        }
    }
    result
}

#[test]
fn test_output_does_not_depend_on_input_order() {
    let inputs = [
        // a 3-cycle, a swap and an unrelated delete
        vec![
            rename("A", "B"),
            rename("B", "C"),
            rename("C", "A"),
            rename("P", "Q"),
            rename("Q", "P"),
            delete("X"),
        ],
        // nested swaps, plus a file inside a new dir
        vec![
            rename("A", "B"),
            rename("B", "A"),
            rename("A/x", "A/y"),
            rename("A/y", "A/x"),
            add_dir("N"),
            add("N/f"),
        ],
        // a swap that depends on a copy, and a rename into a deleted slot
        vec![
            rename("A", "B"),
            rename("B", "A"),
            rename("Q", "P"),
            delete("P"),
            copy("A", "Z"),
            rename("D", "E"),
        ],
    ];

    for input in inputs {
        let expected = plan_actions(&input);
        for perm in permutations(&input) {
            // run each ordering a few times, since `pathfinding` uses randomly seeded hashing
            for _ in 0..3 {
                assert_eq!(expected, plan_actions(&perm), "input: {perm:?}");
            }
        }
    }
}

// --- temporary names ----------------------------------------------------------

#[test]
fn test_temp_is_next_to_renamed_file() {
    assert_eq!(
        vec![
            rename("dir/A", "dir/.A.koil0"),
            rename("dir/B", "dir/A"),
            rename("dir/.A.koil0", "dir/B")
        ],
        plan_actions(&[rename("dir/A", "dir/B"), rename("dir/B", "dir/A")])
    );
}

#[test]
fn test_temp_skips_existing_paths() {
    let taken = |p: &Path| p == Path::new(".A.koil0") || p == Path::new(".A.koil1");
    assert_eq!(
        vec![
            rename("A", ".A.koil2"),
            rename("B", "A"),
            rename(".A.koil2", "B")
        ],
        planner::plan_actions(&[rename("A", "B"), rename("B", "A")], taken)
    );
}

#[test]
fn test_temp_skips_paths_used_by_plan() {
    // `.A.koil0` is created by the plan itself, so it can't be used as a temp
    assert_eq!(
        vec![
            rename("A", ".A.koil1"),
            rename("B", "A"),
            rename(".A.koil1", "B"),
            add(".A.koil0")
        ],
        plan_actions(&[rename("A", "B"), rename("B", "A"), add(".A.koil0")])
    );
}

#[test]
fn test_order_keeps_cycles() {
    assert_eq!(
        vec![
            vec![rename("A", "B"), rename("B", "A")],
            vec![delete("E")],
            vec![rename("C", "D")]
        ],
        planner::order(&[
            delete("E"),
            rename("C", "D"),
            rename("B", "A"),
            rename("A", "B")
        ])
    );
}

#[test]
fn test_needs_swap() {
    // each frees the path the other one takes
    let needs = planner::needs(&[rename("A", "B"), rename("B", "A")]);
    assert_eq!(needs, [vec![1], vec![0]]);
}

#[test]
fn test_needs_chain() {
    // only directly, `A -> B` needs `C -> D` through `B -> C`
    let needs = planner::needs(&[rename("A", "B"), rename("B", "C"), rename("C", "D")]);
    assert_eq!(needs, [vec![1], vec![2], vec![]]);
}

#[test]
fn test_needs_delete() {
    let needs = planner::needs(&[delete("B"), rename("A", "B"), copy("C", "D")]);
    assert_eq!(needs, [vec![], vec![0], vec![]]);
}

#[test]
fn test_needs_new_dirs() {
    let needs = planner::needs(&[
        add_dir("new"),
        add_dir("new/nested"),
        add("new/nested/file"),
        rename("A", "new/A"),
        copy("dir", "dir2"),
        add("dir2/file"),
        add("other/file"),
    ]);
    assert_eq!(
        needs,
        [
            vec![],
            vec![0],
            vec![0, 1],
            vec![0],
            vec![],
            vec![4],
            vec![]
        ]
    );
}

#[test]
fn test_needs_independent() {
    // a copy and a rename of one path, and a move out of a deleted dir, run in either order
    let needs = planner::needs(&[
        copy("A", "B"),
        rename("A", "C"),
        delete("dir"),
        rename("dir/x", "x"),
    ]);
    assert!(needs.iter().all(Vec::is_empty));
}

#[test]
fn test_needs_dir_taken_again() {
    // `P/new` goes into the old `P`, before it is renamed, so it does not need `R -> P`
    let needs = planner::needs(&[rename("P", "Q"), rename("R", "P"), add("P/new")]);
    assert_eq!(needs, [vec![], vec![0], vec![]]);
}
