//! Scratch harness: dump one change's refined diff from a real repo.
//! Usage: cargo run -p jiji-core --example dump_diff -- <repo> <change_id> [path-substring]

use jiji_core::backend::RepoBackend;
use jiji_core::jj::JjBackend;
use jiji_core::snapshot::{DiffLineKind, FileDiffContent};

fn main() {
    let mut args = std::env::args().skip(1);
    let repo = std::path::PathBuf::from(args.next().expect("repo path"));
    let change = args.next().expect("change id");
    let filter = args.next().unwrap_or_default();

    let backend = JjBackend::default();
    let diff = backend.change_diff(&repo, &change).expect("change_diff");
    for file in &diff.files {
        if !file.path.contains(&filter) {
            continue;
        }
        println!("=== {} ===", file.path);
        let FileDiffContent::Text { hunks, .. } = &file.content else {
            println!("(non-text)");
            continue;
        };
        for (h, hunk) in hunks.iter().enumerate() {
            println!(
                "--- hunk {h} @@ -{},+{} ctx={:?}",
                hunk.old_start, hunk.new_start, hunk.context
            );
            for (k, line) in hunk.lines.iter().enumerate() {
                let mark = match line.kind {
                    DiffLineKind::Context => ' ',
                    DiffLineKind::Removed => '-',
                    DiffLineKind::Added => '+',
                };
                let pair = match line.pair {
                    Some(p) => format!("»{p:<3}"),
                    None => "    ".to_string(),
                };
                let text: String = line
                    .segments
                    .iter()
                    .map(|s| {
                        if s.changed {
                            format!("⟦{}⟧", s.text)
                        } else {
                            s.text.clone()
                        }
                    })
                    .collect();
                println!("{k:>3} {mark} {pair} {text}");
            }
        }
    }
}
