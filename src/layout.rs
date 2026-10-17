//! Pure layout: which panels fit, and where.

use crate::panel::PanelId;
use ratatui::layout::{Constraint, Layout, Rect};

/// What one visible, available panel needs. The caller adds a request only for panels
/// that the user shows and that have data (no ZFS → no Zfs request).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelReq {
    pub id: PanelId,
    pub min_h: u16,
    pub want_h: u16,
    pub min_w: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayoutPlan {
    /// Panels to draw, with their areas, in PanelId order.
    pub areas: Vec<(PanelId, Rect)>,
    /// Panels that `plan` removed to fit the space, in removal order.
    pub collapsed: Vec<PanelId>,
}

impl LayoutPlan {
    pub fn area(&self, id: PanelId) -> Option<Rect> {
        self.areas.iter().find(|(p, _)| *p == id).map(|(_, r)| *r)
    }
}

/// The process table keeps at least this many lines (borders + header + 6 rows),
/// the same 6 process rows that rtop shows today at 80×24 with 8 cores.
pub const PROC_MIN_H: u16 = 9;

/// Auto-collapse removes panels in this order. Processes never collapse.
pub const COLLAPSE_ORDER: [PanelId; 7] = [
    PanelId::History,
    PanelId::Sensors,
    PanelId::Storage,
    PanelId::Network,
    PanelId::Zfs,
    PanelId::Memory,
    PanelId::Cpu,
];

/// The rows above the process table, top to bottom: each panel with its width weight.
const ROWS: [&[(PanelId, u16)]; 3] = [
    &[(PanelId::Cpu, 45), (PanelId::History, 55)],
    &[(PanelId::Memory, 1), (PanelId::Network, 1)],
    &[
        (PanelId::Storage, 3),
        (PanelId::Zfs, 3),
        (PanelId::Sensors, 2),
    ],
];

/// One slot for each `PanelId`, indexed by `id as usize`.
type Slots = [Option<PanelReq>; PanelId::ALL.len()];

fn row_of(id: PanelId) -> Option<usize> {
    ROWS.iter()
        .position(|row| row.iter().any(|&(p, _)| p == id))
}

/// The requests of row `r` that are still present.
fn row_reqs(shown: &Slots, r: usize) -> impl Iterator<Item = PanelReq> + '_ {
    ROWS[r].iter().filter_map(|&(id, _)| shown[id as usize])
}

fn row_min_h(shown: &Slots, r: usize) -> u16 {
    row_reqs(shown, r).map(|q| q.min_h).max().unwrap_or(0)
}

fn row_min_w(shown: &Slots, r: usize) -> u16 {
    row_reqs(shown, r).fold(0, |sum, q| sum.saturating_add(q.min_w))
}

fn total_min_h(shown: &Slots) -> u16 {
    (0..ROWS.len()).fold(0, |sum, r| sum.saturating_add(row_min_h(shown, r)))
}

/// Splits `strip` by the weights with `Constraint::Fill`. A panel whose share falls under
/// its `min_w` gets `Constraint::Length(min_w)` and the others share the rest by weight,
/// until every share fits. A lone panel takes the whole strip. The width pass (and the
/// put-back check) keep the `min_w` sum of a row of two or more within the strip width.
fn split_row(strip: Rect, present: &[(PanelReq, u16)]) -> Vec<Rect> {
    let mut pinned = vec![false; present.len()];
    loop {
        let constraints = present.iter().zip(&pinned).map(|(&(req, w), &pin)| {
            if pin {
                Constraint::Length(req.min_w)
            } else {
                Constraint::Fill(w)
            }
        });
        let split = Layout::horizontal(constraints).split(strip);
        let mut changed = false;
        if present.len() >= 2 {
            for (i, (req, _)) in present.iter().enumerate() {
                if !pinned[i] && split[i].width < req.min_w {
                    pinned[i] = true;
                    changed = true;
                }
            }
        }
        // Each pass pins at least one more panel, so the loop ends. It never pins all:
        // the unpinned shares add up to the rest, which is at least their min_w sum.
        if !changed {
            return split.to_vec();
        }
    }
}

/// Decides which panels fit in `body` and where they go (design §6.10).
pub fn plan(body: Rect, reqs: &[PanelReq]) -> LayoutPlan {
    // 1. One slot for each panel; a repeated request is ignored.
    let mut asked: Slots = [None; PanelId::ALL.len()];
    for req in reqs {
        asked[req.id as usize].get_or_insert(*req);
    }
    let procs = asked[PanelId::Processes as usize].is_some();
    let mut shown = asked;
    shown[PanelId::Processes as usize] = None;
    let mut collapsed = Vec::new();

    // 2. Width pass: a row of two or more panels drops panels until their min_w fits.
    for r in 0..ROWS.len() {
        while row_reqs(&shown, r).count() >= 2 && row_min_w(&shown, r) > body.width {
            let Some(victim) = COLLAPSE_ORDER
                .into_iter()
                .find(|&id| row_of(id) == Some(r) && shown[id as usize].is_some())
            else {
                break;
            };
            shown[victim as usize] = None;
            collapsed.push(victim);
        }
    }

    // 3. Height pass: remove in strict COLLAPSE_ORDER until the row minimums fit.
    let budget = if procs {
        body.height.saturating_sub(PROC_MIN_H)
    } else {
        body.height
    };
    let mut height_removed = Vec::new();
    while total_min_h(&shown) > budget {
        let Some(victim) = COLLAPSE_ORDER
            .into_iter()
            .find(|&id| shown[id as usize].is_some())
        else {
            break;
        };
        shown[victim as usize] = None;
        collapsed.push(victim);
        height_removed.push(victim);
    }

    // 4. Put-back pass: return height removals that cost no height, last removed first.
    for &id in height_removed.iter().rev() {
        let (Some(r), Some(req)) = (row_of(id), asked[id as usize]) else {
            continue;
        };
        if row_reqs(&shown, r).next().is_some()
            && req.min_h <= row_min_h(&shown, r)
            && row_min_w(&shown, r).saturating_add(req.min_w) <= body.width
        {
            shown[id as usize] = Some(req);
            collapsed.retain(|&c| c != id);
        }
    }

    // 5. Heights: row minimums, then spare in reverse COLLAPSE_ORDER up to want_h.
    let mut heights = [0u16; ROWS.len()];
    for (r, h) in heights.iter_mut().enumerate() {
        *h = row_min_h(&shown, r);
    }
    let mut spare = budget.saturating_sub(total_min_h(&shown));
    for id in COLLAPSE_ORDER.into_iter().rev() {
        let (Some(r), Some(req)) = (row_of(id), shown[id as usize]) else {
            continue;
        };
        let grow = req.want_h.saturating_sub(heights[r]).min(spare);
        heights[r] = heights[r].saturating_add(grow);
        spare = spare.saturating_sub(grow);
    }

    // 6. The process table takes the rest; without it, the last row takes the spare.
    let rows_h = heights.iter().fold(0u16, |sum, &h| sum.saturating_add(h));
    let procs_h = body.height.saturating_sub(rows_h);
    if !procs
        && let Some(last) = (0..ROWS.len())
            .rev()
            .find(|&r| row_reqs(&shown, r).next().is_some())
    {
        heights[last] = heights[last].saturating_add(spare);
    }

    // 7. Stack the rows and split each one by its width weights, never under min_w.
    let mut areas = Vec::new();
    let mut y = body.y;
    for (r, row) in ROWS.iter().enumerate() {
        let present: Vec<(PanelReq, u16)> = row
            .iter()
            .filter_map(|&(id, w)| shown[id as usize].map(|req| (req, w)))
            .collect();
        if present.is_empty() {
            continue;
        }
        let strip = Rect::new(body.x, y, body.width, heights[r]);
        let split = split_row(strip, &present);
        areas.extend(present.iter().map(|(req, _)| req.id).zip(split));
        y = y.saturating_add(heights[r]);
    }
    if procs {
        areas.push((
            PanelId::Processes,
            Rect::new(body.x, y, body.width, procs_h),
        ));
    }
    crate::order::sort_by(&mut areas, |a, b| (a.0 as u8).cmp(&(b.0 as u8)));

    LayoutPlan { areas, collapsed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PanelId::*;

    fn req(id: PanelId, min_h: u16, want_h: u16, min_w: u16) -> PanelReq {
        PanelReq {
            id,
            min_h,
            want_h,
            min_w,
        }
    }

    fn procs() -> PanelReq {
        req(Processes, PROC_MIN_H, 0, 0)
    }

    /// The requests of worked example 1 in design §6.10, with the given Cpu `want_h`.
    fn example_reqs(cpu_want: u16, with_zfs: bool) -> Vec<PanelReq> {
        let mut reqs = vec![
            req(Cpu, 4, cpu_want, 20),
            req(History, 6, 8, 20),
            req(Memory, 5, 5, 30),
            req(Network, 4, 5, 24),
            req(Storage, 4, 6, 30),
            req(Zfs, 4, 6, 36),
            req(Sensors, 3, 8, 22),
            procs(),
        ];
        if !with_zfs {
            reqs.retain(|r| r.id != Zfs);
        }
        reqs
    }

    fn shown(plan: &LayoutPlan, id: PanelId) -> bool {
        plan.area(id).is_some()
    }

    fn inside(outer: Rect, inner: Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.right() <= outer.right()
            && inner.bottom() <= outer.bottom()
    }

    fn overlap(a: Rect, b: Rect) -> bool {
        !a.is_empty()
            && !b.is_empty()
            && a.x < b.right()
            && b.x < a.right()
            && a.y < b.bottom()
            && b.y < a.bottom()
    }

    /// Checks the invariants that every plan must keep.
    fn assert_sound(body: Rect, plan: &LayoutPlan) {
        for (id, area) in &plan.areas {
            assert!(inside(body, *area), "{id:?} {area:?} outside {body:?}");
        }
        for (i, (a_id, a)) in plan.areas.iter().enumerate() {
            for (b_id, b) in &plan.areas[i + 1..] {
                assert!(!overlap(*a, *b), "{a_id:?} {a:?} overlaps {b_id:?} {b:?}");
            }
        }
        let order: Vec<u8> = plan.areas.iter().map(|(id, _)| *id as u8).collect();
        assert!(
            order.is_sorted(),
            "areas not in PanelId order: {:?}",
            plan.areas
        );
        if [History, Storage, Sensors]
            .iter()
            .any(|&id| shown(plan, id))
        {
            assert!(
                !plan.collapsed.contains(&Memory),
                "Memory collapsed while History, Storage or Sensors shows: {plan:?}"
            );
        }
    }

    /// In a row of two or more panels, each panel gets at least its min_w.
    fn assert_min_widths(reqs: &[PanelReq], plan: &LayoutPlan) {
        for row in ROWS {
            let present: Vec<PanelId> = row
                .iter()
                .map(|&(id, _)| id)
                .filter(|&id| shown(plan, id))
                .collect();
            if present.len() < 2 {
                continue;
            }
            for id in present {
                let min_w = reqs.iter().find(|r| r.id == id).map_or(0, |r| r.min_w);
                let width = plan.area(id).map_or(0, |r| r.width);
                assert!(width >= min_w, "{id:?} {width} < min_w {min_w}: {plan:?}");
            }
        }
    }

    #[test]
    fn row_split_gives_each_panel_its_min_width() {
        let tall = |width: u16| Rect::new(0, 0, width, 60);
        // Row 2 with Zfs (min_w 36): the 3:3:2 split gives Zfs 33 at width 88.
        let row_2 = [
            req(Storage, 4, 6, 30),
            req(Zfs, 4, 6, 36),
            req(Sensors, 3, 8, 22),
        ];
        for width in 88..=120 {
            let plan = plan(tall(width), &row_2);
            assert_eq!(plan.areas.len(), 3, "{width}: {plan:?}");
            assert_min_widths(&row_2, &plan);
            let sum: u16 = plan.areas.iter().map(|(_, r)| r.width).sum();
            assert_eq!(sum, width, "the row fills the body: {plan:?}");
        }
        let pair = [req(Storage, 4, 6, 30), req(Zfs, 4, 6, 36)];
        for width in 66..=80 {
            assert_min_widths(&pair, &plan(tall(width), &pair));
        }
        let top = [req(Cpu, 4, 6, 20), req(History, 6, 8, 20)];
        assert_min_widths(&top, &plan(tall(40), &top));
        let middle = [req(Memory, 5, 5, 30), req(Network, 4, 5, 24)];
        assert_min_widths(&middle, &plan(tall(54), &middle));
    }

    #[test]
    fn worked_example_1_at_80_by_24() {
        let body = Rect::new(0, 1, 80, 22);
        let plan = plan(body, &example_reqs(6, true));
        assert_eq!(plan.collapsed, [Sensors, History]);
        assert_eq!(
            plan.areas,
            [
                (Cpu, Rect::new(0, 1, 80, 4)),
                (Memory, Rect::new(0, 5, 40, 5)),
                (Network, Rect::new(40, 5, 40, 5)),
                (Storage, Rect::new(0, 10, 40, 4)),
                (Zfs, Rect::new(40, 10, 40, 4)),
                (Processes, Rect::new(0, 14, 80, 9)),
            ]
        );
        assert_sound(body, &plan);
    }

    #[test]
    fn worked_example_2_at_100_by_30() {
        let body = Rect::new(0, 1, 100, 28);
        let plan = plan(body, &example_reqs(6, true));
        assert!(plan.collapsed.is_empty());
        assert_eq!(
            plan.areas,
            [
                (Cpu, Rect::new(0, 1, 45, 6)),
                (History, Rect::new(45, 1, 55, 6)),
                (Memory, Rect::new(0, 7, 50, 5)),
                (Network, Rect::new(50, 7, 50, 5)),
                (Storage, Rect::new(0, 12, 38, 8)),
                (Zfs, Rect::new(38, 12, 37, 8)),
                (Sensors, Rect::new(75, 12, 25, 8)),
                (Processes, Rect::new(0, 20, 100, 9)),
            ]
        );
        assert_sound(body, &plan);
    }

    #[test]
    fn matrix_at_80_by_24_keeps_six_process_rows_and_memory() {
        let body = Rect::new(0, 1, 80, 22);
        for cpu_want in [6, 6, 10] {
            for with_zfs in [true, false] {
                let plan = plan(body, &example_reqs(cpu_want, with_zfs));
                let procs = plan.area(Processes).expect("processes shown");
                assert!(
                    procs.height >= PROC_MIN_H,
                    "cpu_want {cpu_want} zfs {with_zfs}: {plan:?}"
                );
                assert!(shown(&plan, Memory), "cpu_want {cpu_want} zfs {with_zfs}");
                assert_sound(body, &plan);
            }
        }
    }

    #[test]
    fn put_back_returns_a_panel_whose_removal_saved_no_height() {
        let body = Rect::new(0, 0, 80, 20);
        let reqs = [
            req(Cpu, 4, 6, 20),
            req(History, 6, 8, 20),
            req(Memory, 5, 5, 30),
            req(Network, 4, 5, 24),
            req(Storage, 4, 6, 30),
            req(Zfs, 4, 6, 36),
            procs(),
        ];
        let plan = plan(body, &reqs);
        assert_eq!(plan.collapsed, [History, Storage, Zfs]);
        assert_eq!(
            plan.areas,
            [
                (Cpu, Rect::new(0, 0, 80, 6)),
                (Memory, Rect::new(0, 6, 40, 5)),
                (Network, Rect::new(40, 6, 40, 5)),
                (Processes, Rect::new(0, 11, 80, 9)),
            ]
        );
    }

    #[test]
    fn spare_goes_to_cpu_memory_and_zfs_rows_before_history() {
        // Row minimums 6 + 5 + 4 = 15. Cpu, Memory and Zfs need 2 + 2 + 3 = 7 more
        // to reach their want_h. History wants 4 more after that.
        let reqs = [
            req(Cpu, 4, 8, 10),
            req(History, 6, 12, 10),
            req(Memory, 5, 7, 10),
            req(Zfs, 4, 7, 10),
            procs(),
        ];
        let tall = |extra: u16| Rect::new(0, 0, 100, 15 + PROC_MIN_H + extra);

        let plan_7 = plan(tall(7), &reqs);
        assert_eq!(plan_7.area(Cpu).map(|r| r.height), Some(8));
        assert_eq!(plan_7.area(History).map(|r| r.height), Some(8));
        assert_eq!(plan_7.area(Memory).map(|r| r.height), Some(7));
        assert_eq!(plan_7.area(Zfs).map(|r| r.height), Some(7));
        assert_eq!(plan_7.area(Processes).map(|r| r.height), Some(PROC_MIN_H));

        let plan_9 = plan(tall(9), &reqs);
        assert_eq!(plan_9.area(History).map(|r| r.height), Some(10));
        assert_eq!(plan_9.area(Memory).map(|r| r.height), Some(7));
        assert_eq!(plan_9.area(Zfs).map(|r| r.height), Some(7));

        let plan_large = plan(tall(40), &reqs);
        assert_eq!(plan_large.area(History).map(|r| r.height), Some(12));
        assert_eq!(
            plan_large.area(Processes).map(|r| r.height),
            Some(PROC_MIN_H + 40 - 11)
        );
    }

    #[test]
    fn all_eight_panels_fit_at_110_by_30() {
        let body = Rect::new(0, 1, 110, 30);
        let plan = plan(body, &example_reqs(6, true));
        assert!(plan.collapsed.is_empty(), "{plan:?}");
        assert_eq!(plan.areas.len(), 8);
        // Heights of the left column (Cpu, Memory, Storage) and Processes cover the body.
        let total: u16 = [Cpu, Memory, Storage, Processes]
            .iter()
            .map(|&id| plan.area(id).map_or(0, |r| r.height))
            .sum();
        assert_eq!(total, body.height);
        assert_sound(body, &plan);
    }

    #[test]
    fn empty_row_2_takes_no_height() {
        let body = Rect::new(0, 1, 80, 22);
        let reqs: Vec<PanelReq> = example_reqs(6, true)
            .into_iter()
            .filter(|r| ![Storage, Zfs, Sensors].contains(&r.id))
            .collect();
        let plan = plan(body, &reqs);
        assert!(plan.collapsed.is_empty(), "{plan:?}");
        // Spare 2 goes to History (row 0: 6 → 8). Row 2 gets nothing.
        assert_eq!(plan.area(Cpu), Some(Rect::new(0, 1, 36, 8)));
        assert_eq!(plan.area(Memory), Some(Rect::new(0, 9, 40, 5)));
        // Processes starts right after row 1.
        assert_eq!(plan.area(Processes), Some(Rect::new(0, 14, 80, 9)));
        assert_sound(body, &plan);
    }

    #[test]
    fn without_processes_the_last_row_takes_the_spare() {
        let body = Rect::new(0, 0, 80, 40);
        let reqs = [
            req(Cpu, 4, 6, 20),
            req(Memory, 5, 5, 30),
            req(Storage, 4, 6, 30),
        ];
        let plan = plan(body, &reqs);
        assert_eq!(plan.area(Processes), None);
        assert_eq!(plan.area(Cpu), Some(Rect::new(0, 0, 80, 6)));
        assert_eq!(plan.area(Memory), Some(Rect::new(0, 6, 80, 5)));
        assert_eq!(plan.area(Storage), Some(Rect::new(0, 11, 80, 29)));
    }

    #[test]
    fn body_height_3_collapses_every_row_panel() {
        let body = Rect::new(0, 1, 80, 3);
        let plan = plan(body, &example_reqs(6, true));
        assert_eq!(plan.areas, [(Processes, Rect::new(0, 1, 80, 3))]);
        assert_eq!(
            plan.collapsed,
            [Sensors, History, Storage, Network, Zfs, Memory, Cpu]
        );
    }

    #[test]
    fn tiny_bodies_do_not_panic() {
        for body in [Rect::new(0, 0, 0, 0), Rect::new(0, 0, 1, 1)] {
            let plan = plan(body, &example_reqs(6, true));
            for (_, area) in &plan.areas {
                assert!(area.width <= 1 && area.height <= 1, "{plan:?}");
            }
            assert_sound(body, &plan);
            let empty = super::plan(body, &[]);
            assert_eq!(empty, LayoutPlan::default());
        }
    }

    #[test]
    fn width_pass_keeps_a_lone_panel() {
        let body = Rect::new(0, 0, 10, 30);
        let plan = plan(body, &[req(Memory, 5, 5, 30), procs()]);
        assert!(plan.collapsed.is_empty());
        assert_eq!(plan.area(Memory), Some(Rect::new(0, 0, 10, 5)));
    }

    #[test]
    fn width_pass_removes_in_collapse_order() {
        // Row 2 needs 88 > 60: Sensors goes, then 66 > 60: Storage goes.
        let body = Rect::new(0, 0, 60, 60);
        let plan = plan(body, &example_reqs(6, true));
        assert_eq!(plan.collapsed, [Sensors, Storage]);
        // Rows 8 (History grows), 5 and 6 (Zfs grows).
        assert_eq!(plan.area(Zfs), Some(Rect::new(0, 13, 60, 6)));
    }

    #[test]
    fn width_removals_are_not_put_back() {
        // Width pass: 30 + 36 + 22 > 80, Sensors goes. Height pass: 5 + 6 > 9, Storage
        // goes. Sensors would now fit next to Zfs (36 + 22, min_h 3 <= 4), but a width
        // removal stays removed.
        let body = Rect::new(0, 0, 80, 9);
        let reqs = [
            req(Memory, 5, 5, 30),
            req(Storage, 6, 6, 30),
            req(Zfs, 4, 4, 36),
            req(Sensors, 3, 3, 22),
        ];
        let plan = plan(body, &reqs);
        assert_eq!(plan.collapsed, [Sensors, Storage]);
        assert_eq!(plan.area(Sensors), None);
        assert_eq!(plan.area(Zfs), Some(Rect::new(0, 5, 80, 4)));
        assert_sound(body, &plan);
    }

    #[test]
    fn duplicate_requests_use_the_first() {
        let body = Rect::new(0, 0, 80, 30);
        let plan = plan(
            body,
            &[req(Memory, 5, 5, 30), req(Memory, 7, 7, 30), procs()],
        );
        assert_eq!(plan.areas.len(), 2);
        assert_eq!(plan.area(Memory).map(|r| r.height), Some(5));
    }

    #[test]
    fn every_size_gives_a_sound_plan() {
        for (cpu_want, with_zfs) in [(6, true), (10, true), (14, true), (10, false)] {
            let reqs = example_reqs(cpu_want, with_zfs);
            for width in (0..=200u16).step_by(7) {
                for height in (0..=60u16).step_by(3) {
                    let body = Rect::new(2, 1, width, height);
                    let plan = plan(body, &reqs);
                    assert_sound(body, &plan);
                    for id in &plan.collapsed {
                        assert!(!shown(&plan, *id), "{id:?} collapsed and shown: {plan:?}");
                    }
                    let procs = plan.area(Processes).expect("processes always shown");
                    assert_eq!(procs.width, width);
                    assert_eq!(procs.bottom(), body.bottom(), "{width}×{height}: {plan:?}");
                    if height >= PROC_MIN_H {
                        assert!(procs.height >= PROC_MIN_H, "{width}×{height}: {plan:?}");
                    }
                    assert_min_widths(&reqs, &plan);
                }
            }
        }
    }
}
