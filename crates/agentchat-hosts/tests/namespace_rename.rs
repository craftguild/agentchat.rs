//! New integration ownership coexists with every deliberately retained old entry.
use agentchat_core::model::HostKind;
use agentchat_hosts::{install::Installer, setup::Environment};
use serde_json::{Value, json};
use std::fs;

#[tokio::test]
async fn new_tool_install_preserves_old_entries_hooks_skills_and_executable() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    let home = temp.path().join("new-state");
    let roots = Environment {
        user_config_root: Some(config.clone()),
        ..Environment::default()
    }
    .resolve()
    .unwrap();
    fs::create_dir_all(roots.codex_config.parent().unwrap()).unwrap();
    fs::create_dir_all(roots.claude_settings.parent().unwrap()).unwrap();
    fs::write(
        &roots.codex_config,
        "[mcp_servers.ac]\ncommand='/old/bin/ac'\n",
    )
    .unwrap();
    let old = json!({"command":"/old/bin/ac","args":["mcp"],"env":{"AC_HOME":"/old/state"}});
    fs::write(
        &roots.claude_registry,
        json!({"mcpServers":{"ac":old}}).to_string(),
    )
    .unwrap();
    let settings = b"{\"hooks\":{\"SessionStart\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"/old/bin/ac hook\"}]}]}}";
    fs::write(&roots.claude_settings, settings).unwrap();
    let old_skills = [&roots.codex_skill, &roots.claude_skill].map(|skill| {
        skill
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("ac/SKILL.md")
    });
    for skill in &old_skills {
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(skill, b"legacy skill sentinel").unwrap();
    }
    let old_binary = temp.path().join("ac");
    fs::write(&old_binary, b"legacy executable sentinel").unwrap();
    let binary = temp.path().join("agentchat");
    let hosts = [HostKind::Codex, HostKind::Claude];
    let installer = Installer::new(&home, roots.tool_roots(&hosts)).unwrap();
    installer
        .install(roots.tool_artifacts(&hosts, &binary, &home).unwrap())
        .await
        .unwrap();
    let toml = fs::read_to_string(&roots.codex_config).unwrap();
    assert!(toml.contains("[mcp_servers.ac]") && toml.contains("[mcp_servers.agentchat]"));
    let registry: Value =
        serde_json::from_slice(&fs::read(&roots.claude_registry).unwrap()).unwrap();
    assert_eq!(registry["mcpServers"]["ac"], old);
    let new = &registry["mcpServers"]["agentchat"];
    assert_eq!(new["command"], binary.to_str().unwrap());
    assert_eq!(new["env"]["AGENTCHAT_HOME"], home.to_str().unwrap());
    assert!(new["env"].get("AC_HOME").is_none());
    assert_eq!(fs::read(&roots.claude_settings).unwrap(), settings);
    for skill in &old_skills {
        assert_eq!(fs::read(skill).unwrap(), b"legacy skill sentinel");
    }
    assert_eq!(
        fs::read(&old_binary).unwrap(),
        b"legacy executable sentinel"
    );
    for skill in [&roots.codex_skill, &roots.claude_skill] {
        let text = fs::read_to_string(skill).unwrap();
        assert!(text.starts_with("---\nname: agentchat\n"));
        assert!(text.contains("[AgentChat:Message]") && text.contains("[AgentChat:Notice]"));
    }
}
