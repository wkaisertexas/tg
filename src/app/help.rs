use crate::config::Config;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub(super) fn config_help_lines(config: &Config) -> Vec<String> {
    vec![
        "Keys: Space ? help · Space p / :providers setup · Esc/q/? close".into(),
        "Completion: Tab/Enter accept · Up/Down or Ctrl-J/Ctrl-K select".into(),
        format!("Preview: {}", config.ui.preview_toggle),
        format!(
            "Commands: :w · :q · :q! · :wq · {} · :r !command",
            config.editor.copy_command
        ),
        format!(
            "[leaders] files={:?} broad_files={:?} symbols={:?}",
            config.leaders.files, config.leaders.broad_files, config.leaders.symbols
        ),
        format!(
            "[leaders] skills={:?} github_issues={:?} github_pull_requests={:?} jira_issues={:?}",
            config.leaders.skills,
            config.leaders.github_issues,
            config.leaders.github_pull_requests,
            config.leaders.jira_issues
        ),
        format!(
            "[editor] line_numbers={} current_line_absolute={} tab_width={} wrap={}",
            format!("{:?}", config.editor.line_numbers).to_ascii_lowercase(),
            config.editor.current_line_absolute,
            config.editor.tab_width,
            config.editor.wrap
        ),
        format!(
            "[ui] preview={} color={} completion={}x{}%",
            format!("{:?}", config.ui.preview).to_ascii_lowercase(),
            format!("{:?}", config.ui.color).to_ascii_lowercase(),
            config.ui.completion_height,
            config.ui.completion_width_percent
        ),
        format!(
            "[search] limit={} debounce_ms={} broad_excludes={:?}",
            config.search.limit, config.search.debounce_ms, config.search.broad_excludes
        ),
        format!(
            "[tokens] tokenizer={:?} decimals={} file={} symbol={} total={}",
            config.tokens.tokenizer,
            config.tokens.decimals,
            config.tokens.show_file,
            config.tokens.show_symbol,
            config.tokens.show_total
        ),
        format!(
            "[skills] profile={:?} mention={:?} roots={}",
            config.skills.profile,
            config.skills.mention,
            config.skills.roots.len()
        ),
        format!(
            "[providers.github] enabled={} command={} limit={} timeout_ms={}",
            config.providers.github.enabled,
            config.providers.github.command.display(),
            config.providers.github.limit,
            config.providers.github.timeout_ms
        ),
        format!(
            "[providers.jira] enabled={} command={} key_prefix={:?} limit={} timeout_ms={}",
            config.providers.jira.enabled,
            config.providers.jira.command.display(),
            config.providers.jira.key_prefix,
            config.providers.jira.limit,
            config.providers.jira.timeout_ms
        ),
    ]
}

pub(super) fn styled_help_lines(lines: &[String], color: bool) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            if line.starts_with('[')
                && let Some(end) = line.find(']')
            {
                return Line::from(vec![
                    Span::styled(
                        line[..=end].to_owned(),
                        if color {
                            Style::default()
                                .fg(Color::Magenta)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().add_modifier(Modifier::BOLD)
                        },
                    ),
                    Span::styled(
                        line[end + 1..].to_owned(),
                        if color {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default()
                        },
                    ),
                ]);
            }
            if let Some((label, value)) = line.split_once(':') {
                return Line::from(vec![
                    Span::styled(
                        format!("{label}:"),
                        if color {
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().add_modifier(Modifier::BOLD)
                        },
                    ),
                    Span::styled(
                        value.to_owned(),
                        if color {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default().add_modifier(Modifier::UNDERLINED)
                        },
                    ),
                ]);
            }
            Line::from(line.clone())
        })
        .collect()
}
