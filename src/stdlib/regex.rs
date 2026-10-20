//! Zero-dependency Thompson NFA regex engine.
//!
//! Supports literals, '.', '*', '+', '?', '^', '$', character classes '[abc]',
//! ranges '[a-z]', negations '[^...]', escapes '\d', '\w', '\s', '\D', '\W', '\S',
//! alternation '|', and grouping '()'. Linear time execution with no exponential backtracking.

#[derive(Clone, Debug, PartialEq)]
enum RegexNode {
    Literal(char),
    Any,
    Class { ranges: Vec<(char, char)>, negated: bool },
    AnchorStart,
    AnchorEnd,
    Concat(Vec<RegexNode>),
    Alt(Vec<RegexNode>),
    Star(Box<RegexNode>),
    Plus(Box<RegexNode>),
    Quest(Box<RegexNode>),
    Empty,
}

struct Parser<'a> {
    chars: Vec<char>,
    pos: usize,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> Parser<'a> {
    fn new(pattern: &'a str) -> Self {
        Self {
            chars: pattern.chars().collect(),
            pos: 0,
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
            return Err(format!("unexpected character '{}' at position {}", self.chars[self.pos], self.pos));
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
                let inner = self.parse_alt()?;
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
    Class { ranges: Vec<(char, char)>, negated: bool },
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
                let mut frags: Vec<Frag> = nodes.iter().map(|n| Self::compile_node(n, states)).collect();
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
                let mut branches: Vec<Frag> = nodes.iter().map(|n| Self::compile_node(n, states)).collect();
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

    /// Finds longest match starting at `start_idx`. Returns Some(end_idx).
    fn match_from(&self, chars: &[char], start_idx: usize) -> Option<usize> {
        let n = chars.len();
        let num_states = self.states.len();
        let mut active = Vec::new();
        let mut visited = vec![false; num_states];
        let mut matched_end = None;

        self.add_state(self.start, start_idx, n, &mut active, &mut visited, &mut matched_end);

        for (cur_idx, &ch) in chars.iter().enumerate().skip(start_idx) {
            if active.is_empty() {
                break;
            }
            let mut next_active = Vec::new();
            visited.fill(false);

            for &s_idx in &active {
                match &self.states[s_idx].kind {
                    StateKind::Char(c) => {
                        if *c == ch {
                            if let Some(nxt) = self.states[s_idx].next1 {
                                self.add_state(nxt, cur_idx + 1, n, &mut next_active, &mut visited, &mut matched_end);
                            }
                        }
                    }
                    StateKind::Any => {
                        if ch != '\n' {
                            if let Some(nxt) = self.states[s_idx].next1 {
                                self.add_state(nxt, cur_idx + 1, n, &mut next_active, &mut visited, &mut matched_end);
                            }
                        }
                    }
                    StateKind::Class { ranges, negated } => {
                        let in_range = ranges.iter().any(|&(lo, hi)| ch >= lo && ch <= hi);
                        let ok = if *negated { !in_range && ch != '\n' } else { in_range };
                        if ok {
                            if let Some(nxt) = self.states[s_idx].next1 {
                                self.add_state(nxt, cur_idx + 1, n, &mut next_active, &mut visited, &mut matched_end);
                            }
                        }
                    }
                    _ => {}
                }
            }
            active = next_active;
        }

        matched_end
    }

    fn add_state(
        &self,
        s_idx: usize,
        cur_idx: usize,
        total_len: usize,
        active: &mut Vec<usize>,
        visited: &mut [bool],
        matched_end: &mut Option<usize>,
    ) {
        if visited[s_idx] {
            return;
        }
        visited[s_idx] = true;
        match &self.states[s_idx].kind {
            StateKind::Split => {
                if let Some(n1) = self.states[s_idx].next1 {
                    self.add_state(n1, cur_idx, total_len, active, visited, matched_end);
                }
                if let Some(n2) = self.states[s_idx].next2 {
                    self.add_state(n2, cur_idx, total_len, active, visited, matched_end);
                }
            }
            StateKind::AnchorStart => {
                if cur_idx == 0 {
                    if let Some(n1) = self.states[s_idx].next1 {
                        self.add_state(n1, cur_idx, total_len, active, visited, matched_end);
                    }
                }
            }
            StateKind::AnchorEnd => {
                if cur_idx == total_len {
                    if let Some(n1) = self.states[s_idx].next1 {
                        self.add_state(n1, cur_idx, total_len, active, visited, matched_end);
                    }
                }
            }
            StateKind::Match => {
                *matched_end = Some(cur_idx);
            }
            _ => {
                active.push(s_idx);
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
    for start in 0..=chars.len() {
        if nfa.match_from(&chars, start).is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn regex_find(pattern: &str, text: &str) -> Result<Option<String>, String> {
    let nfa = regex_compile(pattern)?;
    let chars: Vec<char> = text.chars().collect();
    for start in 0..=chars.len() {
        if let Some(end) = nfa.match_from(&chars, start) {
            let matched: String = chars[start..end].iter().collect();
            return Ok(Some(matched));
        }
    }
    Ok(None)
}

pub fn regex_find_all(pattern: &str, text: &str) -> Result<Vec<String>, String> {
    let nfa = regex_compile(pattern)?;
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start <= chars.len() {
        if let Some(end) = nfa.match_from(&chars, start) {
            let matched: String = chars[start..end].iter().collect();
            out.push(matched);
            if end > start {
                start = end;
            } else {
                start += 1;
            }
        } else {
            start += 1;
        }
    }
    Ok(out)
}

pub fn regex_replace(pattern: &str, text: &str, replacement: &str) -> Result<String, String> {
    let nfa = regex_compile(pattern)?;
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut start = 0;
    while start <= chars.len() {
        if start == chars.len() {
            break;
        }
        if let Some(end) = nfa.match_from(&chars, start) {
            out.push_str(replacement);
            if end > start {
                start = end;
            } else {
                out.push(chars[start]);
                start += 1;
            }
        } else {
            out.push(chars[start]);
            start += 1;
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
        assert_eq!(regex_find(r"\d+", "abc123xyz").unwrap(), Some("123".to_string()));
        assert_eq!(regex_find(r"foo|bar", "start bar end").unwrap(), Some("bar".to_string()));
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
