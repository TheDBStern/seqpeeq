//! Write the example files: a small alignment, its tree, a reference and two
//! BAMs with their indexes. See `examples/README.md` for what each shows.
//!
//! cargo run -p seqpeeq-core --features testing --example make_examples -- examples
//!
//! The output is the same on every run. Existing files are replaced.

use std::path::PathBuf;

use seqpeeq_core::{Command, Session, demo};

fn main() -> std::io::Result<()> {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "examples".into()));
    std::fs::create_dir_all(&dir)?;
    // A BAM newer than its index makes seqpeeq ignore the index and build a
    // fresh one in its cache, so old indexes go first.
    for name in demo::FILES {
        let bai = dir.join(format!("{name}.bai"));
        if bai.exists() {
            std::fs::remove_file(bai)?;
        }
    }
    demo::write_all(&dir)?;
    let cache = std::env::temp_dir().join(format!("seqpeeq-make-examples-{}", std::process::id()));
    for name in ["reads.bam", "reads2.bam"] {
        let mut session = Session::new(None);
        session.set_cache_dir(cache.clone());
        let out = session
            .execute(Command::Open {
                path: dir.join(name),
                kind: None,
            })
            .expect("the BAM opens");
        println!("{name}: {}", out.message.replace('\n', "; "));
    }
    let _ = std::fs::remove_dir_all(&cache);
    let mut names: Vec<String> = demo::FILES.iter().map(|f| f.to_string()).collect();
    names.extend(["reads.bam.bai".into(), "reads2.bam.bai".into()]);
    for name in names {
        let size = std::fs::metadata(dir.join(&name))?.len();
        println!("{:>16} {size:>8} bytes", name);
    }
    Ok(())
}
