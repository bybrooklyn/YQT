// SPDX-License-Identifier: GPL-3.0-only
//! List models. Everything with a rule in it (queue editing, paging, caching, row building) is plain Rust in
//! the `*State` types and tested without Qt; the cxx-qt objects on top only translate [`Change`]s into
//! `beginInsertRows` / `beginMoveRows` / … calls.

pub mod items;
pub mod pager;
pub mod queue;

/// A precise description of how a list changed, in the order a view must be told about it.
#[derive(Debug, Clone, PartialEq)]
pub enum Change<T> {
    /// Everything is new (the list is now exactly these rows).
    Reset(Vec<T>),
    Insert {
        at: usize,
        rows: Vec<T>,
    },
    Remove {
        at: usize,
        count: usize,
    },
    /// The row at `from` ends up at index `to` (indices as in the list before / after the move).
    Move {
        from: usize,
        to: usize,
    },
    /// One row's content changed in place.
    Update {
        at: usize,
        row: T,
    },
}

/// Apply a change to a plain vector. This is exactly what a mirror (a Qt model) does, and what the tests use to
/// prove a state's change descriptors are complete.
pub fn apply<T: Clone>(list: &mut Vec<T>, change: &Change<T>) {
    match change {
        Change::Reset(rows) => *list = rows.clone(),
        Change::Insert { at, rows } => {
            let tail = list.split_off(*at);
            list.extend(rows.iter().cloned());
            list.extend(tail);
        }
        Change::Remove { at, count } => {
            list.drain(*at..*at + *count);
        }
        Change::Move { from, to } => {
            let row = list.remove(*from);
            list.insert(*to, row);
        }
        Change::Update { at, row } => list[*at] = row.clone(),
    }
}

/// Tell a cxx-qt `QAbstractListModel` about a [`Change`], then mutate its mirror vector. `$this` is the
/// `Pin<&mut Model>`, `$rows` a closure-free expression giving `&mut Vec<T>` (evaluated after each begin call).
/// A macro because every bridge has its own `begin_*` wrapper methods.
#[macro_export]
macro_rules! apply_to_model {
    ($this:expr, $change:expr, $rows:expr, $roles:expr) => {{
        use $crate::models::Change;
        let parent = cxx_qt_lib::QModelIndex::default();
        match $change {
            Change::Reset(new_rows) => unsafe {
                $this.as_mut().begin_reset_model();
                *$rows = new_rows;
                $this.as_mut().end_reset_model();
            },
            Change::Insert { at, rows } => {
                if !rows.is_empty() {
                    unsafe {
                        $this.as_mut().begin_insert_rows(&parent, at as i32, (at + rows.len() - 1) as i32);
                        $crate::models::apply($rows, &Change::Insert { at, rows });
                        $this.as_mut().end_insert_rows();
                    }
                }
            }
            Change::Remove { at, count } => {
                if count > 0 {
                    unsafe {
                        $this.as_mut().begin_remove_rows(&parent, at as i32, (at + count - 1) as i32);
                        $rows.drain(at..at + count);
                        $this.as_mut().end_remove_rows();
                    }
                }
            }
            Change::Move { from, to } => {
                if from != to {
                    // Qt's destination is "the row to insert before", in the numbering before the move.
                    let dest = if to > from { to + 1 } else { to };
                    unsafe {
                        if $this.as_mut().begin_move_rows(&parent, from as i32, from as i32, &parent, dest as i32) {
                            let row = $rows.remove(from);
                            $rows.insert(to, row);
                            $this.as_mut().end_move_rows();
                        }
                    }
                }
            }
            Change::Update { at, row } => {
                $rows[at] = row;
                let ix = $this.index(at as i32, 0, &parent);
                let mut roles = cxx_qt_lib::QVector::<i32>::default();
                for r in $roles {
                    roles.append(r);
                }
                $this.as_mut().data_changed(&ix, &ix, &roles);
            }
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_matches_the_descriptions() {
        let mut v = vec![1, 2, 3];
        apply(&mut v, &Change::Insert { at: 1, rows: vec![9, 8] });
        assert_eq!(v, [1, 9, 8, 2, 3]);
        apply(&mut v, &Change::Remove { at: 1, count: 2 });
        assert_eq!(v, [1, 2, 3]);
        apply(&mut v, &Change::Move { from: 0, to: 2 });
        assert_eq!(v, [2, 3, 1]);
        apply(&mut v, &Change::Update { at: 0, row: 7 });
        assert_eq!(v, [7, 3, 1]);
        apply(&mut v, &Change::Reset(vec![5]));
        assert_eq!(v, [5]);
    }
}
