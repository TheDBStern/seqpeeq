//! Reading and writing Newick.

use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;

use flate2::read::MultiGzDecoder;

use crate::error::{Error, Result};
use crate::progress::Progress;
use crate::tree::{NodeId, Tree};
use crate::write::create_text;

/// A tree read from a file, with what the open message should mention.
#[derive(Debug)]
pub struct Parsed {
    pub tree: Tree,
    /// Further trees in the file, not read.
    pub skipped: usize,
    /// Whether `[…]` comments were dropped.
    pub comments: bool,
}

impl Tree {
    /// Read the first tree in a Newick file, plain or gzipped (recognised
    /// by its first two bytes).
    pub fn load(path: &Path, progress: &Progress) -> Result<Parsed> {
        let io = |cause| Error::Io {
            path: path.to_path_buf(),
            cause,
        };
        let mut bytes = std::fs::read(path).map_err(io)?;
        if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut decoder = MultiGzDecoder::new(&bytes[..]);
            let mut plain = Vec::new();
            let mut chunk = vec![0; 1 << 20];
            loop {
                if progress.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let read = decoder.read(&mut chunk).map_err(io)?;
                if read == 0 {
                    break;
                }
                plain.extend_from_slice(&chunk[..read]);
            }
            bytes = plain;
        }
        match parse_with(&bytes, progress) {
            Ok(Some(parsed)) => Ok(parsed),
            Ok(None) => Err(Error::Cancelled),
            Err(message) => Err(Error::Format {
                path: path.to_path_buf(),
                message,
            }),
        }
    }
}

/// Read the first tree in Newick text.
#[cfg(test)]
pub(crate) fn parse(text: &[u8]) -> std::result::Result<Parsed, String> {
    Ok(parse_with(text, &Progress::new())?.expect("nothing cancels it"))
}

/// Bytes that end an unquoted name or a length.
fn ends_word(b: u8) -> bool {
    b.is_ascii_whitespace() || b"()[]':;,".contains(&b)
}

struct Reader<'a> {
    text: &'a [u8],
    at: usize,
    comments: bool,
}

type Step<T> = std::result::Result<T, String>;

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    /// A message naming the line and column of byte `at`, counted from 1.
    fn error_at(&self, at: usize, message: &str) -> String {
        let before = &self.text[..at.min(self.text.len())];
        let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
        let start = before
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        let column = before[start..]
            .iter()
            .filter(|&&b| (b & 0xc0) != 0x80)
            .count()
            + 1;
        format!("line {line}, column {column}: {message}")
    }

    fn error<T>(&self, message: &str) -> Step<T> {
        Err(self.error_at(self.at, message))
    }

    /// Skip whitespace and `[…]` comments.
    fn skip(&mut self) -> Step<()> {
        loop {
            match self.peek() {
                Some(b) if b.is_ascii_whitespace() => self.at += 1,
                Some(b'[') => {
                    let start = self.at;
                    match self.text[start..].iter().position(|&b| b == b']') {
                        Some(end) => self.at = start + end + 1,
                        None => return Err(self.error_at(start, "unterminated comment")),
                    }
                    self.comments = true;
                }
                _ => return Ok(()),
            }
        }
    }

    fn utf8(&self, bytes: &[u8], at: usize) -> Step<Arc<str>> {
        match std::str::from_utf8(bytes) {
            Ok(text) => Ok(Arc::from(text)),
            Err(_) => Err(self.error_at(at, "name is not UTF-8 text")),
        }
    }

    /// A label, quoted or not, if there is one here.
    fn label(&mut self) -> Step<Option<Arc<str>>> {
        self.skip()?;
        let start = self.at;
        if self.peek() == Some(b'\'') {
            let mut name = Vec::new();
            self.at += 1;
            loop {
                match self.peek() {
                    None => return Err(self.error_at(start, "unterminated quote")),
                    Some(b'\'') if self.text.get(self.at + 1) == Some(&b'\'') => {
                        name.push(b'\'');
                        self.at += 2;
                    }
                    Some(b'\'') => {
                        self.at += 1;
                        return self.utf8(&name, start).map(Some);
                    }
                    Some(b) => {
                        name.push(b);
                        self.at += 1;
                    }
                }
            }
        }
        let end = self.text[start..]
            .iter()
            .position(|&b| ends_word(b))
            .map_or(self.text.len(), |i| start + i);
        if end == start {
            return Ok(None);
        }
        self.at = end;
        let name = self.utf8(&self.text[start..end], start)?;
        self.skip()?;
        if self.peek().is_some_and(|b| !b",);:(".contains(&b)) {
            return Err(self.error_at(start, "unquoted name with a space; quote it"));
        }
        Ok(Some(name))
    }

    /// `:length`, if there is one here.
    fn length(&mut self) -> Step<Option<f64>> {
        self.skip()?;
        if self.peek() != Some(b':') {
            return Ok(None);
        }
        self.at += 1;
        self.skip()?;
        let start = self.at;
        let end = self.text[start..]
            .iter()
            .position(|&b| ends_word(b))
            .map_or(self.text.len(), |i| start + i);
        let word = String::from_utf8_lossy(&self.text[start..end]);
        match word.parse::<f64>() {
            Ok(length) if length.is_finite() => {
                self.at = end;
                Ok(Some(length))
            }
            _ => Err(self.error_at(start, &format!("branch length '{word}' is not a number"))),
        }
    }

    /// Label and length of node `n`.
    fn finish_node(&mut self, tree: &mut Tree, n: NodeId) -> Step<()> {
        let label = self.label()?;
        tree.set_label(n, label);
        let length = self.length()?;
        tree.set_length(n, length);
        Ok(())
    }

    /// Trees after the first: each `;` outside quotes and comments ends
    /// one, and text after the last `;` is one more.
    fn count_rest(&mut self) -> usize {
        let mut trees = 0;
        let mut pending = false;
        while let Some(b) = self.peek() {
            self.at += 1;
            match b {
                b';' => {
                    trees += 1;
                    pending = false;
                }
                b'\'' => {
                    pending = true;
                    while let Some(b) = self.peek() {
                        self.at += 1;
                        if b == b'\'' {
                            break;
                        }
                    }
                }
                b'[' => {
                    while let Some(b) = self.peek() {
                        self.at += 1;
                        if b == b']' {
                            break;
                        }
                    }
                }
                b if b.is_ascii_whitespace() => {}
                _ => pending = true,
            }
        }
        trees + usize::from(pending)
    }
}

/// Whether a name must be quoted to read back as itself.
fn needs_quotes(name: &str) -> bool {
    name.is_empty()
        || name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "()[]':;,".contains(c))
}

fn write_label(name: &str, out: &mut dyn Write) -> io::Result<()> {
    if needs_quotes(name) {
        write!(out, "'{}'", name.replace('\'', "''"))
    } else {
        out.write_all(name.as_bytes())
    }
}

/// Write the clade at `from` as a whole tree, ending `;\n`. Its own branch
/// length is left out; its label is kept. Lengths are written in the
/// shortest form that reads back as the same number.
pub(crate) fn write_newick(tree: &Tree, from: NodeId, out: &mut dyn Write) -> io::Result<()> {
    enum Visit {
        Enter(NodeId),
        Exit(NodeId),
        Comma,
    }
    let finish = |n: NodeId, out: &mut dyn Write| -> io::Result<()> {
        if let Some(label) = tree.label(n) {
            write_label(label, out)?;
        }
        if n != from
            && let Some(length) = tree.length(n)
        {
            write!(out, ":{length}")?;
        }
        Ok(())
    };
    let mut stack = vec![Visit::Enter(from)];
    let mut children = Vec::new();
    while let Some(visit) = stack.pop() {
        match visit {
            Visit::Enter(n) if tree.is_tip(n) => finish(n, out)?,
            Visit::Enter(n) => {
                out.write_all(b"(")?;
                stack.push(Visit::Exit(n));
                children.clear();
                children.extend(tree.children(n));
                for (i, &child) in children.iter().enumerate().rev() {
                    stack.push(Visit::Enter(child));
                    if i > 0 {
                        stack.push(Visit::Comma);
                    }
                }
            }
            Visit::Exit(n) => {
                out.write_all(b")")?;
                finish(n, out)?;
            }
            Visit::Comma => out.write_all(b",")?,
        }
    }
    out.write_all(b";\n")
}

/// The clade at `from` as Newick text.
#[cfg(test)]
pub(crate) fn to_newick(tree: &Tree, from: NodeId) -> String {
    let mut out = Vec::new();
    write_newick(tree, from, &mut out).expect("writing to memory cannot fail");
    String::from_utf8(out).expect("names are UTF-8")
}

/// Write the clade at `from` to a new file at `out`, gzip-compressed when
/// the name ends `.gz`. Returns the file's size.
pub(crate) fn save_newick(tree: &Tree, from: NodeId, out: &Path) -> Result<u64> {
    create_text(out, |file| write_newick(tree, from, file))
}

/// Read the first tree; `None` when cancelled.
fn parse_with(text: &[u8], progress: &Progress) -> Step<Option<Parsed>> {
    progress.set_total(text.len() as u64);
    let mut r = Reader {
        text,
        at: 0,
        comments: false,
    };
    r.skip()?;
    if r.peek().is_none() {
        return Err("no tree in the file".to_string());
    }
    let mut tree = Tree::new_root();
    let mut node = tree.root();
    let mut depth = 0usize;
    let mut reported = 0;
    loop {
        if r.at - reported >= 1 << 20 {
            progress.add((r.at - reported) as u64);
            reported = r.at;
            if progress.is_cancelled() {
                return Ok(None);
            }
        }
        // At the start of a node: open its children, or read a tip.
        r.skip()?;
        if r.peek() == Some(b'(') {
            r.at += 1;
            depth += 1;
            node = tree.add_child(node);
            continue;
        }
        r.finish_node(&mut tree, node)?;
        // After a node: a sibling follows, or its parent closes.
        loop {
            r.skip()?;
            match r.peek() {
                Some(b',') if depth > 0 => {
                    r.at += 1;
                    let parent = tree.parent(node).expect("below the root");
                    node = tree.add_child(parent);
                    break;
                }
                Some(b')') if depth > 0 => {
                    r.at += 1;
                    depth -= 1;
                    node = tree.parent(node).expect("below the root");
                    r.finish_node(&mut tree, node)?;
                }
                Some(b';') | None if depth > 0 => return r.error("missing ')'"),
                Some(b';') | None => {
                    if r.peek().is_some() {
                        r.at += 1;
                    }
                    let skipped = r.count_rest();
                    progress.add((text.len() - reported) as u64);
                    return Ok(Some(Parsed {
                        tree,
                        skipped,
                        comments: r.comments,
                    }));
                }
                Some(b) => {
                    return r.error(&format!("unexpected '{}'", char::from(b)));
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    pub(crate) const QUOTED: &str = "(('Homo sapiens (human)':0.011,'Pan troglodytes, chimp':0.013)98:0.02,\
('Mus musculus':0.08,'Rattus norvegicus':0.09,'Cricetulus griseus')71:0.03,'It''s a fish':0.4,unquoted_name)root;";

    /// A tree from Newick text that must be valid.
    pub(crate) fn tree(text: &str) -> Tree {
        parse(text.as_bytes()).unwrap().tree
    }

    /// The tip names, top to bottom.
    pub(crate) fn names(t: &Tree) -> Vec<&str> {
        t.tips().into_iter().map(|n| t.label(n).unwrap()).collect()
    }

    /// `((((t0,t1),t2),t3)…,tN);` with `n` joins.
    pub(crate) fn caterpillar(n: usize, length: &str) -> String {
        let joins: String = (1..=n).map(|i| format!(",t{i}{length}){length}")).collect();
        let joins = joins.strip_suffix(length).unwrap_or(&joins);
        format!("{}t0{length}{joins};", "(".repeat(n))
    }

    #[test]
    fn quoted_names_lengths_supports_and_the_root_label_are_read() {
        let Parsed {
            tree: t,
            skipped,
            comments,
        } = parse(QUOTED.as_bytes()).unwrap();
        assert_eq!(
            names(&t),
            [
                "Homo sapiens (human)",
                "Pan troglodytes, chimp",
                "Mus musculus",
                "Rattus norvegicus",
                "Cricetulus griseus",
                "It's a fish",
                "unquoted_name"
            ]
        );
        assert_eq!(
            (skipped, comments, t.label(t.root())),
            (0, false, Some("root"))
        );
        let hominids = t.parent(t.tips()[0]).unwrap();
        assert_eq!(
            (t.support(hominids), t.length(hominids)),
            (Some("98"), Some(0.02))
        );
        assert_eq!(t.length(t.tips()[4]), None, "Cricetulus has no length");
    }

    #[test]
    fn comments_further_trees_whitespace_and_a_missing_semicolon() {
        let p = parse(b"[&R] ( A:1 ,\n B[x]:2e-3 ) ;\n(C,D);\n(E,F);").unwrap();
        assert_eq!(
            (names(&p.tree), p.skipped, p.comments),
            (vec!["A", "B"], 2, true)
        );
        assert_eq!(p.tree.length(p.tree.tips()[1]), Some(0.002));
        assert_eq!(names(&parse(b"(A,B)").unwrap().tree), ["A", "B"]);
        assert_eq!(names(&parse(b"A;").unwrap().tree), ["A"]);
        assert_eq!(parse(b"(A,B);\n\n  ").unwrap().skipped, 0);
    }

    #[test]
    fn malformed_newick_is_refused_with_where() {
        for (text, says) in [
            ("", "no tree"),
            ("(A,B;", "line 1, column 5"),
            ("(A,B));", "line 1, column 6"),
            ("(Homo sapiens,B);", "unquoted name with a space; quote it"),
            ("(A:x,B);", "branch length"),
            ("(A:inf,B);", "branch length"),
            ("('A,B);", "quote"),
            ("(A,\nB)[x;", "line 2"),
        ] {
            let err = parse(text.as_bytes()).unwrap_err();
            assert!(err.contains(says), "{text}: {err}");
        }
    }

    #[test]
    fn a_deep_caterpillar_is_read_without_recursion() {
        let text = caterpillar(200_000, "");
        assert_eq!(parse(text.as_bytes()).unwrap().tree.tip_count(), 200_001);
    }

    #[test]
    fn a_gzipped_file_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nwk.gz");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, b"(A,B);").unwrap();
        std::fs::write(&path, gz.finish().unwrap()).unwrap();
        let parsed = Tree::load(&path, &Progress::new()).unwrap();
        assert_eq!(names(&parsed.tree), ["A", "B"]);
    }

    /// Random trees of up to 30 tips: any shape, names of any text or
    /// none, lengths missing or any finite number, internal labels missing,
    /// numeric or text. With `unique`, every tip has its own name.
    pub(crate) fn arb_tree(unique: bool) -> impl Strategy<Value = Tree> {
        let name = proptest::option::of("[^\\x00]{0,6}");
        let length = proptest::option::of(any::<f64>().prop_filter("finite", |l| l.is_finite()));
        let label = proptest::option::of(prop_oneof!["[0-9]{1,3}(/[0-9]{1,2})?", "[a-z ]{1,4}"]);
        (2usize..30).prop_flat_map(move |tips| {
            (
                proptest::collection::vec((name.clone(), length.clone()), tips),
                proptest::collection::vec((any::<prop::sample::Index>(), 2usize..4), tips),
                proptest::collection::vec((label.clone(), length.clone()), tips),
            )
                .prop_map(move |(tips, merges, inner)| {
                    let mut t = Tree::new_root();
                    let mut items = Vec::new();
                    for (i, (name, length)) in tips.into_iter().enumerate() {
                        let n = t.add_child(t.root());
                        t.detach(n);
                        let name = if unique {
                            Some(format!("{}#{i}", name.unwrap_or_default()))
                        } else {
                            name
                        };
                        t.set_label(n, name.map(Arc::from));
                        t.set_length(n, length);
                        items.push(n);
                    }
                    let mut merges = merges.into_iter().zip(inner).cycle();
                    while items.len() > 1 {
                        let ((at, width), (label, length)) = merges.next().unwrap();
                        let width = width.min(items.len());
                        let start = at.index(items.len() - width + 1);
                        let node = t.add_child(t.root());
                        t.detach(node);
                        for child in items.drain(start..start + width) {
                            t.attach(node, child);
                        }
                        t.set_label(node, label.map(Arc::from));
                        t.set_length(node, length);
                        items.insert(start, node);
                    }
                    t.set_root(items[0]);
                    t
                })
        })
    }

    #[test]
    fn trees_are_written_back_as_read() {
        for text in [
            QUOTED,
            "((A:1,B:2)90:3,C:4);",
            "(A,B,(C,D)81/95:0.5)root:0;",
            "(-x,b);",
            "(a_b,'é ü');",
        ] {
            let t = tree(text);
            let expect = text.replace("root:0;", "root;"); // the root's own length is not written
            assert_eq!(to_newick(&t, t.root()), format!("{expect}\n"));
        }
    }

    #[test]
    fn a_clade_is_written_as_a_whole_tree() {
        let t = tree("((A:1,B:2)90:3,C:4);");
        let ab = t.parent(t.tips()[0]).unwrap();
        assert_eq!(to_newick(&t, ab), "(A:1,B:2)90;\n");
        assert_eq!(to_newick(&t, t.tips()[2]), "C;\n");
    }

    #[test]
    fn a_name_is_quoted_only_when_it_must_be() {
        let mut t = tree("(A,B);");
        let a = t.tips()[0];
        for (name, written) in [
            ("a_b", "a_b"),
            ("a b", "'a b'"),
            ("It's", "'It''s'"),
            ("x,y", "'x,y'"),
            ("(p)", "'(p)'"),
            ("", "''"),
            ("é", "é"),
            ("a\u{a0}b", "'a\u{a0}b'"),
        ] {
            t.set_label(a, Some(name.into()));
            assert_eq!(
                to_newick(&t, t.root()),
                format!("({written},B);\n"),
                "{name}"
            );
        }
    }

    proptest! {
        #[test]
        fn writing_then_reading_gives_the_same_tree(t in arb_tree(false)) {
            let again = tree(&to_newick(&t, t.root()));
            prop_assert_eq!(to_newick(&again, again.root()), to_newick(&t, t.root()));
            prop_assert_eq!(names_or_none(&again), names_or_none(&t));
        }
    }

    fn names_or_none(t: &Tree) -> Vec<Option<String>> {
        t.preorder()
            .into_iter()
            .map(|n| t.label(n).map(str::to_string))
            .collect()
    }

    #[test]
    fn a_deep_caterpillar_is_written_without_recursion() {
        let text = caterpillar(200_000, "");
        let t = tree(&text);
        assert_eq!(to_newick(&t, t.root()), format!("{text}\n"));
    }

    #[test]
    fn a_tree_file_is_only_ever_created() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("t.nwk");
        let t = tree("(A,B);");
        assert_eq!(save_newick(&t, t.root(), &out).unwrap(), 7);
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "(A,B);\n");
        assert!(matches!(
            save_newick(&t, t.root(), &out),
            Err(Error::Exists(_))
        ));
        let gz = dir.path().join("t.nwk.gz");
        save_newick(&t, t.root(), &gz).unwrap();
        assert_eq!(
            names(&Tree::load(&gz, &Progress::new()).unwrap().tree),
            ["A", "B"]
        );
    }

    #[test]
    fn a_file_that_is_not_newick_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nwk");
        std::fs::write(&path, ">a\nACGT\n").unwrap();
        let err = Tree::load(&path, &Progress::new()).unwrap_err().to_string();
        assert!(
            err.starts_with(&format!("{}: line 1", path.display())),
            "{err}"
        );
    }
}
