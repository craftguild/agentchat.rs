//! Delivery-only image alternatives selected from the same public parsed IR.
use oriha_core::{
    TextMapping,
    ir::{BlockKind, BlockNode, Document, InlineKind, InlineNode},
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn tokens(document: &Document) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut references = BTreeSet::new();
    blocks(&document.blocks, &mut |nodes| {
        images(nodes, &mut found);
        main_references(nodes, &mut references);
    });
    // Only main-flow references select footnote bodies; nested-only references
    // cannot grant delivery authority. Match the renderer's last definition.
    let notes = document
        .footnotes
        .iter()
        .map(|n| (n.label.as_str(), n))
        .collect::<BTreeMap<_, _>>();
    for label in references {
        if let Some(note) = notes.get(label.as_str()) {
            images(&note.children, &mut found);
        }
    }
    found
}

fn blocks(nodes: &[BlockNode], visit: &mut impl FnMut(&[InlineNode])) {
    for node in nodes {
        match &node.kind {
            BlockKind::Paragraph(n) => visit(&n.children),
            BlockKind::Heading(n) => visit(&n.children),
            BlockKind::GfmTable(n) => {
                for cell in n.header.iter().chain(n.rows.iter().flat_map(|r| &r.cells)) {
                    visit(&cell.children);
                }
            }
            BlockKind::Document
            | BlockKind::BlockQuote
            | BlockKind::List(_)
            | BlockKind::ListItem(_) => blocks(&node.children, visit),
            _ => {}
        }
    }
}

fn main_references(nodes: &[InlineNode], found: &mut BTreeSet<String>) {
    for node in nodes {
        match &node.kind {
            InlineKind::FootnoteReference { label } => {
                found.insert(label.clone());
            }
            InlineKind::Emphasis { children }
            | InlineKind::Strong { children }
            | InlineKind::Strikethrough { children }
            | InlineKind::Link { children, .. }
            | InlineKind::Annotation { children, .. } => main_references(children, found),
            _ => {}
        }
    }
}

fn images(nodes: &[InlineNode], found: &mut BTreeSet<String>) {
    for node in nodes {
        match &node.kind {
            InlineKind::Image { alt, .. } => {
                let mut region = AltRegion::default();
                region.walk(alt, found);
                region.flush(found);
            }
            InlineKind::Emphasis { children }
            | InlineKind::Strong { children }
            | InlineKind::Strikethrough { children }
            | InlineKind::Link { children, .. }
            | InlineKind::Annotation { children, .. } => images(children, found),
            _ => {}
        }
    }
}

#[derive(Default)]
struct AltRegion {
    text: String,
    mappings: Vec<TextMapping>,
}
impl AltRegion {
    fn flush(&mut self, found: &mut BTreeSet<String>) {
        found.extend(
            super::lexer::scan_mapped(&self.text, self.mappings.iter())
                .into_iter()
                .map(|m| m.handle),
        );
        self.text.clear();
        self.mappings.clear();
    }
    fn walk(&mut self, nodes: &[InlineNode], found: &mut BTreeSet<String>) {
        for node in nodes {
            match &node.kind {
                InlineKind::Text(text) => {
                    let offset = self.text.len();
                    self.text.push_str(text);
                    self.mappings
                        .extend(node.source_mapping.iter().cloned().map(|mut m| {
                            m.display = offset + m.display.start..offset + m.display.end;
                            m
                        }));
                }
                InlineKind::Emphasis { children }
                | InlineKind::Strong { children }
                | InlineKind::Strikethrough { children }
                | InlineKind::Link { children, .. }
                | InlineKind::Annotation { children, .. } => self.walk(children, found),
                InlineKind::Image { .. } => {
                    self.flush(found);
                    images(std::slice::from_ref(node), found);
                }
                _ => self.flush(found),
            }
        }
    }
}
