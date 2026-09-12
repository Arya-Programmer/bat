#![allow(unused)] // Because indirectly included by e.g. system_wide_config.rs, but not used

/// Removes the ANSI escape sequences that `--color=always` adds, so that tests
/// can assert on the text that ends up on screen.
pub fn strip_escapes(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            output.push(c);
            continue;
        }
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    output
}
