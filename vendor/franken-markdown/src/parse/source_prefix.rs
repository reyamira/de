//! Origins of columns retained by the parser's prefix-only container transforms.
use super::*;

impl DestinationTracker {
    pub(super) fn map_generated_prefix(&mut self, output: &str, original: &str, common: usize) {
        let output_prefix = &output[..output.len() - common];
        if output_prefix.is_empty() || !output_prefix.bytes().all(|byte| byte == b' ') {
            return;
        }
        let original_prefix = &original[..original.len() - common];
        let columns = original_prefix.chars().fold(0usize, |column, character| {
            column + if character == '\t' { 4 - column % 4 } else { 1 }
        });
        let Some(retained_start) = columns.checked_sub(output_prefix.len()) else {
            return;
        };
        let mut column = 0;
        let mut prefix = Vec::new();
        for (offset, character) in original_prefix.char_indices() {
            let next = column + if character == '\t' { 4 - column % 4 } else { 1 };
            let left = column.max(retained_start);
            if left < next
                && matches!(character, '\t' | ' ')
                && let Some(source) =
                    self.text_range(&original[offset..offset + character.len_utf8()])
            {
                prefix.push(TextRun {
                    copied: left - retained_start..next - retained_start,
                    original: source,
                });
            }
            column = next;
        }
        if let Some(map) = self.text.get_mut(&(output.as_ptr() as usize)) {
            prefix.append(&mut map.runs);
            map.runs = prefix;
        }
    }
}
