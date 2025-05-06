use regex::Regex;
use std::collections::HashMap;
use std::collections::VecDeque;

use log::trace;

#[derive(Clone, Debug)]
pub struct BranchFallback {
    pub pattern: Regex,
    pub replacement: String,
}

impl BranchFallback {
    pub fn parse(s: &str) -> Result<BranchFallback, String> {
        let mut chars = s.chars();

        let delim = chars.next().ok_or("Empty fallback string")?;

        let mut parts = Vec::new();
        let mut current = String::new();
        let mut in_escape = false;

        for c in chars {
            if in_escape {
                if c != delim {
                    current.push('\\');
                }
                current.push(c);
                in_escape = false;
            } else if c == '\\' {
                in_escape = true;
            } else if c == delim && parts.len() < 2 {
                parts.push(current);
                current = String::new();
            } else {
                current.push(c);
            }
        }

        if in_escape {
            return Err("Trailing escape character".to_string());
        }

        if !current.is_empty() || parts.len() != 2 {
            return Err(format!(
                "Expected format: {d}regex{d}replacement{d}",
                d = delim
            ));
        }

        let regex_str = &parts[0];
        let replacement = &parts[1];

        let pattern =
            Regex::new(regex_str).map_err(|e| format!("Invalid regex '{}': {}", regex_str, e))?;

        Ok(BranchFallback {
            pattern,
            replacement: replacement.clone(),
        })
    }
}

pub fn resolve<'a, T>(
    target_branch: &'a str,
    fallbacks: &Vec<BranchFallback>,
    available_branches: &HashMap<&'a str, &'a T>,
) -> Option<&'a T> {
    let mut candidates = VecDeque::new();
    candidates.push_back(target_branch.to_string());

    while let Some(cand) = candidates.pop_front() {
        trace!("Trying: {}", cand);
        if let Some(b) = available_branches.get(cand.as_str()) {
            return Some(b);
        }
        for fb in fallbacks {
            trace!(
                "Trying transformation: {}  --> {}",
                fb.pattern.as_str(),
                fb.replacement
            );
            let new_cand = fb.pattern.replace(&cand, &fb.replacement);
            if new_cand != cand {
                trace!("Transformed: {} -> {}", cand, new_cand);
                /* Only allow shorter branches so that it is guarenteede to terminate */
                if new_cand.len() < cand.len() {
                    candidates.push_back(new_cand.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_delitimter_is_quoted() {
        let input = r"%abc%this\is\100\%%";
        let fallback = BranchFallback::parse(input).expect("should parse");

        assert_eq!(fallback.pattern.as_str(), "abc");
        assert_eq!(fallback.replacement, r"this\is\100%");
    }

    #[test]
    fn parses_basic_slash_delimited() {
        let input = r"/foo-(\d+)/bar-$1/";
        let fallback = BranchFallback::parse(input).expect("should parse");

        assert_eq!(fallback.pattern.as_str(), r"foo-(\d+)");
        assert_eq!(fallback.replacement, "bar-$1");
    }

    #[test]
    fn parses_percent_delimited() {
        let input = "%abc%d123%";
        let fallback = BranchFallback::parse(input).expect("should parse");

        assert_eq!(fallback.pattern.as_str(), "abc");
        assert_eq!(fallback.replacement, "d123");
    }

    #[test]
    fn parses_pipe_delimited_with_escape() {
        let input = r"|a\|b|repl\|acement|";
        let fallback = BranchFallback::parse(input).expect("should parse");

        assert_eq!(fallback.pattern.as_str(), "a|b");
        assert_eq!(fallback.replacement, "repl|acement");
    }

    #[test]
    fn error_on_missing_replacement() {
        let err = BranchFallback::parse("/abc/").unwrap_err();
        assert!(
            err.contains("Expected format"),
            "Got unexpected error: {}",
            err
        );
    }

    #[test]
    fn error_on_extra() {
        let err = BranchFallback::parse("%abc%d123%extra").unwrap_err();
        assert!(
            err.contains("Expected format"),
            "Got unexpected error: {}",
            err
        );
    }

    #[test]
    fn error_on_unclosed_escape() {
        let err = BranchFallback::parse("/abc\\/repl\\").unwrap_err();
        assert_eq!(err, "Trailing escape character");
    }

    #[test]
    fn error_on_invalid_regex() {
        let err = BranchFallback::parse("/(unclosed-group/repl/").unwrap_err();
        assert!(
            err.contains("Invalid regex"),
            "Expected regex error, got: {}",
            err
        );
    }

    #[test]
    fn error_on_empty_input() {
        let err = BranchFallback::parse("").unwrap_err();
        assert_eq!(err, "Empty fallback string");
    }
}
