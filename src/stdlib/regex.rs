//! Zero-dependency Thompson NFA regex engine.
//!
//! Supports literals, '.', '*', '+', '?', '^', '$', character classes '[abc]',
//! ranges '[a-z]', negations '[^...]', escapes '\d', '\w', '\s', '\D', '\W', '\S',
//! alternation '|', and grouping '()'. Linear time execution with no exponential backtracking.

#[derive(Clone, Debug, PartialEq)]
enum RegexNode {
    Literal(char),
    Any,
    Class {
        ranges: Vec<(char, char)>,
        negated: bool,
    },
    AnchorStart,
    AnchorEnd,
    Concat(Vec<RegexNode>),
    Alt(Vec<RegexNode>),
    Star(Box<RegexNode>),
    Plus(Box<RegexNode>),
    Quest(Box<RegexNode>),
    Empty,
}

/// Maximum nested-group depth for a regex pattern (mirrors the JSON parser's
/// recursion guard). Without it, a pathological `((((…` pattern recurses the
/// pattern parser without bound and overflows the call stack.
const MAX_REGEX_DEPTH: u32 = 512;

struct Parser<'a> {
    chars: Vec<char>,
    pos: usize,
    depth: u32,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> Parser<'a> {
    fn new(pattern: &'a str) -> Self {
        Self {
            chars: pattern.chars().collect(),
            pos: 0,
            depth: 0,
            _marker: std::marker::PhantomData,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let ch = self.chars.get(self.pos).copied();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn parse(&mut self) -> Result<RegexNode, String> {
        let node = self.parse_alt()?;
        if self.pos < self.chars.len() {
            return Err(format!(
                "unexpected character '{}' at position {}",
                self.chars[self.pos], self.pos
            ));
        }
        Ok(node)
    }

    fn parse_alt(&mut self) -> Result<RegexNode, String> {
        let mut branches = vec![self.parse_concat()?];
        while self.peek() == Some('|') {
            self.next();
            branches.push(self.parse_concat()?);
        }
        if branches.len() == 1 {
            Ok(branches.pop().unwrap())
        } else {
            Ok(RegexNode::Alt(branches))
        }
    }

    fn parse_concat(&mut self) -> Result<RegexNode, String> {
        let mut items = Vec::new();
        while let Some(ch) = self.peek() {
            if ch == '|' || ch == ')' {
                break;
            }
            items.push(self.parse_piece()?);
        }
        if items.is_empty() {
            Ok(RegexNode::Empty)
        } else if items.len() == 1 {
            Ok(items.pop().unwrap())
        } else {
            Ok(RegexNode::Concat(items))
        }
    }

    fn parse_piece(&mut self) -> Result<RegexNode, String> {
        let atom = self.parse_atom()?;
        match self.peek() {
            Some('*') => {
                self.next();
                Ok(RegexNode::Star(Box::new(atom)))
            }
            Some('+') => {
                self.next();
                Ok(RegexNode::Plus(Box::new(atom)))
            }
            Some('?') => {
                self.next();
                Ok(RegexNode::Quest(Box::new(atom)))
            }
            _ => Ok(atom),
        }
    }

    fn parse_atom(&mut self) -> Result<RegexNode, String> {
        match self.next() {
            Some('^') => Ok(RegexNode::AnchorStart),
            Some('$') => Ok(RegexNode::AnchorEnd),
            Some('.') => Ok(RegexNode::Any),
            Some('(') => {
                self.depth += 1;
                if self.depth > MAX_REGEX_DEPTH {
                    return Err(format!(
                        "regex nesting too deep (max {MAX_REGEX_DEPTH} groups)"
                    ));
                }
                let inner = self.parse_alt()?;
                self.depth -= 1;
                if self.next() != Some(')') {
                    return Err("unclosed parenthesis in regex".to_string());
                }
                Ok(inner)
            }
            Some('[') => self.parse_class(),
            Some('\\') => self.parse_escape(),
            Some(ch) if ch == '*' || ch == '+' || ch == '?' => {
                Err(format!("quantifier '{}' without preceding token", ch))
            }
            Some(ch) => Ok(RegexNode::Literal(ch)),
            None => Ok(RegexNode::Empty),
        }
    }

    fn parse_escape(&mut self) -> Result<RegexNode, String> {
        match self.next() {
            Some('d') => Ok(RegexNode::Class {
                ranges: vec![('0', '9')],
                negated: false,
            }),
            Some('D') => Ok(RegexNode::Class {
                ranges: vec![('0', '9')],
                negated: true,
            }),
            Some('w') => Ok(RegexNode::Class {
                ranges: vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')],
                negated: false,
            }),
            Some('W') => Ok(RegexNode::Class {
                ranges: vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')],
                negated: true,
            }),
            Some('s') => Ok(RegexNode::Class {
                ranges: vec![(' ', ' '), ('\t', '\t'), ('\r', '\r'), ('\n', '\n')],
                negated: false,
            }),
            Some('S') => Ok(RegexNode::Class {
                ranges: vec![(' ', ' '), ('\t', '\t'), ('\r', '\r'), ('\n', '\n')],
                negated: true,
            }),
            Some('t') => Ok(RegexNode::Literal('\t')),
            Some('n') => Ok(RegexNode::Literal('\n')),
            Some('r') => Ok(RegexNode::Literal('\r')),
            Some(ch) => Ok(RegexNode::Literal(ch)),
            None => Err("trailing backslash in regex".to_string()),
        }
    }

    fn parse_class(&mut self) -> Result<RegexNode, String> {
        let negated = if self.peek() == Some('^') {
            self.next();
            true
        } else {
            false
        };
        let mut ranges = Vec::new();
        while let Some(ch) = self.next() {
            if ch == ']' {
                return Ok(RegexNode::Class { ranges, negated });
            }
            let first = if ch == '\\' {
                match self.next() {
                    Some('d') => {
                        ranges.push(('0', '9'));
                        continue;
                    }
                    Some('w') => {
                        ranges.extend(vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')]);
                        continue;
                    }
                    Some('s') => {
                        ranges.extend(vec![(' ', ' '), ('\t', '\t'), ('\r', '\r'), ('\n', '\n')]);
                        continue;
                    }
                    Some('t') => '\t',
                    Some('n') => '\n',
                    Some('r') => '\r',
                    Some(c) => c,
                    None => return Err("unclosed escape in character class".to_string()),
                }
            } else {
                ch
            };

            if self.peek() == Some('-') && self.chars.get(self.pos + 1) != Some(&']') {
                self.next(); // consume '-'
                let second = match self.next() {
                    Some('\\') => match self.next() {
                        Some('t') => '\t',
                        Some('n') => '\n',
                        Some('r') => '\r',
                        Some(c) => c,
                        None => return Err("unclosed escape in character class".to_string()),
                    },
                    Some(c) => c,
                    None => return Err("unclosed character class".to_string()),
                };
                ranges.push((first.min(second), first.max(second)));
            } else {
                ranges.push((first, first));
            }
        }
        Err("unclosed character class '['".to_string())
    }
}

#[derive(Clone, Debug)]
enum StateKind {
    Char(char),
    Any,
    Class {
        ranges: Vec<(char, char)>,
        negated: bool,
    },
    AnchorStart,
    AnchorEnd,
    Split,
    Match,
}

#[derive(Clone, Debug)]
struct State {
    kind: StateKind,
    next1: Option<usize>,
    next2: Option<usize>,
}

struct Frag {
    start: usize,
    exits: Vec<(usize, u8)>, // (state_index, 1 for next1 or 2 for next2)
}

struct Nfa {
    states: Vec<State>,
    start: usize,
}

impl Nfa {
    fn new(root: &RegexNode) -> Self {
        let mut states = Vec::new();
        let frag = Self::compile_node(root, &mut states);
        let match_state = states.len();
        states.push(State {
            kind: StateKind::Match,
            next1: None,
            next2: None,
        });
        for (s, slot) in frag.exits {
            if slot == 1 {
                states[s].next1 = Some(match_state);
            } else {
                states[s].next2 = Some(match_state);
            }
        }
        Self {
            states,
            start: frag.start,
        }
    }

    fn compile_node(node: &RegexNode, states: &mut Vec<State>) -> Frag {
        match node {
            RegexNode::Literal(ch) => {
                let s = states.len();
                states.push(State {
                    kind: StateKind::Char(*ch),
                    next1: None,
                    next2: None,
                });
                Frag {
                    start: s,
                    exits: vec![(s, 1)],
                }
            }
            RegexNode::Any => {
                let s = states.len();
                states.push(State {
                    kind: StateKind::Any,
                    next1: None,
                    next2: None,
                });
                Frag {
                    start: s,
                    exits: vec![(s, 1)],
                }
            }
            RegexNode::Class { ranges, negated } => {
                let s = states.len();
                states.push(State {
                    kind: StateKind::Class {
                        ranges: ranges.clone(),
                        negated: *negated,
                    },
                    next1: None,
                    next2: None,
                });
                Frag {
                    start: s,
                    exits: vec![(s, 1)],
                }
            }
            RegexNode::AnchorStart => {
                let s = states.len();
                states.push(State {
                    kind: StateKind::AnchorStart,
                    next1: None,
                    next2: None,
                });
                Frag {
                    start: s,
                    exits: vec![(s, 1)],
                }
            }
            RegexNode::AnchorEnd => {
                let s = states.len();
                states.push(State {
                    kind: StateKind::AnchorEnd,
                    next1: None,
                    next2: None,
                });
                Frag {
                    start: s,
                    exits: vec![(s, 1)],
                }
            }
            RegexNode::Empty => {
                let s = states.len();
                states.push(State {
                    kind: StateKind::Split,
                    next1: None,
                    next2: None,
                });
                Frag {
                    start: s,
                    exits: vec![(s, 1)],
                }
            }
            RegexNode::Concat(nodes) => {
                if nodes.is_empty() {
                    return Self::compile_node(&RegexNode::Empty, states);
                }
                let mut frags: Vec<Frag> = nodes
                    .iter()
                    .map(|n| Self::compile_node(n, states))
                    .collect();
                for i in 0..frags.len() - 1 {
                    let next_start = frags[i + 1].start;
                    for (s, slot) in frags[i].exits.drain(..) {
                        if slot == 1 {
                            states[s].next1 = Some(next_start);
                        } else {
                            states[s].next2 = Some(next_start);
                        }
                    }
                }
                let start = frags[0].start;
                let exits = frags.pop().unwrap().exits;
                Frag { start, exits }
            }
            RegexNode::Alt(nodes) => {
                if nodes.is_empty() {
                    return Self::compile_node(&RegexNode::Empty, states);
                }
                let mut branches: Vec<Frag> = nodes
                    .iter()
                    .map(|n| Self::compile_node(n, states))
                    .collect();
                let mut cur = branches.remove(0);
                for next_branch in branches {
                    let s = states.len();
                    states.push(State {
                        kind: StateKind::Split,
                        next1: Some(cur.start),
                        next2: Some(next_branch.start),
                    });
                    let mut exits = cur.exits;
                    exits.extend(next_branch.exits);
                    cur = Frag { start: s, exits };
                }
                cur
            }
            RegexNode::Star(inner) => {
                let frag = Self::compile_node(inner, states);
                let s = states.len();
                states.push(State {
                    kind: StateKind::Split,
                    next1: Some(frag.start),
                    next2: None,
                });
                for (es, slot) in frag.exits {
                    if slot == 1 {
                        states[es].next1 = Some(s);
                    } else {
                        states[es].next2 = Some(s);
                    }
                }
                Frag {
                    start: s,
                    exits: vec![(s, 2)],
                }
            }
            RegexNode::Plus(inner) => {
                let frag = Self::compile_node(inner, states);
                let s = states.len();
                states.push(State {
                    kind: StateKind::Split,
                    next1: Some(frag.start),
                    next2: None,
                });
                for (es, slot) in frag.exits {
                    if slot == 1 {
                        states[es].next1 = Some(s);
                    } else {
                        states[es].next2 = Some(s);
                    }
                }
                Frag {
                    start: frag.start,
                    exits: vec![(s, 2)],
                }
            }
            RegexNode::Quest(inner) => {
                let frag = Self::compile_node(inner, states);
                let s = states.len();
                states.push(State {
                    kind: StateKind::Split,
                    next1: Some(frag.start),
                    next2: None,
                });
                let mut exits = frag.exits;
                exits.push((s, 2));
                Frag { start: s, exits }
            }
        }
    }

    /// Leftmost match at or after `from`: `(start, end)` in char indices.
    ///
    /// One scan of the input carries a "start anywhere" thread, so the whole
    /// search costs O(n * states). The previous implementation re-ran the NFA
    /// from every possible start index, which is O(n^2 * states) — measured at
    /// 634 ms for 2 500 characters and past 30 s at 20 000 (audit A-15).
    ///
    /// Semantics are unchanged: the earliest start that can match wins, and for
    /// that start the longest end is reported (the old `match_from` also kept
    /// the last `Match` it saw).
    fn search(&self, chars: &[char], from: usize) -> Option<(usize, usize)> {
        let n = chars.len();
        let num_states = self.states.len();
        // (state, start) — the start index is what makes the result leftmost.
        let mut active: Vec<(usize, usize)> = Vec::new();
        let mut visited = vec![false; num_states];
        let mut next: Vec<(usize, usize)> = Vec::new();
        let mut visited_next = vec![false; num_states];
        let mut best: Option<(usize, usize)> = None;

        // A manual cursor rather than `for i in from..=n`: the loop needs the
        // index as a *position* (thread starts, `cur_idx`) and runs one step
        // past the last character, where there is no element to iterate.
        let mut i = from;
        loop {
            // `active` already holds the closures reached at position `i`.
            // Existing threads started earlier, so they claim states first and
            // a later start can never displace them.
            if best.is_none() {
                self.add_thread(self.start, i, i, n, &mut active, &mut visited, &mut best);
            }
            // No live thread only ends the search once a match is in hand: an
            // empty thread set just means nothing is in flight *at this
            // position*, and a later start can still match — that is how `$`
            // finds the empty match at the end of a non-empty string.
            if active.is_empty() && best.is_some() {
                return best;
            }
            if i == n {
                break;
            }
            let ch = chars[i];
            visited_next.fill(false);
            next.clear();
            for &(s_idx, thread_start) in &active {
                // Once a start is known to match, threads that began later
                // cannot improve the answer.
                if let Some((best_start, _)) = best {
                    if thread_start != best_start {
                        continue;
                    }
                }
                let step = match &self.states[s_idx].kind {
                    StateKind::Char(c) => *c == ch,
                    StateKind::Any => ch != '\n',
                    StateKind::Class { ranges, negated } => {
                        let in_range = ranges.iter().any(|&(lo, hi)| ch >= lo && ch <= hi);
                        // A negated class matches newline, as in PCRE/Python/Go.
                        // Only `.` excludes it (the `Any` arm above); excluding
                        // it here too made `[^a]` fail on every newline.
                        if *negated {
                            !in_range
                        } else {
                            in_range
                        }
                    }
                    _ => false,
                };
                if step {
                    if let Some(nxt) = self.states[s_idx].next1 {
                        self.add_thread(
                            nxt,
                            i + 1,
                            thread_start,
                            n,
                            &mut next,
                            &mut visited_next,
                            &mut best,
                        );
                    }
                }
            }
            std::mem::swap(&mut active, &mut next);
            std::mem::swap(&mut visited, &mut visited_next);
            i += 1;
        }

        best
    }

    /// Epsilon-closure of `start`, computed with an explicit stack. A
    /// pathological `a|b|c…` pattern chains one Split state per branch, so a
    /// recursive closure would overflow the call stack and abort the host;
    /// iteration has no depth limit.
    ///
    /// `thread_start` is the input index the thread began at, so a `Match` can
    /// report where the match started. A state already visited at this position
    /// belongs to an equal-or-earlier start and wins, which is what keeps the
    /// result leftmost without tracking every start separately.
    #[allow(clippy::too_many_arguments)]
    fn add_thread(
        &self,
        start: usize,
        cur_idx: usize,
        thread_start: usize,
        total_len: usize,
        active: &mut Vec<(usize, usize)>,
        visited: &mut [bool],
        best: &mut Option<(usize, usize)>,
    ) {
        let mut stack = vec![start];
        while let Some(s_idx) = stack.pop() {
            if visited[s_idx] {
                continue;
            }
            visited[s_idx] = true;
            match &self.states[s_idx].kind {
                StateKind::Split => {
                    if let Some(n2) = self.states[s_idx].next2 {
                        stack.push(n2);
                    }
                    if let Some(n1) = self.states[s_idx].next1 {
                        stack.push(n1);
                    }
                }
                StateKind::AnchorStart => {
                    if cur_idx == 0 {
                        if let Some(n1) = self.states[s_idx].next1 {
                            stack.push(n1);
                        }
                    }
                }
                StateKind::AnchorEnd => {
                    if cur_idx == total_len {
                        if let Some(n1) = self.states[s_idx].next1 {
                            stack.push(n1);
                        }
                    }
                }
                StateKind::Match => match best {
                    None => *best = Some((thread_start, cur_idx)),
                    // Same start: a later `Match` is a longer match.
                    Some((bs, be)) if *bs == thread_start => *be = cur_idx,
                    // A later start cannot beat the leftmost one.
                    Some(_) => {}
                },
                _ => active.push((s_idx, thread_start)),
            }
        }
    }
}

fn regex_compile(pattern: &str) -> Result<Nfa, String> {
    let mut parser = Parser::new(pattern);
    let ast = parser.parse()?;
    Ok(Nfa::new(&ast))
}

pub fn regex_match(pattern: &str, text: &str) -> Result<bool, String> {
    let nfa = regex_compile(pattern)?;
    let chars: Vec<char> = text.chars().collect();
    Ok(nfa.search(&chars, 0).is_some())
}

pub fn regex_find(pattern: &str, text: &str) -> Result<Option<String>, String> {
    let nfa = regex_compile(pattern)?;
    let chars: Vec<char> = text.chars().collect();
    Ok(nfa
        .search(&chars, 0)
        .map(|(s, e)| chars[s..e].iter().collect()))
}

pub fn regex_find_all(pattern: &str, text: &str) -> Result<Vec<String>, String> {
    let nfa = regex_compile(pattern)?;
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut pos = 0;
    // Each iteration resumes the scan where the last match ended, so the input
    // is walked once overall rather than once per candidate start.
    while pos <= chars.len() {
        match nfa.search(&chars, pos) {
            Some((s, e)) => {
                out.push(chars[s..e].iter().collect());
                pos = if e > s { e } else { s + 1 };
            }
            None => break,
        }
    }
    Ok(out)
}

pub fn regex_replace(pattern: &str, text: &str, replacement: &str) -> Result<String, String> {
    let nfa = regex_compile(pattern)?;
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut pos = 0;
    while pos < chars.len() {
        match nfa.search(&chars, pos) {
            Some((s, e)) => {
                // A match that starts at end-of-input is never replaced (the
                // pre-single-pass implementation skipped that position too),
                // but the unmatched prefix still has to be emitted.
                if s == chars.len() {
                    out.extend(&chars[pos..]);
                    break;
                }
                out.extend(&chars[pos..s]);
                out.push_str(replacement);
                if e > s {
                    pos = e;
                } else {
                    // Empty match: emit the character so the loop advances.
                    out.push(chars[s]);
                    pos = s + 1;
                }
            }
            None => {
                out.extend(&chars[pos..]);
                break;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_match() {
        assert!(regex_match("a+b", "aaab").unwrap());
        assert!(!regex_match("^a+b$", "aaabc").unwrap());
        assert!(regex_match("^a+b$", "aaab").unwrap());
    }

    #[test]
    fn test_find() {
        assert_eq!(
            regex_find(r"\d+", "abc123xyz").unwrap(),
            Some("123".to_string())
        );
        assert_eq!(
            regex_find(r"foo|bar", "start bar end").unwrap(),
            Some("bar".to_string())
        );
    }

    #[test]
    fn test_find_all() {
        assert_eq!(
            regex_find_all(r"\d+", "one 1 two 22 three 333").unwrap(),
            vec!["1".to_string(), "22".to_string(), "333".to_string()]
        );
    }

    #[test]
    fn test_replace() {
        assert_eq!(
            regex_replace(r"\d+", "user123_item456", "NUM").unwrap(),
            "userNUM_itemNUM"
        );
    }

    #[test]
    fn test_classes_and_anchors() {
        assert!(regex_match(r"^[a-zA-Z_]\w*$", "valid_var_1").unwrap());
        assert!(!regex_match(r"^[a-zA-Z_]\w*$", "1invalid").unwrap());
    }
}
