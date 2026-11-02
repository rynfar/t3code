//! Thread ordering: `client-runtime/src/state/threadSort.ts` and the inbox sorts in
//! `client-runtime/src/state/threadInbox.ts`.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use super::drag::Assignment;
use super::thread::SidebarThread;
use super::time::Ms;
use crate::thread::locale_compare;

/// `resolveSettledThreadTimestamp`.
pub fn settled_timestamp(thread: &SidebarThread) -> Option<Ms> {
    if let Some(settled_at) = thread.settled_at {
        return Some(settled_at);
    }
    let run = thread.latest_run.as_ref();
    [thread.latest_user_message_at, run.and_then(|r| r.requested_at), run.and_then(|r| r.started_at), run.and_then(|r| r.completed_at)]
        .into_iter()
        .flatten()
        .max()
        .or(thread.updated_at)
}

/// `sortSettledThreads`: settle time desc, then plain id order as the server pages ties.
pub fn sort_settled(threads: &mut [&SidebarThread]) {
    threads.sort_by(|left, right| {
        let (l, r) = (settled_timestamp(left).unwrap_or(0), settled_timestamp(right).unwrap_or(0));
        r.cmp(&l).then_with(|| left.id.cmp(&right.id))
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreadSortOrder {
    #[default]
    UpdatedAt,
    CreatedAt,
}

/// `getThreadSortTimestamp` (shell threads carry no messages, so `latestUserMessageAt` decides).
pub fn thread_sort_timestamp(thread: &SidebarThread, order: ThreadSortOrder) -> Option<Ms> {
    match order {
        ThreadSortOrder::CreatedAt => thread.created_at.or(thread.updated_at),
        ThreadSortOrder::UpdatedAt => thread.latest_user_message_at.or(thread.updated_at).or(thread.created_at),
    }
}

/// `sortThreads`: newest first, ties by id descending.
pub fn sort_threads(threads: &mut [&SidebarThread], order: ThreadSortOrder) {
    threads.sort_by(|left, right| {
        let (l, r) = (thread_sort_timestamp(left, order), thread_sort_timestamp(right, order));
        r.cmp(&l).then_with(|| right.id.cmp(&left.id))
    });
}

fn identity_tiebreak(left: &SidebarThread, right: &SidebarThread) -> Ordering {
    locale_compare(&left.id, &right.id)
}

/// `sortPinnedThreadsByOrderKey`: keyed rows by key, then keyless rows newest first.
pub fn sort_pinned(threads: &mut Vec<&SidebarThread>) {
    let (mut keyed, mut keyless): (Vec<&SidebarThread>, Vec<&SidebarThread>) = threads.iter().partition(|thread| thread.pin_order_key.is_some());
    keyed.sort_by(|left, right| left.pin_order_key.cmp(&right.pin_order_key).then_with(|| identity_tiebreak(left, right)));
    keyless.sort_by(|left, right| right.created_at.unwrap_or(0).cmp(&left.created_at.unwrap_or(0)).then_with(|| identity_tiebreak(left, right)));
    keyed.extend(keyless);
    *threads = keyed;
}

fn active_anchor(thread: &SidebarThread) -> Ms {
    thread.created_at.unwrap_or(0).max(thread.unsettled_at.unwrap_or(0))
}

/// `sortActiveThreadsByOrderKey`: keyless rows first by anchor, then keyed rows by key.
pub fn sort_active_by_order_key(threads: &mut [&SidebarThread]) {
    threads.sort_by(|left, right| {
        let order = match (&left.active_order_key, &right.active_order_key) {
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(l), Some(r)) => l.cmp(r),
            (None, None) => active_anchor(right).cmp(&active_anchor(left)),
        };
        order.then_with(|| identity_tiebreak(left, right))
    });
}

fn sort_newest_first(threads: &mut [&SidebarThread], timestamp: impl Fn(&SidebarThread) -> Ms) {
    threads.sort_by(|left, right| timestamp(right).cmp(&timestamp(left)).then_with(|| identity_tiebreak(left, right)));
}

/// `sortInboxThreadsByReturn`.
pub fn sort_inbox_by_return(threads: &mut [&SidebarThread], observed_return_at: impl Fn(&SidebarThread) -> Option<Ms>) {
    sort_newest_first(threads, |thread| {
        let run = thread.latest_run.as_ref();
        [
            thread.created_at,
            thread.unsettled_at,
            run.and_then(|r| r.requested_at),
            run.and_then(|r| r.completed_at),
            observed_return_at(thread),
        ]
        .into_iter()
        .map(|at| at.unwrap_or(0))
        .max()
        .unwrap_or(0)
    });
}

/// `sortWorkingThreadsBySend`.
pub fn sort_working_by_send(threads: &mut [&SidebarThread]) {
    sort_newest_first(threads, |thread| {
        let sent = match thread.latest_user_authored_message_at {
            None => thread.latest_run.as_ref().and_then(|run| run.requested_at),
            Some(at) => at,
        };
        thread.created_at.unwrap_or(0).max(sent.unwrap_or(0))
    });
}

const PIN_ORDER_DIGITS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

fn is_valid_pin_order_key(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|b| PIN_ORDER_DIGITS.contains(&b)) && !key.ends_with('a')
}

fn digit(c: Option<u8>) -> Option<usize> {
    c.and_then(|c| PIN_ORDER_DIGITS.iter().position(|d| *d == c))
}

fn pin_order_midpoint(a: &str, b: &str) -> String {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    if !b.is_empty() {
        let mut n = 0;
        while n < bb.len() && ab.get(n).copied().unwrap_or(PIN_ORDER_DIGITS[0]) == bb[n] {
            n += 1;
        }
        if n > 0 {
            return format!("{}{}", &b[..n], pin_order_midpoint(a.get(n..).unwrap_or(""), &b[n..]));
        }
    }
    let digit_a = if a.is_empty() { 0 } else { digit(ab.first().copied()).unwrap_or(0) };
    let digit_b = if b.is_empty() { PIN_ORDER_DIGITS.len() } else { digit(bb.first().copied()).unwrap_or(0) };
    if digit_b - digit_a > 1 {
        let mid = ((digit_a + digit_b) as f64 / 2.0).round() as usize;
        return (PIN_ORDER_DIGITS[mid] as char).to_string();
    }
    if b.len() > 1 {
        return b[..1].to_owned();
    }
    format!("{}{}", PIN_ORDER_DIGITS[digit_a] as char, pin_order_midpoint(a.get(1..).unwrap_or(""), ""))
}

/// `pinOrderKeyBetween`: a key strictly between two neighbors, or `None` when keys are corrupt.
pub fn pin_order_key_between(before: Option<&str>, after: Option<&str>) -> Option<String> {
    let (a, b) = (before.unwrap_or(""), after.unwrap_or(""));
    if (!a.is_empty() && !is_valid_pin_order_key(a)) || (!b.is_empty() && !is_valid_pin_order_key(b)) || (!b.is_empty() && a >= b) {
        return None;
    }
    Some(pin_order_midpoint(a, b))
}

/// `topOfPinnedRunOrderKey` (`apps/web/src/hooks/useThreadActions.ts`): a key above every pin.
pub fn top_of_pinned_order_key<'a>(threads: impl IntoIterator<Item = &'a SidebarThread>) -> Option<String> {
    let first = threads.into_iter().filter(|thread| thread.pinned).filter_map(|thread| thread.pin_order_key.as_deref()).min();
    pin_order_key_between(None, first)
}

/// `generateSpreadPinOrderKeys`: evenly spaced keys that leave room between neighbours.
pub fn generate_spread_pin_order_keys(count: usize) -> Vec<String> {
    let base = PIN_ORDER_DIGITS.len() as u128;
    let mut width = 2;
    let mut space = base.pow(2);
    while space <= (count as u128 + 1) * 2 {
        width += 1;
        space *= base;
    }
    let step = space as f64 / (count as f64 + 1.0);
    (0..count)
        .map(|i| {
            let mut value = (step * (i as f64 + 1.0)).round() as u128;
            if value.is_multiple_of(base) {
                value += 1;
            }
            let mut key = vec![0u8; width];
            for slot in key.iter_mut().rev() {
                *slot = PIN_ORDER_DIGITS[(value % base) as usize];
                value /= base;
            }
            String::from_utf8_lossy(&key).into_owned()
        })
        .collect()
}

/// `planPinnedReorder`: one key between the moved row's neighbours when they have keys, else a
/// fresh spread for every listed row. `keys_by_id` may include hidden rows, whose keys stay
/// reserved; only `ordered` rows receive writes.
pub fn plan_pinned_reorder(ordered: &[String], keys_by_id: &HashMap<String, Option<String>>, moved: &str) -> Vec<Assignment> {
    let visible: HashSet<&str> = ordered.iter().map(String::as_str).collect();
    let reserved: HashSet<&str> = keys_by_id.iter().filter(|(id, _)| !visible.contains(id.as_str())).filter_map(|(_, key)| key.as_deref()).collect();
    let Some(index) = ordered.iter().position(|id| id == moved) else { return Vec::new() };
    let key_of = |id: Option<&String>| id.and_then(|id| keys_by_id.get(id).cloned().flatten());
    let before_id = index.checked_sub(1).and_then(|i| ordered.get(i));
    let after_id = ordered.get(index + 1);
    let (before, after) = (key_of(before_id), key_of(after_id));
    if (before_id.is_none() || before.is_some()) && (after_id.is_none() || after.is_some()) {
        let mut key = pin_order_key_between(before.as_deref(), after.as_deref());
        while let Some(candidate) = key.as_deref().filter(|candidate| reserved.contains(candidate)) {
            key = pin_order_key_between(Some(candidate), after.as_deref());
        }
        if let Some(order_key) = key {
            return vec![Assignment { id: moved.to_owned(), order_key }];
        }
    }
    let keys: Vec<String> = generate_spread_pin_order_keys(ordered.len() + reserved.len()).into_iter().filter(|key| !reserved.contains(key.as_str())).take(ordered.len()).collect();
    ordered
        .iter()
        .zip(keys)
        .filter(|(id, key)| keys_by_id.get(*id).and_then(Option::as_deref) != Some(key.as_str()))
        .map(|(id, order_key)| Assignment { id: id.clone(), order_key })
        .collect()
}
