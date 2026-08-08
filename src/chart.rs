//! Bar charts, in blocks or in braille.
//!
//! Both meters and the spectrum draw the same shape — a row of vertical bars,
//! bottom-aligned, in a fixed cell grid — so it is drawn in one place and the
//! two callers differ only in what they hand over.
//!
//! ## Blocks versus braille
//!
//! A cell is one glyph and one colour, and the two glyph sets spend that budget
//! differently:
//!
//! | | horizontal | vertical |
//! |---|---|---|
//! | blocks `▁▂▃▄▅▆▇█` | 1 bar per cell | 8 steps per cell |
//! | braille `⠀..⣿` | **2** bars per cell | 4 steps per cell |
//!
//! So braille is not simply better — it trades half the vertical resolution for
//! twice the horizontal. That is the right trade for a spectrum, where
//! horizontal *is* frequency resolution and the extra columns show real detail,
//! and a defensible one for the 60-second history, where it buys time
//! resolution. It is the wrong trade for a three-row meter, where 12 vertical
//! steps is visibly chunky — which is one more reason the braille theme is
//! opt-in rather than the default.
//!
//! The colour cost is real too: two braille sub-columns share one cell and
//! therefore one colour. Under a *vertical* gradient that is free, because both
//! sub-columns sit at the same height and want the same colour anyway. Under
//! the spec theme, where colour encodes level, it would not be — which is why
//! braille is tied to the theme that colours by height.

/// Bit for the dot at `(sub_column, row_from_top)` of a braille cell.
///
/// Unicode braille numbers its dots 1-8 down the left column then down the
/// right, except that dots 7 and 8 were added later and live in the high bits:
///
/// ```text
///   1 4        0x01 0x08
///   2 5   ->   0x02 0x10
///   3 6        0x04 0x20
///   7 8        0x40 0x80
/// ```
const BRAILLE_DOTS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

pub const BRAILLE_BASE: u32 = 0x2800;

/// Sub-columns each cell holds under the given mode.
pub fn sub_columns(braille: bool) -> usize {
    if braille { 2 } else { 1 }
}

/// One bar chart, as `rows` rows of `cols` cells, **row 0 at the bottom**.
///
/// `values` are normalised to `0.0..=1.0` and there must be one per
/// sub-column: `cols` of them for blocks, `2 * cols` for braille. `None` is an
/// unpainted cell.
///
/// A bar at zero still draws its baseline, matching the meters' rule that a
/// signal sitting at the floor is a reading and a blank chart means no data.
pub fn bars(
    values: &[f32],
    cols: u16,
    rows: u16,
    braille: bool,
    blocks: &[char; 8],
) -> Vec<Vec<Option<char>>> {
    let (w, h) = (cols as usize, rows as usize);
    let mut out = vec![vec![None; w]; h];
    if w == 0 || h == 0 {
        return out;
    }

    if !braille {
        for (x, cell) in values.iter().take(w).enumerate() {
            let subs = (cell.clamp(0.0, 1.0) * h as f32 * 8.0).round() as usize;
            let (full, rem) = (subs / 8, subs % 8);
            for (y, row) in out.iter_mut().enumerate() {
                if y < full {
                    row[x] = Some(blocks[7]);
                } else if y == full && rem > 0 {
                    row[x] = Some(blocks[rem - 1]);
                }
            }
            if subs == 0 {
                out[0][x] = Some(blocks[0]);
            }
        }
        return out;
    }

    // Braille: four sub-rows per cell, two sub-columns.
    let sub_rows = h * 4;
    for x in 0..w {
        for (s, dots) in BRAILLE_DOTS.iter().enumerate() {
            let v = values.get(x * 2 + s).copied().unwrap_or(0.0);
            // At least one sub-row, so the baseline is always drawn.
            let filled = ((v.clamp(0.0, 1.0) * sub_rows as f32).round() as usize).max(1);
            for (y, row) in out.iter_mut().enumerate() {
                let mut bits = 0u8;
                for r in 0..4 {
                    // `r` counts up from the bottom of this cell; the dot table
                    // is indexed from the top.
                    if y * 4 + r < filled {
                        bits |= dots[3 - r];
                    }
                }
                if bits != 0 {
                    let prev = row[x].map(|c| c as u32 - BRAILLE_BASE).unwrap_or(0) as u8;
                    row[x] = char::from_u32(BRAILLE_BASE | (prev | bits) as u32);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

    fn full_cells(g: &[Vec<Option<char>>], x: usize) -> usize {
        g.iter().filter(|r| r[x].is_some()).count()
    }

    #[test]
    fn blocks_match_the_meter_they_replaced() {
        // The same cases meter::column was pinned to, so the shared renderer
        // cannot silently change the screen the spec fixed.
        assert_eq!(bars(&[1.0], 1, 3, false, &B), vec![vec![Some('█')]; 3]);
        assert_eq!(
            bars(&[0.0], 1, 3, false, &B),
            vec![vec![Some('▁')], vec![None], vec![None]],
            "a bar at the floor must draw its baseline"
        );
        // 0.5 of three rows is 12 sub-blocks: one full cell plus a half.
        assert_eq!(
            bars(&[0.5], 1, 3, false, &B),
            vec![vec![Some('█')], vec![Some('▄')], vec![None]]
        );
    }

    #[test]
    fn braille_packs_two_bars_into_every_cell() {
        // Left sub-column full, right at the floor.
        let g = bars(&[1.0, 0.0], 1, 2, true, &B);
        let bottom = g[0][0].expect("a cell") as u32 - BRAILLE_BASE;
        // Left column dots all set: 0x01|0x02|0x04|0x40.
        assert_eq!(bottom & 0x47, 0x47, "left bar is not full");
        // Right column: only its bottom dot.
        assert_eq!(bottom & 0xB8, 0x80, "right bar is not just a baseline");
    }

    #[test]
    fn braille_has_four_vertical_steps_per_cell() {
        // Two cells is eight sub-rows, so eight distinct heights — not nine.
        // Zero and the first step both draw the baseline, by the same rule the
        // block meters follow: a signal at the floor is a reading, and only a
        // blank chart means no data.
        let mut seen = std::collections::HashSet::new();
        for i in 0..=8 {
            let v = i as f32 / 8.0;
            let g = bars(&[v, v], 1, 2, true, &B);
            seen.insert(g.iter().map(|r| r[0]).collect::<Vec<_>>());
        }
        assert_eq!(seen.len(), 8, "braille is not resolving 4 steps per cell");

        // And the collapsed pair really is the floor pair.
        assert_eq!(bars(&[0.0, 0.0], 1, 2, true, &B), bars(&[0.125, 0.125], 1, 2, true, &B));
    }

    #[test]
    fn a_taller_bar_fills_more_cells_in_both_modes() {
        for braille in [false, true] {
            let n = sub_columns(braille);
            let short = bars(&vec![0.2; n], 1, 8, braille, &B);
            let tall = bars(&vec![0.9; n], 1, 8, braille, &B);
            assert!(
                full_cells(&tall, 0) > full_cells(&short, 0),
                "braille={braille}: a louder bar did not draw taller"
            );
        }
    }

    #[test]
    fn nothing_is_drawn_outside_the_grid() {
        for braille in [false, true] {
            for rows in 1..6u16 {
                for cols in 1..6u16 {
                    let n = cols as usize * sub_columns(braille);
                    // Deliberately out of range, and NaN.
                    let vals: Vec<f32> =
                        (0..n).map(|i| if i % 3 == 0 { 5.0 } else { -2.0 }).collect();
                    let g = bars(&vals, cols, rows, braille, &B);
                    assert_eq!(g.len(), rows as usize);
                    assert!(g.iter().all(|r| r.len() == cols as usize));
                }
            }
        }
    }

    #[test]
    fn too_few_values_is_not_a_panic() {
        // A resize can hand over a stale, shorter series for one frame.
        let g = bars(&[0.5], 8, 3, true, &B);
        assert_eq!(g[0].len(), 8);
        let g = bars(&[], 4, 2, false, &B);
        assert!(g.iter().all(|r| r.iter().all(|c| c.is_none())));
    }

    #[test]
    fn every_braille_glyph_is_a_real_braille_codepoint() {
        let g = bars(&[0.3, 0.7, 0.1, 0.95], 2, 3, true, &B);
        for row in &g {
            for cell in row.iter().flatten() {
                let c = *cell as u32;
                assert!((BRAILLE_BASE..BRAILLE_BASE + 256).contains(&c), "{cell:?} is not braille");
            }
        }
    }
}
