//! What's new: changes worth telling the people who use Genie about.
//!
//! Add an entry at the top with the next `id` when something they'd notice
//! changes. Everyone sees a banner for entries newer than the last one they
//! dismissed, and Help → What's new lists them all.

pub struct Item {
    /// Increases with each entry; what "seen" is measured by.
    pub id: u32,
    pub date: &'static str,
    pub title: &'static str,
    pub body: &'static str,
}

/// Newest first.
pub const NEWS: &[Item] = &[Item {
    id: 1,
    date: "9 October 2026",
    title: "You can now record a divorce",
    body: "Edit a person, and in Marriages turn on Divorced next to the spouse's name. Add the date and place if you know them, or leave them blank. It shows on the timeline once saved.",
}];

/// The newest entry's id: what dismissing the banner marks as seen.
pub fn latest() -> u32 {
    NEWS.first().map_or(0, |n| n.id)
}

/// Entries newer than `seen`, newest first.
pub fn unseen(seen: u32) -> &'static [Item] {
    let n = NEWS.iter().take_while(|n| n.id > seen).count();
    &NEWS[..n]
}
