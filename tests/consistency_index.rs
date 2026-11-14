use procurement_review::consistency::markdown::MarkdownIndex;

#[test]
fn split_tables_repeat_the_header_and_note_within_the_encoded_limit() {
    let mut markdown = String::from("| № | Нагрузка |\n|---|---|\n");
    for i in 0..40 {
        markdown.push_str(&format!("| {i} | 300 кг |\n"));
    }
    markdown.push_str("| Примечание: при монтаже | |\n\n");
    markdown.push_str(&"я".repeat(2000));
    let index = MarkdownIndex::new(&markdown).unwrap();
    let groups = index.packets(1300);
    assert!(groups.packets.len() > 1);
    assert_eq!(groups.oversized, [index.blocks.len() - 1]);
    let mut seen = Vec::new();
    for packet in groups.packets {
        assert!(serde_json::to_vec(&packet).unwrap().len() <= 1300);
        for id in &packet.subjects {
            if index.resolve(*id, "300 кг").is_some() {
                for quote in ["Нагрузка", "Примечание: при монтаже"] {
                    assert!(
                        packet
                            .blocks
                            .iter()
                            .any(|b| index.resolve(b.id, quote).is_some())
                    );
                }
            }
        }
        seen.extend(packet.subjects);
    }
    assert_eq!(seen, (0..index.blocks.len() - 1).collect::<Vec<_>>());
}

#[test]
fn changing_the_section_changes_the_source_of_an_identical_table_value() {
    for section in ["Монтаж нового лифта", "Демонтаж старого лифта"]
    {
        let markdown = format!(
            "# Оборудование\n\n## {section}\n\n| Позиция | Масса |\n|---|---|\n| A | 500 кг |\n| B | 500 кг |\n"
        );
        let index = MarkdownIndex::new(&markdown).unwrap();
        let addresses: Vec<_> = index
            .blocks
            .iter()
            .filter_map(|block| {
                let value = index.resolve(block.id, "500 кг")?;
                assert!(
                    block
                        .headings
                        .iter()
                        .any(|heading| index.resolve(*heading, section).is_some())
                );
                assert!(
                    block
                        .table_context
                        .iter()
                        .any(|header| index.resolve(*header, "Масса").is_some())
                );
                assert_eq!(&markdown[value.range.clone()], "500 кг");
                Some(value.range)
            })
            .collect();
        assert_eq!(addresses.len(), 2);
        assert!(addresses[0].end < addresses[1].start);
    }
}

#[test]
fn table_geometry_keeps_rows_titles_sections_and_spans_distinct() {
    let source = "Описание таблицы:\n\n| № | Параметр | Значение |\n|---|---|---|\n| 1 | Масса | 300 |\n| Раздел | | |\n| 2 | Масса | 500 |\n\nПосторонний абзац.\n\n<table><tr><td>Имя</td><td>Значение</td></tr><tr><td rowspan=\"2\">Лифт</td><td>300</td></tr><tr><td>500</td></tr></table>\n\n<table><tr><td>Другое</td></tr></table>";
    let index = MarkdownIndex::new(source).unwrap();
    let find = |q: &str| {
        index
            .blocks
            .iter()
            .find(|b| index.resolve(b.id, q).is_some())
            .unwrap()
    };
    let value = find("300");
    let row = index.same_row_ids(value.id);
    assert!(row.contains(&value.id));
    assert!(row.contains(&find("Масса").id));
    assert!(!row.contains(&find("500").id));
    let table = value.table.as_ref().unwrap();
    assert_eq!(
        index
            .resolve(table.title.unwrap(), "Описание таблицы:")
            .unwrap()
            .block_id,
        table.title.unwrap()
    );
    let second_value = index
        .blocks
        .iter()
        .find(|b| {
            b.table
                .as_ref()
                .is_some_and(|t| t.id == table.id && t.rows.start == 3 && t.columns.start == 2)
        })
        .unwrap();
    assert_eq!(
        second_value.table.as_ref().unwrap().section,
        Some(find("Раздел").id)
    );
    let html_value = index
        .blocks
        .iter()
        .find(|b| {
            b.kind == procurement_review::consistency::markdown::BlockKind::HtmlTableCell
                && index.resolve(b.id, "500").is_some()
        })
        .unwrap();
    assert!(html_value.table.as_ref().unwrap().title.is_none());
    assert!(index.same_row_ids(html_value.id).contains(&find("Лифт").id));
    assert!(
        !index
            .same_row_ids(html_value.id)
            .contains(&find("Другое").id)
    );
}

#[test]
fn source_lift_table_retains_addressable_description_and_columns() {
    let source = "Технические характеристики поставляемого для монтажа лифта: \n\n<table>\n<tr><td>№ п/п</td><td>Наименование оборудования</td><td>Технические характеристики</td></tr>\n<tr><td>3</td><td>Грузоподъемность, кг</td><td>300</td></tr>\n</table>";
    let index = MarkdownIndex::new(source).unwrap();
    let value = index
        .blocks
        .iter()
        .find(|b| {
            b.table.as_ref().is_some_and(|t| t.columns.start == 2)
                && index.resolve(b.id, "300").is_some()
        })
        .unwrap();
    let table = value.table.as_ref().unwrap();
    assert!(
        index
            .resolve(
                table.title.unwrap(),
                "Технические характеристики поставляемого для монтажа лифта"
            )
            .is_some()
    );
    assert!(
        index
            .same_row_ids(value.id)
            .iter()
            .any(|id| index.resolve(*id, "Грузоподъемность, кг").is_some())
    );
    assert!(index.blocks.iter().any(|b| {
        b.table
            .as_ref()
            .is_some_and(|t| t.id == table.id && t.rows.start == 0 && t.columns.start == 2)
            && index.resolve(b.id, "Технические характеристики").is_some()
    }));
}

#[test]
fn defect_statement_caption_is_bound_only_to_its_immediate_table() {
    let source = "# ДЕФЕКТНАЯ ВЕДОМОСТЬ\n\nна выполнение работ по замене грузового лифта\n\n<table><tr><td>Наименование работ</td><td>Объем</td></tr><tr><td>Демонтаж</td><td>1</td></tr></table>";
    let index = MarkdownIndex::new(source).unwrap();
    for quote in ["Наименование работ", "Демонтаж"] {
        let block = index
            .blocks
            .iter()
            .find(|b| index.resolve(b.id, quote).is_some())
            .unwrap();
        let title = block.table.as_ref().unwrap().title.unwrap();
        assert!(
            index
                .resolve(title, "на выполнение работ по замене грузового лифта")
                .is_some()
        );
    }

    for source in [
        "# ДЕФЕКТНАЯ ВЕДОМОСТЬ\n\nОбычный абзац\n\n<table><tr><td>значение</td></tr></table>",
        "# ДЕФЕКТНАЯ ВЕДОМОСТЬ\n\nна выполнение работ по замене грузового лифта\n\n## Другой раздел\n\n<table><tr><td>значение</td></tr></table>",
        "# Другой раздел\n\nна выполнение работ по замене грузового лифта\n\n<table><tr><td>значение</td></tr></table>",
    ] {
        let index = MarkdownIndex::new(source).unwrap();
        let value = index
            .blocks
            .iter()
            .find(|b| index.resolve(b.id, "значение").is_some())
            .unwrap();
        assert!(value.table.as_ref().unwrap().title.is_none());
    }
}

#[test]
fn numbered_title_and_html_column_headers_are_packet_context() {
    let source = "1. Характеристики лифта:\n\n<table><tr><td>Название</td><td>Значение</td></tr><tr><td>Масса</td><td>300</td></tr></table>";
    let index = MarkdownIndex::new(source).unwrap();
    let value = index
        .blocks
        .iter()
        .find(|b| index.resolve(b.id, "300").is_some())
        .unwrap();
    let title = value.table.as_ref().unwrap().title.unwrap();
    assert!(index.resolve(title, "Характеристики лифта:").is_some());
    let packets = index.packets(2000);
    assert!(
        packets
            .packets
            .iter()
            .find(|p| p.subjects.contains(&value.id))
            .unwrap()
            .blocks
            .iter()
            .any(|b| index.resolve(b.id, "Значение").is_some())
    );
    assert!(
        packets
            .packets
            .iter()
            .find(|p| p.subjects.contains(&value.id))
            .unwrap()
            .blocks
            .iter()
            .any(|b| b.id == title)
    );
}
