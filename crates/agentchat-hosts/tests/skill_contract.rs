//! The installed skill is product policy: protect its decision boundaries.

const SKILL: &str = include_str!("../assets/skills/agentchat/SKILL.md");

fn contains_all(section: &str, terms: &[&str]) {
    for term in terms {
        assert!(section.contains(term), "missing skill contract: {term}");
    }
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn section(start: &str, end: &str) -> String {
    let skill = normalized(SKILL);
    let (_, tail) = skill.split_once(start).expect("missing skill section");
    tail.split_once(end)
        .map_or(tail, |(section, _)| section)
        .to_owned()
}

fn excludes_all(section: &str, terms: &[&str]) {
    for term in terms {
        assert!(
            !section.contains(term),
            "contradictory skill policy: {term}"
        );
    }
}

#[test]
fn skill_describes_managed_activation_without_the_explicit_only_baseline() {
    let lifecycle = section(
        "## Launch identity and participation",
        "## Select the collaboration source",
    );
    contains_all(
        &lifecycle,
        &[
            "joins automatically outside the model",
            "startup participation is not an instruction to call `agentchat_join`",
            "same-project Codex `/new` and `/resume`",
            "same-root recovery left",
            "distinct confirmed root",
            "cross-project selections do not grant target participation",
            "auto-enrolls only the configured project defaults",
            "does not inherit the predecessor conversation's non-default or Epic memberships",
            "explicitly Join each relevant Epic channel again",
        ],
    );
    excludes_all(
        &lifecycle,
        &[
            "establish managed authorization and start unjoined",
            "In-process `/new`, `/resume` and `/fork` switching is unsupported",
            "Call `agentchat_join` only when the user explicitly asks to participate",
            "inherits every Epic membership",
        ],
    );
}

#[test]
fn collaboration_stays_provider_independent_and_member_scoped() {
    let source = section(
        "## Select the collaboration source",
        "## Join the Epic conversation",
    );
    contains_all(
        &source,
        &[
            "Use the declared existing source and its own skill",
            "Beads, GitHub and other providers are examples",
            "label new agreements provisional",
            "do not claim a source update or silently",
            "preserve newer ownership/completion",
            "without replaying",
        ],
    );
    for embedded_provider_command in ["`bd ", "`gh ", "bd update", "gh issue"] {
        assert!(
            !source.contains(embedded_provider_command),
            "provider command leaked into shared skill: {embedded_provider_command}"
        );
    }

    let epic = section("## Join the Epic conversation", "## Receive and respond");
    contains_all(
        &epic,
        &[
            "state: \"all\"",
            "channel creation enrolls nobody",
            "Multiple simultaneous Epic memberships are allowed",
            "Never bypass membership with observer reads",
        ],
    );
    excludes_all(
        &epic,
        &[
            "channel creation enrolls its creator",
            "observer reads may bypass membership",
            "automatically Join every Epic",
        ],
    );

    let receiving = section("## Receive and respond", "## Execute and delegate");
    contains_all(
        &receiving,
        &[
            "explicitly mention the requester",
            "explicit mentions for everyone who must act",
            "do not implicitly wake their author",
        ],
    );
    excludes_all(
        &receiving,
        &[
            "A reply implicitly mentions its author",
            "tracker assignment wakes the recipient",
        ],
    );

    let execution = section("## Execute and delegate", "## Calls and recovery");
    contains_all(
        &execution,
        &[
            "early design consultation",
            "`mention_results` and `warnings`",
            "do not poll models, inboxes or trackers idly",
        ],
    );
    excludes_all(
        &execution,
        &["poll inboxes periodically", "schedule idle tracker polling"],
    );
}

#[test]
fn delivery_facts_and_task_ownership_remain_distinct() {
    contains_all(
        &normalized(SKILL),
        &[
            "Complete native acceptance settles delivery",
            "fixed deadline starts at first processing",
            "reply `read_message_ids` are optional",
            "Delivery, either read kind and substantive task acceptance differ",
            "Do not poll inboxes or receipts idly",
            "Reading is not acceptance",
            "obtain explicit substantive acceptance",
            "Reservations do not enforce OS writes",
            "Do not invent a binding, source update",
        ],
    );
}

#[test]
fn current_guidance_has_no_removed_ack_operation_or_policy() {
    excludes_all(
        SKILL,
        &[
            "`agentchat_ack`",
            "`--auto-ack",
            "manual-ACK",
            "true/true/60",
        ],
    );
    contains_all(
        SKILL,
        &[
            "`failed_timeout`",
            "`delivered_late`",
            "true/3600",
            "first processing",
            "`agentchat install --new-project`",
        ],
    );
}
