//! Source file ids and byte-offset spans.

/// Stable id assigned by `SourceManager` when a source file is loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

/// Half-open source span `[start, end)` in a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    /// Source file containing the span.
    pub file_id: FileId,
    /// Start byte offset.
    pub start: usize,
    /// End byte offset.
    pub end: usize,
}

impl SourceSpan {
    /// Creates a source span from file id and byte offsets.
    pub fn new(file_id: FileId, start: usize, end: usize) -> Self {
        Self {
            file_id,
            start,
            end,
        }
    }

    /// Returns the byte length of the span.
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Returns true when the span has no byte width.
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }
}
