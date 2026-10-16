//! Working every cell out, each formula after the cells it reads.
//!
//! The old engine (5c6a55aa^) evaluated formulas in one pass in reading order, so `=B1` in A1
//! read B1's value from before the edit, and a loop between two cells was never noticed. Here the
//! formulas are put in dependency order first (Kahn's algorithm over the cells and ranges each
//! one names); whatever cannot be ordered is in a loop or depends on one, and says so.

use std::collections::VecDeque;

use crate::address::Cell;
use crate::formula::{self, eval::Eval, eval::Grid, Error, Kind, Value};

struct View<'a> {
    rows: usize,
    cols: usize,
    values: &'a [Value],
}

impl Grid for View<'_> {
    fn value(&self, cell: Cell) -> Value {
        if cell.row < self.rows && cell.col < self.cols {
            self.values[cell.row * self.cols + cell.col].clone()
        } else {
            Value::Empty
        }
    }
    fn rows(&self) -> usize {
        self.rows
    }
    fn cols(&self) -> usize {
        self.cols
    }
}

/// The value of every cell of a `rows × cols` grid whose text is `raw`, row by row.
pub fn recalc(rows: usize, cols: usize, raw: &[String]) -> Vec<Value> {
    let mut values: Vec<Value> = Vec::with_capacity(raw.len());
    let mut formulas: Vec<(usize, Result<formula::Expr, Error>)> = Vec::new();
    for (i, text) in raw.iter().enumerate() {
        if formula::is_formula(text) {
            let looped = Error::new(
                Kind::Cycle,
                format!(
                    "{} is part of a loop of references (it ends up reading itself), or reads a cell that is",
                    at(i, cols).name()
                ),
            );
            values.push(Value::Error(looped));
            formulas.push((i, formula::compile(text)));
        } else {
            values.push(Value::from_entry(text));
        }
    }

    // Where each formula sits in `formulas`, by grid index, so a reference finds its formula.
    let mut slot = vec![usize::MAX; raw.len()];
    for (k, (i, _)) in formulas.iter().enumerate() {
        slot[*i] = k;
    }

    // Edges: a formula another formula reads must be worked out first.
    let mut readers: Vec<Vec<usize>> = vec![Vec::new(); formulas.len()];
    let mut waiting = vec![0usize; formulas.len()];
    for (k, (_, compiled)) in formulas.iter().enumerate() {
        let Ok(expr) = compiled else { continue };
        let (mut cells, mut ranges) = (Vec::new(), Vec::new());
        formula::references(expr, &mut cells, &mut ranges);
        let mut deps: Vec<usize> = cells
            .iter()
            .filter(|c| c.row < rows && c.col < cols)
            .map(|c| slot[c.row * cols + c.col])
            .filter(|&d| d != usize::MAX)
            .collect();
        for r in &ranges {
            deps.extend(formulas.iter().enumerate().filter_map(|(d, (i, _))| {
                let c = at(*i, cols);
                (c.row >= r.first.row && c.row <= r.last.row && c.col >= r.first.col && c.col <= r.last.col)
                    .then_some(d)
            }));
        }
        deps.sort_unstable();
        deps.dedup();
        waiting[k] = deps.len();
        for d in deps {
            readers[d].push(k);
        }
    }

    let mut ready: VecDeque<usize> = (0..formulas.len()).filter(|&k| waiting[k] == 0).collect();
    while let Some(k) = ready.pop_front() {
        let (i, compiled) = &formulas[k];
        let v = match compiled {
            Err(e) => Value::Error(e.clone()),
            Ok(expr) => {
                let view = View { rows, cols, values: &values };
                Eval { grid: &view }.run(expr)
            }
        };
        values[*i] = v;
        for &r in &readers[k] {
            waiting[r] -= 1;
            if waiting[r] == 0 {
                ready.push_back(r);
            }
        }
    }
    // Anything still waiting keeps the loop error it started with.
    values
}

fn at(i: usize, cols: usize) -> Cell {
    Cell::new(i / cols.max(1), i % cols.max(1))
}
