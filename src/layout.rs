//! Where everything goes, at whatever size the terminal happens to be.
//!
//! LITE.md fixes an 80x24 grid and gives exact row numbers and exact table
//! columns — `1 / 17 / 40 / 51 / 62 / 70`, identical across all four Lites. It
//! says nothing about any other size, and the first implementation answered
//! that by letterboxing: an 80x24 island centred in whatever you actually had.
//! On a full-screen terminal that wasted most of the screen, and it made the
//! meters small in exactly the case where there was room to make them large.
//!
//! So the spec's numbers become the *minimum* rather than the size. Every
//! constant it fixes is reproduced exactly when the terminal is 80x24, and the
//! tests assert that — the generalisation has to pass through the spec's own
//! values, or it is a different design wearing the same name.
//!
//! ## Where the extra space goes
//!
//! **Height.** Eleven rows are structural (header, labels, axis, vitals, note,
//! table head and rule, prompt, footer) and never grow. The rest is split: a
//! third of the surplus to the meters, in the spec's 3:2 output-to-input ratio,
//! and the remainder to the stream list. Meters get less than half because a
//! meter is a level, and a level does not become more legible with forty rows;
//! a stream list does become more useful with forty entries.
//!
//! **Width.** The five numeric columns — LEVEL, RATE, LAT — hold values whose
//! formats are fixed, so widening them buys nothing but whitespace. The surplus
//! goes to the two fields that are actually truncated in practice (DEVICE takes
//! half, APP a quarter — real CoreAudio device descriptions run past 40
//! characters) and to the sparkline (the last quarter), which is the only table
//! column that carries more information the wider it gets.

use crate::grid::{Field, MIN_COLS, MIN_ROWS, content};

/// Which chrome the screen is wearing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Chrome {
    /// The full product: header, tab bar, body, footer rule, footer.
    #[default]
    Tabs,
    /// SoundWatch Lite: one screen, no tab bar, the handoff's row numbers
    /// exactly. Kept because the Lite spec is a real spec with real tests, and
    /// growing a tab bar is not a licence to renumber it.
    Lite,
}

/// Rows the tab chrome takes off the top: the bar and its rule.
pub const TAB_ROWS: u16 = 2;

/// Rows the detail block occupies when it is open.
pub const DETAIL_ROWS: u16 = 3;

/// Every row and field for one terminal size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub w: u16,
    pub h: u16,
    pub chrome: Chrome,
    pub content: Field,

    /// Tab bar and the rule under it. Only drawn under [`Chrome::Tabs`].
    pub row_tab_bar: u16,
    pub row_tab_rule: u16,
    /// First row and height of the region a tab may draw in.
    pub body_top: u16,
    pub body_rows: u16,
    /// The rule above the footer. Only drawn under [`Chrome::Tabs`].
    pub row_footer_rule: u16,

    pub row_header: u16,
    pub row_out_label: u16,
    pub row_out_meter: u16,
    pub out_meter_h: u16,
    pub row_in_label: u16,
    pub row_in_meter: u16,
    pub in_meter_h: u16,
    pub row_axis: u16,
    pub row_vitals: u16,
    pub row_note: u16,
    pub row_table_head: u16,
    pub row_table_rule: u16,
    pub row_list: u16,
    /// Rows the stream table can use with the detail block closed.
    pub list_rows: u16,
    pub row_prompt: u16,
    pub row_footer: u16,

    pub f_app: Field,
    pub f_device: Field,
    /// Input rows spend two columns of DEVICE on the mic-in-use dot.
    pub f_device_in: Field,
    pub f_level: Field,
    pub f_rate: Field,
    pub f_lat: Field,
    pub f_spark: Field,
}

impl Layout {
    /// The Lite chrome, at the handoff's exact numbers.
    pub fn lite(w: u16, h: u16) -> Self {
        Self::new(w, h, Chrome::Lite)
    }

    pub fn new(w: u16, h: u16, chrome: Chrome) -> Self {
        let w = w.max(MIN_COLS);
        let h = h.max(MIN_ROWS);

        // ── chrome ──────────────────────────────────────────────────────────
        let tabs = chrome == Chrome::Tabs;
        let row_tab_bar = 1;
        let row_tab_rule = 2;
        let body_top = if tabs { row_tab_rule + 1 } else { 0 };
        let row_footer = h - 1;
        let row_footer_rule = row_footer.saturating_sub(1);
        // The body ends above the footer rule under Tabs, and above the prompt
        // row under Lite.
        let body_bottom = if tabs { row_footer_rule.saturating_sub(1) } else { row_footer - 1 };
        let body_rows = (body_bottom + 1).saturating_sub(body_top);

        // ── height ──────────────────────────────────────────────────────────
        // The Lite rows are laid out inside the body, so they are the spec's
        // verbatim when the body starts at row 0 and shift wholesale when a tab
        // bar is above them.
        let extra = body_rows.saturating_sub(MIN_ROWS - if tabs { TAB_ROWS + 1 } else { 0 });
        // A third of the surplus to the meters, split 3:2 as the spec has them.
        let meter_extra = extra / 3;
        let out_extra = meter_extra * 3 / 5;
        let out_meter_h = 3 + out_extra;
        let in_meter_h = 2 + (meter_extra - out_extra);

        let row_out_label = body_top + 2;
        let row_out_meter = row_out_label + 1;
        let row_in_label = row_out_meter + out_meter_h;
        let row_in_meter = row_in_label + 1;
        let row_axis = row_in_meter + in_meter_h;
        let row_vitals = row_axis + 1;
        let row_note = row_vitals + 1;
        let row_table_head = row_note + 1;
        let row_table_rule = row_table_head + 1;
        let row_list = row_table_rule + 1;
        let row_prompt = if tabs { row_footer_rule.saturating_sub(1) } else { row_footer - 1 };
        // Whatever is left between the rule and the prompt.
        let list_rows = row_prompt.saturating_sub(row_list);

        // ── width ───────────────────────────────────────────────────────────
        let content = content(w);
        let extra_w = w - MIN_COLS;
        let app_w = 15 + extra_w / 4;
        let device_w = 22 + extra_w / 2;
        // The remainder rather than another quarter, so rounding never leaves a
        // column stranded between the sparkline and the right margin.
        let spark_w = 9 + (extra_w - extra_w / 4 - extra_w / 2);

        // Laid out left to right with the spec's one-column gutters. At w=80
        // this reproduces 1 / 17 / 40 / 51 / 62 / 70 exactly.
        let mut x = 1;
        let f_app = Field::at(x, app_w);
        x += app_w + 1;
        let f_device = Field::at(x, device_w);
        let f_device_in = Field::at(x + 2, device_w - 2);
        x += device_w + 1;
        let f_level = Field::at(x, 10);
        x += 11;
        let f_rate = Field::at(x, 10);
        x += 11;
        let f_lat = Field::at(x, 7);
        x += 8;
        let f_spark = Field::at(x, spark_w);

        Self {
            w,
            h,
            chrome,
            content,
            row_tab_bar,
            row_tab_rule,
            body_top,
            body_rows,
            row_footer_rule,
            row_header: 0,
            row_out_label,
            row_out_meter,
            out_meter_h,
            row_in_label,
            row_in_meter,
            in_meter_h,
            row_axis,
            row_vitals,
            row_note,
            row_table_head,
            row_table_rule,
            row_list,
            list_rows,
            row_prompt,
            row_footer,
            f_app,
            f_device,
            f_device_in,
            f_level,
            f_rate,
            f_lat,
            f_spark,
        }
    }

    /// Rows the stream table can use in the given mode.
    pub fn list_rows_for(&self, detail_open: bool) -> u16 {
        if detail_open { self.list_rows.saturating_sub(DETAIL_ROWS) } else { self.list_rows }
    }

    /// First row of the detail block: the last [`DETAIL_ROWS`] of the list area.
    pub fn row_detail(&self) -> u16 {
        self.row_prompt.saturating_sub(DETAIL_ROWS)
    }

    // Used by the spectrum screen, which lands next.
    /// The full-width span of the spectrum graph, and its first row.
    ///
    /// Spectrum replaces the meters, the axis, the vitals, the note and the
    /// table, because the question has changed from *which app* to *what is in
    /// this signal*. It keeps the header and the footer so the machine's health
    /// is still visible while you stare at the graph.
    #[allow(dead_code)]
    pub fn spectrum(&self) -> (u16, u16) {
        let top = self.row_out_meter;
        // Leave the axis, a blank, the verdict and the prompt below it.
        let bottom = self.row_prompt.saturating_sub(3);
        (top, bottom.saturating_sub(top) + 1)
    }

    #[allow(dead_code)]
    pub fn row_spectrum_axis(&self) -> u16 {
        self.row_prompt.saturating_sub(2)
    }

    #[allow(dead_code)]
    pub fn row_spectrum_verdict(&self) -> u16 {
        self.row_prompt.saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generalisation has to pass through the spec's own numbers. If this
    /// fails, the layout is a different design wearing the same name.
    #[test]
    fn eighty_by_twentyfour_is_the_spec_verbatim() {
        let l = Layout::lite(80, 24);
        assert_eq!(l.content, Field::new(1, 78));

        assert_eq!(l.row_header, 0);
        assert_eq!(l.row_out_label, 2);
        assert_eq!(l.row_out_meter, 3);
        assert_eq!(l.out_meter_h, 3);
        assert_eq!(l.row_in_label, 6);
        assert_eq!(l.row_in_meter, 7);
        assert_eq!(l.in_meter_h, 2);
        assert_eq!(l.row_axis, 9);
        assert_eq!(l.row_vitals, 10);
        assert_eq!(l.row_note, 11);
        assert_eq!(l.row_table_head, 12);
        assert_eq!(l.row_table_rule, 13);
        assert_eq!(l.row_list, 14);
        assert_eq!(l.list_rows, 8);
        assert_eq!(l.row_detail(), 19);
        assert_eq!(l.row_prompt, 22);
        assert_eq!(l.row_footer, 23);
        assert_eq!(l.list_rows_for(true), 5);
    }

    /// `1 / 17 / 40 / 51 / 62 / 70`, identical across all four Lites.
    #[test]
    fn the_family_column_numbers_survive() {
        let l = Layout::lite(80, 24);
        assert_eq!((l.f_app.x0, l.f_app.width()), (1, 15));
        assert_eq!((l.f_device.x0, l.f_device.width()), (17, 22));
        assert_eq!((l.f_device_in.x0, l.f_device_in.width()), (19, 20));
        assert_eq!((l.f_level.x0, l.f_level.width()), (40, 10));
        assert_eq!((l.f_rate.x0, l.f_rate.width()), (51, 10));
        assert_eq!((l.f_lat.x0, l.f_lat.width()), (62, 7));
        assert_eq!((l.f_spark.x0, l.f_spark.width()), (70, 9));
    }

    #[test]
    fn a_smaller_terminal_clamps_to_the_minimum() {
        assert_eq!(Layout::lite(40, 10), Layout::lite(80, 24));
    }

    /// Nothing may overlap, overflow the content field, or leave a gap other
    /// than the spec's single-column gutters — at any size.
    #[test]
    fn table_fields_tile_the_row_at_every_width() {
        for w in [80u16, 81, 82, 83, 100, 120, 137, 200, 400] {
            let l = Layout::lite(w, 24);
            let fields = [l.f_app, l.f_device, l.f_level, l.f_rate, l.f_lat, l.f_spark];
            for pair in fields.windows(2) {
                assert_eq!(
                    pair[1].x0,
                    pair[0].x1 + 2,
                    "w={w}: gutter between {:?} and {:?} is not one column",
                    pair[0],
                    pair[1]
                );
            }
            assert_eq!(l.f_app.x0, l.content.x0, "w={w}: table does not start at the margin");
            assert_eq!(l.f_spark.x1, l.content.x1, "w={w}: table does not reach the right margin");
        }
    }

    /// Every row is inside the terminal and in the order the spec puts them.
    #[test]
    fn rows_are_ordered_and_inside_the_screen_at_every_height() {
        for h in [24u16, 25, 26, 27, 30, 40, 60, 100] {
            let l = Layout::lite(80, h);
            let rows = [
                l.row_header,
                l.row_out_label,
                l.row_out_meter,
                l.row_in_label,
                l.row_in_meter,
                l.row_axis,
                l.row_vitals,
                l.row_note,
                l.row_table_head,
                l.row_table_rule,
                l.row_list,
                l.row_prompt,
                l.row_footer,
            ];
            for pair in rows.windows(2) {
                assert!(pair[0] < pair[1], "h={h}: rows out of order at {:?}", pair);
            }
            assert!(l.row_footer < h, "h={h}: footer off screen");
            assert!(l.list_rows >= 8, "h={h}: list shrank below the spec's eight rows");
            assert!(l.row_list + l.list_rows <= l.row_prompt, "h={h}: list runs into the prompt");
            // The detail block has to fit inside the list area it displaces.
            assert!(l.list_rows_for(true) >= 1, "h={h}: detail leaves no list");
            assert!(l.row_detail() >= l.row_list, "h={h}: detail above the list");
        }
    }

    /// Surplus height reaches the meters and the list, and neither starves.
    #[test]
    fn extra_height_is_shared_between_the_meters_and_the_list() {
        let small = Layout::lite(80, 24);
        let big = Layout::lite(80, 60);
        assert!(big.out_meter_h > small.out_meter_h, "meters did not grow");
        assert!(big.in_meter_h > small.in_meter_h, "input meter did not grow");
        assert!(big.list_rows > small.list_rows, "list did not grow");
        // The list gets the larger share: forty stream rows are worth more than
        // forty rows of one number.
        let meter_gain =
            (big.out_meter_h + big.in_meter_h) - (small.out_meter_h + small.in_meter_h);
        assert!(
            big.list_rows - small.list_rows > meter_gain,
            "meters took more of the surplus than the list"
        );
        // And the spec's 3:2 ratio is preserved as they grow.
        assert!(big.out_meter_h > big.in_meter_h);
    }

    #[test]
    fn the_spectrum_fits_between_the_header_and_the_prompt() {
        for h in [24u16, 30, 60, 100] {
            let l = Layout::lite(80, h);
            let (top, rows) = l.spectrum();
            assert!(rows >= 1, "h={h}: no room for the spectrum");
            assert!(top > l.row_header, "h={h}: spectrum overwrites the header");
            assert!(top + rows <= l.row_spectrum_axis(), "h={h}: spectrum runs into its own axis");
            assert!(l.row_spectrum_axis() < l.row_spectrum_verdict());
            assert!(l.row_spectrum_verdict() < l.row_prompt);
        }
    }
}
