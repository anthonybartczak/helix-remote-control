//! Path rules shared by the `usb` and `fake` backends: which cell a block may
//! go into and where the split and merge may sit, checked on
//! [`parse::Routing`] before anything is written.
//!
//! Path B's cell `n` is drawn under Path A's cell `n`, and a split and merge
//! attach before a cell of Path A. Path B's blocks are kept between them, so
//! the drawing never shows a block on Path B that the bracket does not reach.
//! The first block on an empty Path B claims the whole line (split after the
//! input, merge before the output), as TonePush's GUI does: the device
//! initialises the attach points to zero, which parallels nothing.

use serde::Deserialize;

use crate::device::{DeviceError, Result};
use crate::parse::Routing;

/// The fork or the merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Junction {
    Split,
    Join,
}

/// Where a block is about to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A cell of Path A.
    Main,
    /// A cell of Path B. `claim`: Path B is empty, so the split and merge
    /// must be attached around the whole line after the block lands.
    Branch { claim: bool },
}

/// Cell of Path B (left to right) a position is, if it is one.
pub fn branch_cell(r: &Routing, position: u32) -> Option<u32> {
    r.branch
        .iter()
        .position(|&p| p == position)
        .map(|i| i as u32)
}

/// Where the split and merge sit while Path B is in use: (split, merge), as
/// cells of Path A they sit before.
pub fn bracket(r: &Routing) -> Option<(u32, u32)> {
    Some((r.split.as_ref()?.before?, r.join.as_ref()?.before?))
}

/// Path B's occupied cells, left to right.
fn used(r: &Routing, occupied: impl Fn(u32) -> bool) -> Vec<u32> {
    r.branch
        .iter()
        .enumerate()
        .filter(|&(_, &p)| occupied(p))
        .map(|(i, _)| i as u32)
        .collect()
}

/// Check that a block may go into the empty cell at `position`. A move onto
/// Path B is checked against the bracket as it stands, even when the moving
/// block is Path B's only one.
pub fn check_cell(r: &Routing, position: u32) -> Result<Place> {
    if r.main.contains(&position) {
        return Ok(Place::Main);
    }
    let Some(cell) = branch_cell(r, position) else {
        return Err(DeviceError::Invalid(format!(
            "position {position} is not a block slot on Path A or Path B"
        )));
    };
    match bracket(r) {
        // Path B is empty: this block claims the whole line, any cell will do.
        None => Ok(Place::Branch { claim: true }),
        Some((split, join)) if (split..join).contains(&cell) => Ok(Place::Branch { claim: false }),
        Some((split, join)) => Err(DeviceError::Invalid(format!(
            "B{} is outside the split and merge (B{} to B{join}); move them first",
            cell + 1,
            split + 1,
        ))),
    }
}

/// Check that the split or merge may sit before cell `before` of Path A:
/// split before merge, inside the path, and every block of Path B still
/// between them. `occupied` says whether a position holds a block.
pub fn check_junction(
    r: &Routing,
    occupied: impl Fn(u32) -> bool,
    which: Junction,
    before: u32,
) -> Result<()> {
    let (split, join) = bracket(r)
        .ok_or_else(|| DeviceError::Invalid("Path B is empty: add a block to it first".into()))?;
    let (split, join) = match which {
        Junction::Split => (before, join),
        Junction::Join => (split, before),
    };
    let cells = r.main.len() as u32;
    if join > cells {
        return Err(DeviceError::Invalid(format!(
            "Path A has {cells} cells; the merge can sit before the output at most"
        )));
    }
    if split >= join {
        return Err(DeviceError::Invalid(
            "the split must come before the merge".into(),
        ));
    }
    let blocks = used(r, occupied);
    if let (Some(&first), Some(&last)) = (blocks.first(), blocks.last()) {
        if first < split || last >= join {
            return Err(DeviceError::Invalid(format!(
                "Path B's blocks run from B{} to B{}; the split and merge must stay around them",
                first + 1,
                last + 1
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Junction as At;

    /// Path A 2-10, Path B 13-21, split 12 and join 22, as the fake has them.
    fn routing(bracket: Option<(u32, u32)>) -> Routing {
        Routing {
            main: (2..=10).collect(),
            branch: (13..=21).collect(),
            split: Some(At {
                position: 12,
                before: bracket.map(|b| b.0),
            }),
            join: Some(At {
                position: 22,
                before: bracket.map(|b| b.1),
            }),
        }
    }

    #[test]
    fn the_first_block_on_path_b_claims_the_line() {
        let r = routing(None);
        assert_eq!(check_cell(&r, 3).unwrap(), Place::Main);
        assert_eq!(check_cell(&r, 17).unwrap(), Place::Branch { claim: true });
        assert!(check_cell(&r, 11).is_err(), "the output");
        assert!(check_cell(&r, 12).is_err(), "the split");
    }

    #[test]
    fn path_b_blocks_stay_under_the_bracket() {
        // Split before cell 1, merge before cell 5: B2 to B5.
        let r = routing(Some((1, 5)));
        assert_eq!(check_cell(&r, 15).unwrap(), Place::Branch { claim: false });
        assert!(check_cell(&r, 13).is_err(), "B1");
        assert!(check_cell(&r, 18).is_err(), "B6");
    }

    #[test]
    fn junctions_keep_their_order_and_path_b_between_them() {
        let r = routing(Some((1, 5)));
        let on_b2 = |p| p == 14;
        assert!(check_junction(&r, on_b2, Junction::Split, 0).is_ok());
        assert!(
            check_junction(&r, on_b2, Junction::Join, 9).is_ok(),
            "before the output"
        );
        assert!(
            check_junction(&r, on_b2, Junction::Join, 10).is_err(),
            "past the output"
        );
        assert!(
            check_junction(&r, on_b2, Junction::Split, 2).is_err(),
            "past B2"
        );
        assert!(
            check_junction(&r, on_b2, Junction::Join, 1).is_err(),
            "before B2"
        );
        assert!(
            check_junction(&r, |_| false, Junction::Split, 5).is_err(),
            "split at merge"
        );
        assert!(check_junction(&routing(None), |_| false, Junction::Split, 0).is_err());
    }
}
