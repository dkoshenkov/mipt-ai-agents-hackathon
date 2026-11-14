use std::collections::BTreeSet;

use serde::Serialize;

use super::markdown::{BlockKind, MarkdownIndex, TablePosition};

/// Subject blocks are extracted once. Their context can recur in several packets.
#[derive(Debug, Serialize)]
pub struct Packet<'a> {
    pub version: &'a str,
    pub subjects: Vec<usize>,
    pub blocks: Vec<PacketBlock<'a>>,
}

#[derive(Debug, Serialize)]
pub struct PacketBlock<'a> {
    pub id: usize,
    pub kind: &'a BlockKind,
    pub text: Vec<&'a str>,
    pub headings: &'a [usize],
    pub table_context: &'a [usize],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: &'a Option<TablePosition>,
}

pub struct Packets<'a> {
    pub packets: Vec<Packet<'a>>,
    pub oversized: Vec<usize>,
}

impl MarkdownIndex {
    pub(super) fn packet_for(&self, subjects: &[usize]) -> Packet<'_> {
        let mut ids = BTreeSet::new();
        for &id in subjects {
            ids.insert(id);
            ids.extend(&self.blocks[id].headings);
            ids.extend(&self.blocks[id].table_context);
            ids.extend(self.table_column_ids(id));
        }
        Packet {
            version: &self.version,
            subjects: subjects.to_vec(),
            blocks: ids.into_iter().map(|id| self.packet_block(id)).collect(),
        }
    }
    fn packet_block(&self, id: usize) -> PacketBlock<'_> {
        let block = &self.blocks[id];
        PacketBlock {
            id,
            kind: &block.kind,
            text: block
                .text_ranges
                .iter()
                .map(|range| &self.source[range.clone()])
                .collect(),
            headings: &block.headings,
            table_context: &block.table_context,
            table: &block.table,
        }
    }

    /// Limit the encoded data, including repeated context, IDs and document version.
    pub fn packets(&self, max_bytes: usize) -> Packets<'_> {
        let mut result = Packets {
            packets: Vec::new(),
            oversized: Vec::new(),
        };
        let empty = || Packet {
            version: self.version.as_str(),
            subjects: Vec::new(),
            blocks: Vec::new(),
        };
        let empty_size = serde_json::to_vec(&empty())
            .expect("packet contains only JSON data")
            .len();
        let mut packet = empty();
        let mut included = BTreeSet::new();
        let mut size = empty_size;
        let blocks: Vec<_> = (0..self.blocks.len())
            .map(|id| self.packet_block(id))
            .collect();
        let sizes: Vec<_> = blocks
            .iter()
            .map(|b| {
                serde_json::to_vec(b)
                    .expect("block contains only JSON data")
                    .len()
            })
            .collect();
        for block in &self.blocks {
            let ids: BTreeSet<_> = std::iter::once(block.id)
                .chain(block.headings.iter().copied())
                .chain(block.table_context.iter().copied())
                .chain(self.table_column_ids(block.id))
                .collect();
            loop {
                let mut next_size =
                    size + block.id.to_string().len() + usize::from(!packet.subjects.is_empty());
                for (i, id) in ids.difference(&included).enumerate() {
                    next_size = next_size
                        .saturating_add(sizes[*id] + usize::from(i > 0 || !included.is_empty()));
                }
                if next_size <= max_bytes {
                    size = next_size;
                    included.extend(&ids);
                    packet.subjects.push(block.id);
                    break;
                }
                if packet.subjects.is_empty() {
                    result.oversized.push(block.id);
                    break;
                }
                packet.blocks = std::mem::take(&mut included)
                    .into_iter()
                    .map(|id| self.packet_block(id))
                    .collect();
                result.packets.push(std::mem::replace(&mut packet, empty()));
                size = empty_size;
            }
        }
        if !packet.subjects.is_empty() {
            packet.blocks = included
                .into_iter()
                .map(|id| self.packet_block(id))
                .collect();
            result.packets.push(packet);
        }
        result
    }
}
