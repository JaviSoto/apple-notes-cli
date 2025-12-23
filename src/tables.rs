use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Row as ComfyRow, Table};
use std::io::IsTerminal;
use terminal_size::{Width as TermWidth, terminal_size};
use unicode_width::UnicodeWidthStr;

pub trait TableRow {
    const HEADERS: &'static [&'static str];
    fn cells(&self) -> Vec<Cell>;
}

pub fn table_string<T: TableRow>(rows: Vec<T>, width: Option<u16>, color: bool) -> String {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::DynamicFullWidth);

    if let Some(w) = width {
        table.set_width(w);
    }

    table.set_header(ComfyRow::from(
        T::HEADERS
            .iter()
            .map(|h| header_cell(h, color))
            .collect::<Vec<_>>(),
    ));
    for row in rows {
        table.add_row(ComfyRow::from(row.cells()));
    }

    table.to_string()
}

pub fn terminal_width() -> Option<u16> {
    if let Ok(cols) = std::env::var("COLUMNS")
        && let Ok(v) = cols.parse::<u16>()
    {
        return Some(v);
    }
    terminal_size().map(|(TermWidth(w), _)| w)
}

pub fn shorten_id_for_table(id: &str) -> String {
    let id = id.trim();
    let max = 28usize;
    if id.is_empty() || id.width() <= max {
        return id.to_string();
    }
    let prefix_len = 14usize;
    let suffix_len = 10usize;
    if id.len() <= prefix_len + suffix_len + 1 {
        return id.to_string();
    }
    format!("{}…{}", &id[..prefix_len], &id[id.len() - suffix_len..])
}

pub fn render_table<T: TableRow>(rows: Vec<T>) {
    let out = table_string(rows, terminal_width(), should_color());
    println!("{out}");
}

fn header_cell(text: &str, color: bool) -> Cell {
    if color {
        Cell::new(text)
            .add_attribute(Attribute::Bold)
            .fg(Color::Cyan)
    } else {
        Cell::new(text)
    }
}

fn should_color() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::io::stdout().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;
    use comfy_table::Cell;

    #[derive(Debug)]
    struct DemoRow {
        id: String,
        title: String,
    }

    impl TableRow for DemoRow {
        const HEADERS: &'static [&'static str] = &["ID", "Title"];
        fn cells(&self) -> Vec<Cell> {
            vec![Cell::new(&self.id), Cell::new(&self.title)]
        }
    }

    #[test]
    fn shorten_id_for_table_keeps_short_ids() {
        assert_eq!(shorten_id_for_table("abc"), "abc");
    }

    #[test]
    fn shorten_id_for_table_shortens_long_ids() {
        let id = "x-coredata://AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE/ICNote/p1393";
        let s = shorten_id_for_table(id);
        assert!(s.contains('…'));
        assert!(s.starts_with("x-coredata://"));
        assert!(s.ends_with("p1393"));
    }

    #[test]
    fn table_string_snapshot_no_color_fixed_width() {
        let s = table_string(
            vec![
                DemoRow {
                    id: "x-coredata://AAA/ICNote/p1".into(),
                    title: "Home Electrical".into(),
                },
                DemoRow {
                    id: "x-coredata://AAA/ICNote/p2".into(),
                    title: "Check breakers".into(),
                },
            ],
            Some(48),
            false,
        );
        insta::assert_snapshot!(s);
    }
}
