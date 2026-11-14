//! Offline grammar contracts without creating a project or starting a host.

#[allow(dead_code, unused_imports)]
#[path = "../src/args.rs"]
mod args;
#[allow(dead_code)]
#[path = "../src/terminal.rs"]
mod terminal;

use args::{Cli, Command};
use clap::{CommandFactory, Parser};

fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("agentchat").chain(arguments.iter().copied()))
}

#[test]
fn actor_selection_is_exact_complete_bounded_and_command_applicable() {
    for host in ["codex", "claude"] {
        let cli = parse(&[
            "--by-host",
            host,
            "--by-agent-id",
            "Opaque:日本語-ID",
            "whoami",
        ])
        .unwrap();
        cli.actor.validate(&cli.command).unwrap();
    }
    for arguments in [
        vec!["--by-host", "Codex", "--by-agent-id", "id", "whoami"],
        vec!["--by-host", "cLaude", "--by-agent-id", "id", "whoami"],
        vec!["--by-host", "codex", "whoami"],
        vec!["--by-agent-id", "id", "whoami"],
        vec![
            "--by-host",
            "codex",
            "--by-agent-id",
            "id",
            "--by-user",
            "whoami",
        ],
    ] {
        assert!(parse(&arguments).is_err(), "{arguments:?}");
    }
    for id in [String::new(), "あ".repeat(342)] {
        let cli = parse(&["--by-host", "codex", "--by-agent-id", &id, "whoami"]).unwrap();
        assert!(cli.actor.validate(&cli.command).is_err());
    }
    for arguments in [vec!["whoami"], vec!["join"], vec!["leave"]] {
        let cli = parse(&arguments).unwrap();
        assert!(cli.actor.validate(&cli.command).is_err(), "{arguments:?}");
    }
    for arguments in [
        vec!["--by-user", "viewer"],
        vec!["--by-user", "launch", "codex"],
        vec!["--by-user", "mcp"],
        vec!["--by-user", "install"],
        vec!["--by-user", "uninstall"],
        vec!["--by-user", "join", "--name", "other"],
    ] {
        let cli = parse(&arguments).unwrap();
        assert!(cli.actor.validate(&cli.command).is_err(), "{arguments:?}");
    }
    for arguments in [
        vec!["--by-user", "join"],
        vec!["--by-user", "whoami"],
        vec!["history"],
        vec!["--by-user", "history"],
    ] {
        let cli = parse(&arguments).unwrap();
        cli.actor.validate(&cli.command).unwrap();
    }
}

#[test]
fn exactly_nine_leaf_commands_accept_finite_pages() {
    let message = "00000000-0000-0000-0000-000000000001";
    for leaf in [
        vec!["agents", "list"],
        vec!["channel", "list"],
        vec!["channel", "members", "dev"],
        vec!["message", message],
        vec!["history"],
        vec!["search", "literal"],
        vec!["inbox"],
        vec!["events"],
        vec!["reservations", "list"],
    ] {
        let mut valid = leaf.clone();
        valid.extend(["--limit", "200", "--after", "9223372036854775807"]);
        assert!(parse(&valid).is_ok(), "{valid:?}");
        for limit in ["0", "201", "-1", "1.5", "huge"] {
            let mut invalid = leaf.clone();
            invalid.extend(["--limit", limit]);
            assert!(parse(&invalid).is_err(), "{invalid:?}");
        }
        let mut invalid = leaf;
        invalid.extend(["--after", "-1"]);
        assert!(parse(&invalid).is_err(), "{invalid:?}");
    }
    for arguments in [
        vec!["--limit", "20", "history"],
        vec!["agents", "--limit", "20", "list"],
        vec!["channel", "--after", "0", "list"],
        vec!["whoami", "--limit", "20"],
        vec!["post", "--body", "body", "--after", "0"],
    ] {
        assert!(parse(&arguments).is_err(), "{arguments:?}");
    }
    let mut root = Cli::command();
    for command in ["", "agents", "channel", "reservations"] {
        let help = if command.is_empty() {
            root.render_long_help().to_string()
        } else {
            root.find_subcommand_mut(command)
                .unwrap()
                .render_long_help()
                .to_string()
        };
        assert!(!help.contains("--limit"), "{command}: {help}");
        assert!(!help.contains("--after"), "{command}: {help}");
    }
}

#[test]
fn obsolete_public_names_and_options_are_rejected_without_aliases() {
    for arguments in [
        vec!["status"],
        vec!["backup", "copy.sqlite3"],
        vec!["watch"],
        vec!["delivery", "ack"],
        vec![
            "--context",
            "00000000-0000-0000-0000-000000000001",
            "whoami",
        ],
        vec!["--agent-id", "id", "whoami"],
        vec!["--user", "whoami"],
        vec![
            "mcp",
            "--restore-context",
            "00000000-0000-0000-0000-000000000001",
        ],
        vec!["post", "--body", "body", "--read-timeout", "5m"],
        vec!["events", "--since", "0"],
    ] {
        assert!(parse(&arguments).is_err(), "{arguments:?}");
    }
    assert!(parse(&["ack", "00000000-0000-0000-0000-000000000001"]).is_err());
    assert!(parse(&["install", "--auto-ack", "true"]).is_err());
}

#[test]
fn installation_preserves_explicit_false_and_offline_help_short_circuits_actor_checks() {
    let cli = parse(&[
        "install",
        "--auto-read",
        "false",
        "--delivery-confirmation-timeout",
        "5m",
    ])
    .unwrap();
    let Command::Init {
        auto_read,
        delivery_confirmation_timeout,
        ..
    } = cli.command
    else {
        panic!("install")
    };
    assert_eq!(auto_read, Some(false));
    assert_eq!(delivery_confirmation_timeout.as_deref(), Some("5m"));
    for arguments in [
        vec!["--by-user", "viewer", "--help"],
        vec!["whoami", "--help"],
        vec!["--version"],
    ] {
        let error = parse(&arguments).unwrap_err();
        assert!(matches!(
            error.kind(),
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
        ));
    }
}
