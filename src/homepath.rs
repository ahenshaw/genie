//! The lineage from the selected person back to the home person, so views
//! can keep it on screen and highlight it: after climbing a tree, the way
//! back down is always visible.

use std::sync::Arc;

use egui::{Color32, Context, Id};

use crate::kinship::{self, Kinship, Link};
use crate::model::Document;
use crate::views::app_people;

/// The shortest lineage from `from` to the home person, cached per tree
/// revision. `None` when there's no home person, `from` is home, or the
/// two aren't related.
pub fn find(ctx: &Context, doc: &Document, from: &str) -> Option<Arc<HomePath>> {
    let home = app_people(ctx).1?;
    if home == from || doc.person(&home).is_none() {
        return None;
    }
    let key = Id::new("home_path");
    type Cached = (u64, String, String, Option<Arc<HomePath>>);
    if let Some((rev, f, h, path)) = ctx.data(|d| d.get_temp::<Cached>(key))
        && rev == doc.revision
        && f == from
        && h == home
    {
        return path;
    }
    let path = kinship::find(doc, from, &home).into_iter().next().map(|k| Arc::new(HomePath(k)));
    ctx.data_mut(|d| d.insert_temp::<Cached>(key, (doc.revision, from.to_string(), home, path.clone())));
    path
}

/// Amber, to stand apart from the blue selection ring.
pub fn color(p: &elegance::Palette) -> Color32 {
    p.amber
}

#[derive(Debug)]
pub struct HomePath(Kinship);

impl HomePath {
    pub fn home(&self) -> &str {
        self.0.people.last().map(String::as_str).unwrap_or("")
    }

    /// The relative one step toward home.
    pub fn next(&self) -> Option<&str> {
        self.0.people.get(1).map(String::as_str)
    }

    /// Whether `a` and `b` are neighbours on the path, either way round.
    pub fn joins(&self, a: &str, b: &str) -> bool {
        self.0.people.windows(2).any(|w| (w[0] == a && w[1] == b) || (w[0] == b && w[1] == a))
    }

    /// The descendants the path runs through while it only goes down from
    /// its start: child, grandchild, … (all of it when home is a descendant).
    pub fn down_line(&self) -> &[String] {
        let n = self.0.links.iter().take_while(|l| **l == Link::Down).count();
        &self.0.people[1..=n]
    }
}
