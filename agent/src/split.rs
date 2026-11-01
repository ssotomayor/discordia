/// Cuts at a line, else a space, past half the limit; a reply past the last
/// part ends in an ellipsis rather than being dropped or flooding the channel.
pub(crate) fn split_reply(text: &str, max_bytes: usize, max_parts: usize) -> Vec<String> {
    const MORE: &str = "…";
    let mut out = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() && out.len() < max_parts {
        if rest.len() <= max_bytes {
            out.push(rest.to_string());
            break;
        }
        if out.len() + 1 == max_parts {
            let cut = floor_boundary(rest, max_bytes - MORE.len());
            out.push(format!("{}{MORE}", rest[..cut].trim_end()));
            break;
        }
        let hard = floor_boundary(rest, max_bytes);
        let soft = rest[..hard]
            .rfind('\n')
            .or_else(|| rest[..hard].rfind(' '))
            .filter(|&i| i > max_bytes / 2);
        let cut = soft.unwrap_or(hard);
        out.push(rest[..cut].trim_end().to_string());
        rest = rest[cut..].trim_start();
    }
    out
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_part() {
        assert_eq!(split_reply("  hi  ", 2000, 4), vec!["hi"]);
        assert!(split_reply("   ", 2000, 4).is_empty());
    }

    #[test]
    fn parts_fit_and_prefer_line_breaks() {
        let text = format!("{}\n{}", "a".repeat(1500), "b".repeat(1500));
        let parts = split_reply(&text, 2000, 4);
        assert_eq!(parts, vec!["a".repeat(1500), "b".repeat(1500)]);
    }

    #[test]
    fn multibyte_text_is_never_cut_inside_a_character() {
        let text = "é".repeat(3000);
        let parts = split_reply(&text, 2000, 4);
        assert!(parts.iter().all(|p| p.len() <= 2000));
        assert_eq!(parts.concat().chars().count(), 3000);
    }

    #[test]
    fn past_the_last_part_it_ends_in_an_ellipsis() {
        let parts = split_reply(&"word ".repeat(2000), 2000, 2);
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| p.len() <= 2000));
        assert!(parts[1].ends_with('…'));
    }
}
