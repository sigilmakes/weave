use pa_test_scratch as _;
use pa_tui::ansi::strip_ansi;
use pa_tui::chat::{ChatEntry, StatusKind};
use pa_tui::theme::{ColorMode, Theme};
use pa_tui::view::AgentView;
use pa_tui::Line;

fn session(entries: usize) -> (AgentView, Vec<String>) {
    let mut view = AgentView::new(Theme::builtin("prime", ColorMode::TrueColor));
    view.splash_suppressed = true;
    let mut expected = Vec::new();
    for index in 0..entries {
        let first = format!("entry-{index}/first");
        let second = format!("entry-{index}/second");
        view.push_entry(ChatEntry::Status {
            text: format!("{first}\n{second}"),
            kind: StatusKind::Info,
        });
        expected.extend([String::new(), format!(" {first}"), format!(" {second}")]);
    }
    (view, expected)
}

fn plain(rows: &[Line]) -> Vec<String> {
    rows.iter()
        .map(|line| {
            let text: String = line.iter().map(|span| span.content.as_str()).collect();
            strip_ansi(&text).trim_end().to_string()
        })
        .collect()
}

fn assert_window(view: &mut AgentView, expected: &[String], start: usize) {
    let frame = view.render_frame(100, 30);
    let checked = view.page_size();
    assert_eq!(
        plain(&frame[1..=checked]),
        expected[start..start + checked],
        "transcript window at row {start}"
    );
    if view.is_following() {
        assert_eq!(
            plain(&frame[checked + 1..=checked + 1]),
            expected[start + checked..=start + checked]
        );
    }
}

fn traverse(view: &mut AgentView, expected: &[String]) {
    view.scroll_to_bottom();
    view.render_frame(100, 30);
    let step = view.page_size();
    let tail = expected.len() - (step + 1);
    assert_window(view, expected, tail);
    assert!(view.is_following());

    view.scroll_to_top();
    assert_window(view, expected, 0);
    assert!(!view.is_following());
    let mut start = 0;
    for _ in (0..tail).step_by(step) {
        view.scroll_by(isize::try_from(step).expect("viewport fits isize"));
        start = (start + step).min(tail);
        assert_window(view, expected, start);
    }
    assert_eq!(start, tail);
    assert!(view.is_following());

    for _ in (0..tail).step_by(step) {
        view.scroll_by(-isize::try_from(step).expect("viewport fits isize"));
        start = start.saturating_sub(step);
        assert_window(view, expected, start);
    }
    assert_eq!(start, 0);
    assert!(!view.is_following());
}

fn verify(entries: usize) {
    let (mut view, mut expected) = session(entries);
    traverse(&mut view, &expected);

    view.scroll_to_bottom();
    view.render_frame(100, 30);
    let edited = entries / 2;
    let replacement = format!("entry-{edited}/edited\nentry-{edited}/extra\nentry-{edited}/last");
    view.prepare_entry_mutation(edited);
    view.chat[edited] = ChatEntry::Status {
        text: replacement.clone(),
        kind: StatusKind::Warning,
    };
    view.mark_entry_stale(edited);
    expected.splice(
        edited * 3 + 1..edited * 3 + 3,
        replacement.lines().map(|line| format!(" {line}")),
    );
    traverse(&mut view, &expected);
}

#[test]
fn transcript_100_entries() {
    verify(100);
}

#[test]
fn transcript_1000_entries() {
    verify(1_000);
}

#[test]
fn transcript_10000_entries() {
    verify(10_000);
}
