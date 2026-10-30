//! Line coverage for the bytecode VM.
//!
//! The VM already carries a source span for every instruction (that is what
//! makes the debugger's line stepping exact), so coverage is a counter in the
//! dispatch loop rather than an instrumentation pass: every executed
//! instruction credits its `(file, line)`. The denominator comes from the
//! compiled chunks themselves - a line counts as coverable when some
//! instruction was emitted for it - so comments, blank lines and declarations
//! with no code do not drag the percentage down.
//!
//! What this is **not**: branch coverage, or coverage of the tree interpreter.
//! The tree engine has no instruction stream to hook; run `--engine vm` (which
//! is also the engine `spid3r test` already compares). Stated here because a
//! coverage number that silently describes a different engine is worse than no
//! number.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::bytecode::Chunk;

/// How deep to follow embedded modules when collecting coverable lines. A
/// module graph is acyclic in practice; the bound exists so a corrupt artifact
/// cannot recurse forever.
const MAX_MODULE_DEPTH: usize = 16;

#[derive(Default)]
pub struct Coverage {
    /// file -> line -> times executed
    hits: HashMap<String, HashMap<u32, u64>>,
    /// file -> every line that has at least one instruction
    coverable: HashMap<String, BTreeSet<u32>>,
}

impl Coverage {
    pub fn record(&mut self, file: &str, line: u32) {
        *self
            .hits
            .entry(file.to_string())
            .or_default()
            .entry(line)
            .or_insert(0) += 1;
    }

    /// Walk a chunk (and the modules it embeds) and note every span line.
    pub fn instrument(&mut self, chunk: &Chunk) {
        self.instrument_at(chunk, 0);
    }

    fn instrument_at(&mut self, chunk: &Chunk, depth: usize) {
        for span in &chunk.inst_spans {
            if span.line > 0 {
                self.coverable
                    .entry(span.file.to_string())
                    .or_default()
                    .insert(span.line);
            }
        }
        if depth >= MAX_MODULE_DEPTH {
            return;
        }
        for f in &chunk.functions {
            self.instrument_at(&f.chunk, depth + 1);
        }
        for m in &chunk.modules {
            self.instrument_at(&m.func.chunk, depth + 1);
        }
    }

    /// Per file: (covered, coverable, percent).
    pub fn summary(&self) -> Vec<(String, usize, usize, f64)> {
        let mut out = Vec::new();
        for (file, lines) in &self.coverable {
            let hit = self.hits.get(file);
            let covered = lines
                .iter()
                .filter(|l| hit.map(|h| h.contains_key(l)).unwrap_or(false))
                .count();
            let total = lines.len();
            let pct = if total == 0 {
                100.0
            } else {
                covered as f64 * 100.0 / total as f64
            };
            out.push((file.clone(), covered, total, pct));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Totals across every file: (covered, coverable, percent).
    pub fn total(&self) -> (usize, usize, f64) {
        let (mut covered, mut total) = (0usize, 0usize);
        for (_, c, t, _) in self.summary() {
            covered += c;
            total += t;
        }
        let pct = if total == 0 {
            100.0
        } else {
            covered as f64 * 100.0 / total as f64
        };
        (covered, total, pct)
    }

    /// A human-readable table, one line per file plus a total.
    pub fn report(&self) -> String {
        let mut out = String::new();
        for (file, covered, total, pct) in self.summary() {
            out.push_str(&format!(
                "{:>6.1}%  {:>5}/{:<5}  {}\n",
                pct, covered, total, file
            ));
        }
        let (covered, total, pct) = self.total();
        out.push_str(&format!(
            "{:>6.1}%  {:>5}/{:<5}  (total)\n",
            pct, covered, total
        ));
        out
    }

    /// LCOV (`lcov.info`) format, which `genhtml`, Codecov and Coveralls all
    /// read. Lines that were never executed are emitted with a count of 0 so a
    /// report shows the gaps instead of omitting them.
    pub fn lcov(&self) -> String {
        let mut out = String::new();
        let mut files: BTreeMap<&String, &BTreeSet<u32>> = BTreeMap::new();
        for (f, lines) in &self.coverable {
            files.insert(f, lines);
        }
        for (file, lines) in files {
            out.push_str(&format!("SF:{}\n", file));
            let hits = self.hits.get(file);
            let mut found = 0;
            for line in lines {
                let count = hits.and_then(|h| h.get(line)).copied().unwrap_or(0);
                if count > 0 {
                    found += 1;
                }
                out.push_str(&format!("DA:{},{}\n", line, count));
            }
            out.push_str(&format!("LF:{}\n", lines.len()));
            out.push_str(&format!("LH:{}\n", found));
            out.push_str("end_of_record\n");
        }
        out
    }
}
