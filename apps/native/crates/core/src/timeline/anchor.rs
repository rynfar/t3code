//! Keeps the reader's place when transcript rows change, as the web list's
//! `maintainVisibleContentPosition` does (`MessagesTimeline.tsx`): the first row in view stays
//! where it was on screen when rows are inserted or replaced around it, such as the earlier turns
//! "Load earlier turns" brings in above it. Positions are the list's logical scroll top, an item
//! and how far into it the viewport starts; items before the rows (the history control) are the
//! header, which the web renders outside its data as `ListHeaderComponent`.

/// The list's scroll position: the viewport's top is `offset` px into `item` (a negative offset
/// leaves `item` that far below it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollTop {
    pub item: usize,
    pub offset: f32,
}

/// The rows in view before a change: id, row index, and the row's top relative to the
/// viewport's top (negative while it is cut off above).
#[derive(Debug, Clone, PartialEq)]
pub struct VisibleRows {
    scroll_top: ScrollTop,
    header: usize,
    rows: Vec<(String, usize, f32)>,
    disclosure: bool,
}

/// Where to put the scroll top after a change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anchor {
    At(ScrollTop),
    /// `item`'s top sits `gap` px below the viewport's top, with the rows above it in the gap.
    Below { item: usize, gap: f32 },
}

/// The rows in view, measured with `item_top` (an item's top in the list's content). `None` while
/// the list follows its tail or has not laid out, unless a disclosure anchors its own row.
pub fn capture<T>(rows: &[T], id: impl Fn(&T) -> &str, header: usize, scroll_top: ScrollTop, viewport: f32, item_top: impl Fn(usize) -> f32, disclosure: Option<&str>) -> Option<VisibleRows> {
    let at_tail = scroll_top.item >= header + rows.len();
    if viewport <= 0. || (at_tail && disclosure.is_none()) {
        return None;
    }
    // At the tail GPUI reports the end of all items (including the footer), not the viewport top.
    let scrolled = item_top(scroll_top.item) + scroll_top.offset - if at_tail { viewport } else { 0. };
    if let Some(key) = disclosure {
        let index = rows.iter().position(|row| id(row) == key)?;
        let top = item_top(header + index) - scrolled;
        return (top < viewport).then(|| VisibleRows { scroll_top, header, rows: vec![(key.to_owned(), index, top)], disclosure: true });
    }
    let first = scroll_top.item.max(header) - header;
    let rows: Vec<(String, usize, f32)> = (first..rows.len())
        .map(|index| (index, item_top(header + index) - scrolled))
        .take_while(|(_, top)| *top < viewport)
        .map(|(index, top)| (id(&rows[index]).to_owned(), index, top))
        .collect();
    (!rows.is_empty()).then_some(VisibleRows { scroll_top, header, rows, disclosure: false })
}

/// Puts the first captured row that is still there back where it was. `None` when the list
/// already shows it there (`current` is the list's scroll top after the change) or none is left.
pub fn restore<T>(visible: &VisibleRows, rows: &[T], id: impl Fn(&T) -> &str, header: usize, current: ScrollTop) -> Option<Anchor> {
    let (candidate, index, old_index, top) = visible.rows.iter().enumerate().find_map(|(candidate, (row_id, old_index, top))| {
        let same_place = rows.get(*old_index).is_some_and(|row| id(row) == row_id);
        let index = if same_place { Some(*old_index) } else { rows.iter().position(|row| id(row) == row_id) };
        index.map(|index| (candidate, index, *old_index, *top))
    })?;
    let item = header + index;
    // Unmoved only when the first row in view kept its place: a row in view above the anchor that
    // went may have taken a different height with it.
    if !visible.disclosure && candidate == 0 && item == visible.header + old_index && current == visible.scroll_top {
        return None;
    }
    let anchor = if visible.disclosure || top <= 0. { Anchor::At(ScrollTop { item, offset: -top }) } else { Anchor::Below { item, gap: top } };
    (anchor != Anchor::At(current)).then_some(anchor)
}

/// Includes rows above a negatively offset anchor in the paint range, preserving any remaining
/// bottom-alignment space above the first item instead of clamping it away.
pub fn include_preceding_rows(mut top: ScrollTop, item_height: impl Fn(usize) -> f32) -> ScrollTop {
    while top.offset < 0. && top.item > 0 {
        top.item -= 1;
        top.offset += item_height(top.item);
    }
    top
}

#[cfg(test)]
mod tests;
