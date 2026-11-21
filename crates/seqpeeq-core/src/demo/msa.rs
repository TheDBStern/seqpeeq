//! The example alignment and its tree.

/// Columns in the example alignment.
pub const COLUMNS: usize = 900;

use super::Lcg;

/// Each lineage: its name, the year in its tips' names, and how many tips.
const LINEAGES: [(&str, &str, usize); 5] = [
    ("Alpha", "2019", 5),
    ("Beta", "2020", 4),
    ("Gamma", "2020", 4),
    ("Delta", "2021", 6),
    ("Omicron", "2021", 5),
];

/// A node of the tree being built: a tip has a name and no children.
struct Node {
    name: Option<String>,
    length: f64,
    support: Option<u64>,
    children: Vec<Node>,
}

impl Node {
    fn joined(rng: &mut Lcg, children: Vec<Node>, support: Option<u64>) -> Node {
        Node {
            name: None,
            length: branch_length(rng),
            support,
            children,
        }
    }

    fn newick(&self, out: &mut String, root: bool) {
        if self.children.is_empty() {
            out.push_str(self.name.as_deref().unwrap_or_default());
        } else {
            out.push('(');
            for (i, child) in self.children.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                child.newick(out, false);
            }
            out.push(')');
            if let Some(support) = self.support {
                out.push_str(&support.to_string());
            }
        }
        if !root {
            out.push_str(&format!(":{:.4}", self.length));
        }
    }

    /// Mutate `seq` along this node's branch, then hand it to the children;
    /// tips land in `rows` in pre-order.
    fn evolve(
        &self,
        rng: &mut Lcg,
        mut seq: Vec<u8>,
        root: bool,
        rows: &mut Vec<(String, Vec<u8>)>,
    ) {
        if !root {
            let changes = 1 + (self.length * COLUMNS as f64) as u64;
            for _ in 0..changes {
                let col = rng.range(0, COLUMNS as u64 - 1) as usize;
                let mut base = rng.base();
                while base == seq[col] {
                    base = rng.base();
                }
                seq[col] = base;
            }
        }
        match &self.name {
            Some(name) => rows.push((name.clone(), seq)),
            None => {
                for child in &self.children {
                    child.evolve(rng, seq.clone(), false, rows);
                }
            }
        }
    }
}

/// 0.001 to 0.050.
fn branch_length(rng: &mut Lcg) -> f64 {
    rng.range(10, 500) as f64 / 10_000.0
}

/// A random binary tree on `tips`, by splitting the list at random points.
fn clade(rng: &mut Lcg, tips: &[String]) -> Node {
    if let [only] = tips {
        return Node {
            name: Some(only.clone()),
            length: branch_length(rng),
            support: None,
            children: Vec::new(),
        };
    }
    let at = rng.range(1, tips.len() as u64 - 1) as usize;
    let children = vec![clade(rng, &tips[..at]), clade(rng, &tips[at..])];
    Node::joined(rng, children, None)
}

/// The example alignment as FASTA text (one line per sequence) and its tree
/// as one line of Newick.
pub fn alignment_and_tree(seed: u64) -> (String, String) {
    let mut rng = Lcg(seed);
    let mut lineages = LINEAGES
        .iter()
        .map(|&(lineage, year, n)| {
            let mut tips: Vec<String> = (1..=n).map(|i| format!("{lineage}_{year}.{i}")).collect();
            // Fisher-Yates, so the tips aren't drawn in number order.
            for i in (1..tips.len()).rev() {
                tips.swap(i, rng.range(0, i as u64) as usize);
            }
            let mut root = clade(&mut rng, &tips);
            root.support = Some(rng.range(70, 100));
            root
        })
        .collect::<Vec<_>>()
        .into_iter();
    // ((((Alpha,Beta),Gamma),Delta),Omicron)
    let mut tree = lineages.next().expect("lineages");
    for lineage in lineages {
        let support = Some(rng.range(70, 100));
        tree = Node::joined(&mut rng, vec![tree, lineage], support);
    }
    tree.support = None;

    let root_seq: Vec<u8> = (0..COLUMNS).map(|_| rng.base()).collect();
    let mut rows = Vec::new();
    tree.evolve(&mut rng, root_seq, true, &mut rows);

    let mut fasta = String::new();
    for (name, mut seq) in rows {
        let mut gap = |a: usize, b: usize| seq[a - 1..b].fill(b'-');
        if name.starts_with("Omicron") {
            gap(301, 309);
        }
        if !name.starts_with("Delta") {
            gap(611, 622);
        }
        if name.starts_with("Beta") || name.starts_with("Gamma") {
            gap(881, 900);
        }
        fasta.push_str(&format!(
            ">{name}\n{}\n",
            String::from_utf8(seq).expect("ASCII")
        ));
    }
    let mut newick = String::new();
    tree.newick(&mut newick, true);
    newick.push_str(";\n");
    (fasta, newick)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_fasta(text: &str) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        for line in text.lines() {
            match line.strip_prefix('>') {
                Some(name) => rows.push((name.to_string(), String::new())),
                None => rows.last_mut().expect("a name first").1.push_str(line),
            }
        }
        rows
    }

    #[test]
    fn the_same_seed_gives_the_same_files() {
        let (fasta, newick) = alignment_and_tree(1);
        assert!(!fasta.is_empty() && newick.ends_with(";\n"));
        assert_eq!((fasta, newick), alignment_and_tree(1));
    }

    #[test]
    fn tips_and_rows_match_and_have_planted_gaps() {
        let (fasta, newick) = alignment_and_tree(1);
        let rows = parse_fasta(&fasta);
        assert_eq!(rows.len(), 24);
        assert!(rows.iter().all(|(_, s)| s.len() == COLUMNS));
        let tree = crate::tree::newick::parse(newick.as_bytes()).unwrap().tree;
        let mut tips: Vec<String> = tree
            .tips()
            .into_iter()
            .map(|n| tree.label(n).unwrap().to_string())
            .collect();
        let mut names: Vec<String> = rows.iter().map(|(n, _)| n.clone()).collect();
        // Rows come in the tree's pre-order.
        assert_eq!(tips, names);
        tips.sort();
        names.sort();
        assert_eq!(tips, names);
        assert!(names.contains(&"Alpha_2019.2".into()) && names.contains(&"Delta_2021.1".into()));
        for (name, seq) in &rows {
            let col = |a: usize, b: usize| &seq[a - 1..b];
            assert_eq!(
                col(301, 309) == "-".repeat(9),
                name.starts_with("Omicron"),
                "{name}"
            );
            assert_eq!(
                col(611, 622) == "-".repeat(12),
                !name.starts_with("Delta"),
                "{name}"
            );
            let ragged = name.starts_with("Beta") || name.starts_with("Gamma");
            assert_eq!(col(881, 900) == "-".repeat(20), ragged, "{name}");
            assert!(col(1, 300).bytes().all(|b| b"ACGT".contains(&b)), "{name}");
        }
    }

    #[test]
    fn supports_and_lengths_are_written() {
        let (_, newick) = alignment_and_tree(1);
        let tree = crate::tree::newick::parse(newick.as_bytes()).unwrap().tree;
        let supports = tree
            .preorder()
            .into_iter()
            .filter(|&n| tree.support(n).is_some())
            .count();
        assert!(supports >= 8, "{supports} supports");
        assert!(
            tree.preorder()
                .into_iter()
                .skip(1)
                .all(|n| tree.length(n).is_some())
        );
    }
}
