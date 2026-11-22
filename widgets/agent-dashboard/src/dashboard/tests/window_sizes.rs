use super::*;

const SIZES: [Size; 5] = [
    Size::new(1100.0, 700.0),
    Size::new(1100.0, 1200.0),
    Size::new(1440.0, 900.0),
    Size::new(1920.0, 700.0),
    Size::new(1920.0, 1200.0),
];

/// Whether two rects share area (touching edges, ±0.5, do not count).
fn overlap(a: [f32; 4], b: [f32; 4]) -> bool {
    let apart = a[0] + a[2] <= b[0] + 0.5
        || b[0] + b[2] <= a[0] + 0.5
        || a[1] + a[3] <= b[1] + 0.5
        || b[1] + b[3] <= a[1] + 0.5;
    !apart
}

#[test]
fn panels_stay_apart_and_inside_the_window() {
    // One store for every screen: loading the system fonts is what a new
    // store costs, and the screens only add text layouts to it.
    let mut text = TextStore::new(Default::default());
    for size in SIZES {
        let window = [0.0, 0.0, size.width, size.height];
        for combination in 0..32_usize {
            let shown: [bool; 5] = std::array::from_fn(|p| combination & (1 << p) != 0);
            let mut dashboard = Dashboard::showing(View {
                shown,
                ..View::of(Grouping::Orchestrator)
            });
            dashboard.prepare(size, &mut text);
            let what = format!("{size:?} shown {shown:?}");
            let panels: Vec<(Panel, [f32; 4])> = Panel::ALL
                .into_iter()
                .filter_map(|p| Some((p, dashboard.panels[p as usize]?)))
                .map(|(p, id)| (p, rect(&dashboard.tree, id)))
                .collect();
            let expected = Panel::ALL.into_iter().filter(|p| shown[*p as usize]);
            assert_eq!(panels.len(), expected.count(), "{what}");
            for (panel, r) in &panels {
                assert!(r[2] > 0.0 && r[3] > 0.0, "{what}: {panel:?} is empty {r:?}");
                assert!(
                    inside(*r, window),
                    "{what}: {panel:?} {r:?} leaves the window"
                );
            }
            for (i, (a, ra)) in panels.iter().enumerate() {
                for (b, rb) in &panels[i + 1..] {
                    assert!(
                        !overlap(*ra, *rb),
                        "{what}: {a:?} {ra:?} overlaps {b:?} {rb:?}"
                    );
                }
            }
        }
    }
}
