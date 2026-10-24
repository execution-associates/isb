//! `isb skill`: isb's agent skill, the one `isb serve` serves over MCP at
//! `skill://isb/`, compiled into this binary so it matches its version.

use super::*;

/// The files of isb's skill: `SKILL.md` and the docs it links to, at the
/// same paths, so its relative links resolve under `skill://isb/`.
const SKILL_FILES: &[(&str, &str)] = &[
    ("SKILL.md", include_str!("../../../SKILL.md")),
    ("docs/index.md", include_str!("../../../docs/index.md")),
    (
        "docs/guides/agents.md",
        include_str!("../../../docs/guides/agents.md"),
    ),
    (
        "docs/guides/egress.md",
        include_str!("../../../docs/guides/egress.md"),
    ),
    (
        "docs/guides/isbx.md",
        include_str!("../../../docs/guides/isbx.md"),
    ),
    (
        "docs/reference/compose.md",
        include_str!("../../../docs/reference/compose.md"),
    ),
    (
        "docs/reference/mcp-tools.md",
        include_str!("../../../docs/reference/mcp-tools.md"),
    ),
];

#[derive(Args)]
pub(crate) struct SkillArgs {
    /// A file of the skill to print instead of SKILL.md, e.g.
    /// docs/reference/compose.md.
    path: Option<String>,
    /// List the skill's files.
    #[arg(long, conflicts_with = "path")]
    list: bool,
}

/// isb's own skill, as `isb serve` serves it.
pub(crate) fn skill() -> Result<isb::server::Skill> {
    isb::server::Skill::new(SKILL_FILES.iter().copied())
}

pub(crate) fn print(a: SkillArgs) -> Result<u8> {
    if a.list {
        for (p, _) in SKILL_FILES {
            println!("{p}");
        }
        return Ok(0);
    }
    let path = a.path.as_deref().unwrap_or("SKILL.md");
    let Some((_, text)) = SKILL_FILES.iter().find(|(p, _)| *p == path) else {
        return Err(Error::Invalid(format!(
            "the isb skill has no {path}; isb skill --list shows its files"
        )));
    };
    print!("{text}");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_serves_every_file_it_links_to() {
        let s = skill().unwrap();
        assert_eq!(s.name(), "isb");
        let md = SKILL_FILES[0].1;
        let served: Vec<&str> = SKILL_FILES.iter().map(|(p, _)| *p).collect();
        for link in md.split("](").skip(1).filter_map(|l| l.split(')').next()) {
            if link.contains("://") || link.starts_with('#') {
                continue;
            }
            let path = link.split('#').next().unwrap();
            assert!(
                served.contains(&path),
                "SKILL.md links {path}, which the skill does not serve"
            );
        }
    }
}
