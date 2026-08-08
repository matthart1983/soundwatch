//! [8] Routing — what is built out of what.
//!
//! Aggregate and multi-output devices are where confusing audio comes from: the
//! thing you selected is not a device, it is a bundle of devices with one of
//! them holding the clock and the others drifting against it. Nothing else on
//! any screen shows that the "device" you picked is three devices in a trench
//! coat, or which of them owns the clock.

use crate::app::App;
use crate::grid::Canvas;
use crate::layout::Layout;
use crate::model::{Device, Transport};
use crate::theme;

pub fn draw(c: &mut Canvas, l: &Layout, app: &App) {
    let f = l.content;
    let mut y = l.body_top;
    let bottom = l.body_top + l.body_rows;

    super::section(c, f, y, "signal path");
    y += 1;

    // The two paths in the order the signal travels them.
    for (label, dev, arrow) in [
        ("apps", app.snap.default_out.as_ref(), c.g.out),
        ("mic ", app.snap.default_in.as_ref(), c.g.inp),
    ] {
        if y >= bottom {
            return;
        }
        match dev {
            None => {
                c.left(f, y, &format!("{label}  {}  nothing", arrow), theme::FAINT);
                y += 1;
            }
            Some(d) => {
                let n = app.snap.streams.iter().filter(|s| s.device_id == d.id).count();
                let x = c.text(f, f.x0, y, label, theme::DIM, false);
                let x = c.text(f, x + 1, y, arrow, theme::direction(d.direction.is_input()), true);
                let x = c.text(f, x + 1, y, &format!("{n} streams"), theme::DIM, false);
                let x = c.text(f, x + 2, y, c.g.arrow, theme::FAINT, false);
                let x = c.text(f, x + 2, y, &d.name, theme::FG, true);
                c.text(f, x + 1, y, &format!("({})", d.transport.name()), theme::DIM, false);
                y += 1;
            }
        }
    }
    y += 1;

    // ── composites ──────────────────────────────────────────────────────────
    let composites: Vec<&Device> = app
        .snap
        .devices
        .iter()
        .filter(|d| !d.sub_device_uids.is_empty() || d.transport == Transport::Aggregate)
        .collect();

    if y >= bottom {
        return;
    }
    super::section(c, f, y, "aggregate and multi-output devices");
    y += 1;

    if composites.is_empty() {
        super::empty(c, f, y, "none \u{2014} every device is a real one");
        y += 2;
    } else {
        // Sub-devices are named by UID, so resolve them back to names where the
        // machine still has the device that owns them.
        let by_uid: std::collections::HashMap<&str, &str> = app
            .snap
            .devices
            .iter()
            .filter_map(|d| d.uid.as_deref().map(|u| (u, d.name.as_str())))
            .collect();

        for d in composites {
            if y + 1 >= bottom {
                break;
            }
            c.left(f, y, &d.name, theme::FG);
            c.right(
                f,
                y,
                &format!("{} \u{b7} clock domain {}", d.transport.name(), d.clock_domain),
                theme::DIM,
            );
            y += 1;
            for (i, uid) in d.sub_device_uids.iter().enumerate() {
                if y >= bottom {
                    break;
                }
                let last = i + 1 == d.sub_device_uids.len();
                let stem = if last { c.g.corner } else { c.g.tee };
                let name = by_uid.get(uid.as_str()).copied().unwrap_or(uid.as_str());
                let x = c.text(f, f.x0 + 2, y, stem, theme::FAINT, false);
                c.text(f, x + 1, y, name, theme::DIM, false);
                y += 1;
            }
        }
        y += 1;
    }

    // ── clock domains ───────────────────────────────────────────────────────
    if y >= bottom {
        return;
    }
    super::section(c, f, y, "clock domains");
    y += 1;
    let mut domains: std::collections::BTreeMap<u32, Vec<&str>> = Default::default();
    for d in &app.snap.devices {
        domains.entry(d.clock_domain).or_default().push(d.name.as_str());
    }
    if domains.len() <= 1 {
        super::empty(c, f, y, "one domain \u{2014} nothing here needs drift compensation");
        return;
    }
    for (domain, names) in domains {
        if y >= bottom {
            return;
        }
        let mut uniq: Vec<&str> = names;
        uniq.sort_unstable();
        uniq.dedup();
        let label = if domain == 0 { "unsynced".to_string() } else { format!("domain {domain}") };
        let x = c.text(f, f.x0, y, &label, theme::DIM, false);
        c.text(f, x + 2, y, &uniq.join(", "), theme::FG, false);
        y += 1;
    }
}
