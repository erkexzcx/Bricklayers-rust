//! Plane geometry, shared by both transforms.
//!
//! Neither transform has a model to work from — only the paths a slicer wrote —
//! so both of them have to recover shape from those paths. [`footprint`]
//! quantises where a layer's material sits, which is what answers "is anything
//! above this?" and, for [`zaa`](crate::zaa), how far it is across the strip a
//! layer leaves exposed. [`inset`] moves a closed loop sideways without
//! changing what kind of loop it is.
//!
//! The two are re-exported together because callers want a `Cells` and an
//! `Edge` in the same breath and do not care which file each came from.

pub mod footprint;
pub mod inset;

pub use footprint::{Arc, CELL, Cells, Grid, Trace, cells, turn};
pub use inset::Edge;
