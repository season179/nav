pub(super) fn preview_output(output: &str, max_lines: usize, max_chars: usize) -> String {
    if output.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = output.lines().collect();
    let mut shown = Vec::new();
    let mut used_chars = 0usize;
    let mut truncated_by_chars = false;
    for line in lines.iter().take(max_lines) {
        let line_chars = line.chars().count();
        if used_chars.saturating_add(line_chars) > max_chars {
            let remaining = max_chars.saturating_sub(used_chars);
            let mut partial = line.chars().take(remaining).collect::<String>();
            partial.push('…');
            shown.push(partial);
            truncated_by_chars = true;
            break;
        }
        shown.push((*line).to_string());
        used_chars += line_chars;
    }
    let hidden_lines = lines.len().saturating_sub(shown.len());
    if hidden_lines > 0 {
        shown.push(format!("… {hidden_lines} more lines hidden"));
    } else if truncated_by_chars {
        shown.push("… output truncated".to_string());
    }
    shown.join("\n")
}
