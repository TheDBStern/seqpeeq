//! The README's pictures: the example files in the real interface, drawn as
//! SVG into `docs/screens/`. Each test fails when its picture no longer
//! matches what the interface draws;
//! `UPDATE_SCREENS=1 cargo test -p seqpeeq screens_match` writes them again.

use std::path::PathBuf;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Size;
use seqpeeq_core::{BamCmd, Command};

use crate::app::App;
use crate::app::bam::tests::id;
use crate::ui::svg::{buffer_to_svg, swatch_svg};
use crate::ui::tests::{render, settle, with_file};

const WIDTH: u16 = 100;
const HEIGHT: u16 = 30;

/// The example files, from the crate's folder (where tests run), so the
/// screens hold no machine's paths.
const EXAMPLES: &str = "../../examples";

fn screens() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/screens")
}

fn example(name: &str) -> String {
    format!("{EXAMPLES}/{name}")
}

/// An app showing the example `name` on the README's screen size.
fn showing(name: &str) -> App {
    let mut app = with_file(&example(name));
    app.resize(Size::new(WIDTH, HEIGHT));
    settle(&mut app);
    app
}

fn open(app: &mut App, command: Command) {
    app.enqueue(command);
    settle(app);
}

/// Compare the picture of `terminal` with `docs/screens/<name>.svg`, or
/// write it when `UPDATE_SCREENS` is set.
fn check(name: &str, app: &mut App) {
    app.status.clear();
    let terminal: Terminal<TestBackend> = render(app, WIDTH, HEIGHT);
    let svg = buffer_to_svg(terminal.backend().buffer());
    let path = screens().join(format!("{name}.svg"));
    if std::env::var_os("UPDATE_SCREENS").is_some() {
        std::fs::create_dir_all(screens()).unwrap();
        std::fs::write(&path, svg).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == svg,
        "docs/screens/{name}.svg is stale: UPDATE_SCREENS=1 cargo test -p seqpeeq screens_match"
    );
}

/// Show the current BAM from `left` with `per_col` bases a column.
fn bam_at(app: &mut App, left: u64, per_col: u64) {
    let id = id(app);
    let view = app.bams.get_mut(&id).unwrap();
    view.per_col = per_col;
    view.left = left;
    view.cursor = left;
    view.repack();
    settle(app);
}

fn scene_alignment() {
    let mut app = showing("aln.fa");
    let id = app.session.datasets()[app.active].id;
    let view = app.views.get_mut(&id).unwrap();
    view.left = 579;
    view.cursor.col = 579;
    settle(&mut app);
    check("alignment", &mut app);
}

fn scene_tree() {
    let mut app = showing("aln.fa");
    open(
        &mut app,
        Command::Open {
            path: example("tree.nwk").into(),
            kind: None,
        },
    );
    check("tree", &mut app);
}

fn scene_bam_bases() {
    let mut app = showing("reads.bam");
    open(
        &mut app,
        Command::Bam(BamCmd::Ref {
            file: example("ref.fa").into(),
        }),
    );
    bam_at(&mut app, 1_001, 1);
    check("bam-bases", &mut app);
}

fn scene_bam_zoomed() {
    let mut app = showing("reads.bam");
    bam_at(&mut app, 9_001, 64);
    check("bam-zoomed", &mut app);
}

fn scene_bam_stack() {
    let mut app = showing("reads.bam");
    open(
        &mut app,
        Command::Open {
            path: example("reads2.bam").into(),
            kind: None,
        },
    );
    open(
        &mut app,
        Command::Bam(BamCmd::Ref {
            file: example("ref.fa").into(),
        }),
    );
    bam_at(&mut app, 1_001, 1);
    check("bam-stack", &mut app);
}

/// Zoomed out with the reference: the coverage row coloured where the reads
/// differ, ticked reads, and the readout of the half-and-half SNP at 1,045.
fn scene_bam_variants() {
    let mut app = showing("reads.bam");
    open(
        &mut app,
        Command::Bam(BamCmd::Ref {
            file: example("ref.fa").into(),
        }),
    );
    bam_at(&mut app, 1_001, 2);
    let id = id(&app);
    let view = app.bams.get_mut(&id).unwrap();
    view.cursor = 1_045;
    view.readout = true;
    check("bam-variants", &mut app);
}

/// The legends' color swatches, by file name, each from the palette it
/// shows: `docs/screens/swatches/<name>.svg`.
fn swatches() -> Vec<(&'static str, ratatui::style::Color)> {
    use crate::bam_view::ReadKind;
    use crate::palette;
    use seqpeeq_core::bam::Signal;
    let fg = |style: ratatui::style::Style| style.fg.expect("a foreground");
    let variant = |fraction, indel| {
        fg(palette::variant(&Signal {
            pos: 1,
            fraction,
            indel,
        }))
    };
    vec![
        ("snp-15", variant(0.15, false)),
        ("snp-35", variant(0.35, false)),
        ("snp-65", variant(0.65, false)),
        ("snp-90", variant(0.9, false)),
        ("indel-15", variant(0.15, true)),
        ("indel-65", variant(0.65, true)),
        ("tick-1", fg(palette::tick(1, false))),
        ("tick-2", fg(palette::tick(2, false))),
        ("tick-5", fg(palette::tick(5, false))),
        ("tick-indel", fg(palette::tick(0, true))),
        ("read-split", fg(palette::read(ReadKind::Split))),
        (
            "read-mate-elsewhere",
            fg(palette::read(ReadKind::MateElsewhere)),
        ),
        ("read-improper", fg(palette::read(ReadKind::Improper))),
        ("coverage", fg(palette::DEPTH)),
        ("insertion", palette::INSERTION.bg.expect("a background")),
        ("selected", palette::SELECTED.bg.expect("a background")),
        ("base-a", fg(palette::base(b'A'))),
        ("base-c", fg(palette::base(b'C'))),
        ("base-g", fg(palette::base(b'G'))),
        ("base-t", fg(palette::base(b'T'))),
    ]
}

/// Compare each swatch with `docs/screens/swatches/`, or write them when
/// `UPDATE_SCREENS` is set.
fn scene_swatches() {
    let dir = screens().join("swatches");
    for (name, color) in swatches() {
        let svg = swatch_svg(color);
        let path = dir.join(format!("{name}.svg"));
        if std::env::var_os("UPDATE_SCREENS").is_some() {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, svg).unwrap();
            continue;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == svg,
            "docs/screens/swatches/{name}.svg is stale: UPDATE_SCREENS=1 cargo test -p seqpeeq screens_match"
        );
    }
}

/// One test draws every scene in turn: five at once, each loading BAMs,
/// would starve the timing tests running beside them.
#[test]
fn screens_match_the_interface() {
    scene_alignment();
    scene_tree();
    scene_bam_bases();
    scene_bam_zoomed();
    scene_bam_stack();
    scene_bam_variants();
    scene_swatches();
}

#[test]
fn the_docs_name_only_swatches_there_are() {
    let names: Vec<&str> = swatches().iter().map(|(name, _)| *name).collect();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut used = 0;
    for doc in ["README.md", "docs/keys.md"] {
        let text = std::fs::read_to_string(root.join(doc)).unwrap();
        for (i, _) in text.match_indices("swatches/") {
            let name = text[i + "swatches/".len()..].split(".svg").next().unwrap();
            assert!(names.contains(&name), "{doc} names swatch {name:?}");
            used += 1;
        }
    }
    assert!(
        used >= names.len(),
        "only {used} swatches shown in the docs"
    );
}

#[test]
fn screens_hold_no_machine_paths() {
    for name in [
        "alignment",
        "tree",
        "bam-bases",
        "bam-zoomed",
        "bam-stack",
        "bam-variants",
    ] {
        let svg = std::fs::read_to_string(screens().join(format!("{name}.svg"))).expect(name);
        for path in ["/Users/", "/home/", "../"] {
            assert!(!svg.contains(path), "{name}.svg holds {path}");
        }
    }
}
