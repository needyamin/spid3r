//! Semantic versioning (`Version`) and constraint matching (`Constraint`)
//! with standard SemVer caret (`^0.x` / `^x.y.z`), tilde (`~`), comparator
//! (`>=`, `<=`, `>`, `<`, `=`), wildcard (`*`), and compound (`>=1.0, <2.0`)
//! ranges (fixes B3).

use std::fmt;

/// A `major.minor.patch` semantic version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parse `1.2.3`, `1.2` (`1.2.0`), or `1` (`1.0.0`).
    pub fn parse(text: &str) -> Result<Version, String> {
        let trimmed = text.trim();
        let parts: Vec<&str> = trimmed.split('.').collect();
        if parts.is_empty() || parts.len() > 3 || parts.iter().any(|p| p.is_empty()) {
            return Err(format!("malformed version '{}'", text));
        }
        let mut nums = [0u64; 3];
        for (i, p) in parts.iter().enumerate() {
            nums[i] = p
                .parse()
                .map_err(|_| format!("malformed version '{}' (component '{}')", text, p))?;
        }
        Ok(Version {
            major: nums[0],
            minor: nums[1],
            patch: nums[2],
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A version constraint declared on a dependency or compiler requirement.
#[derive(Debug, Clone, PartialEq)]
pub enum Constraint {
    /// `^x.y.z`: standard SemVer compatibility range.
    Caret(Version),
    /// `~x.y.z`: same major & minor, at least patch.
    Tilde(Version),
    /// `>=x.y.z`.
    AtLeast(Version),
    /// `<=x.y.z`.
    AtMost(Version),
    /// `>x.y.z`.
    Greater(Version),
    /// `<x.y.z`.
    Less(Version),
    /// `=x.y.z` (or bare `x.y.z`).
    Exact(Version),
    /// `*` (matches any valid version).
    Any,
    /// Comma-separated compound AND constraint (e.g. `>=1.0.0, <2.0.0`).
    All(Vec<Constraint>),
}

impl Constraint {
    /// Parse `^1.2.3`, `~1.2.3`, `>=1.2`, `<=2.0`, `>1.0`, `<2.0`, `=1.2.3`,
    /// bare `1.2.3`, `*`, or comma-separated combinations (`>=1.0.0, <2.0.0`).
    pub fn parse(text: &str) -> Result<Constraint, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("empty version constraint".to_string());
        }
        if trimmed.contains(',') {
            let mut parts = Vec::new();
            for item in trimmed.split(',') {
                let sub = item.trim();
                if sub.is_empty() {
                    return Err(format!("malformed compound constraint '{}'", text));
                }
                parts.push(Self::parse_single(sub)?);
            }
            if parts.len() == 1 {
                return Ok(parts.remove(0));
            }
            return Ok(Constraint::All(parts));
        }
        Self::parse_single(trimmed)
    }

    fn parse_single(text: &str) -> Result<Constraint, String> {
        let text = text.trim();
        if text == "*" {
            return Ok(Constraint::Any);
        }
        if let Some(rest) = text.strip_prefix('^') {
            return Ok(Constraint::Caret(Version::parse(rest)?));
        }
        if let Some(rest) = text.strip_prefix('~') {
            return Ok(Constraint::Tilde(Version::parse(rest)?));
        }
        if let Some(rest) = text.strip_prefix(">=") {
            return Ok(Constraint::AtLeast(Version::parse(rest)?));
        }
        if let Some(rest) = text.strip_prefix("<=") {
            return Ok(Constraint::AtMost(Version::parse(rest)?));
        }
        if let Some(rest) = text.strip_prefix('>') {
            return Ok(Constraint::Greater(Version::parse(rest)?));
        }
        if let Some(rest) = text.strip_prefix('<') {
            return Ok(Constraint::Less(Version::parse(rest)?));
        }
        let rest = text.strip_prefix('=').unwrap_or(text);
        Ok(Constraint::Exact(Version::parse(rest)?))
    }

    /// Does `v` satisfy this constraint?
    pub fn matches(&self, v: &Version) -> bool {
        match self {
            Constraint::Exact(want) => v == want,
            Constraint::AtLeast(want) => v >= want,
            Constraint::AtMost(want) => v <= want,
            Constraint::Greater(want) => v > want,
            Constraint::Less(want) => v < want,
            Constraint::Any => true,
            Constraint::Tilde(want) => {
                v.major == want.major && v.minor == want.minor && v.patch >= want.patch
            }
            Constraint::Caret(want) => {
                if want.major > 0 {
                    v.major == want.major && (v.minor, v.patch) >= (want.minor, want.patch)
                } else if want.minor > 0 {
                    v.major == 0 && v.minor == want.minor && v.patch >= want.patch
                } else {
                    v == want
                }
            }
            Constraint::All(parts) => parts.iter().all(|c| c.matches(v)),
        }
    }

    /// Parse `ver` and evaluate [`Constraint::matches`].
    pub fn matches_version_text(&self, ver: &str) -> Result<bool, String> {
        let parsed = Version::parse(ver)?;
        Ok(self.matches(&parsed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_zero_minor_and_patch_follow_semver() {
        let c02 = Constraint::parse("^0.2.1").unwrap();
        assert!(c02.matches(&Version::parse("0.2.1").unwrap()));
        assert!(c02.matches(&Version::parse("0.2.9").unwrap()));
        assert!(!c02.matches(&Version::parse("0.2.0").unwrap()));
        assert!(!c02.matches(&Version::parse("0.3.0").unwrap()));

        let c003 = Constraint::parse("^0.0.3").unwrap();
        assert!(c003.matches(&Version::parse("0.0.3").unwrap()));
        assert!(!c003.matches(&Version::parse("0.0.4").unwrap()));
        assert!(!c003.matches(&Version::parse("0.1.0").unwrap()));
    }

    #[test]
    fn tilde_and_compound_ranges_match_correctly() {
        let tilde = Constraint::parse("~1.2.3").unwrap();
        assert!(tilde.matches(&Version::parse("1.2.3").unwrap()));
        assert!(tilde.matches(&Version::parse("1.2.9").unwrap()));
        assert!(!tilde.matches(&Version::parse("1.3.0").unwrap()));

        let range = Constraint::parse(">=1.0.0, <2.0.0").unwrap();
        assert!(range.matches(&Version::parse("1.0.0").unwrap()));
        assert!(range.matches(&Version::parse("1.9.9").unwrap()));
        assert!(!range.matches(&Version::parse("2.0.0").unwrap()));
        assert!(!range.matches(&Version::parse("0.9.9").unwrap()));
    }
}
