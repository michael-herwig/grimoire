// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The Agent Skills standard: `SKILL.md`/rule frontmatter, validation, and
//! packing.
//!
//! This subsystem turns a local skill directory or rule file into the
//! exact uncompressed-tar layout the install materializer extracts, and
//! back — the build/release ↔ install round-trip. Spec:
//! <https://agentskills.io/specification>.

pub mod agent_frontmatter;
pub mod rule_frontmatter;
pub mod skill_description;
pub mod skill_error;
pub mod skill_frontmatter;
pub mod skill_name;
pub mod skill_package;

#[allow(unused_imports)]
pub use agent_frontmatter::{AgentFrontmatter, ParsedAgent};
#[allow(unused_imports)]
pub use rule_frontmatter::{ParsedRule, RuleFrontmatter};
#[allow(unused_imports)]
pub use skill_description::SkillDescription;
#[allow(unused_imports)]
pub use skill_error::{SkillError, SkillErrorKind};
#[allow(unused_imports)]
pub use skill_frontmatter::SkillFrontmatter;
#[allow(unused_imports)]
pub use skill_name::SkillName;
#[allow(unused_imports)]
pub use skill_package::{
    pack_agent_file, pack_rule_file, pack_skill_dir, validate_agent_file, validate_rule_file, validate_skill_dir,
};
