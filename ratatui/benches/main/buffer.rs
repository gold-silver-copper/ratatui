use std::hint::black_box;

use criterion::{BenchmarkId, Criterion};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;

criterion::criterion_group!(benches, empty, filled, with_lines, set_string, diff);

const fn rect(size: u16) -> Rect {
    Rect::new(0, 0, size, size)
}

fn empty(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/empty");
    for size in [16, 64, 255] {
        let area = rect(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &area, |b, &area| {
            b.iter(|| {
                let _buffer = Buffer::empty(black_box(area));
            });
        });
    }
    group.finish();
}

/// This likely should have the same performance as `empty`, but it's here for completeness
/// and to catch any potential performance regressions.
fn filled(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/filled");
    for size in [16, 64, 255] {
        let area = rect(size);
        let cell = Cell::new("AAAA"); // simulate a multi-byte character
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &(area, cell),
            |b, (area, cell)| {
                b.iter(|| {
                    let _buffer = Buffer::filled(black_box(*area), cell.clone());
                });
            },
        );
    }
    group.finish();
}

fn with_lines(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/with_lines");
    for size in [16, 64, 255] {
        let word_count = 50;
        let lines = fakeit::words::sentence(word_count);
        let lines = lines.lines().map(Line::from);
        group.bench_with_input(BenchmarkId::from_parameter(size), &lines, |b, lines| {
            b.iter(|| {
                let _buffer = Buffer::with_lines(black_box(lines.clone()));
            });
        });
    }
    group.finish();
}

/// Rows of mixed narrow, wide and emoji text, the content the write path and the diff have to
/// handle specially.
const MIXED: [&str; 3] = [
    "ratatui 是一个用于构建终端用户界面的库 ",
    "表格 列表 图表 ❤️ 😀 段落 and plain ascii ",
    "plain ascii only, no wide characters at all ",
];

fn fill_rows(area: Rect, text: &str, style: Style) -> Buffer {
    let mut buffer = Buffer::empty(area);
    let line = text.repeat(usize::from(area.width) / text.len() + 1);
    for y in area.top()..area.bottom() {
        buffer.set_stringn(area.x, y, &line, usize::from(area.width), style);
    }
    buffer
}

fn set_string(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/set_string");
    let area = Rect::new(0, 0, 200, 50);
    for (name, text) in ["cjk", "emoji", "ascii"].into_iter().zip(MIXED) {
        group.bench_with_input(BenchmarkId::from_parameter(name), &text, |b, text| {
            b.iter(|| fill_rows(area, black_box(text), Style::new()));
        });
    }
    group.finish();
}

fn diff(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer/diff");
    let area = Rect::new(0, 0, 200, 50);
    for (name, text) in ["cjk", "emoji", "ascii"].into_iter().zip(MIXED) {
        let prev = fill_rows(area, text, Style::new());
        let unchanged = prev.clone();
        let restyled = fill_rows(area, text, Style::new().bg(Color::Blue));
        let replaced = fill_rows(area, "x", Style::new());
        for (case, next) in [
            ("unchanged", unchanged),
            ("restyled", restyled),
            ("replaced", replaced),
        ] {
            group.bench_with_input(
                BenchmarkId::new(name, case),
                &(&prev, &next),
                |b, (prev, next)| b.iter(|| black_box(prev.diff_iter(next).count())),
            );
        }
    }
    group.finish();
}
